//! Separate event-only loopback transport. No MCP commands or injection sink.
//! HostBridge stays on its !Send host UI thread; Service owns network/file work.
mod authority;
mod bridge;
mod network;
pub(crate) use authority::{ClientPermit, Config, IssuedClient, ServiceSnapshot};
pub(crate) use bridge::{HostBridge, NetworkEndpoint, TickReport};
pub(crate) use network::Service;

#[cfg(test)]
mod tests;
