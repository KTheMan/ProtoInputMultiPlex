#![cfg(windows)]
//! Real cross-process hit testing with an external click-through overlay and a native child control.
use std::{
    ffi::c_void,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::WindowsAndMessaging::*,
};

type DesktopHandle = *mut c_void;
// These user32 APIs live behind a windows-sys feature that this crate does not enable.
// Keep the test fixture self-contained instead of changing product dependencies.
#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "CreateDesktopW"]
    fn create_desktop(
        name: *const u16,
        device: *const u16,
        mode: *const c_void,
        flags: u32,
        access: u32,
        security: *const c_void,
    ) -> DesktopHandle;
    #[link_name = "OpenDesktopW"]
    fn open_desktop(name: *const u16, flags: u32, inherit: i32, access: u32) -> DesktopHandle;
    #[link_name = "GetThreadDesktop"]
    fn get_thread_desktop(thread: u32) -> DesktopHandle;
    #[link_name = "SetThreadDesktop"]
    fn set_thread_desktop(desktop: DesktopHandle) -> i32;
    #[link_name = "CloseDesktop"]
    fn close_desktop(desktop: DesktopHandle) -> i32;
}

// DESKTOP_CREATEWINDOW | DESKTOP_ENUMERATE | DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS.
// No SWITCHDESKTOP access is requested, and this test never changes the visible desktop.
const FIXTURE_DESKTOP_ACCESS: u32 = 0x00c3;

struct FixtureDesktop {
    original: DesktopHandle,
    private: DesktopHandle,
}

impl FixtureDesktop {
    fn original() -> DesktopHandle {
        let original = unsafe { get_thread_desktop(GetCurrentThreadId()) };
        assert!(
            !original.is_null(),
            "read original thread desktop: {}",
            std::io::Error::last_os_error()
        );
        original
    }

    fn attach(private: DesktopHandle, original: DesktopHandle) -> Self {
        if unsafe { set_thread_desktop(private) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe { close_desktop(private) };
            panic!("attach fixture thread to private desktop: {error}");
        }
        Self { original, private }
    }

    fn create(name: &str) -> Self {
        let original = Self::original();
        let name = wide(name);
        let private = unsafe {
            create_desktop(
                name.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                FIXTURE_DESKTOP_ACCESS,
                std::ptr::null(),
            )
        };
        assert!(
            !private.is_null(),
            "create private fixture desktop: {}",
            std::io::Error::last_os_error()
        );
        Self::attach(private, original)
    }

    fn open(name: &str) -> Self {
        let original = Self::original();
        let name = wide(name);
        let private = unsafe { open_desktop(name.as_ptr(), 0, 0, FIXTURE_DESKTOP_ACCESS) };
        assert!(
            !private.is_null(),
            "open private fixture desktop: {}",
            std::io::Error::last_os_error()
        );
        Self::attach(private, original)
    }
}

