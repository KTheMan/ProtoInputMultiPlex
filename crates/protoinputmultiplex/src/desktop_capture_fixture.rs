//! Disposable opaque desktop surface for screenshot-export validation only.
use anyhow::{Context, Result, ensure};
use mux_core::desktop::Bounds;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentProcessId},
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
        WindowsAndMessaging::*,
    },
};

const TIMER_ID: usize = 1;
const REPORT_INTERVAL_MS: u32 = 250;
const COLOR_RGB: [u8; 3] = [24, 48, 72];
const COLORREF: u32 = (72 << 16) | (48 << 8) | 24;

static LIVE: OnceLock<Mutex<LiveReport>> = OnceLock::new();
static BRUSH_HANDLE: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Serialize)]
struct WindowFlags {
    popup: bool,
    topmost: bool,
    no_activate: bool,
    transparent: bool,
    tool_window: bool,
    visible: bool,
}

#[derive(Serialize)]
struct Report {
    pid: u32,
    hwnd: usize,
    desktop_bounds: Bounds,
    window_bounds: Option<Bounds>,
    client_bounds: Option<Bounds>,
    color_rgb: [u8; 3],
    frames: u64,
    paint_count: u64,
    window_flags: Option<WindowFlags>,
    foreground_before: usize,
    foreground_after_show: Option<usize>,
    foreground_after_close: Option<usize>,
    foreground_unchanged: Option<bool>,
    max_seconds: u64,
    timed_out: bool,
    closed: bool,
    exit_reason: Option<String>,
    error: Option<String>,
}

struct LiveReport {
    path: PathBuf,
    started: Instant,
    max_duration: Duration,
    snapshot: Report,
}

struct FixtureResources {
    hwnd: HWND,
    module: HINSTANCE,
    class_name: Vec<u16>,
    brush: HBRUSH,
    timer: usize,
    registered: bool,
}

impl Drop for FixtureResources {
    fn drop(&mut self) {
        unsafe {
            if self.timer != 0 && !self.hwnd.is_null() {
                KillTimer(self.hwnd, TIMER_ID);
            }
            if !self.hwnd.is_null() && IsWindow(self.hwnd) != 0 {
                DestroyWindow(self.hwnd);
            }
            if !self.brush.is_null() {
                BRUSH_HANDLE.store(0, Ordering::Relaxed);
                DeleteObject(self.brush);
            }
            if self.registered {
                UnregisterClassW(self.class_name.as_ptr(), self.module);
            }
        }
    }
}

