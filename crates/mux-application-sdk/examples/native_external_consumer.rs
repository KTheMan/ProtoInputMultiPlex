//! Source-only external GUI Service consumer fixture; never run implicitly.
//! Consumes one privately GUI-issued descriptor; creates no Host, recipient or user.
//! No graphics, device enumeration, physical input, hooks or injection are used.
#[cfg(not(windows))]
fn main() {
    eprintln!("native_external_consumer requires Windows and --features native-client");
    std::process::exit(10);
}

#[cfg(windows)]
fn main() {
    fixture::main();
}

#[cfg(windows)]
mod fixture {
    use mux_application_sdk::{
        ClientCommand, ClientRequest, ClientResponse, Event, FailureCode, Input, KeyCode, PROTOCOL_VERSION,
        OwnerResponse, PointerState, Poll, Reply, ServerMessage, SourceId, Transition, UserId,
        UserView,
    };
    use serde::{Deserialize, Serialize};
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
        sync::Mutex,
        thread,
        time::{Duration, Instant},
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
    type WindowRegister = unsafe extern "C" fn(u64, usize, u32, *mut u64) -> i32;
    type WindowObserve = unsafe extern "C" fn(u64, u64, u32, *mut u64) -> i32;
    type WindowRetire = unsafe extern "C" fn(u64, u64, *mut u64) -> i32;
    type Unary = unsafe extern "C" fn(u64) -> i32;
    type Size = unsafe extern "C" fn(u64, *mut usize) -> i32;
    type ReadFn = unsafe extern "C" fn(u64, *mut u8, usize, *mut usize) -> i32;

