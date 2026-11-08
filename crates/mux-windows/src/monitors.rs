//! Read-only inventory of connected desktop displays.
use anyhow::{Context, Result, bail, ensure};
use mux_core::desktop::{Bounds, BoundsMode, CursorSettings};
use serde::Serialize;
use std::mem::size_of;
use windows_sys::{
    Win32::{
        Foundation::{LPARAM, RECT},
        Graphics::Gdi::{
            DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors,
            GetMonitorInfoW, HDC, HMONITOR, MONITORENUMPROC, MONITORINFO, MONITORINFOEXW,
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::EDD_GET_DEVICE_INTERFACE_NAME,
        },
    },
    core::BOOL,
};

/// Stable monitor identity comes from the OS monitor-interface path. A false
/// `stable_id` means `id` is only a GDI adapter name and is unsuitable for
/// persisting a monitor-bound profile.
#[derive(Clone, Debug, Serialize)]
pub struct Monitor {
    pub id: String,
    pub stable_id: bool,
    pub name: String,
    pub bounds: Bounds,
    pub work_area: Bounds,
    pub primary: bool,
    /// Per-monitor DPI is not reported by this inventory yet.
    pub dpi: Option<u32>,
}

#[derive(Default)]
struct Enumeration {
    monitors: Vec<Monitor>,
    error: Option<String>,
}

/// Enumerate attached desktop monitors without retaining transient HMONITOR handles.
pub fn inventory() -> Result<Vec<Monitor>> {
    let _dpi = DpiAwareness::per_monitor_v2()?;
    let mut enumeration = Enumeration::default();
    let result = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect_monitor),
            &mut enumeration as *mut Enumeration as LPARAM,
        )
    };
    if let Some(error) = enumeration.error {
        bail!("enumerate display monitors: {error}");
    }
    ensure!(result != 0, "EnumDisplayMonitors failed");
    for index in 0..enumeration.monitors.len() {
        let duplicate = enumeration
            .monitors
            .iter()
            .enumerate()
            .any(|(other, monitor)| {
                other != index
                    && monitor
                        .id
                        .eq_ignore_ascii_case(&enumeration.monitors[index].id)
            });
        if duplicate {
            enumeration.monitors[index].stable_id = false;
        }
    }
    Ok(enumeration.monitors)
}

/// Read virtual desktop bounds using the same physical coordinate space as monitor inventory.
pub fn physical_desktop_bounds() -> Result<Bounds> {
    let _dpi = DpiAwareness::per_monitor_v2()?;
    Ok(crate::targets::desktop_bounds())
}

/// Resolve settings to physical desktop pixels, validating connected monitor IDs.
/// The returned bounds use physical coordinates and never retain an HMONITOR.
pub fn resolve(settings: &CursorSettings, window: Option<Bounds>) -> Result<Bounds> {
    let _dpi = DpiAwareness::per_monitor_v2()?;
    settings.validate()?;
    let monitor = if matches!(
        settings.bounds_mode,
        BoundsMode::Monitor | BoundsMode::Region
    ) {
        if let Some(monitor_id) = settings.monitor_id.as_deref() {
            let displays = inventory()?;
            let selected = displays
                .iter()
                .find(|display| display.id.eq_ignore_ascii_case(monitor_id))
                .context("The selected monitor is not connected")?;
            ensure!(
                selected.stable_id,
                "The selected monitor has only an unverified adapter-name identity; choose a monitor with a stable interface path"
            );
            Some(if settings.monitor_work_area {
                selected.work_area
            } else {
                selected.bounds
            })
        } else {
            ensure!(
                settings.bounds_mode == BoundsMode::Region,
                "Monitor bounds require a stable monitor identity"
            );
            None
        }
    } else {
        None
    };
    settings.resolve_bounds(crate::targets::desktop_bounds(), window, monitor)
}

/// Validate bounds settings before persisting or applying them to a live router.
pub fn validate_settings(settings: &CursorSettings) -> Result<()> {
    resolve(settings, None)?;
    Ok(())
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let enumeration = unsafe { &mut *(data as *mut Enumeration) };
    match unsafe { read_monitor(monitor) } {
        Ok(info) => {
            enumeration.monitors.push(info);
            1
        }
        Err(error) => {
            enumeration.error = Some(format!("{error:#}"));
            0
        }
    }
}

unsafe fn read_monitor(monitor: HMONITOR) -> Result<Monitor> {
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    ensure!(
        unsafe {
            GetMonitorInfoW(
                monitor,
                &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
            )
        } != 0,
        "GetMonitorInfoW failed"
    );

    let adapter_name = wide_string(&info.szDevice);
    let (interface_path, display_name) = unsafe { display_identity(&info.szDevice) };
    let stable_id = interface_path.is_some();
    let id = interface_path.unwrap_or_else(|| format!("unverified-adapter:{adapter_name}"));
    let name = display_name.unwrap_or_else(|| adapter_name.clone());
    Ok(Monitor {
        id,
        stable_id,
        name,
        bounds: rect_to_bounds(info.monitorInfo.rcMonitor),
        work_area: rect_to_bounds(info.monitorInfo.rcWork),
        primary: info.monitorInfo.dwFlags & 1 != 0,
        dpi: None,
    })
}

unsafe fn display_identity(adapter_name: &[u16]) -> (Option<String>, Option<String>) {
    let mut index = 0;
    loop {
        let mut display = unsafe { std::mem::zeroed::<DISPLAY_DEVICEW>() };
        display.cb = size_of::<DISPLAY_DEVICEW>() as u32;
        let found = unsafe {
            EnumDisplayDevicesW(
                adapter_name.as_ptr(),
                index,
                &mut display,
                EDD_GET_DEVICE_INTERFACE_NAME,
            )
        } != 0;
        if !found {
            return (None, None);
        }
        index += 1;
        if display.StateFlags & DISPLAY_DEVICE_ACTIVE == 0 {
            continue;
        }
        let path = wide_string(&display.DeviceID);
        let name = wide_string(&display.DeviceString);
        if !path.is_empty() {
            return (Some(path), (!name.is_empty()).then_some(name));
        }
    }
}

struct DpiAwareness(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);

impl DpiAwareness {
    fn per_monitor_v2() -> Result<Self> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        ensure!(
            !previous.is_null(),
            "Could not set per-monitor DPI awareness context"
        );
        Ok(Self(previous))
    }
}

impl Drop for DpiAwareness {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

fn wide_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

fn rect_to_bounds(rect: RECT) -> Bounds {
    Bounds {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

const _: MONITORENUMPROC = Some(collect_monitor);
