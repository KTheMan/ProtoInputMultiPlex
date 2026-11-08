#![cfg(all(windows, target_arch = "x86_64"))]
use mux_core::ResolvedSeat;
use mux_windows::protoinput::Backend;
use std::{
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child() -> ChildGuard {
    ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fixture_child"])
            .env("MUX_ABI_CHILD", "1")
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn fixture_window(pid: u32) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(windows) = mux_windows::targets::windows()
            && let Some(target) = windows
                .into_iter()
                .filter(|window| window.pid == pid)
                .find(|window| mux_windows::targets::client_bounds(window.hwnd, pid).is_ok())
        {
            return target.hwnd;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture PID {pid} did not publish a visible valid window"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn fixture_child() {
    if std::env::var_os("MUX_ABI_CHILD").is_some() {
        use windows_sys::Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
            Graphics::Gdi::UpdateWindow,
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG,
                RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WNDCLASSW,
                WS_EX_NOACTIVATE, WS_POPUP, WS_VISIBLE,
            },
        };
        unsafe extern "system" fn window_proc(
            hwnd: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }

        let class_name: Vec<u16> = "ProtoInputMultiPlexAbiFixtureClass\0"
            .encode_utf16()
            .collect();
        let title: Vec<u16> = "ProtoInputMultiPlex ABI Fixture\0".encode_utf16().collect();
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            assert!(!instance.is_null());
            let mut class: WNDCLASSW = std::mem::zeroed();
            class.lpfnWndProc = Some(window_proc);
            class.hInstance = instance;
            class.lpszClassName = class_name.as_ptr();
            assert_ne!(RegisterClassW(&class), 0);
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP | WS_VISIBLE,
                -32000,
                -32000,
                640,
                480,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance as HINSTANCE,
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            UpdateWindow(hwnd);
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

#[test]
fn closing_job_terminates_owned_worker() {
    let mut worker = child();
    let watched = mux_windows::targets::ProcessWatch::open([worker.0.id()]).unwrap();
    assert_eq!(watched.exited(), None);
    let job = mux_windows::job::Job::own(&worker.0).unwrap();
    assert!(worker.0.try_wait().unwrap().is_none());
    drop(job);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while worker.0.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "worker survived closing its job"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(watched.exited(), Some(worker.0.id()));
}
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ffi_configuration_and_partial_failure_cleanup() {
    let folder =
        Temp(std::env::temp_dir().join(format!("protoinputmultiplex-abi-{}", std::process::id())));
    std::fs::create_dir_all(&folder.0).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loader.rs");
    let dll = folder.0.join("ProtoInputLoader64.dll");
    let result = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-type=cdylib")
        .arg(source)
        .arg("-o")
        .arg(&dll)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for name in [
        "ProtoInputIJ64.exe",
        "ProtoInputIJP64.dll",
        "ProtoInputHooks64.dll",
    ] {
        std::fs::write(folder.0.join(name), []).unwrap();
    }
    let a = child();
    let mut b = child();
    let seats = vec![
        ResolvedSeat {
            name: "Blue".into(),
            mouse: 123,
            keyboard: Some(456),
            target_pid: a.0.id(),
        },
        ResolvedSeat {
            name: "Coral".into(),
            mouse: 789,
            keyboard: None,
            target_pid: b.0.id(),
        },
    ];
    unsafe {
        let fixture = libloading::Library::new(&dll).unwrap();
        let reset = fixture
            .get::<unsafe extern "C" fn(u32)>(b"TestReset\0")
            .unwrap();
        let read = fixture
            .get::<unsafe extern "C" fn(usize, *mut u32) -> bool>(b"TestRead\0")
            .unwrap();
        let read_fence = fixture
            .get::<unsafe extern "C" fn(usize, *mut u64) -> bool>(b"TestReadFence\0")
            .unwrap();
        let calls = || {
            let mut records = Vec::new();
            let mut row = [0; 3];
            while read(records.len(), row.as_mut_ptr()) {
                records.push(row);
            }
            records
        };
        let fences = || {
            let mut records = Vec::new();
            let mut row = [0; 4];
            while read_fence(records.len(), row.as_mut_ptr()) {
                records.push(row);
            }
            records
        };
        reset(0);
        let window_a = fixture_window(a.0.id());
        let _window_b = fixture_window(b.0.id());
        let session = Backend::load(&folder.0).unwrap().attach(&seats).unwrap();
        assert!(
            Backend::load(&folder.0).is_err(),
            "upstream global state requires one owner per host process"
        );
        assert_eq!(session.instance_count(), 2);
        let before_invalid_window = calls().len();
        assert!(session.desktop(0, 0x1234, true).is_err());
        assert_eq!(calls().len(), before_invalid_window);
        session.pause(true).unwrap();
        session.pause(false).unwrap();
        let active = calls();
        assert!(active.contains(&[8, 1, 1]));
        assert!(active.contains(&[6, 1, 123]));
        assert!(active.contains(&[7, 1, 456]));
        assert!(active.contains(&[10, 1, 15]));
        assert!(active.contains(&[10, 2, 7]));
        assert!(!active.iter().any(|c| c[0] == 7 && c[1] == 2));
        drop(session);
        let stopped = calls();
        assert!(stopped.contains(&[20, 1, 0x02A3]));
        assert!(stopped.contains(&[12, 1, 0]));
        assert!(stopped.contains(&[12, 2, 0]));
        assert_eq!(stopped.iter().filter(|c| c[0] == 3).count(), 22);
        assert_eq!(stopped.last(), Some(&[3, 1, 0]));
        assert!(
            fences().is_empty(),
            "unselected physical inputs need no fence"
        );

        reset(2);
        assert!(Backend::load(&folder.0).unwrap().attach(&seats).is_err());
        let failed = calls();
        assert!(failed.contains(&[12, 1, 0]));
        assert_eq!(failed.iter().filter(|c| c[0] == 3).count(), 11);

        reset(0);
        let mut virtual_seats = seats.clone();
        virtual_seats[0].mouse = 0xF1000002;
        virtual_seats[0].keyboard = Some(0xF1000003);
        let mut session = Backend::load(&folder.0)
            .unwrap()
            .attach(&virtual_seats)
            .unwrap();
        let packet = mux_core::emulated::Packet::motion(0xF1000002, -40, 25);
        session.submit(packet).unwrap();
        let input = calls();
        assert!(input.contains(&[17, 1, window_a as u32]));
        assert!(input.contains(&[13, 1, packet.device]));
        assert!(
            input.contains(&[13, 2, packet.device]),
            "native filters must receive the broadcast"
        );
        assert!(input.contains(&[15, (-40i32) as u32, 25]));
        let before = input.len();
        assert!(
            session
                .submit(mux_core::emulated::Packet::motion(0xF1000004, 1, 0))
                .is_err()
        );
        assert!(
            session
                .submit(mux_core::emulated::Packet::motion(123, 1, 0))
                .is_err()
        );
        assert_eq!(calls().len(), before, "invalid packets must not cross FFI");
        let before_failed_shutdown = calls().len();
        assert!(session.shutdown().is_err());
        let fence_calls = fences();
        assert_eq!(fence_calls.len(), 1);
        assert_eq!(fence_calls[0][0], 1);
        assert_eq!(fence_calls[0][1], window_a as u64);
        assert_ne!(fence_calls[0][2], 0);
        assert_ne!(fence_calls[0][3], 0);
        let after_failed_shutdown = calls();
        let shutdown_calls = &after_failed_shutdown[before_failed_shutdown..];
        assert!(
            !shutdown_calls
                .iter()
                .any(|call| call[1] == 1 && (call[0] == 3 || call[0] == 5)),
            "the target whose fence failed must retain release normalization"
        );
        for instance in [1, 2] {
            assert!(
                shutdown_calls.contains(&[9, instance, 1]),
                "every live target must freeze"
            );
            assert!(
                shutdown_calls.contains(&[12, instance, 0]),
                "every live focus loop must stop"
            );
            assert!(
                shutdown_calls.contains(&[10, instance, 0]),
                "every live target must stop emission"
            );
        }
        assert!(
            shutdown_calls
                .iter()
                .any(|call| call[1] == 2 && call[0] == 3)
        );
        assert!(
            shutdown_calls
                .iter()
                .any(|call| call[1] == 2 && call[0] == 5),
            "the healthy target with no pending recipients can still retire"
        );
        drop(session);
        assert_eq!(
            calls().len(),
            after_failed_shutdown.len(),
            "Drop must not retry a failed one-shot fence"
        );

        reset(0);
        let mut desktop_devices = virtual_seats.clone();
        desktop_devices[1].mouse = 0xF1000004;
        desktop_devices[1].keyboard = Some(0xF1000005);
        let mut desktop = Backend::load(&folder.0)
            .unwrap()
            .desktop_session(desktop_devices)
            .unwrap();
        assert!(
            calls().is_empty(),
            "automatic mode must not inject until a window is selected"
        );
        let first = desktop.open(a.0.id()).unwrap();
        let second = desktop.open(b.0.id()).unwrap();
        assert_eq!(desktop.open(a.0.id()).unwrap(), first);
        assert_eq!(
            calls().iter().filter(|c| c[0] == 0).count(),
            2,
            "reuse one native instance per process"
        );
        let key = mux_core::emulated::Packet::key(0xF1000003, 0x57, false);
        let before_unpositioned_submit = calls().len();
        assert!(desktop.submit(first, key).is_err());
        assert_eq!(calls().len(), before_unpositioned_submit);
        let bounds = mux_windows::targets::client_bounds(window_a, a.0.id()).unwrap();
        desktop
            .position(
                first,
                window_a,
                0xF1000002,
                bounds.left + 70,
                bounds.top + 90,
                bounds,
            )
            .unwrap();
        let before = calls().len();
        desktop.submit(first, key).unwrap();
        let direct = calls();
        assert!(direct[before..].contains(&[13, 1, 0xF1000003]));
        assert!(
            !direct[before..].iter().any(|c| c[0] == 13 && c[1] == 2),
            "automatic routing must not broadcast into other applications"
        );
        desktop
            .position(
                first,
                window_a,
                0xF1000002,
                bounds.left + 70,
                bounds.top + 90,
                bounds,
            )
            .unwrap();
        assert!(calls().contains(&[17, 1, window_a as u32]));
        assert!(calls().contains(&[18, 1, 0]));
        assert!(calls().contains(&[19, 1, 0x02A3]));
        assert!(calls().contains(&[15, 70, 90]));
        desktop.pause(true).unwrap();
        desktop.pause(false).unwrap();
        b.0.kill().unwrap();
        b.0.wait().unwrap();
        assert!(!desktop.alive(second));
        assert_eq!(desktop.exited(), vec![b.0.id()]);
        assert!(desktop.exited().is_empty());
        assert!(desktop.submit(second, packet).is_err());
        let before = calls().len();
        assert!(desktop.shutdown().is_err());
        let failed_cleanup = calls();
        assert!(
            !failed_cleanup[before..]
                .iter()
                .any(|call| call[0] == 3 || call[0] == 5)
        );
        assert!(
            !failed_cleanup[before..].iter().any(|call| call[1] == 2),
            "the exited second target receives no shutdown callbacks"
        );
        assert_eq!(fences().len(), 1);
        drop(desktop);
        let cleanup = calls();
        assert_eq!(
            cleanup[before..].iter().filter(|c| c[0] == 3).count(),
            0,
            "do not unhook before a live target acknowledges its fence"
        );
        assert_eq!(
            cleanup.len(),
            failed_cleanup.len(),
            "Drop must not retry shutdown"
        );

        reset(0);
        std::fs::remove_file(folder.0.join("ProtoInputHooks64.dll")).unwrap();
        assert!(Backend::load(&folder.0).unwrap().attach(&seats).is_err());
        assert!(
            calls().is_empty(),
            "preflight must finish before any injection"
        );
    }
}
