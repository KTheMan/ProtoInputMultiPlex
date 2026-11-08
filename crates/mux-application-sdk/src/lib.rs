//! Cooperative application SDK. No physical-input provider or target-injection sink.
//! `Host` is a trusted embedding boundary, not a remote command server. Network
//! adapters authenticate identities and bind recipients before calling dispatch.
#[cfg(feature = "native-ffi")]
pub mod abi_contract;
pub mod client;
#[cfg(feature = "native-ffi")]
pub mod ffi;
pub mod host;
pub mod protocol;
mod windows;

pub use client::{ClientState, EventClientState, FoldError, FoldOutcome, StreamState};
pub use host::{Host, WindowBackend};
pub use protocol::*;
pub use windows::PlatformWindows;

#[cfg(test)]
mod tests;

pub use mux_core::application_event::*;
pub use mux_core::desktop::{CursorShape, MouseButton};

#[cfg(all(windows, feature = "native-client"))]
pub mod native_client;
