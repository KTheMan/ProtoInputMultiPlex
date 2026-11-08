//! Cooperative SDK window operations. No target injection or OS input synthesis.
//!
//! Use on the window's creating thread with per-monitor DPI awareness. Register
//! only the embedding application's own top-level windows, and call `retire`
//! from its destruction path before returning from WM_NCDESTROY. Native handles
//! stay in this adapter; wire snapshots contain session-local WindowIds only.
use mux_core::application_event::{
    Engine, Error, OwnWindowAdapter, RecipientId, WindowAction, WindowControl, WindowId,
    WindowInfo, WindowMetadata, WindowObservation,
};
use mux_core::desktop::Bounds;
use std::{
    cell::Cell,
    collections::BTreeMap,
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};
use windows_sys::Win32::{
    Foundation::{GetLastError, HWND, POINT, RECT, SetLastError},
    Graphics::Gdi::{
        ClientToScreen, GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST, MonitorFromWindow,
    },
    System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
    UI::{
        HiDpi::{
            DPI_AWARENESS_PER_MONITOR_AWARE, GetAwarenessFromDpiAwarenessContext,
            GetDpiForWindow, GetThreadDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::{EnableWindow, GetFocus, IsWindowEnabled, SetFocus},
        WindowsAndMessaging::*,
    },
};

static NEXT_ADAPTER: AtomicU64 = AtomicU64::new(1);

struct Binding {
    hwnd: HWND,
    owner: RecipientId,
    cookie: Box<u8>,
    wake_pending: Cell<bool>,
}

/// Until Engine registration succeeds, keep the same private cookie on the HWND.
/// Drop removes only our exact property, including every early-error path.
struct PendingRegistration<'a> {
    hwnd: HWND,
    property: &'a [u16],
    cookie: Option<Box<u8>>,
}

impl<'a> PendingRegistration<'a> {
    fn install(hwnd: HWND, property: &'a [u16]) -> Result<Self, Error> {
        if !unsafe { GetPropW(hwnd, property.as_ptr()) }.is_null() {
            return Err(Error::Invalid("window property already set"));
        }
        let cookie = Box::new(1_u8);
        let value = (&*cookie as *const u8).cast_mut().cast();
        if unsafe { SetPropW(hwnd, property.as_ptr(), value) } == 0 {
            return Err(api_error("SetPropW"));
        }
        Ok(Self {
            hwnd,
            property,
            cookie: Some(cookie),
        })
    }

    fn matches(&self) -> bool {
        let cookie = self.cookie.as_ref().expect("pending cookie");
        unsafe { GetPropW(self.hwnd, self.property.as_ptr()) }
            == (&**cookie as *const u8).cast_mut().cast()
    }

    fn into_binding(mut self, owner: RecipientId) -> Binding {
        Binding {
            hwnd: self.hwnd,
            owner,
            cookie: self.cookie.take().expect("pending cookie"),
            wake_pending: Cell::new(false),
        }
    }
}

impl Drop for PendingRegistration<'_> {
    fn drop(&mut self) {
        if self.cookie.is_some() && self.matches() {
            unsafe {
                RemovePropW(self.hwnd, self.property.as_ptr());
            }
        }
    }
}

/// !Send/!Sync: window controls and lifetime notifications belong to their UI thread.
pub struct WindowAdapter {
    thread: u32,
    property: Vec<u16>,
    wake_message: u32,
    windows: BTreeMap<WindowId, Binding>,
    _thread_only: PhantomData<Rc<()>>,
}

fn api_error(operation: &str) -> Error {
    Error::Adapter(format!("{operation}: {}", std::io::Error::last_os_error()))
}

