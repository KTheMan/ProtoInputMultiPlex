//! Explicit, disposable-process DLL lifetime qualification; never run implicitly.
//! This peer uses the production protocol/Host/Engine, not GUI Service admission.
//! No graphics, device enumeration, physical input, hooks or injection are used.
#[cfg(not(windows))]
fn main() {
    eprintln!("native_dll_lifecycle requires Windows and --features native-client");
    std::process::exit(10);
}

#[cfg(windows)]
fn main() {
    fixture::main();
}

#[cfg(windows)]
mod fixture {
    use futures_util::{SinkExt, StreamExt};
    use mux_application_sdk::{
        ClientCommand, ClientMessage, ClientRequest, ClientResponse, ControlGrant, EventHello,
        Grant, Host, Input, KeyCode, PROTOCOL_VERSION, Point, Reply, ScreenState, ServerMessage,
        Transition, UserId, UserScope,
    };
    use serde_json::{Value, json};
    use std::{
        cell::Cell,
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        panic::{AssertUnwindSafe, catch_unwind},
        path::{Path, PathBuf},
        ptr::{null, null_mut},
        sync::{Mutex, mpsc},
        thread,
        time::{Duration, Instant},
    };
    use tokio::net::TcpListener;
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{
            Message,
            handshake::server::{Request, Response},
        },
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, DuplicateHandle, FreeLibrary, HANDLE, HMODULE, HWND, LPARAM, LRESULT,
            WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
        },
        System::{
            LibraryLoader::{
                GetModuleFileNameW, GetModuleHandleW, GetProcAddress,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
            },
            Threading::{GetCurrentProcess, THREAD_SYNCHRONIZE, WaitForSingleObject},
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::*,
        },
    };

    const OK: i32 = 0;
    const INVALID: i32 = 1;
    const INVALID_HANDLE: i32 = 2;
    const BUSY: i32 = 3;
    const SHORT: i32 = 4;
    const NO_RESPONSE: i32 = 5;
    const BOUND: Duration = Duration::from_secs(15);
    type Check<T = ()> = Result<T, String>;
    type Connect = unsafe extern "C" fn(usize, *const u8, usize, *mut u64, *mut u32) -> i32;
    type Enable = unsafe extern "C" fn(u64, u32, u32) -> i32;
    type RequestFn = unsafe extern "C" fn(u64, *const u8, usize, *mut u64) -> i32;
    type Unary = unsafe extern "C" fn(u64) -> i32;
    type Size = unsafe extern "C" fn(u64, *mut usize) -> i32;
    type ReadFn = unsafe extern "C" fn(u64, *mut u8, usize, *mut usize) -> i32;

    // No Drop impl: failures must never unload a DLL with unproven lifetime.
    #[derive(Clone, Copy)]
    struct Api {
        connect: Connect,
        enable: Enable,
        request: RequestFn,
        drain: Unary,
        query: Unary,
        size: Size,
        read: ReadFn,
        discard: Unary,
        stop: Unary,
        completion: Size,
        destroy: Unary,
    }
    impl Api {
        unsafe fn load(module: HMODULE) -> Check<Self> {
            macro_rules! symbol {
                ($name:literal, $ty:ty) => {{
                    let raw = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) }
                        .ok_or_else(|| concat!("missing export ", $name).to_owned())?;
                    // Exact C calling convention and widths from the generated ABI v2 header.
                    unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(raw) }
                }};
            }
            Ok(Self {
                connect: symbol!("pim_app_native_connect", Connect),
                enable: symbol!("pim_app_native_enable_input", Enable),
                request: symbol!("pim_app_native_request", RequestFn),
                drain: symbol!("pim_app_native_drain", Unary),
                query: symbol!("pim_app_native_query", Unary),
                size: symbol!("pim_app_native_response_size", Size),
                read: symbol!("pim_app_native_response_read", ReadFn),
                discard: symbol!("pim_app_native_response_discard", Unary),
                stop: symbol!("pim_app_native_stop", Unary),
                completion: symbol!("pim_app_native_completion_handle", Size),
                destroy: symbol!("pim_app_native_destroy", Unary),
            })
        }
    }
    thread_local! {
        static WINDOW_API: Cell<Option<Api>> = const { Cell::new(None) };
        static TOKEN: Cell<u64> = const { Cell::new(0) };
        static WAKE: Cell<u32> = const { Cell::new(0) };
        static WAKES: Cell<u32> = const { Cell::new(0) };
        static BAD_WAKE: Cell<bool> = const { Cell::new(false) };
        static STOP_STATUS: Cell<Option<i32>> = const { Cell::new(None) };
        static OLD_PROC: Cell<WNDPROC> = const { Cell::new(None) };
    }
    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WAKE.get() && message != 0 {
            WAKES.set(WAKES.get().saturating_add(1));
            if wparam != 0 || lparam != 0 {
                BAD_WAKE.set(true);
            }
            // Data is drained immediately after DispatchMessage on this same UI thread.
            return 0;
        }
        if message == WM_NCDESTROY {
            if let Some(api) = WINDOW_API.get() {
                let token = TOKEN.get();
                if token != 0 {
                    STOP_STATUS.set(Some(unsafe { (api.stop)(token) }));
                }
            }
        }
        unsafe { CallWindowProcW(OLD_PROC.get(), hwnd, message, wparam, lparam) }
    }

    struct Report(Mutex<File>);
    impl Report {
        fn record(&self, stage: &str, detail: Value) -> Check {
            let mut file = self.0.lock().map_err(|_| "report lock poisoned")?;
            serde_json::to_writer(
                &mut *file,
                &json!({
                    "schema": "pim.native-dll-lifecycle.v2", "stage": stage, "detail": detail,
                }),
            )
            .map_err(|e| e.to_string())?;
            file.write_all(b"\n").map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            file.sync_data().map_err(|e| e.to_string())
        }
    }
    fn ensure(condition: bool, message: &str) -> Check {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }
    fn status(actual: i32, expected: i32, action: &str) -> Check {
        ensure(
            actual == expected,
            &format!("{action}: expected {expected}, got {actual}"),
        )
    }
    fn wide(value: &std::ffi::OsStr) -> Check<Vec<u16>> {
        let mut result: Vec<u16> = value.encode_wide().collect();
        ensure(!result.contains(&0), "embedded path NUL")?;
        result.push(0);
        Ok(result)
    }

    pub fn main() {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() != 6 || args[0] != "--dll" || args[2] != "--sha256" || args[4] != "--report" {
            eprintln!(
                "usage: native_dll_lifecycle --dll ABSOLUTE_OWN_SDK_DLL --sha256 64_HEX --report NEW_ABSOLUTE_JSONL"
            );
            std::process::exit(10);
        }
        let dll = PathBuf::from(&args[1]);
        let expected = args[3].to_string_lossy().to_ascii_lowercase();
        let report_path = PathBuf::from(&args[5]);
        if !dll.is_absolute()
            || !report_path.is_absolute()
            || expected.len() != 64
            || !expected.bytes().all(|b| b.is_ascii_hexdigit())
        {
            eprintln!("absolute paths and an exact SHA-256 are required");
            std::process::exit(10);
        }
        // Every file open, report lock, write, flush and sync stays on the owner.
        // The parent deadline must never depend on report I/O or its mutex.
        let watchdog = Instant::now() + Duration::from_secs(90);
        let owner = thread::spawn(move || {
            let file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(report_path)
            {
                Ok(file) => file,
                Err(_) => std::process::exit(10),
            };
            let report = Report(Mutex::new(file));
            // Catch on the owner BEFORE thread return/TLS teardown. Any contract
            // failure retains the raw module until process retirement.
            let result =
                catch_unwind(AssertUnwindSafe(|| {
                    report.record(
                        "started",
                        json!({"runtime_claim": "DLL lifecycle only",
                    "expected_sha256": expected}),
                    )?;
                    owner(&dll, &expected, &report)?;
                    // This is an honest pre-exit milestone. Only external exit 0 can
                    // prove the parent's actual OS termination fence and completed join.
                    report.record("owner_ready_to_exit_after_unload", json!({
                    "result": "OWNER_READY_REQUIRES_PARENT_EXIT_0",
                    "gui_service_qualified": false, "native_web_same_users_qualified": false,
                    "hardware_qualified": false, "full_sdk_complete": false,
                }))
                }));
            match result {
                Ok(Ok(())) => (),
                failure => {
                    let reason = match failure {
                        Ok(Err(reason)) => reason,
                        _ => "owner panic; no unload safety established".into(),
                    };
                    // This best-effort owner write may stall. The parent remains
                    // independent and retires the process on its original deadline.
                    let _ = report.record("failed", json!({"reason": reason,
                        "cleanup_safe_claim": false, "module_policy": "retain until process retirement", "exit_code": 20}));
                    std::process::exit(20);
                }
            }
            // Owner exits AFTER successful FreeLibrary and the synced pre-exit
            // milestone. Native TLS teardown still must pass the parent's OS fence.
        });
        let process = unsafe { GetCurrentProcess() };
        let mut raw_owner_completion = null_mut();
        // Duplicate the actual owner thread object with SYNCHRONIZE only. No
        // inheritance, SAME_ACCESS, pre-exit event or extra reaper/watchdog thread.
        if unsafe {
            DuplicateHandle(
                process,
                owner.as_raw_handle() as HANDLE,
                process,
                &mut raw_owner_completion,
                THREAD_SYNCHRONIZE,
                0,
                0,
            )
        } == 0
            || raw_owner_completion.is_null()
        {
            std::process::exit(60);
        }
        // SAFETY: successful DuplicateHandle transfers one caller-owned handle.
        let owner_completion = unsafe { OwnedHandle::from_raw_handle(raw_owner_completion) };
        loop {
            let remaining = watchdog.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // No report lock, write, flush, fsync, cleanup or DLL call here.
                // The external controller classifies nonzero exit + partial JSONL.
                std::process::exit(50);
            }
            let outcome = unsafe {
                WaitForSingleObject(
                    owner_completion.as_raw_handle() as HANDLE,
                    remaining.as_millis().min(1000) as u32,
                )
            };
            match outcome {
                WAIT_OBJECT_0 => break,
                WAIT_TIMEOUT => (),
                _ => std::process::exit(60),
            }
        }
        // An actual signaled Windows thread object fences native TLS epilogue.
        // is_finished is only the second fence, never permission to stop waiting.
        if Instant::now() >= watchdog {
            std::process::exit(50);
        }
        if !owner.is_finished() {
            std::process::exit(60);
        }
        if owner.join().is_err() {
            std::process::exit(30);
        }
        drop(owner_completion);
        // No parent report I/O can hang after the watchdog. External exit 0 plus
        // the synced owner_ready_to_exit_after_unload marker qualifies this run.
    }

    fn owner(dll: &Path, expected: &str, report: &Report) -> Check {
        ensure(
            sha256(b"") == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                && sha256(b"abc")
                    == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "SHA-256 known-answer self-check failed",
        )?;
        // Open without write/delete sharing and hold this file through unload to
        // fence ordinary path replacement between SHA verification and loading.
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(dll)
            .map_err(|e| format!("open exact DLL: {e}"))?;
        let path = dll.canonicalize().map_err(|e| e.to_string())?;
        ensure(
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("dll")),
            "DLL extension required",
        )?;
        let mut bytes = Vec::new();
        (&locked)
            .take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        ensure(
            bytes.len() <= 256 * 1024 * 1024,
            "DLL exceeds fixture bound",
        )?;
        ensure(sha256(&bytes) == expected, "DLL SHA-256 mismatch")?;
        drop(bytes);
        report.record(
            "dll_hash_verified",
            json!({"path": path, "sha256": expected}),
        )?;
        let name = wide(path.as_os_str())?;
        ensure(
            unsafe { GetModuleHandleW(name.as_ptr()) }.is_null(),
            "DLL was already loaded",
        )?;
        let module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        ensure(!module.is_null(), "LoadLibraryExW failed")?;
        let mut loaded = vec![0u16; 32768];
        let length = unsafe { GetModuleFileNameW(module, loaded.as_mut_ptr(), loaded.len() as u32) }
            as usize;
        ensure(
            length != 0 && length < loaded.len(),
            "loaded module path unavailable",
        )?;
        let loaded_path =
            PathBuf::from(String::from_utf16(&loaded[..length]).map_err(|e| e.to_string())?);
        ensure(
            loaded_path.canonicalize().map_err(|e| e.to_string())? == path,
            "loader resolved another DLL",
        )?;
        let api = unsafe { Api::load(module)? };
        report.record("dll_exports_resolved", json!({"native_exports": 11, "calling_convention": "C", "native_abi_header_version": 2}))?;

        let previous_dpi =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        ensure(
            !previous_dpi.is_null(),
            "per-monitor V2 owner DPI setup failed",
        )?;
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                32,
                32,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            )
        };
        ensure(!hwnd.is_null(), "hidden top-level window creation failed")?;
        #[cfg(target_pointer_width = "64")]
        let previous =
            unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wndproc as *const () as isize) };
        #[cfg(target_pointer_width = "32")]
        let previous =
            unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wndproc as *const () as i32) };
        ensure(previous != 0, "window subclass failed")?;
        OLD_PROC.set(Some(unsafe {
            std::mem::transmute::<
                usize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(previous as usize)
        }));
        WINDOW_API.set(Some(api));
        report.record(
            "owner_hidden_window_created",
            json!({"visible": false, "per_monitor_v2": true}),
        )?;

        let (setup_tx, setup_rx) = mpsc::channel();
        let (peer_tx, peer_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            let result = peer(setup_tx);
            let _ = peer_tx.send(result);
        });
        let (endpoint, users) = setup_rx
            .recv_timeout(BOUND)
            .map_err(|_| "peer setup timeout")??;
        let config = serde_json::to_vec(&json!({"endpoint": endpoint,
            "exact_origin": "http://localhost:41739", "event_credential": "a".repeat(64)}))
        .map_err(|e| e.to_string())?;
        let mut token = 0;
        let mut wake = 0;
        status(
            unsafe {
                (api.connect)(
                    hwnd as usize,
                    config.as_ptr(),
                    config.len(),
                    &mut token,
                    &mut wake,
                )
            },
            OK,
            "connect",
        )?;
        TOKEN.set(token);
        WAKE.set(wake);
        ensure(
            token != 0 && (0xc000..=0xffff).contains(&wake),
            "invalid token/registered wake",
        )?;
        let mut completion_raw = 0;
        // No message pumping/yield to UI between connect and completion duplication.
        status(
            unsafe { (api.completion)(token, &mut completion_raw) },
            OK,
            "completion_handle",
        )?;
        ensure(completion_raw != 0, "completion handle missing")?;
        let completion = completion_raw as HANDLE;
        report.record(
            "client_connected_completion_retained",
            json!({"registered_wake": wake}),
        )?;

        let deadline = Instant::now() + BOUND;
        let mut subscribed = false;
        let mut request_id = None;
        while !subscribed {
            pump_wait(completion, deadline)?;
            ensure(!BAD_WAKE.get(), "wake contains payload")?;
            if WAKES.get() == 0 {
                continue;
            }
            loop {
                status(unsafe { (api.drain)(token) }, OK, "drain")?;
                let value = retained(api, token, false)?;
                if value.is_null() {
                    break;
                }
                let message: ServerMessage =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                match message {
                    ServerMessage::Ready { .. } => {
                        ensure(request_id.is_none(), "duplicate Ready")?;
                        report.record(
                            "ready_via_data_free_window_wake",
                            json!({"wake_count": WAKES.get()}),
                        )?;
                        request_id = Some(request(
                            api,
                            token,
                            ClientCommand::Subscribe { capacity: 8 },
                            OK,
                        )?);
                    }
                    ServerMessage::Response {
                        request_id: received,
                        reply:
                            Reply::Ok {
                                response: ClientResponse::Subscribed { snapshot, .. },
                                ..
                            },
                    } => {
                        ensure(Some(received) == request_id, "response request id mismatch")?;
                        ensure(
                            snapshot.users.iter().map(|u| u.id).collect::<Vec<_>>() == users,
                            "snapshot must contain the peer's same two users",
                        )?;
                        subscribed = true;
                    }
                    _ => return Err("unexpected peer response".into()),
                }
            }
        }
        report.record(
            "subscribed_snapshot_folded",
            json!({"users": users, "local_admission_is_not_remote_result": true}),
        )?;
        status(unsafe { (api.query)(token) }, OK, "query")?;
        let model = retained(api, token, true)?;
        ensure(
            model["users"].as_array().is_some_and(|u| u.len() == 2),
            "query users missing",
        )?;
        ensure(
            model["status"]["pointer_enabled"] == false && model["status"]["key_enabled"] == false,
            "input must default off",
        )?;
        ensure(
            model["status"]["live"] == true
                && model["status"]["terminal"].is_null()
                && model["status"]["remote_cleanup_unconfirmed"] == false,
            "live client must not claim terminal cleanup uncertainty",
        )?;
        for input in [
            Input::Motion {
                position: Point { x: 7, y: 9 },
            },
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        ] {
            request(
                api,
                token,
                ClientCommand::Input {
                    user: users[0],
                    input,
                },
                INVALID,
            )?;
        }
        status(
            unsafe { (api.enable)(token, 2, 0) },
            INVALID,
            "invalid enable flag",
        )?;
        status(
            unsafe { (api.enable)(token, 0, 0) },
            OK,
            "explicit disabled flags",
        )?;
        status(unsafe { (api.query)(token) }, OK, "query for discard")?;
        status(unsafe { (api.discard)(token) }, OK, "response discard")?;
        let mut ignored = 0;
        status(
            unsafe { (api.size)(token, &mut ignored) },
            NO_RESPONSE,
            "discard consumes response",
        )?;
        report.record(
            "retained_short_buffer_and_default_off_checked",
            json!({"physical_input": false, "queue_saturation_tested": false}),
        )?;

        // Retain a real pre-stop query so WM_NCDESTROY must fence that payload.
        status(
            unsafe { (api.query)(token) },
            OK,
            "retain before window death",
        )?;
        ensure(unsafe { DestroyWindow(hwnd) } != 0, "DestroyWindow failed")?;
        ensure(
            STOP_STATUS.get() == Some(OK),
            "Stop was not successful in WM_NCDESTROY",
        )?;
        status(
            unsafe { (api.size)(token, &mut ignored) },
            NO_RESPONSE,
            "Stop retained-output fence",
        )?;
        status(
            unsafe { (api.query)(token) },
            OK,
            "terminal query after Stop",
        )?;
        let terminal = retained(api, token, false)?;
        ensure(
            terminal["status"]["live"] == false
                && terminal["status"]["terminal"] == "local_stop"
                && terminal["status"]["remote_cleanup_unconfirmed"] == true,
            "Stop must report local termination with remote cleanup unconfirmed",
        )?;
        ensure(
            terminal["users"]
                .as_array()
                .is_some_and(|users| users.is_empty()),
            "Stop must clear local model",
        )?;
        report.record(
            "hwnd_destroyed_stop_returned",
            json!({"stop_in": "WM_NCDESTROY", "retained_output_cleared": true}),
        )?;

        let first_destroy = unsafe { (api.destroy)(token) };
        ensure(
            first_destroy == BUSY || first_destroy == OK,
            "Destroy returned neither BUSY nor OK",
        )?;
        report.record(
            "initial_destroy_schedule",
            json!({"status": first_destroy,
            "busy_required": false, "reason": "actual worker schedule; no artificial TLS barrier"}),
        )?;
        // Even if Destroy already succeeded, verify the independent OS duplicate.
        // Keep the owner message loop alive with no HWND until ACTUAL thread signal.
        let deadline = Instant::now() + BOUND;
        while unsafe { WaitForSingleObject(completion, 0) } != WAIT_OBJECT_0 {
            pump_wait(completion, deadline)?;
        }
        report.record(
            "actual_worker_os_thread_signaled_after_hwnd_death",
            json!({"logical_signal_only": false}),
        )?;
        if first_destroy == BUSY {
            status(unsafe { (api.destroy)(token) }, OK, "final destroy")?;
        }
        // Success consumes the token, including the completed Rust JoinHandle.
        status(
            unsafe { (api.query)(token) },
            INVALID_HANDLE,
            "destroy consumes token",
        )?;
        status(
            unsafe { (api.destroy)(token) },
            INVALID_HANDLE,
            "second destroy invalid",
        )?;
        TOKEN.set(0);
        WINDOW_API.set(None);
        WAKE.set(0);
        OLD_PROC.set(None);
        ensure(
            unsafe { CloseHandle(completion) } != 0,
            "CloseHandle failed",
        )?;
        report.record(
            "client_destroyed_completion_closed",
            json!({"token_consumed": true}),
        )?;
        peer_rx
            .recv_timeout(BOUND)
            .map_err(|_| "peer retirement timeout")??;
        peer.join().map_err(|_| "peer panic")?;
        report.record(
            "bounded_peer_joined",
            json!({"gui_service_admission_qualified": false,
            "authoritative_remote_hold_cleanup_claim": false}),
        )?;
        ensure(
            !unsafe { SetThreadDpiAwarenessContext(previous_dpi) }.is_null(),
            "DPI restore failed",
        )?;
        // No SDK token, call, worker, callback or window procedure remains. API is
        // plain Copy storage with no destructor. Do not call any function afterwards.
        ensure(unsafe { FreeLibrary(module) } != 0, "FreeLibrary failed")?;
        ensure(
            unsafe { GetModuleHandleW(name.as_ptr()) }.is_null(),
            "DLL still loaded after FreeLibrary",
        )?;
        report.record(
            "dll_unloaded_before_owner_exit",
            json!({"all_native_calls_returned": true}),
        )?;
        drop(locked);
        Ok(())
    }

    fn request(api: Api, token: u64, command: ClientCommand, expected: i32) -> Check<u64> {
        let bytes = serde_json::to_vec(&ClientRequest {
            version: PROTOCOL_VERSION,
            command,
        })
        .map_err(|e| e.to_string())?;
        let mut id = u64::MAX;
        status(
            unsafe { (api.request)(token, bytes.as_ptr(), bytes.len(), &mut id) },
            expected,
            "request admission",
        )?;
        if expected == OK {
            ensure(id != u64::MAX && id != 0, "admission id missing")?;
        } else {
            ensure(id == u64::MAX, "failed admission changed output")?;
        }
        Ok(id)
    }
    fn retained(api: Api, token: u64, check_short: bool) -> Check<Value> {
        let mut length = 0;
        status(
            unsafe { (api.size)(token, &mut length) },
            OK,
            "response_size",
        )?;
        ensure(
            length > 0 && length <= 4 * 1024 * 1024,
            "invalid retained length",
        )?;
        if check_short {
            status(
                unsafe { (api.query)(token) },
                BUSY,
                "retained result blocks query",
            )?;
            let mut short = vec![0xa5; length - 1];
            let mut needed = 0;
            status(
                unsafe { (api.read)(token, short.as_mut_ptr(), short.len(), &mut needed) },
                SHORT,
                "short read",
            )?;
            ensure(
                needed == length && short.iter().all(|b| *b == 0xa5),
                "short read changed bytes/size",
            )?;
            let mut again = 0;
            status(
                unsafe { (api.size)(token, &mut again) },
                OK,
                "short read retains output",
            )?;
            ensure(again == length, "retained size changed")?;
        }
        let mut bytes = vec![0; length];
        let mut written = 0;
        status(
            unsafe { (api.read)(token, bytes.as_mut_ptr(), bytes.len(), &mut written) },
            OK,
            "response_read",
        )?;
        ensure(written == length, "response length mismatch")?;
        status(
            unsafe { (api.size)(token, &mut length) },
            NO_RESPONSE,
            "full read consumes output",
        )?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
    fn pump_wait(completion: HANDLE, deadline: Instant) -> Check {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure(!remaining.is_zero(), "bounded owner wait timed out")?;
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                &completion,
                remaining.as_millis().min(u32::MAX as u128) as u32,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        ensure(
            result == WAIT_OBJECT_0 || result == WAIT_OBJECT_0 + 1 || result == WAIT_TIMEOUT,
            "message/completion wait failed",
        )?;
        let mut message: MSG = unsafe { std::mem::zeroed() };
        while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            ensure(
                message.message != WM_QUIT,
                "unexpected owner quit before cleanup",
            )?;
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    fn peer(setup: mpsc::Sender<Check<(String, Vec<UserId>)>>) -> Check {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            tokio::time::timeout(BOUND + BOUND, async {
                let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.map_err(|e| e.to_string())?;
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                let bounds = mux_core::desktop::Bounds { left: 0, top: 0, right: 800, bottom: 600 };
                let mut host: Host = Host::new(91, ScreenState { revision: 1, desktop: bounds, screens: vec![bounds] })
                    .map_err(|e| e.to_string())?;
                let owner = host.register_recipient("fixture-owner").map_err(|e| e.to_string())?;
                let native = host.register_recipient("fixture-dll-consumer").map_err(|e| e.to_string())?;
                let users = vec![host.create_owned_user(owner, "One").map_err(|e| e.to_string())?,
                    host.create_owned_user(owner, "Two").map_err(|e| e.to_string())?];
                let scope = UserScope::Only(users.iter().copied().collect());
                // Explicit owner/master consent on this SAME Host/Engine. Receiver
                // grants and local input enables remain independent authorities.
                host.approve_events(owner, Grant { users: UserScope::AllOwned,
                    pointer_events: true, key_events: true, owned_windows: false })
                    .map_err(|e| e.to_string())?;
                host.approve_events(native, Grant { users: scope.clone(), pointer_events: true,
                    key_events: true, owned_windows: false }).map_err(|e| e.to_string())?;
                host.approve_controls(native, ControlGrant { users: scope, delegated_users: true,
                    pointer_input: true, key_input: true, ..Default::default() }).map_err(|e| e.to_string())?;
                setup.send(Ok((format!("ws://127.0.0.1:{port}/application-events/v1"), users)))
                    .map_err(|_| "owner setup receiver gone")?;
                let (stream, address) = listener.accept().await.map_err(|e| e.to_string())?;
                ensure(address.ip().is_loopback(), "peer must be loopback")?;
                drop(listener); // Exactly one client; no reconnect acceptance.
                let expected_host = format!("127.0.0.1:{port}");
                #[allow(clippy::result_large_err)]
                let upgrade = move |request: &Request, response: Response| {
                    if request.uri().path() != "/application-events/v1" || request.uri().query().is_some()
                        || request.headers().get("origin").and_then(|h| h.to_str().ok()) != Some("http://localhost:41739")
                        || request.headers().get("host").and_then(|h| h.to_str().ok()) != Some(expected_host.as_str())
                    {
                        return Err(tokio_tungstenite::tungstenite::http::Response::builder()
                            .status(403).body(Some("fixture upgrade denied".into())).expect("constant response"));
                    }
                    Ok(response)
                };
                let mut socket = accept_hdr_async(stream, upgrade).await.map_err(|e| e.to_string())?;
                let hello = socket.next().await.ok_or("missing hello")?.map_err(|e| e.to_string())?;
                let hello: EventHello = serde_json::from_str(hello.to_text().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                ensure(hello.version == PROTOCOL_VERSION && hello.credential == "a".repeat(64), "fixture credential mismatch")?;
                let ready = host.ready_for(native).map_err(|e| e.to_string())?;
                socket.send(Message::Text(serde_json::to_string(&ServerMessage::Ready { ready })
                    .map_err(|e| e.to_string())?.into())).await.map_err(|e| e.to_string())?;
                let request = socket.next().await.ok_or("missing Subscribe")?.map_err(|e| e.to_string())?;
                let request: ClientMessage = serde_json::from_str(request.to_text().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                ensure(matches!(request.request.command, ClientCommand::Subscribe { capacity: 8 }), "unexpected request reached peer")?;
                let response = host
                    .dispatch(native, request.request)
                    .map_err(|failure| failure.message)?;
                socket.send(Message::Text(serde_json::to_string(&ServerMessage::Response {
                    request_id: request.request_id, reply: Reply::Ok { version: PROTOCOL_VERSION, response }
                }).map_err(|e| e.to_string())?.into())).await.map_err(|e| e.to_string())?;
                // No terminal server wake is required for retirement: the owner destroys
                // its HWND and uses the duplicated worker OS thread handle afterwards.
                match socket.next().await {
                    None | Some(Ok(Message::Close(_))) => Ok(()),
                    Some(Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake))) => Ok(()),
                    _ => Err("unexpected extra wire request; default-off admission failed".into()),
                }
            }).await.map_err(|_| "bounded peer timeout")?
        })
    }

    // Small dependency-free SHA-256 used only to bind this explicitly supplied file.
    fn sha256(input: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut data = input.to_vec();
        data.push(0x80);
        while data.len() % 64 != 56 {
            data.push(0);
        }
        data.extend_from_slice(&((input.len() as u64) * 8).to_be_bytes());
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        for chunk in data.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (slot, bytes) in w[..16].iter_mut().zip(chunk.chunks_exact(4)) {
                *slot = u32::from_be_bytes(bytes.try_into().expect("four-byte SHA word"));
            }
            for i in 16..64 {
                let a = w[i - 15];
                let b = w[i - 2];
                let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
                let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut z] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let t1 = z
                    .wrapping_add(s1)
                    .wrapping_add((e & f) ^ ((!e) & g))
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
                z = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, z]) {
                *slot = slot.wrapping_add(value);
            }
        }
        h.iter().map(|word| format!("{word:08x}")).collect()
    }
}
