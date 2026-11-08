//! Target-scoped horizontal wheel delivery for protocols that lack a native packet type.
use anyhow::{Result, ensure};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_MOUSEHWHEEL};

/// Posts a horizontal wheel event to one validated target window.
///
/// `modifiers` uses Win32 MK_* bits and is built from this virtual user's held state;
/// this helper never samples physical keyboard or mouse state. A successful result means
/// only that Windows accepted the queued message, not that the application processed it.
pub fn post_horizontal_wheel(
    hwnd: usize,
    expected_pid: u32,
    screen: (i32, i32),
    delta: i16,
    modifiers: u16,
) -> Result<()> {
    ensure!(
        hwnd != 0 && hwnd != 0xffff && expected_pid != 0,
        "horizontal wheel requires one explicit target window and PID"
    );
    const MK_MASK: u16 = 0x007f;
    ensure!(
        modifiers & !MK_MASK == 0,
        "invalid horizontal wheel modifier flags"
    );
    ensure!(
        (i16::MIN as i32..=i16::MAX as i32).contains(&screen.0)
            && (i16::MIN as i32..=i16::MAX as i32).contains(&screen.1),
        "horizontal wheel screen coordinates exceed the Win32 message range"
    );
    crate::targets::client_bounds(hwnd, expected_pid)?;

    let x = screen.0 as i16 as u16 as u32;
    let y = screen.1 as i16 as u16 as u32;
    let lparam = (x | (y << 16)) as isize;
    let wparam = (u32::from(modifiers) | (u32::from(delta as u16) << 16)) as usize;
    let posted = unsafe { PostMessageW(hwnd as _, WM_MOUSEHWHEEL, wparam, lparam) };
    ensure!(
        posted != 0,
        "post horizontal wheel to target window: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