impl WindowAdapter {
    pub fn new() -> Result<Self, Error> {
        let property: Vec<u16> = format!(
            "ProtoInputMultiPlex.ApplicationSDK.{}.{}",
            unsafe { GetCurrentProcessId() },
            NEXT_ADAPTER.fetch_add(1, Ordering::Relaxed)
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let name: Vec<u16> = "ProtoInputMultiPlex.ApplicationEvents.v1\0"
            .encode_utf16()
            .collect();
        let wake_message = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        if wake_message == 0 {
            return Err(api_error("RegisterWindowMessageW"));
        }
        let result = Self {
            thread: unsafe { GetCurrentThreadId() },
            property,
            wake_message,
            windows: BTreeMap::new(),
            _thread_only: PhantomData,
        };
        result.check_thread()?;
        Ok(result)
    }

    fn check_thread(&self) -> Result<(), Error> {
        if unsafe { GetCurrentThreadId() } != self.thread {
            return Err(Error::Denied);
        }
        if unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) }
            != DPI_AWARENESS_PER_MONITOR_AWARE
        {
            return Err(Error::Unsupported(
                "physical-pixel window controls require per-monitor DPI awareness",
            ));
        }
        Ok(())
    }

    fn owned(&self, hwnd: HWND) -> Result<(), Error> {
        self.check_thread()?;
        let mut process = 0;
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
        if hwnd.is_null()
            || thread != self.thread
            || process != unsafe { GetCurrentProcessId() }
            || unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd
        {
            return Err(Error::Denied);
        }
        Ok(())
    }

    fn binding(&self, id: WindowId) -> Result<&Binding, Error> {
        let binding = self.windows.get(&id).ok_or(Error::MissingWindow)?;
        self.owned(binding.hwnd)?;
        if unsafe { GetPropW(binding.hwnd, self.property.as_ptr()) }
            != (&*binding.cookie as *const u8).cast_mut().cast()
        {
            return Err(Error::MissingWindow);
        }
        Ok(binding)
    }

    /// Registration is explicit, local, and checks both process and creating thread.
    pub fn register(
        &mut self,
        engine: &mut Engine,
        owner: RecipientId,
        native_window: usize,
    ) -> Result<WindowId, Error> {
        let hwnd = native_window as HWND;
        self.owned(hwnd)?;
        if self.windows.values().any(|entry| entry.hwnd == hwnd) {
            return Err(Error::Invalid("window already registered"));
        }
        // Install before any observation that can reenter the owned WndProc.
        let pending = PendingRegistration::install(hwnd, &self.property)?;
        let observed = observe(hwnd)?;
        self.owned(hwnd)?;
        if !pending.matches() {
            return Err(Error::MissingWindow);
        }
        let id = engine.register_owned_window(owner, observed)?;
        self.windows.insert(id, pending.into_binding(owner));
        Ok(id)
    }

    pub fn refresh(&self, engine: &mut Engine, id: WindowId) -> Result<WindowInfo, Error> {
        let binding = self.binding(id)?;
        let observed = info(id, observe(binding.hwnd)?);
        self.binding(id)?;
        engine.update_owned_window(binding.owner, observed.clone())?;
        Ok(observed)
    }

    /// Invoke in the application's WM_NCDESTROY path as well as explicit unbinding.
    pub fn retire(&mut self, engine: &mut Engine, id: WindowId) -> Result<(), Error> {
        self.check_thread()?;
        let binding = self.windows.remove(&id).ok_or(Error::MissingWindow)?;
        self.remove_property(&binding);
        engine.destroy_window(id)
    }

    fn remove_property(&self, binding: &Binding) {
        if unsafe { GetPropW(binding.hwnd, self.property.as_ptr()) }
            == (&*binding.cookie as *const u8).cast_mut().cast()
        {
            unsafe {
                RemovePropW(binding.hwnd, self.property.as_ptr());
            }
        }
    }

    /// A wake hint only: no event data, keys, pointer coordinates or credentials
    /// cross the window message boundary. Drain the bounded engine subscription
    /// from the recipient's UI thread. A successful post is not an input receipt.
    pub fn notify(&self, id: WindowId) -> Result<(), Error> {
        let binding = self.binding(id)?;
        if binding.wake_pending.get() {
            return Ok(());
        }
        if unsafe { PostMessageW(binding.hwnd, self.wake_message, 0, 0) } == 0 {
            return Err(api_error("PostMessageW"));
        }
        binding.wake_pending.set(true);
        Ok(())
    }

    /// Clear the coalesced wake before draining the subscription in the UI loop.
    pub fn acknowledge_wake(&self, id: WindowId) -> Result<(), Error> {
        self.binding(id)?.wake_pending.set(false);
        Ok(())
    }

    pub fn wake_message(&self) -> u32 {
        self.wake_message
    }

    fn set_style(&self, id: WindowId, style: u32) -> Result<(), Error> {
        let hwnd = self.binding(id)?.hwnd;
        unsafe {
            SetLastError(0);
        }
        let previous = unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as _) };
        if previous == 0 && unsafe { GetLastError() } != 0 {
            return Err(api_error("SetWindowLongPtrW"));
        }
        self.binding(id)?;
        if unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        } == 0
        {
            return Err(api_error("SetWindowPos styles"));
        }
        self.binding(id)?;
        Ok(())
    }
}

impl Drop for WindowAdapter {
    fn drop(&mut self) {
        for binding in self.windows.values() {
            self.remove_property(binding);
        }
    }
}

