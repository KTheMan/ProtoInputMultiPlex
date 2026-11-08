//! Private own-HWND binding. A message is only a wake hint; it carries no payload.
use std::{marker::PhantomData, rc::Rc, sync::atomic::{AtomicUsize, Ordering}};
use windows_sys::Win32::{System::Threading::{GetCurrentProcessId, GetCurrentThreadId}, UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*}};
static NEXT_COOKIE: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
pub(super) struct Target {
    hwnd: usize,
    thread: u32,
    process: u32,
    cookie: usize,
    property: Vec<u16>,
    pub message: u32,
    #[cfg(test)] mock: Option<std::sync::Arc<AtomicUsize>>,
}
impl Target {
    /// Called while the queue fence is locked. Values are numeric tokens, never Rust pointers.
    pub fn post(&self) -> bool {
        #[cfg(test)] if let Some(counter) = &self.mock { counter.fetch_add(1, Ordering::Relaxed); return true; }
        let hwnd = self.hwnd as windows_sys::Win32::Foundation::HWND;
        let mut process = 0;
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
        thread == self.thread && process == self.process
            && unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd
            && unsafe { GetPropW(hwnd, self.property.as_ptr()) } as usize == self.cookie
            && unsafe { GetDpiForWindow(hwnd) } != 0
            && unsafe { PostMessageW(hwnd, self.message, 0, 0) } != 0
    }
}
/// UI-thread owned; the worker receives only a clone of immutable numeric metadata.
pub(super) struct Binding { pub target: Target, _local: PhantomData<Rc<()>> }
impl Binding {
    pub fn new(hwnd: usize) -> Result<Self, super::ClientError> {
        let native = hwnd as windows_sys::Win32::Foundation::HWND;
        let thread = unsafe { GetCurrentThreadId() };
        let process = unsafe { GetCurrentProcessId() };
        let mut owner = 0;
        if hwnd == 0 || unsafe { GetWindowThreadProcessId(native, &mut owner) } != thread
            || owner != process || unsafe { GetAncestor(native, GA_ROOT) } != native
            || unsafe { GetDpiForWindow(native) } == 0 {
            return Err(super::ClientError::Window);
        }
        let cookie = NEXT_COOKIE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| super::ClientError::Limit)?;
        let property: Vec<u16> = format!("ProtoInputMultiPlex.NativeEventClient.{process}.{cookie}\0").encode_utf16().collect();
        let name: Vec<u16> = "ProtoInputMultiPlex.NativeEventClient.Wake.v1\0".encode_utf16().collect();
        let message = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        if message == 0 || unsafe { SetPropW(native, property.as_ptr(), cookie as _) } == 0 {
            return Err(super::ClientError::Window);
        }
        Ok(Self { target: Target { hwnd, thread, process, cookie, property, message, #[cfg(test)] mock: None }, _local: PhantomData })
    }
}
impl Drop for Binding {
    fn drop(&mut self) {
        let t = &self.target;
        #[cfg(test)] if t.mock.is_some() { return; }
        let hwnd = t.hwnd as windows_sys::Win32::Foundation::HWND;
        if unsafe { GetCurrentThreadId() } == t.thread
            && unsafe { GetPropW(hwnd, t.property.as_ptr()) } as usize == t.cookie {
            unsafe { RemovePropW(hwnd, t.property.as_ptr()); }
        }
    }
}

#[cfg(test)]
impl Binding {
    pub fn mock(counter: std::sync::Arc<AtomicUsize>) -> Self {
        Self { target: Target { hwnd: 0, thread: 0, process: 0, cookie: 1, property: vec![0], message: 0xc001, mock: Some(counter) }, _local: PhantomData }
    }
}