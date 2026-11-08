//! Injection-independent cooperative application input. The embedding host owns authority.
//!
//! This module neither discovers physical devices nor dispatches OS input. An accepted
//! event means publication to the cooperative engine, never delivery to an OS target.
//! External adapters must authenticate recipients before the host approves a grant.
mod engine;
mod types;
mod window;
pub use engine::Engine;
pub use types::*;
pub use window::*;

#[cfg(test)]
mod tests;