fn observe(hwnd: HWND) -> Result<WindowObservation, Error> {
    // Recheck ownership before reading details, even after the adapter's binding check.
    let mut process = 0;
    let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
    if hwnd.is_null()
        || thread != unsafe { GetCurrentThreadId() }
        || process != unsafe { GetCurrentProcessId() }
        || unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd
    {
        return Err(Error::Denied);
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return Err(api_error("GetWindowRect"));
    }
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return Err(api_error("GetClientRect"));
    }
    let mut top_left = POINT {
        x: client.left,
        y: client.top,
    };
    let mut bottom_right = POINT {
        x: client.right,
        y: client.bottom,
    };
    if unsafe { ClientToScreen(hwnd, &mut top_left) } == 0
        || unsafe { ClientToScreen(hwnd, &mut bottom_right) } == 0
    {
        return Err(api_error("ClientToScreen"));
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        return Err(api_error("GetDpiForWindow"));
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return Err(api_error("MonitorFromWindow"));
    }
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut monitor_info) } == 0 {
        return Err(api_error("GetMonitorInfoW"));
    }
    let mut title_buffer = [0_u16; 257];
    unsafe {
        SetLastError(0);
    }
    let title_len = unsafe {
        GetWindowTextW(hwnd, title_buffer.as_mut_ptr(), title_buffer.len() as i32)
    };
    if title_len == 0 && unsafe { GetLastError() } != 0 {
        return Err(api_error("GetWindowTextW"));
    }
    let title = bounded_title(&title_buffer, title_len)?;
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let mut opacity = 255;
    if style & WS_EX_LAYERED != 0 {
        let mut flags = 0;
        if unsafe {
            GetLayeredWindowAttributes(hwnd, std::ptr::null_mut(), &mut opacity, &mut flags)
        } == 0
        {
            return Err(Error::Unsupported(
                "per-pixel layered windows require a separate observation adapter",
            ));
        }
        if flags & LWA_ALPHA == 0 {
            opacity = 255;
        }
    }
    Ok(WindowObservation {
        bounds: bounds(rect),
        metadata: WindowMetadata {
            title: Some(title),
            process_id: Some(process),
            ui_thread_id: Some(thread),
            client_bounds: Some(Bounds {
                left: top_left.x,
                top: top_left.y,
                right: bottom_right.x,
                bottom: bottom_right.y,
            }),
            dpi: Some(dpi),
            monitor_bounds: Some(bounds(monitor_info.rcMonitor)),
            monitor_work_area: Some(bounds(monitor_info.rcWork)),
        },
        visible: unsafe { IsWindowVisible(hwnd) } != 0,
        enabled: unsafe { IsWindowEnabled(hwnd) } != 0,
        topmost: style & WS_EX_TOPMOST != 0,
        opacity,
        minimized: unsafe { IsIconic(hwnd) } != 0,
        maximized: unsafe { IsZoomed(hwnd) } != 0,
        click_through: style & (WS_EX_LAYERED | WS_EX_TRANSPARENT)
            == (WS_EX_LAYERED | WS_EX_TRANSPARENT),
    })
}

fn bounded_title(buffer: &[u16], length: i32) -> Result<String, Error> {
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length < buffer.len())
        .ok_or_else(|| Error::Adapter("GetWindowTextW returned an invalid length".into()))?;
    let mut title = String::from_utf16_lossy(&buffer[..length]);
    while title.len() > 512 {
        title.pop();
    }
    Ok(title)
}

#[cfg(test)]
mod metadata_tests {
    use super::bounded_title;

    #[test]
    fn title_decoder_bounds_native_length_and_utf8() {
        let buffer = [0_u16; 257];
        assert!(bounded_title(&buffer, -1).is_err());
        assert!(bounded_title(&buffer, 257).is_err());
        assert_eq!(bounded_title(&buffer, 0).unwrap(), "");
        let long = [0x4e2d_u16; 257];
        let title = bounded_title(&long, 256).unwrap();
        assert!(title.len() <= 512);
        assert_eq!(title.chars().count(), 170);
    }
}

