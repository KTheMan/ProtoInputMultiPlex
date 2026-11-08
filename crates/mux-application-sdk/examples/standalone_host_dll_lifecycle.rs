//! Explicit disposable-process qualification of the eight standalone Host DLL exports.
//! No statically linked Host, GUI, network, input, hooks, injection or orphan case.
#[cfg(not(windows))]
fn main() {
    eprintln!("standalone_host_dll_lifecycle requires Windows and --features native-client");
    std::process::exit(10);
}

#[cfg(windows)]
fn main() {
    fixture::main();
}

#[cfg(windows)]
mod fixture {
    use mux_application_sdk::{
        ClientCommand, ClientRequest, ControlGrant, Grant, HostCommand, HostRequest, RecipientId,
        SubscriptionId, UserScope, WindowId, PROTOCOL_VERSION,
    };
    use serde_json::{Value, json};
    use std::{
        ffi::OsStr,
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        panic::{AssertUnwindSafe, catch_unwind},
        path::{Path, PathBuf},
        ptr::null_mut,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::{
            DuplicateHandle, FreeLibrary, GetLastError, HANDLE, HMODULE, HWND, LPARAM, LRESULT,
            POINT, RECT, SetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
        },
        Graphics::Gdi::{
            ClientToScreen, GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST,
            MonitorFromWindow,
        },
        System::{
            LibraryLoader::{
                GetModuleFileNameW, GetModuleHandleW, GetProcAddress,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
            },
            Threading::{
                GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, THREAD_SYNCHRONIZE,
                WaitForSingleObject,
            },
        },
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, GetDpiForWindow,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
                GetWindowLongPtrW, GetWindowRect, MoveWindow, SetWindowLongPtrW, SetWindowTextW,
                GWLP_USERDATA, GWLP_WNDPROC, WM_NCDESTROY, WS_POPUP,
            },
        },
    };

    const OK: i32 = 0;
    const INVALID_HANDLE: i32 = 2;
    const BUSY: i32 = 3;
    const SHORT: i32 = 4;
    const NO_RESPONSE: i32 = 5;
    const CYCLES: u32 = 2;
    const MAX_RESPONSE: usize = 4 * 1024 * 1024;
    type Check<T = ()> = Result<T, String>;
    type Version = unsafe extern "C" fn() -> u32;
    type Create = unsafe extern "C" fn(u64, *const u8, usize, *mut u64) -> i32;
    type Unary = unsafe extern "C" fn(u64) -> i32;
    type HostCall = unsafe extern "C" fn(u64, *const u8, usize) -> i32;
    type ClientCall = unsafe extern "C" fn(u64, u64, *const u8, usize) -> i32;
    type Size = unsafe extern "C" fn(u64, *mut usize) -> i32;
    type ReadFn = unsafe extern "C" fn(u64, *mut u8, usize, *mut usize) -> i32;

    // No Drop impl and no automatic module guard. Failure retains the raw module
    // until process retirement; cleanup permission is never inferred from failure.
    struct Api {
        version: Version,
        create: Create,
        destroy: Unary,
        host_call: HostCall,
        client_call: ClientCall,
        size: Size,
        read: ReadFn,
        discard: Unary,
    }
    impl Api {
        unsafe fn load(module: HMODULE) -> Check<Self> {
            macro_rules! symbol {
                ($name:literal, $ty:ty) => {{
                    let raw = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) }
                        .ok_or_else(|| concat!("missing export ", $name).to_owned())?;
                    // Exact __cdecl widths from generated base ABI version 1 header.
                    unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(raw) }
                }};
            }
            Ok(Self {
                version: symbol!("pim_app_abi_version", Version),
                create: symbol!("pim_app_host_create", Create),
                destroy: symbol!("pim_app_host_destroy", Unary),
                host_call: symbol!("pim_app_host_call", HostCall),
                client_call: symbol!("pim_app_client_call", ClientCall),
                size: symbol!("pim_app_response_size", Size),
                read: symbol!("pim_app_response_read", ReadFn),
                discard: symbol!("pim_app_response_discard", Unary),
            })
        }
    }

    struct Report {
        file: File,
        cycle: u32,
        sequence: u32,
        schema: &'static str,
    }
    impl Report {
        fn record(&mut self, stage: &str, detail: Value) -> Check {
            self.sequence += 1;
            serde_json::to_writer(
                &mut self.file,
                &json!({"schema": self.schema, "cycle": self.cycle,
                    "sequence": self.sequence, "stage": stage, "detail": detail}),
            )
            .map_err(|e| e.to_string())?;
            self.file.write_all(b"\n").map_err(|e| e.to_string())?;
            self.file.flush().map_err(|e| e.to_string())?;
            self.file.sync_data().map_err(|e| e.to_string())
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
        let window_metadata = args.len() == 7 && args[6] == "--window-metadata";
        if !(args.len() == 6 || window_metadata)
            || args[0] != "--dll"
            || args[2] != "--dll-sha256"
            || args[4] != "--report"
        {
            eprintln!(
                "usage: standalone_host_dll_lifecycle --dll ABSOLUTE_OWN_SDK_DLL --dll-sha256 64_HEX --report NEW_ABSOLUTE_JSONL [--window-metadata]"
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
        // Parent never performs report I/O. It can retire the process if file I/O,
        // a DLL call, or owner TLS epilogue stalls. External supervisor also required.
        let deadline = Instant::now() + Duration::from_secs(90);
        let cycles = if window_metadata { 1 } else { CYCLES };
        for cycle in 1..=cycles {
            let dll = dll.clone();
            let expected = expected.clone();
            let report_path = report_path.clone();
            let (unloaded_tx, unloaded_rx) = mpsc::channel();
            let (ack_tx, ack_rx) = mpsc::channel();
            let owner = thread::spawn(move || {
                let mut options = OpenOptions::new();
                options.write(true);
                if cycle == 1 {
                    options.create_new(true);
                } else {
                    options.append(true);
                }
                let file = match options.open(report_path) {
                    Ok(file) => file,
                    Err(_) => std::process::exit(10),
                };
                let mut report = Report {
                    file,
                    cycle,
                    sequence: 0,
                    schema: if window_metadata {
                        "pim.standalone_host_window_metadata.v2"
                    } else {
                        "pim.standalone_host_dll_lifecycle.v1"
                    },
                };
                let result = catch_unwind(AssertUnwindSafe(|| {
                    if window_metadata {
                        report.record(
                            "started",
                            json!({"cycles": 1, "expected_sha256": expected,
                            "runtime_claim": "opt-in owned-window metadata fixture only"}),
                        )?;
                    } else {
                        report.record(
                            "started",
                            json!({"cycles": CYCLES, "expected_sha256": expected,
                            "previous_owner_os_exit_and_join_confirmed": cycle > 1,
                            "runtime_claim": "standalone Host DLL lifetime prerequisite only"}),
                        )?;
                    }
                    exercise_owner(&dll, &expected, cycle, window_metadata, &mut report)?;
                    // Remain alive after unload until parent has observed the real
                    // owner thread object as nonsignaled and explicitly acknowledges.
                    unloaded_tx
                        .send(())
                        .map_err(|_| "parent unload receiver gone")?;
                    ack_rx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|_| "post-unload acknowledgment timeout")?;
                    report.record(
                        "post_unload_barrier_acknowledged",
                        json!({
                        "parent_observed_owner_os_thread_alive": true}),
                    )?;
                    report.record(
                        "owner_ready_to_exit_after_unload",
                        json!({
                        "result": "REQUIRES_PARENT_OS_WAIT_JOIN_AND_EXTERNAL_EXIT_0",
                        "gui_service_qualified": false, "native_web_same_users_qualified": false,
                        "hardware_qualified": false, "full_sdk_complete": false}),
                    )
                }));
                match result {
                    Ok(Ok(())) => (),
                    failure => {
                        let reason = match failure {
                            Ok(Err(reason)) => reason,
                            _ => "owner panic; no unload safety established".into(),
                        };
                        let _ = report.record("failed", json!({"reason": reason,
                            "cleanup_safe_claim": false, "module_policy": "retain any live module until process retirement"}));
                        std::process::exit(20);
                    }
                }
            });
            let process = unsafe { GetCurrentProcess() };
            let mut raw = null_mut();
            if unsafe {
                DuplicateHandle(
                    process,
                    owner.as_raw_handle() as HANDLE,
                    process,
                    &mut raw,
                    THREAD_SYNCHRONIZE,
                    0,
                    0,
                )
            } == 0
                || raw.is_null()
            {
                std::process::exit(60);
            }
            // SAFETY: successful DuplicateHandle transferred this owned duplicate.
            let completion = unsafe { OwnedHandle::from_raw_handle(raw) };
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    std::process::exit(50);
                }
                match unloaded_rx.recv_timeout(remaining.min(Duration::from_secs(1))) {
                    Ok(()) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                    Err(mpsc::RecvTimeoutError::Disconnected) => std::process::exit(60),
                }
            }
            if unsafe { WaitForSingleObject(completion.as_raw_handle() as HANDLE, 0) }
                != WAIT_TIMEOUT
                || ack_tx.send(()).is_err()
            {
                std::process::exit(60);
            }
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    std::process::exit(50);
                }
                match unsafe {
                    WaitForSingleObject(
                        completion.as_raw_handle() as HANDLE,
                        remaining.as_millis().min(1000) as u32,
                    )
                } {
                    WAIT_OBJECT_0 => break,
                    WAIT_TIMEOUT => (),
                    _ => std::process::exit(60),
                }
            }
            // Actual OS termination fences the native TLS epilogue. An event,
            // channel message, marker, or is_finished alone never passes this gate.
            if Instant::now() >= deadline {
                std::process::exit(50);
            }
            if !owner.is_finished() {
                std::process::exit(60);
            }
            if owner.join().is_err() {
                std::process::exit(30);
            }
            drop(completion);
        }
        // External exit 0 plus both complete ordered cycle reports proves this
        // fixture passed both real owner OS waits and joins, not a universal proof.
    }

    fn exercise_owner(
        dll: &Path,
        expected: &str,
        cycle: u32,
        window_metadata: bool,
        report: &mut Report,
    ) -> Check {
        ensure(
            sha256(b"") == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                && sha256(b"abc")
                    == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "SHA-256 known-answer self-check failed",
        )?;
        // Deny ordinary write/delete sharing and hold this exact file through unload.
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
            "DLL already loaded",
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
        {
            let api = unsafe { Api::load(module)? };
            ensure(unsafe { (api.version)() } == 1, "base ABI version mismatch")?;
            report.record(
                "base_exports_resolved",
                json!({"base_exports": 8,
                "calling_convention": "C", "base_abi_version": 1}),
            )?;
            if window_metadata {
                exercise_window_metadata(&api, cycle, report)?;
            } else {
                exercise_host(&api, cycle, report)?;
            }
        } // All DLL function pointers leave scope before unloading; never re-enter.
        ensure(unsafe { FreeLibrary(module) } != 0, "FreeLibrary failed")?;
        ensure(
            unsafe { GetModuleHandleW(name.as_ptr()) }.is_null(),
            "DLL still loaded after release",
        )?;
        report.record(
            "dll_unloaded_owner_still_alive",
            json!({"live_host_handles": 0,
            "outstanding_sdk_calls": 0, "future_sdk_entry_fenced": true}),
        )?;
        drop(locked);
        Ok(())
    }

    fn exercise_host(api: &Api, cycle: u32, report: &mut Report) -> Check {
        let bounds = json!({"left": 0, "top": 0, "right": 800, "bottom": 600});
        let screens = serde_json::to_vec(&json!({"revision": 1, "desktop": bounds,
            "screens": [bounds]}))
        .map_err(|e| e.to_string())?;
        let mut host = 0;
        status(
            unsafe {
                (api.create)(
                    7000 + u64::from(cycle),
                    screens.as_ptr(),
                    screens.len(),
                    &mut host,
                )
            },
            OK,
            "host create",
        )?;
        ensure(host != 0, "host token missing")?;
        report.record(
            "host_created",
            json!({"export": "pim_app_host_create", "workers": 0}),
        )?;
        let register = serde_json::to_vec(&json!({"version": 1, "command": {
            "kind": "register_recipient", "local_identity": "standalone-dll-fixture"}}))
        .map_err(|e| e.to_string())?;
        status(
            unsafe { (api.host_call)(host, register.as_ptr(), register.len()) },
            OK,
            "trusted setup",
        )?;
        status(
            unsafe { (api.host_call)(host, register.as_ptr(), register.len()) },
            BUSY,
            "pending reply busy",
        )?;
        let registered = retained(api, host)?;
        ensure(
            registered["response"]["kind"] == "recipient_registered",
            "recipient reply kind",
        )?;
        let recipient = registered["response"]["recipient"]
            .as_u64()
            .filter(|id| *id != 0)
            .ok_or("missing bound recipient")?;
        // Registration defaults export consent off. Snapshot requires an enabled
        // event grant even with zero users; approve observation of Only(empty),
        // without control authority, and consume the host acknowledgment first.
        let approve = serde_json::to_vec(&json!({"version": 1, "command": {
            "kind": "approve_events", "recipient": recipient, "grant": {
                "users": {"kind": "only", "users": []}, "pointer_events": true,
                "key_events": false, "owned_windows": false}}}))
        .map_err(|e| e.to_string())?;
        status(
            unsafe { (api.host_call)(host, approve.as_ptr(), approve.len()) },
            OK,
            "explicit event approval",
        )?;
        let approved = retained(api, host)?;
        ensure(
            approved["response"]["kind"] == "acknowledged",
            "event approval reply kind",
        )?;
        report.record(
            "trusted_setup_retained_reply_checked",
            json!({"short_copy_retained": true,
            "pending_call_busy": true, "bound_identity_from_host_reply": true}),
        )?;
        let snapshot = serde_json::to_vec(&json!({"version": 1, "command": {"kind": "snapshot"}}))
            .map_err(|e| e.to_string())?;
        status(
            unsafe { (api.client_call)(host, recipient, snapshot.as_ptr(), snapshot.len()) },
            OK,
            "client snapshot",
        )?;
        let value = retained(api, host)?;
        ensure(
            value["response"]["kind"] == "snapshot"
                && value["response"]["snapshot"]["users"]
                    .as_array()
                    .is_some_and(|users| users.is_empty()),
            "expected empty standalone snapshot",
        )?;
        status(
            unsafe { (api.client_call)(host, recipient, snapshot.as_ptr(), snapshot.len()) },
            OK,
            "queue discard reply",
        )?;
        status(unsafe { (api.discard)(host) }, OK, "response discard")?;
        let mut size = 0;
        status(
            unsafe { (api.size)(host, &mut size) },
            NO_RESPONSE,
            "discard consumed reply",
        )?;
        // Destroy must also consume a retained reply; no explicit End call masks
        // the actual exported host_destroy cleanup path under qualification.
        status(
            unsafe { (api.client_call)(host, recipient, snapshot.as_ptr(), snapshot.len()) },
            OK,
            "queue before destroy",
        )?;
        status(unsafe { (api.destroy)(host) }, OK, "host destroy")?;
        status(
            unsafe { (api.size)(host, &mut size) },
            INVALID_HANDLE,
            "destroy invalidated response token",
        )?;
        status(
            unsafe { (api.destroy)(host) },
            INVALID_HANDLE,
            "destroy stale token",
        )?;
        report.record(
            "host_destroyed_retained_reply_retired",
            json!({"destroy_status": OK,
            "stale_handle_status": INVALID_HANDLE, "client_call_exercised": true,
            "response_discard_exercised": true, "virtual_users_created": 0}),
        )
    }

    fn host_command(api: &Api, host: u64, command: HostCommand) -> Check<Value> {
        let bytes = serde_json::to_vec(&HostRequest {
            version: PROTOCOL_VERSION,
            command,
        })
        .map_err(|e| e.to_string())?;
        status(
            unsafe { (api.host_call)(host, bytes.as_ptr(), bytes.len()) },
            OK,
            "typed host command",
        )?;
        retained(api, host)
    }

    fn client_command(
        api: &Api,
        host: u64,
        recipient: RecipientId,
        command: ClientCommand,
    ) -> Check<Value> {
        let bytes = serde_json::to_vec(&ClientRequest {
            version: PROTOCOL_VERSION,
            command,
        })
        .map_err(|e| e.to_string())?;
        status(
            unsafe {
                (api.client_call)(host, recipient_number(recipient)?, bytes.as_ptr(), bytes.len())
            },
            OK,
            "typed client command",
        )?;
        retained(api, host)
    }

    fn client_command_any(
        api: &Api,
        host: u64,
        recipient: RecipientId,
        command: ClientCommand,
    ) -> Check<Value> {
        let bytes = serde_json::to_vec(&ClientRequest {
            version: PROTOCOL_VERSION,
            command,
        })
        .map_err(|e| e.to_string())?;
        status(
            unsafe {
                (api.client_call)(host, recipient_number(recipient)?, bytes.as_ptr(), bytes.len())
            },
            OK,
            "typed client command with expected denial",
        )?;
        retained_any(api, host)
    }

    fn recipient_number(recipient: RecipientId) -> Check<u64> {
        serde_json::to_value(recipient)
            .map_err(|e| e.to_string())?
            .as_u64()
            .ok_or_else(|| "recipient token was not a u64".into())
    }

    fn coordinates(value: &Value) -> Check<[i64; 4]> {
        Ok([
            value["left"].as_i64().ok_or("missing left")?,
            value["top"].as_i64().ok_or("missing top")?,
            value["right"].as_i64().ok_or("missing right")?,
            value["bottom"].as_i64().ok_or("missing bottom")?,
        ])
    }

    fn rect_coordinates(rect: RECT) -> [i64; 4] {
        [
            i64::from(rect.left),
            i64::from(rect.top),
            i64::from(rect.right),
            i64::from(rect.bottom),
        ]
    }

    struct NativeWindowFacts {
        outer: [i64; 4],
        client: [i64; 4],
        dpi: u32,
        monitor: [i64; 4],
        work_area: [i64; 4],
    }

    /// Sample Win32 independently after each public ABI observation.
    fn native_window_facts(hwnd: HWND) -> Check<NativeWindowFacts> {
        let mut outer = RECT::default();
        let mut client = RECT::default();
        ensure(
            unsafe { GetWindowRect(hwnd, &mut outer) } != 0,
            "GetWindowRect fixture",
        )?;
        ensure(
            unsafe { GetClientRect(hwnd, &mut client) } != 0,
            "GetClientRect fixture",
        )?;
        let mut top_left = POINT {
            x: client.left,
            y: client.top,
        };
        let mut bottom_right = POINT {
            x: client.right,
            y: client.bottom,
        };
        ensure(
            unsafe { ClientToScreen(hwnd, &mut top_left) } != 0
                && unsafe { ClientToScreen(hwnd, &mut bottom_right) } != 0,
            "ClientToScreen fixture",
        )?;
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        ensure(dpi != 0, "GetDpiForWindow fixture")?;
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        ensure(!monitor.is_null(), "MonitorFromWindow fixture")?;
        let mut monitor_info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        ensure(
            unsafe { GetMonitorInfoW(monitor, &mut monitor_info) } != 0,
            "GetMonitorInfoW fixture",
        )?;
        Ok(NativeWindowFacts {
            outer: rect_coordinates(outer),
            client: [
                i64::from(top_left.x),
                i64::from(top_left.y),
                i64::from(bottom_right.x),
                i64::from(bottom_right.y),
            ],
            dpi,
            monitor: rect_coordinates(monitor_info.rcMonitor),
            work_area: rect_coordinates(monitor_info.rcWork),
        })
    }

    fn check_metadata(window: &Value, hwnd: HWND, title: &str, rectangle: [i64; 4]) -> Check {
        let native = native_window_facts(hwnd)?;
        let metadata = &window["metadata"];
        ensure(
            window.get("hwnd").is_none() && metadata.get("hwnd").is_none()
                && metadata.get("hmonitor").is_none(),
            "native handles absent from window wire metadata",
        )?;
        ensure(
            native.outer == rectangle && native.client == rectangle,
            "known borderless native client/outer geometry",
        )?;
        ensure(
            coordinates(&window["bounds"])? == native.outer,
            "outer window bounds equal Win32",
        )?;
        ensure(metadata["title"] == title, "observed owned-window title")?;
        ensure(
            metadata["process_id"].as_u64() == Some(u64::from(unsafe { GetCurrentProcessId() }))
                && metadata["ui_thread_id"].as_u64()
                    == Some(u64::from(unsafe { GetCurrentThreadId() })),
            "observed owner PID/UI thread",
        )?;
        ensure(
            coordinates(&metadata["client_bounds"])? == native.client,
            "physical desktop client bounds equal ClientToScreen",
        )?;
        ensure(
            metadata["dpi"].as_u64() == Some(u64::from(native.dpi)),
            "observed window DPI equals GetDpiForWindow",
        )?;
        ensure(
            coordinates(&metadata["monitor_bounds"])? == native.monitor
                && coordinates(&metadata["monitor_work_area"])? == native.work_area,
            "observed monitor bounds and work area equal GetMonitorInfoW",
        )?;
        ensure(
            native.monitor[0] < native.monitor[2]
                && native.monitor[1] < native.monitor[3]
                && native.client[0] >= native.monitor[0]
                && native.client[1] >= native.monitor[1]
                && native.client[2] <= native.monitor[2]
                && native.client[3] <= native.monitor[3]
                && native.work_area[0] >= native.monitor[0]
                && native.work_area[1] >= native.monitor[1]
                && native.work_area[2] <= native.monitor[2]
                && native.work_area[3] <= native.monitor[3]
                && native.work_area[0] < native.work_area[2]
                && native.work_area[1] < native.work_area[3],
            "native monitor and work-area containment",
        )
    }

    fn check_native_geometry(hwnd: HWND, rectangle: [i64; 4]) -> Check {
        let native = native_window_facts(hwnd)?;
        ensure(
            native.outer == rectangle && native.client == rectangle,
            "known borderless native client/outer geometry",
        )
    }

    struct RetireOnDestroy {
        api: *const Api,
        host: u64,
        window: WindowId,
        previous_proc: isize,
        result: Option<Check<Value>>,
    }

    unsafe extern "system" fn retire_on_nc_destroy(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let context = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut RetireOnDestroy;
        if context.is_null() {
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        // The stack context and DLL function pointers outlive synchronous DestroyWindow.
        let context = unsafe { &mut *context };
        if message == WM_NCDESTROY {
            context.result = Some(
                catch_unwind(AssertUnwindSafe(|| {
                    host_command(
                        unsafe { &*context.api },
                        context.host,
                        HostCommand::RetireOwnWindow { window: context.window },
                    )
                }))
                .unwrap_or_else(|_| Err("retire callback panicked".into())),
            );
        }
        let previous = unsafe {
            std::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(context.previous_proc)
        };
        let result = unsafe { CallWindowProcW(Some(previous), hwnd, message, wparam, lparam) };
        if message == WM_NCDESTROY {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        }
        result
    }

    fn exercise_window_metadata(api: &Api, cycle: u32, report: &mut Report) -> Check {
        ensure(
            !unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE) }
                .is_null(),
            "per-monitor DPI context required",
        )?;
        let screens = serde_json::to_vec(&json!({"revision": 1,
            "desktop": {"left": 0, "top": 0, "right": 800, "bottom": 600},
            "screens": [{"left": 0, "top": 0, "right": 800, "bottom": 600}]}))
            .map_err(|e| e.to_string())?;
        let mut host = 0;
        status(
            unsafe {
                (api.create)(
                    9000 + u64::from(cycle),
                    screens.as_ptr(),
                    screens.len(),
                    &mut host,
                )
            },
            OK,
            "metadata host create",
        )?;
        ensure(host != 0, "metadata host token missing")?;
        let registered = host_command(
            api,
            host,
            HostCommand::RegisterRecipient {
                local_identity: "standalone-owned-window-metadata-fixture".into(),
            },
        )?;
        ensure(
            registered["response"]["kind"] == "recipient_registered",
            "recipient registration",
        )?;
        let recipient: RecipientId =
            serde_json::from_value(registered["response"]["recipient"].clone())
                .map_err(|e| e.to_string())?;
        let grant = Grant {
            users: UserScope::AllOwned,
            pointer_events: false,
            key_events: false,
            owned_windows: true,
        };
        ensure(
            host_command(api, host, HostCommand::ApproveEvents { recipient, grant })?["response"]
                ["kind"]
                == "acknowledged",
            "window observation grant",
        )?;
        let controls = ControlGrant {
            users: UserScope::AllOwned,
            delegated_users: false,
            user_lifecycle: false,
            cursor: false,
            pointer_input: false,
            key_input: false,
            window_tracking: true,
            window_control: false,
        };
        ensure(
            host_command(
                api,
                host,
                HostCommand::ApproveControls {
                    recipient,
                    grant: controls,
                },
            )?["response"]["kind"]
                == "acknowledged",
            "window tracking grant",
        )?;

        let class = wide(OsStr::new("STATIC"))?;
        let first_title = "PIM owned metadata A";
        let second_title = "PIM owned metadata B";
        let title = wide(OsStr::new(first_title))?;
        let first = [64, 72, 344, 252];
        let second = [96, 104, 416, 324];
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                64,
                72,
                280,
                180,
                null_mut(),
                null_mut(),
                null_mut(),
                std::ptr::null(),
            )
        };
        ensure(!hwnd.is_null(), "owned top-level fixture window creation")?;
        check_native_geometry(hwnd, first)?;
        let registered = host_command(
            api,
            host,
            HostCommand::RegisterOwnWindow {
                recipient,
                native_window: hwnd as usize,
            },
        )?;
        ensure(
            registered["response"]["kind"] == "window_registered",
            "window registration",
        )?;
        let window: WindowId = serde_json::from_value(registered["response"]["window"].clone())
            .map_err(|e| e.to_string())?;
        let query = client_command(api, host, recipient, ClientCommand::WindowQuery { window })?;
        check_metadata(&query["response"]["window"], hwnd, first_title, first)?;
        let subscribed = client_command(
            api,
            host,
            recipient,
            ClientCommand::Subscribe { capacity: 8 },
        )?;
        ensure(subscribed["response"]["kind"] == "subscribed", "window subscription")?;
        let subscription: SubscriptionId =
            serde_json::from_value(subscribed["response"]["subscription"].clone())
                .map_err(|e| e.to_string())?;
        let tracked = client_command(api, host, recipient, ClientCommand::WindowTrack { window })?;
        check_metadata(&tracked["response"]["window"], hwnd, first_title, first)?;
        let poll = client_command(
            api,
            host,
            recipient,
            ClientCommand::Poll {
                subscription,
                max_events: 8,
            },
        )?;
        ensure(poll["response"]["poll"]["kind"] == "events", "tracked poll kind")?;
        let tracked_events = poll["response"]["poll"]["events"]
            .as_array()
            .ok_or("tracked events missing")?;
        ensure(
            tracked_events.len() == 1
                && tracked_events[0]["event"]["kind"] == "window"
                && tracked_events[0]["event"]["update"]["kind"] == "tracked",
            "tracked event",
        )?;
        check_metadata(
            &tracked_events[0]["event"]["update"]["window"],
            hwnd,
            first_title,
            first,
        )?;

        let changed_title = wide(OsStr::new(second_title))?;
        ensure(
            unsafe { SetWindowTextW(hwnd, changed_title.as_ptr()) } != 0,
            "SetWindowTextW fixture",
        )?;
        ensure(unsafe { MoveWindow(hwnd, 96, 104, 320, 220, 0) } != 0, "MoveWindow fixture")?;
        check_native_geometry(hwnd, second)?;
        let refreshed = host_command(api, host, HostCommand::RefreshOwnWindow { window })?;
        ensure(refreshed["response"]["kind"] == "window", "refresh reply kind")?;
        check_metadata(&refreshed["response"]["window"], hwnd, second_title, second)?;
        let poll = client_command(
            api,
            host,
            recipient,
            ClientCommand::Poll {
                subscription,
                max_events: 8,
            },
        )?;
        ensure(poll["response"]["poll"]["kind"] == "events", "changed poll kind")?;
        let changed_events = poll["response"]["poll"]["events"]
            .as_array()
            .ok_or("changed events missing")?;
        ensure(
            changed_events.len() == 1
                && changed_events[0]["event"]["kind"] == "window"
                && changed_events[0]["event"]["update"]["kind"] == "changed",
            "changed event",
        )?;
        check_metadata(
            &changed_events[0]["event"]["update"]["window"],
            hwnd,
            second_title,
            second,
        )?;
        let query = client_command(api, host, recipient, ClientCommand::WindowQuery { window })?;
        check_metadata(&query["response"]["window"], hwnd, second_title, second)?;
        let snapshot = client_command(api, host, recipient, ClientCommand::Snapshot)?;
        let windows = snapshot["response"]["snapshot"]["windows"]
            .as_array()
            .ok_or("window snapshot missing")?;
        ensure(windows.len() == 1, "tracked window snapshot size")?;
        check_metadata(&windows[0], hwnd, second_title, second)?;
        report.record("owned_window_metadata_checked", json!({
            "host_create": true, "registered_same_process_ui_thread": true,
            "query_track_event_refresh_snapshot_checked": true,
            "fixture_titles_and_exact_physical_clients_checked": true,
            "exact_os_dpi_and_monitor_checked": true, "host_input_sent": false,
        }))?;

        let mut retirement = RetireOnDestroy {
            api: api as *const Api,
            host,
            window,
            previous_proc: 0,
            result: None,
        };
        unsafe {
            SetLastError(0);
        }
        let previous_data = unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (&mut retirement as *mut RetireOnDestroy) as isize,
            )
        };
        ensure(
            previous_data == 0 && unsafe { GetLastError() } == 0,
            "fixture user data slot",
        )?;
        unsafe {
            SetLastError(0);
        }
        retirement.previous_proc =
            unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, retire_on_nc_destroy as isize) };
        ensure(
            retirement.previous_proc != 0 || unsafe { GetLastError() } == 0,
            "install destruction observer",
        )?;
        ensure(retirement.previous_proc != 0, "previous fixture WndProc missing")?;
        ensure(unsafe { DestroyWindow(hwnd) } != 0, "DestroyWindow fixture")?;
        let retired = retirement.result.take().ok_or("WM_NCDESTROY callback absent")??;
        ensure(retired["response"]["kind"] == "acknowledged", "WM_NCDESTROY retired token")?;
        let stale = client_command_any(
            api,
            host,
            recipient,
            ClientCommand::WindowQuery { window },
        )?;
        ensure(
            stale["status"] == "error" && stale["error"]["code"] == "missing",
            "retired window query denied",
        )?;
        report.record("nc_destroy_retired_and_stale_denied", json!({
            "wm_nc_destroy_retirement": true, "stale_query_code": "missing",
            "owned_window_destroyed": true,
        }))?;

        status(unsafe { (api.destroy)(host) }, OK, "metadata host destroy")?;
        let mut size = 0;
        status(
            unsafe { (api.size)(host, &mut size) },
            INVALID_HANDLE,
            "metadata host stale token",
        )?;
        report.record("host_destroyed_after_window_retirement", json!({
            "destroy_status": OK, "stale_handle_status": INVALID_HANDLE,
            "window_retired_before_destroy": true, "virtual_users_created": 0,
        }))
    }

    fn retained(api: &Api, host: u64) -> Check<Value> {
        let value = retained_any(api, host)?;
        ensure(
            value["status"] == "ok" && value["version"] == 1,
            "invalid successful reply envelope",
        )?;
        Ok(value)
    }

    fn retained_any(api: &Api, host: u64) -> Check<Value> {
        let mut needed = 0;
        status(
            unsafe { (api.size)(host, &mut needed) },
            OK,
            "response size",
        )?;
        ensure((2..=MAX_RESPONSE).contains(&needed), "response bound")?;
        let original = needed;
        let mut tiny = [0u8; 1];
        status(
            unsafe { (api.read)(host, tiny.as_mut_ptr(), tiny.len(), &mut needed) },
            SHORT,
            "short copy",
        )?;
        ensure(needed == original, "short copy changed size")?;
        status(
            unsafe { (api.read)(host, null_mut(), 0, &mut needed) },
            SHORT,
            "zero capacity retry",
        )?;
        ensure(needed == original, "retry changed size")?;
        let mut queried = 0;
        status(
            unsafe { (api.size)(host, &mut queried) },
            OK,
            "retained size",
        )?;
        ensure(queried == original, "reply not retained")?;
        let mut bytes = vec![0u8; needed];
        status(
            unsafe { (api.read)(host, bytes.as_mut_ptr(), bytes.len(), &mut needed) },
            OK,
            "full copy",
        )?;
        ensure(needed == original, "full copy changed size")?;
        status(
            unsafe { (api.size)(host, &mut queried) },
            NO_RESPONSE,
            "copy consumed reply",
        )?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        ensure(value["version"] == 1, "invalid reply envelope version")?;
        Ok(value)
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