pub fn run(report: PathBuf, max_duration: Duration) -> Result<()> {
    ensure!(
        (Duration::from_secs(1)..=Duration::from_secs(180)).contains(&max_duration),
        "desktop capture fixture limit must be between 1 and 180 seconds"
    );
    ensure!(
        LIVE.get().is_none(),
        "desktop capture fixture is already initialized"
    );
    unsafe {
        ensure!(
            !SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_null(),
            "set physical desktop fixture DPI context"
        );
    }
    let desktop = mux_windows::targets::desktop_bounds();
    ensure!(
        desktop.valid_region(),
        "physical virtual desktop bounds are invalid"
    );
    let width = i32::try_from(i64::from(desktop.right) - i64::from(desktop.left))?;
    let height = i32::try_from(i64::from(desktop.bottom) - i64::from(desktop.top))?;
    let max_seconds = max_duration.as_secs();
    let pid = unsafe { GetCurrentProcessId() };
    let foreground_before = unsafe { GetForegroundWindow() as usize };
    LIVE.set(Mutex::new(LiveReport {
        path: report,
        started: Instant::now(),
        max_duration,
        snapshot: Report {
            pid,
            hwnd: 0,
            desktop_bounds: desktop,
            window_bounds: None,
            client_bounds: None,
            color_rgb: COLOR_RGB,
            frames: 0,
            paint_count: 0,
            window_flags: None,
            foreground_before,
            foreground_after_show: None,
            foreground_after_close: None,
            foreground_unchanged: None,
            max_seconds,
            timed_out: false,
            closed: false,
            exit_reason: None,
            error: None,
        },
    }))
    .map_err(|_| anyhow::anyhow!("desktop capture fixture was initialized more than once"))?;

    let mut resources = FixtureResources {
        hwnd: std::ptr::null_mut(),
        module: std::ptr::null_mut(),
        class_name: wide("ProtoInputMultiPlex.DesktopCaptureFixture"),
        brush: std::ptr::null_mut(),
        timer: 0,
        registered: false,
    };
    unsafe {
        resources.module = GetModuleHandleW(std::ptr::null());
        ensure!(
            !resources.module.is_null(),
            "get desktop fixture module handle"
        );
        resources.brush = CreateSolidBrush(COLORREF);
        ensure!(
            !resources.brush.is_null(),
            "create solid desktop fixture brush"
        );
        BRUSH_HANDLE.store(resources.brush as usize, Ordering::Relaxed);
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: resources.module,
            hbrBackground: resources.brush,
            lpszClassName: resources.class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        ensure!(
            RegisterClassW(&window_class) != 0,
            "register desktop fixture window class"
        );
        resources.registered = true;

        let extended_style =
            WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW;
        let hwnd = CreateWindowExW(
            extended_style,
            resources.class_name.as_ptr(),
            wide("ProtoInputMultiPlex desktop capture fixture").as_ptr(),
            WS_POPUP,
            desktop.left,
            desktop.top,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            resources.module,
            std::ptr::null(),
        );
        ensure!(!hwnd.is_null(), "create desktop capture fixture window");
        resources.hwnd = hwnd;
        resources.timer = SetTimer(hwnd, TIMER_ID, REPORT_INTERVAL_MS, None);
        ensure!(resources.timer != 0, "create desktop fixture report timer");
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        ensure!(
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                desktop.left,
                desktop.top,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ) != 0,
            "size desktop fixture over the physical virtual desktop"
        );
        UpdateWindow(hwnd);

        let flags = window_flags(hwnd);
        let actual_window_bounds = window_bounds(hwnd)?;
        let actual_client_bounds = client_bounds(hwnd)?;
        let foreground_after_show = GetForegroundWindow() as usize;
        {
            let mut live = lock_live();
            live.snapshot.hwnd = hwnd as usize;
            live.snapshot.window_bounds = Some(actual_window_bounds);
            live.snapshot.client_bounds = Some(actual_client_bounds);
            live.snapshot.window_flags = Some(flags);
            live.snapshot.foreground_after_show = Some(foreground_after_show);
            live.snapshot.foreground_unchanged = Some(foreground_before == foreground_after_show);
        }
        write_report()?;
        let setup_error = if actual_window_bounds != desktop || actual_client_bounds != desktop {
            Some("fixture client and window must cover exact physical desktop bounds".to_owned())
        } else if !flags.popup
            || !flags.topmost
            || !flags.no_activate
            || !flags.transparent
            || !flags.tool_window
            || !flags.visible
        {
            Some("fixture window flags do not match the required transparent topmost popup".into())
        } else if foreground_before != foreground_after_show {
            Some("desktop fixture changed the foreground window".to_owned())
        } else {
            None
        };
        if let Some(error) = setup_error {
            lock_live().snapshot.error = Some(error);
            lock_live().snapshot.exit_reason = Some("fixture_setup_failed".into());
            DestroyWindow(hwnd);
        } else {
            let mut message: MSG = std::mem::zeroed();
            loop {
                let status = GetMessageW(&mut message, std::ptr::null_mut(), 0, 0);
                if status == 0 {
                    break;
                }
                if status == -1 {
                    let error = std::io::Error::last_os_error();
                    lock_live().snapshot.error =
                        Some(format!("read desktop fixture message: {error}"));
                    break;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        lock_live().snapshot.foreground_after_close = Some(GetForegroundWindow() as usize);
    }

    let (timed_out, closed, exit_reason, error) = {
        let live = lock_live();
        (
            live.snapshot.timed_out,
            live.snapshot.closed,
            live.snapshot.exit_reason.clone(),
            live.snapshot.error.clone(),
        )
    };
    let write_result = write_report();
    drop(resources);
    write_result?;
    if let Some(error) = error {
        anyhow::bail!("{error}");
    }
    ensure!(
        !timed_out,
        "desktop capture fixture timed out and closed itself"
    );
    ensure!(
        closed && exit_reason.as_deref() == Some("wm_close"),
        "fixture requires normal WM_CLOSE shutdown"
    );
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            if !dc.is_null() {
                let mut rect: RECT = unsafe { std::mem::zeroed() };
                if unsafe { GetClientRect(hwnd, &mut rect) } != 0 {
                    let brush = BRUSH_HANDLE.load(Ordering::Relaxed) as HBRUSH;
                    if !brush.is_null() {
                        unsafe { FillRect(dc, &rect, brush) };
                    }
                }
                unsafe { EndPaint(hwnd, &paint) };
            }
            if let Some(live) = LIVE.get() {
                let mut report = lock_live_with(live);
                report.snapshot.paint_count = report.snapshot.paint_count.saturating_add(1);
            }
            0
        }
        WM_TIMER if _wparam == TIMER_ID => {
            let timed_out = if let Some(live) = LIVE.get() {
                let mut report = lock_live_with(live);
                report.snapshot.frames = report.snapshot.frames.saturating_add(1);
                report.snapshot.window_bounds = window_bounds(hwnd).ok();
                report.snapshot.client_bounds = client_bounds(hwnd).ok();
                report.snapshot.window_flags = Some(window_flags(hwnd));
                let timed_out = report.started.elapsed() >= report.max_duration;
                if timed_out {
                    report.snapshot.timed_out = true;
                    report.snapshot.exit_reason = Some("timeout_failsafe".into());
                }
                timed_out
            } else {
                false
            };
            let _ = write_report();
            if timed_out {
                unsafe { DestroyWindow(hwnd) };
            } else {
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                    UpdateWindow(hwnd);
                }
            }
            0
        }
        WM_CLOSE => {
            if let Some(live) = LIVE.get() {
                let mut report = lock_live_with(live);
                if !report.snapshot.timed_out {
                    report.snapshot.exit_reason = Some("wm_close".into());
                }
            }
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            if let Some(live) = LIVE.get() {
                lock_live_with(live).snapshot.closed = true;
            }
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, _wparam, _lparam) },
    }
}