fn bounds(rect: RECT) -> Bounds {
    Bounds {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn info(id: WindowId, value: WindowObservation) -> WindowInfo {
    WindowInfo {
        id,
        bounds: value.bounds,
        metadata: value.metadata,
        visible: value.visible,
        enabled: value.enabled,
        topmost: value.topmost,
        opacity: value.opacity,
        minimized: value.minimized,
        maximized: value.maximized,
        click_through: value.click_through,
    }
}

impl OwnWindowAdapter for WindowAdapter {
    fn supported(&self, id: WindowId, _action: WindowAction) -> bool {
        self.binding(id).is_ok()
    }

    fn apply(
        &mut self,
        owner: RecipientId,
        current: &WindowInfo,
        control: &WindowControl,
    ) -> Result<WindowInfo, Error> {
        let binding = self.binding(current.id)?;
        if binding.owner != owner {
            return Err(Error::Denied);
        }
        let hwnd = binding.hwnd;
        match control {
            WindowControl::Show
            | WindowControl::Hide
            | WindowControl::Minimize
            | WindowControl::Maximize
            | WindowControl::Restore => {
                let command = match control {
                    WindowControl::Show => SW_SHOWNOACTIVATE,
                    WindowControl::Hide => SW_HIDE,
                    WindowControl::Minimize => SW_SHOWMINNOACTIVE,
                    WindowControl::Maximize => SW_MAXIMIZE,
                    _ => SW_RESTORE,
                };
                unsafe {
                    ShowWindow(hwnd, command);
                }
            }
            WindowControl::Foreground => {
                if unsafe { SetForegroundWindow(hwnd) } == 0
                    || unsafe { GetForegroundWindow() } != hwnd
                {
                    return Err(Error::Adapter("foreground request was not granted".into()));
                }
            }
            WindowControl::Focus => {
                unsafe {
                    SetFocus(hwnd);
                }
                if unsafe { GetFocus() } != hwnd {
                    return Err(Error::Adapter("focus request was not granted".into()));
                }
            }
            WindowControl::Raise | WindowControl::Topmost(_) => {
                let insert = match control {
                    WindowControl::Topmost(true) => HWND_TOPMOST,
                    WindowControl::Topmost(false) => HWND_NOTOPMOST,
                    _ => HWND_TOP,
                };
                if unsafe {
                    SetWindowPos(
                        hwnd,
                        insert,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    )
                } == 0
                {
                    return Err(api_error("SetWindowPos order"));
                }
            }
            WindowControl::Enable | WindowControl::Disable => unsafe {
                EnableWindow(hwnd, i32::from(matches!(control, WindowControl::Enable)));
            },
            WindowControl::Close => {
                if unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } == 0 {
                    return Err(api_error("PostMessageW close"));
                }
            }
            WindowControl::MoveResize(bounds) => {
                let width = bounds
                    .right
                    .checked_sub(bounds.left)
                    .filter(|x| *x > 0)
                    .ok_or(Error::Invalid("window width"))?;
                let height = bounds
                    .bottom
                    .checked_sub(bounds.top)
                    .filter(|x| *x > 0)
                    .ok_or(Error::Invalid("window height"))?;
                if unsafe {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        bounds.left,
                        bounds.top,
                        width,
                        height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                } == 0
                {
                    return Err(api_error("SetWindowPos bounds"));
                }
            }
            WindowControl::Opacity(alpha) => {
                let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
                let mut color_key = 0;
                let mut previous_alpha = 255;
                let mut previous_flags = 0;
                if style & WS_EX_LAYERED != 0
                    && unsafe {
                        GetLayeredWindowAttributes(
                            hwnd,
                            &mut color_key,
                            &mut previous_alpha,
                            &mut previous_flags,
                        )
                    } == 0
                {
                    return Err(Error::Unsupported(
                        "per-pixel layered window opacity is unavailable",
                    ));
                }
                if style & WS_EX_LAYERED == 0 {
                    self.set_style(current.id, style | WS_EX_LAYERED)?;
                }
                // Preserve a host-owned color key when changing uniform opacity.
                if unsafe {
                    SetLayeredWindowAttributes(
                        hwnd,
                        color_key,
                        *alpha,
                        LWA_ALPHA | (previous_flags & LWA_COLORKEY),
                    )
                } == 0
                {
                    return Err(api_error("SetLayeredWindowAttributes"));
                }
            }
            WindowControl::ClickThrough(enabled) => {
                let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
                // WS_EX_TRANSPARENT alone only affects painting. Layered windows
                // give the documented mouse-event pass-through behavior.
                self.set_style(
                    current.id,
                    if *enabled {
                        style | WS_EX_TRANSPARENT | WS_EX_LAYERED
                    } else {
                        style & !WS_EX_TRANSPARENT
                    },
                )?;
                if *enabled
                    && style & WS_EX_LAYERED == 0
                    && unsafe { SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA) } == 0
                {
                    return Err(api_error("SetLayeredWindowAttributes click-through"));
                }
            }
        }
        // A callback can destroy a window during a synchronous window operation.
        self.binding(current.id)?;
        let observed = observe(hwnd)?;
        self.binding(current.id)?;
        Ok(info(current.id, observed))
    }
}