impl Drop for FixtureDesktop {
    fn drop(&mut self) {
        let restored = unsafe { set_thread_desktop(self.original) };
        let restore_error = std::io::Error::last_os_error();
        let closed = if restored != 0 {
            unsafe { close_desktop(self.private) }
        } else {
            0
        };
        let close_error = std::io::Error::last_os_error();
        if restored == 0 || closed == 0 {
            let message = format!(
                "restore fixture desktop: {restored} ({restore_error}); close private desktop: {closed} ({close_error})"
            );
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}

struct TargetWindow(HWND);
impl Drop for TargetWindow {
    fn drop(&mut self) {
        assert_ne!(
            unsafe { DestroyWindow(self.0) },
            0,
            "destroy owned target window"
        );
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}
#[test]
fn surface_fixture() {
    let Some(kind) = std::env::var_os("MUX_HIT_FIXTURE") else {
        return;
    };
    let desktop_name = std::env::var("MUX_HIT_DESKTOP").unwrap();
    let _desktop = FixtureDesktop::open(&desktop_name);
    let path = PathBuf::from(std::env::var_os("MUX_HIT_PATH").unwrap());
    let mut target_window = None;
    let mut overlay = None;
    if kind == "overlay" {
        let mut cursor = mux_windows::overlay::Overlay::new("External cursor fixture").unwrap();
        cursor
            .draw(
                64,
                80,
                "External cursor fixture",
                &mux_core::desktop::CursorSettings::default(),
                false,
            )
            .unwrap();
        std::fs::write(&path, cursor.status.hwnd.to_string()).unwrap();
        overlay = Some(cursor);
    } else {
        unsafe {
            let class = wide("ProtoInputMultiPlex.HitFixture");
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class.as_ptr(),
                wide("Desktop target fixture").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                10,
                10,
                400,
                300,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());
            target_window = Some(TargetWindow(hwnd));
            let edit = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE,
                20,
                20,
                200,
                80,
                hwnd,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            assert!(!edit.is_null());
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                10,
                10,
                400,
                300,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            std::fs::write(&path, format!("{} {}", hwnd as usize, edit as usize)).unwrap();
        }
    }
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(overlay);
    drop(target_window);
}
struct Fixture {
    child: Child,
    path: PathBuf,
}
impl Fixture {
    fn start(kind: &str, desktop_name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mux-hit-{}-{kind}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "surface_fixture"])
            .env("MUX_HIT_FIXTURE", kind)
            .env("MUX_HIT_DESKTOP", desktop_name)
            .env("MUX_HIT_PATH", &path)
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self { child, path }
    }
    fn ready(&mut self) -> Vec<usize> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(text) = std::fs::read_to_string(&self.path)
                && !text.is_empty()
            {
                return text
                    .split_whitespace()
                    .map(|s| s.parse().unwrap())
                    .collect();
            }
            assert!(self.child.try_wait().unwrap().is_none(), "fixture exited");
            assert!(Instant::now() < until, "fixture startup timeout");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn assert_only_fixture_windows(target: (usize, u32), overlay: (usize, u32)) {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        unsafe {
            let windows = &mut *(data as *mut Vec<(usize, u32)>);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            windows.push((hwnd as usize, pid));
        }
        1
    }
    let mut windows = Vec::new();
    assert_ne!(
        unsafe { EnumWindows(Some(collect), &mut windows as *mut _ as LPARAM) },
        0,
        "enumerate private fixture desktop: {}",
        std::io::Error::last_os_error()
    );
    assert!(
        windows.contains(&target),
        "target absent from private desktop: {windows:?}"
    );
    assert!(
        windows.contains(&overlay),
        "overlay absent from private desktop: {windows:?}"
    );
    assert!(
        windows
            .iter()
            .all(|(_, pid)| *pid == target.1 || *pid == overlay.1),
        "private desktop contains an unrelated window: {windows:?}"
    );
}

#[test]
fn external_clickthrough_overlay_does_not_mask_child_target_or_scope() {
    let desktop_name = format!(
        "ProtoInputMultiPlex.HitTest.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _desktop = FixtureDesktop::create(&desktop_name);
    let mut target = Fixture::start("target", &desktop_name);
    let ids = target.ready();
    let rect = mux_windows::targets::client_bounds(ids[1], target.child.id()).unwrap();
    let x = rect.left + 50;
    let y = rect.top + 40;
    let mut overlay = Fixture::start("overlay", &desktop_name);
    let overlay_ids = overlay.ready();
    assert_only_fixture_windows(
        (ids[0], target.child.id()),
        (overlay_ids[0], overlay.child.id()),
    );
    // The overlay belongs to another process and overlaps this point; it must be ignored.
    assert!((64..284).contains(&x) && (80..164).contains(&y));
    for scope in [vec![], vec![target.child.id()]] {
        let hit = mux_windows::targets::at_point(x, y, &[std::process::id()], &scope)
            .unwrap()
            .unwrap();
        assert_eq!(hit.window.pid, target.child.id());
        assert_eq!(hit.window.hwnd, ids[1]);
        assert_eq!(hit.region, HTCLIENT as i32);
    }
    assert!(
        mux_windows::targets::at_point(x, y, &[target.child.id()], &[])
            .unwrap()
            .is_none()
    );
    assert!(
        mux_windows::targets::at_point(x, y, &[], &[overlay.child.id()])
            .unwrap()
            .is_none(),
        "an ineligible foreground window must not fall through into an unrelated app"
    );
}