fn window_flags(hwnd: HWND) -> WindowFlags {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let extended = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        WindowFlags {
            popup: style & WS_POPUP != 0,
            topmost: extended & WS_EX_TOPMOST != 0,
            no_activate: extended & WS_EX_NOACTIVATE != 0,
            transparent: extended & WS_EX_TRANSPARENT != 0,
            tool_window: extended & WS_EX_TOOLWINDOW != 0,
            visible: IsWindowVisible(hwnd) != 0,
        }
    }
}

fn window_bounds(hwnd: HWND) -> Result<Bounds> {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { GetWindowRect(hwnd, &mut rect) } != 0,
        "read desktop fixture window bounds"
    );
    Ok(Bounds {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    })
}

fn client_bounds(hwnd: HWND) -> Result<Bounds> {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { GetClientRect(hwnd, &mut rect) } != 0,
        "read desktop fixture client bounds"
    );
    let mut origin = POINT {
        x: rect.left,
        y: rect.top,
    };
    let mut corner = POINT {
        x: rect.right,
        y: rect.bottom,
    };
    ensure!(
        unsafe { ClientToScreen(hwnd, &mut origin) } != 0,
        "read desktop fixture client origin"
    );
    ensure!(
        unsafe { ClientToScreen(hwnd, &mut corner) } != 0,
        "read desktop fixture client corner"
    );
    Ok(Bounds {
        left: origin.x,
        top: origin.y,
        right: corner.x,
        bottom: corner.y,
    })
}

fn lock_live() -> std::sync::MutexGuard<'static, LiveReport> {
    lock_live_with(
        LIVE.get()
            .expect("fixture report initialized before window messages"),
    )
}

fn lock_live_with(live: &'static Mutex<LiveReport>) -> std::sync::MutexGuard<'static, LiveReport> {
    live.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_report() -> Result<()> {
    let live = lock_live();
    let bytes =
        serde_json::to_vec_pretty(&live.snapshot).context("serialize desktop fixture report")?;
    std::fs::write(&live.path, bytes).context("write desktop fixture report")
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
