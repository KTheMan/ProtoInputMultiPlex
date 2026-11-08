#![deny(unsafe_op_in_unsafe_fn)]
#[cfg(windows)]
pub mod annotation_overlay;
#[cfg(windows)]
pub mod app_cursor;
#[cfg(windows)]
pub mod desktop_capture;
#[cfg(windows)]
pub mod job;
#[cfg(windows)]
pub mod keyboard;
#[cfg(windows)]
pub mod keyboard_focus;
#[cfg(windows)]
mod keyboard_state;
#[cfg(windows)]
pub mod lifecycle;
pub mod magnifier_overlay;
#[cfg(windows)]
pub mod monitors;
#[cfg(windows)]
pub mod power;
#[cfg(windows)]
mod process_arch;
#[cfg(windows)]
pub mod protoinput;
#[cfg(windows)]
mod queue_fence;
#[cfg(windows)]
pub mod raw_input;
#[cfg(windows)]
mod system_cursor;
#[cfg(windows)]
pub mod target_capture;
#[cfg(windows)]
pub mod targets;
#[cfg(windows)]
pub mod wheel;

#[cfg(windows)]
pub mod overlay;

#[cfg(windows)]
mod cursor_presenter;

#[cfg(windows)]
mod cursor_rasterizer;

#[cfg(windows)]
mod cursor_telemetry;

#[cfg(all(windows, feature = "cursor-presentation-evidence"))]
mod cursor_evidence;
