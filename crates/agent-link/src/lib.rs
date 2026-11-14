//! The agent's side of the transport (spec/transport.md), in plain Rust so
//! that it runs in host tests. The agent DLL drives a [`Link`] from DCS's
//! frames; the fake agent drives one from a timer and breaks it on purpose.
//!
//! [`config`] reads `agent.toml` and the token (spec/config.md). [`sampler`]
//! keeps the object table and plans the sampling within the frame's slice
//! (spec/sampling.md).

pub mod config;
mod link;
pub mod revisions;
pub mod sampler;

pub use link::{Admit, Link, Options, Stamps};

#[cfg(feature = "faults")]
pub use link::Faults;
