//! Bounded capture of a caller-selected physical desktop rectangle.
use anyhow::{Context, Result, ensure};
use mux_core::{desktop::Bounds, magnifier::MAX_PIXELS};
use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::*,
    System::Threading::GetCurrentProcessId,
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
    UI::WindowsAndMessaging::{
        GetWindowDisplayAffinity, GetWindowThreadProcessId, SetWindowDisplayAffinity,
        WDA_EXCLUDEFROMCAPTURE,
    },
};

struct DpiContext(isize);

impl Drop for DpiContext {
    fn drop(&mut self) {
        unsafe { SetThreadDpiAwarenessContext(self.0 as _) };
    }
}

struct ScreenDc(HDC);

impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe { ReleaseDC(std::ptr::null_mut(), self.0) };
    }
}

struct BitmapCanvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Drop for BitmapCanvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

/// Enables and verifies capture exclusion on an HWND owned by this process only.
pub fn exclude_window(hwnd: usize) -> Result<bool> {
    ensure!(hwnd != 0, "cannot exclude a null HWND from desktop capture");
    let hwnd = hwnd as HWND;
    unsafe {
        let mut owner_pid = 0_u32;
        ensure!(
            GetWindowThreadProcessId(hwnd, &mut owner_pid) != 0,
            "desktop capture exclusion HWND is not valid"
        );
        ensure!(
            owner_pid == GetCurrentProcessId(),
            "desktop capture exclusion is limited to this process's own windows"
        );
        if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) == 0 {
            return Ok(false);
        }
        let mut affinity = 0_u32;
        Ok(
            GetWindowDisplayAffinity(hwnd, &mut affinity) != 0
                && affinity == WDA_EXCLUDEFROMCAPTURE,
        )
    }
}

/// Copies only `bounds` from the physical virtual desktop into straight-alpha RGBA pixels.
/// The caller must authorize the requested region before calling this native capture API.
pub fn region(bounds: Bounds) -> Result<(Bounds, Vec<u8>)> {
    ensure!(bounds.valid_region(), "desktop capture region is invalid");
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    ensure!(
        !previous.is_null(),
        "set physical desktop capture coordinates"
    );
    let _dpi = DpiContext(previous as isize);

    let desktop = crate::targets::desktop_bounds();
    ensure!(
        desktop.valid_region() && desktop.intersection(bounds) == Some(bounds),
        "desktop capture region lies outside the physical virtual desktop"
    );
    let width = i32::try_from(i64::from(bounds.right) - i64::from(bounds.left))?;
    let height = i32::try_from(i64::from(bounds.bottom) - i64::from(bounds.top))?;
    let pixels = u64::try_from(width)?
        .checked_mul(u64::try_from(height)?)
        .context("desktop capture pixel count overflow")?;
    ensure!(
        (1..=MAX_PIXELS).contains(&pixels),
        "desktop capture exceeds the 16 million pixel limit"
    );
    let byte_count = usize::try_from(
        pixels
            .checked_mul(4)
            .context("desktop capture byte count overflow")?,
    )
    .context("desktop capture is too large")?;

    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        ensure!(!screen.is_null(), "get physical desktop device context");
        let screen = ScreenDc(screen);
        let dc = CreateCompatibleDC(screen.0);
        ensure!(!dc.is_null(), "create desktop capture drawing context");
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
            screen.0,
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
            anyhow::bail!("create physical desktop capture bitmap");
        }
        let previous = SelectObject(dc, bitmap);
        if previous.is_null() || previous as isize == -1 {
            DeleteObject(bitmap);
            DeleteDC(dc);
            anyhow::bail!("select physical desktop capture bitmap");
        }
        let canvas = BitmapCanvas {
            dc,
            bitmap,
            previous,
        };
        ensure!(
            BitBlt(
                canvas.dc,
                0,
                0,
                width,
                height,
                screen.0,
                bounds.left,
                bounds.top,
                SRCCOPY,
            ) != 0,
            "copy selected physical desktop region: {}",
            std::io::Error::last_os_error()
        );
        GdiFlush();
        let bgra = std::slice::from_raw_parts(bits.cast::<u8>(), byte_count);
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(byte_count)
            .context("reserve desktop capture pixels")?;
        rgba.resize(byte_count, 0);
        for (source, destination) in bgra
            .as_chunks::<4>()
            .0
            .iter()
            .zip(rgba.as_chunks_mut::<4>().0.iter_mut())
        {
            destination.copy_from_slice(&[source[2], source[1], source[0], 255]);
        }
        drop(canvas);
        drop(screen);
        Ok((bounds, rgba))
    }
}
