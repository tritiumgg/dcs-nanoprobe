//! The sidecar's public API server (plan 4.5): gRPC services over the world
//! store and the reconciler, the controller lease with its fencing epoch,
//! tokens and roles, idempotency keys and bounded subscriptions.
//!
//! [`hub::Hub`] holds the state and runs the driver; [`server::serve`] puts
//! the services on a socket. The simulation is a [`hub::Backend`]: the fake
//! sim in development and CI, the real agent in production.

pub mod auth;
pub mod config;
pub mod convert;
pub mod drawing;
pub mod events;
pub mod group_template;
pub mod hub;
pub mod idem;
pub mod lease;
pub mod loadouts;
pub mod lua;
pub mod next;
pub mod payload_preset;
pub mod real;
pub mod server;
pub mod services;
pub mod sidecar_toml;
pub mod static_template;
pub mod template_file;
pub mod timings;
