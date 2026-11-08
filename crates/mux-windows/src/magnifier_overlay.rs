//! Transparent no-activate magnifier surface with capture-exclusion telemetry.
use mux_core::desktop::Bounds;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagnifierOverlayStatus {
    pub hwnd: usize,
    pub bounds: Bounds,
    pub visible: bool,
    pub click_through: bool,
    pub no_activate: bool,
    pub painted_pixels: usize,
    pub pixel_hash: u64,
    pub excluded_from_capture: bool,
}

#[cfg(windows)]
mod native {
    use super::{Bounds, MagnifierOverlayStatus};
    use anyhow::{Context, Result, ensure};
    use mux_core::magnifier::MAX_PIXELS;
    use std::{marker::PhantomData, rc::Rc};
    use windows_sys::Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentProcessId},
        UI::WindowsAndMessaging::*,
    };

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCHITTEST => HTTRANSPARENT as LRESULT,
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn geometry(bounds: Bounds) -> Result<(i32, i32, usize)> {
        ensure!(
            bounds.valid_region(),
            "magnifier overlay bounds are invalid"
        );
        let width = i64::from(bounds.right) - i64::from(bounds.left);
        let height = i64::from(bounds.bottom) - i64::from(bounds.top);
        let pixels = u64::try_from(width)
            .context("magnifier overlay width is invalid")?
            .checked_mul(u64::try_from(height).context("magnifier overlay height is invalid")?)
            .context("magnifier overlay pixel count overflow")?;
        ensure!(
            pixels <= MAX_PIXELS,
            "magnifier overlay exceeds the 16 million pixel limit"
        );
        let byte_count = usize::try_from(
            pixels
                .checked_mul(4)
                .context("magnifier overlay size overflow")?,
        )
        .context("magnifier overlay is too large")?;
        Ok((i32::try_from(width)?, i32::try_from(height)?, byte_count))
    }

    fn window_flags(hwnd: HWND) -> (bool, bool, bool) {
        unsafe {
            let styles = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            (
                IsWindowVisible(hwnd) != 0,
                styles & WS_EX_TRANSPARENT != 0 && styles & WS_EX_LAYERED != 0,
                styles & WS_EX_NOACTIVATE != 0,
            )
        }
    }

    fn capture_excluded(hwnd: HWND) -> bool {
        unsafe {
            let mut owner_pid = 0_u32;
            if GetWindowThreadProcessId(hwnd, &mut owner_pid) == 0
                || owner_pid != GetCurrentProcessId()
            {
                return false;
            }
            let mut affinity = 0_u32;
            GetWindowDisplayAffinity(hwnd, &mut affinity) != 0 && affinity == WDA_EXCLUDEFROMCAPTURE
        }
    }

    pub struct MagnifierOverlay {
        hwnd: HWND,
        _thread: PhantomData<Rc<()>>,
        status: MagnifierOverlayStatus,
    }

    impl MagnifierOverlay {
        pub fn new(bounds: Bounds) -> Result<Self> {
            geometry(bounds)?;
            unsafe {
                let class = wide("ProtoInputMultiPlex.Magnifier");
                let instance = GetModuleHandleW(std::ptr::null());
                let window_class = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&window_class);
                let title = wide("ProtoInputMultiPlex magnifier");
                let hwnd = CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP,
                    bounds.left,
                    bounds.top,
                    1,
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                ensure!(
                    !hwnd.is_null(),
                    "create magnifier overlay: {}",
                    std::io::Error::last_os_error()
                );
                // Capture exclusion is best-effort on unsupported Windows versions. The
                // explicit helper lets Desktop-source callers require and report the flag.
                let excluded_from_capture = crate::desktop_capture::exclude_window(hwnd as usize)?;
                let (visible, click_through, no_activate) = window_flags(hwnd);
                Ok(Self {
                    hwnd,
                    _thread: PhantomData,
                    status: MagnifierOverlayStatus {
                        hwnd: hwnd as usize,
                        bounds,
                        visible,
                        click_through,
                        no_activate,
                        painted_pixels: 0,
                        pixel_hash: 0,
                        excluded_from_capture,
                    },
                })
            }
        }

        /// Draw straight-alpha RGBA pixels, converting to premultiplied BGRA for GDI.
        pub fn draw(&mut self, bounds: Bounds, rgba: &[u8]) -> Result<MagnifierOverlayStatus> {
            let (width, height, byte_count) = geometry(bounds)?;
            ensure!(
                rgba.len() == byte_count,
                "magnifier pixel buffer length does not match its bounds"
            );
            unsafe {
                let dc = CreateCompatibleDC(std::ptr::null_mut());
                ensure!(!dc.is_null(), "create magnifier drawing context");
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
                    anyhow::bail!("create magnifier bitmap");
                }
                let previous_bitmap = SelectObject(dc, bitmap);
                if previous_bitmap.is_null() || previous_bitmap as isize == -1 {
                    DeleteObject(bitmap);
                    DeleteDC(dc);
                    anyhow::bail!("select magnifier bitmap");
                }
                let dib = std::slice::from_raw_parts_mut(bits.cast::<u8>(), byte_count);
                let mut painted_pixels = 0_usize;
                let mut hash = 0xcbf29ce484222325_u64;
                for (source, destination) in rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dib.as_chunks_mut::<4>().0.iter_mut())
                {
                    let [red, green, blue, alpha] = [source[0], source[1], source[2], source[3]];
                    let alpha_u16 = u16::from(alpha);
                    destination[0] = ((u16::from(blue) * alpha_u16 + 127) / 255) as u8;
                    destination[1] = ((u16::from(green) * alpha_u16 + 127) / 255) as u8;
                    destination[2] = ((u16::from(red) * alpha_u16 + 127) / 255) as u8;
                    destination[3] = alpha;
                    if alpha != 0 {
                        painted_pixels += 1;
                    }
                    for byte in source {
                        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
                    }
                }
                GdiFlush();
                let destination = POINT {
                    x: bounds.left,
                    y: bounds.top,
                };
                let source = POINT { x: 0, y: 0 };
                let size = SIZE {
                    cx: width,
                    cy: height,
                };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let updated = UpdateLayeredWindow(
                    self.hwnd,
                    std::ptr::null_mut(),
                    &destination,
                    &size,
                    dc,
                    &source,
                    0,
                    &blend,
                    ULW_ALPHA,
                );
                SelectObject(dc, previous_bitmap);
                DeleteObject(bitmap);
                DeleteDC(dc);
                ensure!(
                    updated != 0,
                    "update magnifier overlay: {}",
                    std::io::Error::last_os_error()
                );
                ensure!(
                    SetWindowPos(
                        self.hwnd,
                        HWND_TOPMOST,
                        bounds.left,
                        bounds.top,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    ) != 0,
                    "position magnifier overlay: {}",
                    std::io::Error::last_os_error()
                );
                let (visible, click_through, no_activate) = window_flags(self.hwnd);
                self.status = MagnifierOverlayStatus {
                    hwnd: self.hwnd as usize,
                    bounds,
                    visible,
                    click_through,
                    no_activate,
                    painted_pixels,
                    pixel_hash: hash,
                    excluded_from_capture: capture_excluded(self.hwnd),
                };
                Ok(self.status.clone())
            }
        }

        pub fn hide(&mut self) -> Result<MagnifierOverlayStatus> {
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
                let (visible, click_through, no_activate) = window_flags(self.hwnd);
                self.status.visible = visible;
                self.status.click_through = click_through;
                self.status.no_activate = no_activate;
                self.status.painted_pixels = 0;
                self.status.pixel_hash = 0;
                self.status.excluded_from_capture = capture_excluded(self.hwnd);
                Ok(self.status.clone())
            }
        }

        pub fn status(&self) -> &MagnifierOverlayStatus {
            &self.status
        }
    }

    impl Drop for MagnifierOverlay {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.hwnd) };
        }
    }

    pub use self::MagnifierOverlay as Overlay;
}

#[cfg(windows)]
pub use native::Overlay as MagnifierOverlay;
