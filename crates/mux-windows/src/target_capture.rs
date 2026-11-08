//! Captures one already-authorized client window. Never reads the desktop DC.
use crate::targets;
use anyhow::{Result, ensure};
use mux_core::{desktop::Bounds, routing::Window};
use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::*,
    Storage::Xps::{PW_CLIENTONLY, PrintWindow},
    UI::{HiDpi::*, WindowsAndMessaging::PW_RENDERFULLCONTENT},
};

/// Called in the disposable routing worker, whose supervisor bounds blocking native calls.
/// PrintWindow requires the target to cooperate; success is not a general capture guarantee.
pub fn client(window: Window) -> Result<(Bounds, Vec<u8>)> {
    struct Dpi(isize);
    impl Drop for Dpi {
        fn drop(&mut self) {
            unsafe { SetThreadDpiAwarenessContext(self.0 as _) };
        }
    }
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    ensure!(!previous.is_null(), "set physical capture coordinates");
    let _dpi = Dpi(previous as isize);
    let bounds = targets::client_bounds(window.hwnd, window.pid)?;
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let pixels = i64::from(width) * i64::from(height);
    ensure!(
        (1..=16_000_000).contains(&pixels),
        "target capture exceeds image budget"
    );
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        ensure!(!dc.is_null(), "create target capture DC");
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            DeleteDC(dc);
            anyhow::bail!("create target capture bitmap");
        }
        let old = SelectObject(dc, bitmap);
        let byte_count = pixels as usize * 4;
        std::ptr::write_bytes(bits, 0, byte_count);
        let ok = PrintWindow(
            window.hwnd as HWND,
            dc,
            PW_CLIENTONLY | PW_RENDERFULLCONTENT,
        );
        GdiFlush();
        let mut rgba = Vec::new();
        if ok != 0 {
            rgba = std::slice::from_raw_parts(bits as *const u8, byte_count).to_vec();
            for pixel in rgba.as_chunks_mut::<4>().0.iter_mut() {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
        }
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        ensure!(ok != 0, "selected target does not support client capture");
        // Reject a changed/reused window rather than attaching ink to the wrong source.
        ensure!(
            targets::client_bounds(window.hwnd, window.pid)? == bounds,
            "target moved during capture; retry"
        );
        Ok((bounds, rgba))
    }
}
