//! The sidecar: world store, reconciler, persistence and API server.

#[cfg(feature = "disk")]
pub mod campaigns;
pub mod conformance;
pub mod eventfile;
pub mod grid;
pub mod ids;
pub mod intents;
pub mod lifecycle;
pub mod link;
pub mod mirror;
pub mod players;
pub mod reconciler;
pub mod replay;
pub mod sim;
pub mod stock;
pub mod store;
