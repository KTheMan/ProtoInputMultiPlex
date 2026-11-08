//! Rust/WASM cooperative event consumer. No physical input or target injection.
//! Browser glue, local authority gates, rendering and event handlers live in Rust.
pub mod event_hooks;
pub mod geometry;
pub mod local_input;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