    // No Drop impl: failures must never unload a DLL with unproven lifetime.
    #[derive(Clone, Copy)]
    struct Api {
        connect: Connect,
        enable: Enable,
        request: RequestFn,
        window_register: WindowRegister,
        window_refresh: WindowObserve,
        window_notify: WindowObserve,
        window_retire: WindowRetire,
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
                    // Exact C calling convention and widths from the generated ABI v3 header.
                    unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(raw) }
                }};
            }
            Ok(Self {
                connect: symbol!("pim_app_native_connect", Connect),
                enable: symbol!("pim_app_native_enable_input", Enable),
                request: symbol!("pim_app_native_request", RequestFn),
                window_register: symbol!("pim_app_native_window_register", WindowRegister),
                window_refresh: symbol!("pim_app_native_window_refresh", WindowObserve),
                window_notify: symbol!("pim_app_native_window_notify", WindowObserve),
                window_retire: symbol!("pim_app_native_window_retire", WindowRetire),
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
        static EVENTS: Cell<usize> = const { Cell::new(0) };
        static MESSAGES: Cell<usize> = const { Cell::new(0) };
        static WINDOW_API: Cell<Option<Api>> = const { Cell::new(None) };
        static TOKEN: Cell<u64> = const { Cell::new(0) };
        static WINDOW_TOKEN: Cell<u64> = const { Cell::new(0) };
        static WINDOW_CHANGED: Cell<bool> = const { Cell::new(false) };
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
        if WINDOW_TOKEN.get() != 0 && matches!(message,
            WM_WINDOWPOSCHANGED | WM_SHOWWINDOW | WM_SETTEXT | WM_STYLECHANGED | WM_ENABLE | WM_DPICHANGED)
        {
            // Win32 callbacks can reenter during an SDK action. Defer observation
            // until the next owner-loop turn, outside the SDK TLS borrow.
            WINDOW_CHANGED.set(true);
        }
        if message == WM_NCDESTROY
            && let Some(api) = WINDOW_API.get()
        {
            let token = TOKEN.get();
            if token != 0 {
                let window = WINDOW_TOKEN.replace(0);
                if window != 0 {
                    let mut request_id = 0;
                    let _ = unsafe { (api.window_retire)(token, window, &mut request_id) };
                }
                STOP_STATUS.set(Some(unsafe { (api.stop)(token) }));
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
                    "schema": "pim.external-native-consumer.v2", "stage": stage, "detail": detail,
                }),
            )
            .map_err(|_| "fixture I/O or encoding failure")?;
            file.write_all(b"\n")
                .map_err(|_| "fixture I/O or encoding failure")?;
            file.flush()
                .map_err(|_| "fixture I/O or encoding failure")?;
            file.sync_data()
                .map_err(|_| "invalid retained JSON".to_owned())
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
        // Suppress arbitrary panic payloads: neither secrets nor server text belong in diagnostics.
        std::panic::set_hook(Box::new(|_| eprintln!("external native fixture panic")));
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        let route_args = (args.len() == 9 && args[8] == "--window-route")
            || (args.len() == 11 && args[8] == "--window-route" && args[9] == "--observe-seconds");
        let key_args = args.len() == 8
            || (args.len() == 10 && args[8] == "--virtual-key")
            || (args.len() == 12 && args[8] == "--virtual-key" && args[10] == "--observe-seconds");
        if !(route_args || key_args)
            || args[0] != "--dll"
            || args[2] != "--sha256"
            || args[4] != "--descriptor"
            || args[6] != "--report"
        {
            eprintln!(
                "usage: native_external_consumer --dll ABSOLUTE_DLL --sha256 64_HEX --descriptor PRIVATE_GUI_JSON --report NEW_PRIVATE_JSONL [--window-route [--observe-seconds 20..60] | --virtual-key HID_CODE [--observe-seconds 20..60]]"
            );
            std::process::exit(10);
        }
        let dll = PathBuf::from(&args[1]);
        let expected = args[3].to_string_lossy().to_ascii_lowercase();
        let descriptor_path = PathBuf::from(&args[5]);
        let report_path = PathBuf::from(&args[7]);
        let window_route = route_args;
        let key = if args.len() >= 10 && !window_route {
            match args[9]
                .to_str()
                .and_then(|s| s.parse::<u16>().ok())
                .map(KeyCode)
            {
                Some(key) if key.valid() => Some(key),
                _ => std::process::exit(10),
            }
        } else {
            None
        };
        let observation_seconds = if args.len() == 12 || args.len() == 11 {
            let seconds = if window_route { &args[10] } else { &args[11] };
            match seconds
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
            {
                Some(seconds) if (20..=60).contains(&seconds) => seconds,
                _ => std::process::exit(10),
            }
        } else {
            20
        };
        if !dll.is_absolute()
            || !descriptor_path.is_absolute()
            || !report_path.is_absolute()
            || descriptor_path == report_path
            || dll == report_path
            || expected.len() != 64
            || !expected.bytes().all(|b| b.is_ascii_hexdigit())
        {
            eprintln!("distinct absolute paths and an exact SHA-256 are required");
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
            let result = catch_unwind(AssertUnwindSafe(|| {
                report.record(
                        "started",
                        json!({"runtime_claim": "external GUI Service consumer; paired acceptance pending",
                    "expected_sha256": expected, "observation_seconds": observation_seconds}),
                    )?;
                owner(
                    &dll,
                    &expected,
                    &descriptor_path,
                    key,
                    window_route,
                    observation_seconds,
                    &report,
                )?;
                // This is an honest pre-exit milestone. Only external exit 0 can
                // prove the parent's actual OS termination fence and completed join.
                report.record("owner_ready_to_exit_after_unload", json!({
                    "result": "OWNER_READY_REQUIRES_PARENT_EXIT_0",
                    "gui_service_descriptor_consumed": true, "native_web_same_users_qualified": false,
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

    fn owner(
        dll: &Path,
        expected: &str,
        descriptor_path: &Path,
        key: Option<KeyCode>,
        window_route: bool,
        observation_seconds: u64,
        report: &Report,
    ) -> Check {
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
            .map_err(|_| "open exact DLL failed")?;
        let path = dll
            .canonicalize()
            .map_err(|_| "fixture I/O or encoding failure")?;
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
            .map_err(|_| "fixture I/O or encoding failure")?;
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
        let loaded_path = PathBuf::from(
            String::from_utf16(&loaded[..length]).map_err(|_| "fixture I/O or encoding failure")?,
        );
        ensure(
            loaded_path
                .canonicalize()
                .map_err(|_| "fixture I/O or encoding failure")?
                == path,
            "loader resolved another DLL",
        )?;
        let api = unsafe { Api::load(module)? };
        report.record("dll_exports_resolved", json!({"native_exports": 15, "calling_convention": "C", "native_abi_header_version": 3}))?;

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

        // Translate the actual private GUI Descriptor to the native ConnectConfig ABI.
        let mut config = descriptor_config(descriptor_path)?;
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
        config.fill(0);
        drop(config);
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

        await_ready(api, token, completion, report)?;
        if window_route {
            exercise_owned_window_route(api, token, completion, hwnd, observation_seconds, report)?;
        } else {
        let response = round_trip(
            api,
            token,
            completion,
            ClientCommand::Subscribe { capacity: 256 },
            report,
        )?;
        let ClientResponse::Subscribed { snapshot, .. } = response else {
            return Err("Subscribe returned unexpected response".into());
        };
        ensure(
            snapshot.users.len() == 2,
            "require exactly two GUI-approved users",
        )?;
        let baseline = identities(&snapshot.users);
        ensure(
            baseline[0].id != baseline[1].id && baseline[0].source != baseline[1].source,
            "duplicate GUI identity/source",
        )?;
        report.record("gui_snapshot", json!({"epoch": snapshot.epoch, "sequence": snapshot.through_sequence,
            "users": evidence(&snapshot.users), "provenance": "authenticated descriptor-selected subscription; GUI provenance requires paired harness",
            "paired_wasm_verified": false}))?;
        let model = query(api, token, true)?;
        ensure(
            model["status"]["pointer_enabled"] == false && model["status"]["key_enabled"] == false,
            "input must default off",
        )?;
        ensure(
            model["status"]["live"] == true
                && model["status"]["terminal"].is_null()
                && model["status"]["remote_cleanup_unconfirmed"] == false,
            "expected live native model",
        )?;
        let user = snapshot.users[0].id;
        let probe_key = key.unwrap_or(KeyCode(4));
        request(
            api,
            token,
            ClientCommand::Input {
                user,
                input: Input::Key {
                    key: probe_key,
                    transition: Transition::Down,
                },
            },
            INVALID,
        )?;
        ensure(
            query(api, token, false)?["status"]["pending_requests"] == 0,
            "default-off probe was queued",
        )?;
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
        report.record(
            "default_off_checked",
            json!({"locally_rejected": true,
            "virtual_input_requested": key.is_some(), "physical_input": false}),
        )?;
        if let Some(key) = key {
            // Explicit opt-in is independent of GUI control/delegation/input consent.
            status(
                unsafe { (api.enable)(token, 0, 1) },
                OK,
                "explicit key enable",
            )?;
            for transition in [Transition::Down, Transition::Up] {
                let reply = round_trip(
                    api,
                    token,
                    completion,
                    ClientCommand::Input {
                        user,
                        input: Input::Key { key, transition },
                    },
                    report,
                )?;
                let ClientResponse::InputAccepted { sequences } = reply else {
                    return Err("virtual input returned unexpected response".into());
                };
                ensure(
                    !sequences.is_empty(),
                    "input acknowledgement lacks sequence",
                )?;
                report.record(
                    "virtual_key_acknowledged",
                    json!({"user": user, "key": key,
                    "transition": transition, "sequences": sequences,
                    "acknowledged_by_remote_response": true}),
                )?;
            }
            // Disabling an enabled input family intentionally retires the production
            // connection. Preserve keys enabled throughout overlap; WM_NCDESTROY's
            // final Stop fences the producer and begins remote cleanup instead.
        }
        // Default 20-second overlap; the explicitly opted-in hook fixture may use up to 60 seconds.
        let observation_end = Instant::now() + Duration::from_secs(observation_seconds);
        while Instant::now() < observation_end {
            let until = (Instant::now() + Duration::from_millis(250)).min(observation_end);
            pump_wait(completion, until)?;
            drain_events(api, token, report)?;
            let model = query(api, token, false)?;
            ensure(
                model["status"]["live"] == true && model["status"]["terminal"].is_null(),
                "native consumer ended during overlap",
            )?;
            let current: Vec<UserView> = serde_json::from_value(model["users"].clone())
                .map_err(|_| "invalid model users")?;
            ensure(
                identities(&current) == baseline,
                "GUI user/source identities changed",
            )?;
        }
        let final_model = query(api, token, false)?;
        let final_users: Vec<UserView> = serde_json::from_value(final_model["users"].clone())
            .map_err(|_| "invalid final users")?;
        report.record(
            "overlap_snapshot",
            json!({"users": evidence(&final_users),
            "sequence": final_model["sequence"].as_u64(),
            "paired_wasm_verified": false, "remote_cleanup_confirmed": false}),
        )?;
        }
        status(unsafe { (api.query)(token) }, OK, "query for discard")?;
        status(unsafe { (api.discard)(token) }, OK, "response discard")?;
        let mut ignored = 0;
        status(
            unsafe { (api.size)(token, &mut ignored) },
            NO_RESPONSE,
            "discard consumes response",
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

    // No Debug/Serialize: descriptor strings must never reach any report or diagnostic.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GuiDescriptor {
        protoinputmultiplex_application_events: u32,
        endpoint: String,
        exact_origin: String,
        credential: String,
        expires_unix_ms: u64,
    }
    fn descriptor_config(path: &Path) -> Check<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| "private descriptor open failed")?
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| "private descriptor read failed")?;
        let result = convert_descriptor(&bytes);
        bytes.fill(0);
        result
    }
    fn convert_descriptor(bytes: &[u8]) -> Check<Vec<u8>> {
        ensure(
            !bytes.is_empty() && bytes.len() <= 65536,
            "private descriptor size invalid",
        )?;
        let descriptor: GuiDescriptor =
            serde_json::from_slice(bytes).map_err(|_| "private descriptor schema invalid")?;
        ensure(
            descriptor.protoinputmultiplex_application_events == PROTOCOL_VERSION,
            "private descriptor version invalid",
        )?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "system clock invalid")?
            .as_millis();
        ensure(
            u128::from(descriptor.expires_unix_ms) > now,
            "private descriptor expired",
        )?;
        ensure(
            descriptor.credential.len() == 64
                && descriptor.credential.bytes().all(|b| b.is_ascii_hexdigit()),
            "private descriptor credential invalid",
        )?;
        // Native connect performs the exact loopback endpoint/origin validation.
        // This transient JSON value is never returned through report/log/query.
        serde_json::to_vec(&json!({"endpoint": descriptor.endpoint,
            "exact_origin": descriptor.exact_origin, "event_credential": descriptor.credential}))
        .map_err(|_| "private descriptor conversion failed".to_owned())
    }

    #[derive(Serialize, PartialEq, Eq)]
    struct IdentityEvidence {
        id: UserId,
        source: SourceId,
    }
    fn identities(users: &[UserView]) -> Vec<IdentityEvidence> {
        let mut result: Vec<_> = users
            .iter()
            .map(|u| IdentityEvidence {
                id: u.id,
                source: u.source,
            })
            .collect();
        result.sort_by_key(|u| u.id);
        result
    }
    #[derive(Serialize)]
    struct UserEvidence<'a> {
        id: UserId,
        source: SourceId,
        pointer: Option<&'a PointerState>,
        held_keys: Option<&'a Vec<KeyCode>>,
    }
    fn evidence(users: &[UserView]) -> Vec<UserEvidence<'_>> {
        users
            .iter()
            .map(|u| UserEvidence {
                id: u.id,
                source: u.source,
                pointer: u.pointer.as_ref(),
                held_keys: u.held_keys.as_ref(),
            })
            .collect()
    }
    fn await_owner_response(
        api: Api,
        client: u64,
        completion: HANDLE,
        expected_id: Option<u64>,
    ) -> Check<OwnerResponse> {
        let deadline = Instant::now() + BOUND;
        loop {
            ensure(Instant::now() < deadline, "owner report deadline exceeded; no replay")?;
            if let Some(message) = next_message(api, client)? {
                match message {
                    ServerMessage::OwnerResponse { request_id, reply } => {
                        if let Some(expected) = expected_id {
                            ensure(request_id == expected, "owner response ID mismatch")?;
                        }
                        return match reply {
                            Reply::Ok { version, response } if version == PROTOCOL_VERSION => Ok(response),
                            Reply::Error { .. } => Err("GUI rejected native owner report".into()),
                            _ => Err("owner response version mismatch".into()),
                        };
                    }
                    ServerMessage::EventStream { .. } => (),
                    _ => return Err("unexpected owner response frame".into()),
                }
            } else {
                pump_wait(completion, deadline)?;
            }
        }
    }

    fn exercise_owned_window_route(
        api: Api,
        client: u64,
        completion: HANDLE,
        hwnd: HWND,
        observation_seconds: u64,
        report: &Report,
    ) -> Check {
        let ready = query(api, client, false)?;
        ensure(
            ready["ready"]["events"]["owned_windows"] == true
                && ready["ready"]["controls"]["window_publication"] == true,
            "native publisher lacks GUI window approval",
        )?;
        ensure(
            ready["users"].as_array().is_some_and(|users| users.is_empty()),
            "window-only route unexpectedly has virtual users",
        )?;
        let mut local = 0;
        status(
            unsafe { (api.window_register)(client, hwnd as usize, 0, &mut local) },
            OK,
            "own window register",
        )?;
        ensure(local != 0, "missing local window token")?;
        WINDOW_TOKEN.set(local);
        let OwnerResponse::Registered { window } =
            await_owner_response(api, client, completion, None)?
        else {
            return Err("native registration response kind mismatch".into());
        };
        report.record("native_window_published", json!({
            "identity_metadata_consent": false,
            "source": "native_client_reported", "users_required": false,
        }))?;
        let title: Vec<u16> = "Owned SDK fixture window\0".encode_utf16().collect();
        ensure(unsafe { SetWindowTextW(hwnd, title.as_ptr()) } != 0, "SetWindowTextW failed")?;
        ensure(WINDOW_CHANGED.replace(false), "owner WndProc missed title change")?;
        let mut notify_id = 0;
        status(
            unsafe { (api.window_notify)(client, local, 0, &mut notify_id) },
            OK,
            "owner change notify",
        )?;
        ensure(notify_id != 0, "changed owner observation was suppressed")?;
        let OwnerResponse::Window { window: changed } =
            await_owner_response(api, client, completion, Some(notify_id))?
        else {
            return Err("owner change response kind mismatch".into());
        };
        ensure(changed.id == window && changed.metadata.title.as_deref() == Some("Owned SDK fixture window"),
            "GUI did not acknowledge the observed title")?;
        report.record("native_window_changed", json!({
            "actual_title_change": true,
            "metadata_identity_absent": changed.metadata.process_id.is_none() && changed.metadata.ui_thread_id.is_none(),
        }))?;
        ensure(unsafe { SetWindowPos(hwnd, 0, 150, 120, 400, 300, SWP_NOZORDER | SWP_NOACTIVATE) } != 0,
            "SetWindowPos failed")?;
        ensure(WINDOW_CHANGED.replace(false), "owner WndProc missed external window move")?;
        let mut move_id = 0;
        status(unsafe { (api.window_notify)(client, local, 0, &mut move_id) }, OK, "owner move notify")?;
        ensure(move_id != 0, "owner move observation was suppressed")?;
        let OwnerResponse::Window { window: moved } =
            await_owner_response(api, client, completion, Some(move_id))?
        else {
            return Err("owner move response kind mismatch".into());
        };
        let mut os_rect = unsafe { std::mem::zeroed() };
        ensure(unsafe { GetWindowRect(hwnd, &mut os_rect) } != 0, "owner OS read after move failed")?;
        let os_bounds = [os_rect.left, os_rect.top, os_rect.right, os_rect.bottom];
        ensure(moved.id == window && [moved.bounds.left, moved.bounds.top, moved.bounds.right, moved.bounds.bottom] == os_bounds,
            "GUI owner observation disagrees with OS move")?;
        report.record("native_window_moved", json!({
            "actual_os_move": true,
            "os_bounds": os_bounds,
        }))?;
        let mut refresh_id = 0;
        status(
            unsafe { (api.window_refresh)(client, local, 0, &mut refresh_id) },
            OK,
            "explicit window refresh",
        )?;
        ensure(refresh_id != 0, "missing refresh request")?;
        let OwnerResponse::Window { window: refreshed } =
            await_owner_response(api, client, completion, Some(refresh_id))?
        else {
            return Err("owner refresh response kind mismatch".into());
        };
        ensure(refreshed.id == window, "refresh changed GUI WindowId")?;
        let end = Instant::now() + Duration::from_secs(observation_seconds);
        let mut applies = 0;
        let mut receipts = 0;
        let mut late_rejections = 0;
        let mut timeout_pause = false;
        let mut last_refresh = Instant::now();
        while Instant::now() < end {
            pump_wait(completion, (Instant::now() + Duration::from_millis(250)).min(end))?;
            for _ in 0..64 {
                match next_message(api, client)? {
                    None => break,
                    Some(ServerMessage::WindowApply { window: selected, control, .. }) => {
                        ensure(selected == window, "action targeted another window")?;
                        let mut rect = unsafe { std::mem::zeroed() };
                        ensure(unsafe { GetWindowRect(hwnd, &mut rect) } != 0, "owner OS read after action failed")?;
                        applies += 1;
                        report.record("native_window_action_executed", json!({
                            "control": control,
                            "os_bounds": [rect.left, rect.top, rect.right, rect.bottom],
                            "os_visible": unsafe { IsWindowVisible(hwnd) } != 0,
                        }))?;
                    }
                    Some(ServerMessage::OwnerResponse { reply: Reply::Ok { response: OwnerResponse::Acknowledged, .. }, .. }) => {
                        receipts += 1;
                    }
                    Some(ServerMessage::OwnerResponse { reply: Reply::Error { error, .. }, .. })
                        if timeout_pause && error.code == FailureCode::Missing => {
                            late_rejections += 1;
                    }
                    Some(ServerMessage::OwnerResponse { reply: Reply::Ok { response: OwnerResponse::Window { .. }, .. }, .. }) => (),
                    Some(ServerMessage::Disconnected { .. }) => return Err("GUI disconnected during window route".into()),
                    _ => return Err("unexpected shared window frame".into()),
                }
            }
            if WINDOW_CHANGED.replace(false) {
                let mut id = 0;
                status(unsafe { (api.window_notify)(client, local, 0, &mut id) }, OK, "owner state notify")?;
            }
            if last_refresh.elapsed() >= Duration::from_secs(1) {
                let mut id = 0;
                status(unsafe { (api.window_refresh)(client, local, 0, &mut id) }, OK, "owner observation refresh")?;
                ensure(id != 0, "missing periodic owner observation")?;
                last_refresh = Instant::now();
            }
            if applies >= 3 && receipts >= 3 && !timeout_pause {
                report.record("native_window_timeout_ready", json!({"first_three_actions_executed": true}))?;
                // The owner message loop deliberately pauses after three real controls.
                // A fourth request can time out at the GUI while its queued owner
                // dispatch may still run later. The consumer must never replay it.
                thread::sleep(Duration::from_secs(8));
                timeout_pause = true;
            }
            if applies >= 4 && receipts >= 3 && late_rejections >= 1 {
                break;
            }
        }
        ensure(applies >= 4 && receipts >= 3 && late_rejections >= 1 && timeout_pause,
            "distinct approved actions and late owner dispatch were not observed")?;
        report.record("native_window_timeout_late_action", json!({
            "fourth_action_executed_after_owner_pause": true,
            "actions": applies,
            "late_receipt_rejected": late_rejections == 1,
        }))?;
        // Keep the owned window live briefly so the GUI can revoke one exact
        // observer and the browser can demonstrate its post-revoke fence.
        thread::sleep(Duration::from_secs(5));
        let mut retire_id = 0;
        status(unsafe { (api.window_retire)(client, local, &mut retire_id) }, OK, "owner window retire")?;
        WINDOW_TOKEN.set(0);
        ensure(retire_id != 0, "missing remote retire report")?;
        ensure(matches!(await_owner_response(api, client, completion, Some(retire_id))?, OwnerResponse::Acknowledged),
            "GUI did not acknowledge window retirement")?;
        report.record("native_window_retired", json!({
            "actions": applies, "owner_receipts": receipts,
            "late_rejections": late_rejections,
            "retired_before_hwnd_destroy": true,
        }))?;
        Ok(())
    }
    fn query(api: Api, token: u64, short: bool) -> Check<Value> {
        status(unsafe { (api.query)(token) }, OK, "query")?;
        retained(api, token, short)
    }
    fn next_message(api: Api, token: u64) -> Check<Option<ServerMessage>> {
        ensure(!BAD_WAKE.get(), "wake contains payload")?;
        status(unsafe { (api.drain)(token) }, OK, "drain")?;
        let value = retained(api, token, false)?;
        if value.is_null() {
            return Ok(None);
        }
        let count = MESSAGES.get().saturating_add(1);
        MESSAGES.set(count);
        ensure(count <= 4096, "fixture incoming message budget exceeded")?;
        serde_json::from_value(value)
            .map(Some)
            .map_err(|_| "invalid server message".to_owned())
    }
    fn await_ready(api: Api, token: u64, completion: HANDLE, report: &Report) -> Check {
        let deadline = Instant::now() + BOUND;
        loop {
            ensure(Instant::now() < deadline, "Ready deadline exceeded")?;
            pump_wait(completion, deadline)?;
            if let Some(message) = next_message(api, token)? {
                ensure(WAKES.get() > 0, "Ready arrived without data-free wake")?;
                return match message {
                    ServerMessage::Ready { ready } => report.record(
                        "gui_ready",
                        json!({"version": ready.version, "epoch": ready.epoch,
                            "events": ready.events, "controls": ready.controls}),
                    ),
                    _ => Err("expected Ready".into()),
                };
            }
        }
    }
    fn round_trip(
        api: Api,
        token: u64,
        completion: HANDLE,
        command: ClientCommand,
        report: &Report,
    ) -> Check<ClientResponse> {
        let id = request(api, token, command, OK)?;
        // Admission and completion are separately recorded; no retry/replay.
        report.record(
            "command_locally_admitted",
            json!({"request_id": id, "remote_success": false}),
        )?;
        let deadline = Instant::now() + BOUND;
        loop {
            ensure(
                Instant::now() < deadline,
                "command response deadline exceeded; no replay",
            )?;
            if let Some(message) = next_message(api, token)? {
                match message {
                    ServerMessage::Response { request_id, reply } => {
                        ensure(request_id == id, "response request id mismatch")?;
                        match reply {
                            Reply::Ok { version, response } => {
                                ensure(version == PROTOCOL_VERSION, "response version mismatch")?;
                                report.record(
                                    "command_remotely_completed",
                                    json!({"request_id": id}),
                                )?;
                                return Ok(response);
                            }
                            Reply::Error { error, .. } => {
                                // Failure.message is peer supplied: emit only its bounded typed code.
                                report.record(
                                    "command_remotely_rejected",
                                    json!({"request_id": id, "code": error.code}),
                                )?;
                                return Err("remote command rejected".into());
                            }
                        }
                    }
                    ServerMessage::EventStream { stream, .. } => record_stream(stream, report)?,
                    _ => return Err("unexpected message while awaiting response".into()),
                }
            } else {
                pump_wait(completion, deadline)?;
            }
        }
    }
    fn record_stream(stream: Poll, report: &Report) -> Check {
        match stream {
            Poll::Events { events } => {
                for envelope in events {
                    let count = EVENTS.get().saturating_add(1);
                    EVENTS.set(count);
                    ensure(count <= 4096, "fixture event budget exceeded")?;
                    match envelope.event {
                        Event::Input {
                            user,
                            source,
                            input,
                            position,
                            transform_revision,
                        } => {
                            report.record("input_observed", json!({"epoch": envelope.epoch,
                                "sequence": envelope.sequence, "user": user, "source": source,
                                "input": input, "position": position, "transform_revision": transform_revision}))?;
                        }
                        Event::UserDisposed { user } => {
                            report.record("gui_user_disposed", json!({"user": user}))?;
                            return Err("GUI user disposed during bounded acceptance".into());
                        }
                        _ => (),
                    }
                }
                Ok(())
            }
            Poll::Gap {
                after_sequence,
                lost_through,
            } => {
                report.record(
                    "stream_gap",
                    json!({"after_sequence": after_sequence,
                    "lost_through": lost_through, "gap_recovery_qualified": false}),
                )?;
                Err("unexpected stream gap; recovery outside fixture scope".into())
            }
            Poll::Ended { reason } => {
                report.record(
                    "stream_ended",
                    json!({"reason": reason, "remote_cleanup_confirmed": false}),
                )?;
                Err("GUI stream ended during overlap".into())
            }
        }
    }
    fn drain_events(api: Api, token: u64, report: &Report) -> Check {
        // Hard turn budget: a continuous producer cannot starve deadline/cleanup checks.
        for _ in 0..64 {
            match next_message(api, token)? {
                None => return Ok(()),
                Some(ServerMessage::EventStream { stream, .. }) => record_stream(stream, report)?,
                _ => return Err("unexpected unsolicited server response".into()),
            }
        }
        Err("drain turn budget exceeded".into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn malformed_private_descriptor_has_constant_error() {
            let error = convert_descriptor(
                br#"{"credential":"PRIVATE_MARKER","unknown":"PRIVATE_MARKER"}"#,
            )
            .err()
            .expect("reject malformed descriptor");
            assert_eq!(error, "private descriptor schema invalid");
            assert!(!error.contains("PRIVATE_MARKER"));
        }
        #[test]
        fn evidence_excludes_names_and_preserves_exact_identity() {
            let user = UserView {
                id: UserId {
                    epoch: 91,
                    local: 17,
                },
                owned_by_recipient: false,
                name: "PRIVATE_MARKER".into(),
                source: SourceId(44),
                pointer: None,
                cursor: None,
                held_keys: Some(vec![KeyCode(4)]),
            };
            let users = [user];
            let value = serde_json::to_value(evidence(&users)).expect("typed evidence");
            assert_eq!(value[0]["id"], json!({"epoch": 91, "local": 17}));
            assert_eq!(value[0]["source"], 44);
            assert!(value[0].get("name").is_none());
            assert!(!value.to_string().contains("PRIVATE_MARKER"));
        }
    }

    fn request(api: Api, token: u64, command: ClientCommand, expected: i32) -> Check<u64> {
        let bytes = serde_json::to_vec(&ClientRequest {
            version: PROTOCOL_VERSION,
            command,
        })
        .map_err(|_| "fixture I/O or encoding failure")?;
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
        serde_json::from_slice(&bytes).map_err(|_| "invalid retained JSON".to_owned())
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
        for _ in 0..256 {
            if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } == 0 {
                return Ok(());
            }
            ensure(
                Instant::now() < deadline && message.message != WM_QUIT,
                "unexpected owner quit before cleanup",
            )?;
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
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
