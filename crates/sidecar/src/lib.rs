//! The sidecar: world store, reconciler, persistence and API server.

#[cfg(feature = "disk")]
pub mod campaigns;
pub mod conformance;
#[cfg(feature = "disk")]
pub mod eventfile;
#[cfg(feature = "disk")]
pub mod export;
pub mod grid;
pub mod ids;
pub mod intents;
pub mod lifecycle;
pub mod link;
pub mod mirror;
pub mod players;
pub mod reconciler;
pub mod replay;
#[cfg(feature = "disk")]
pub mod server;
#[cfg(feature = "disk")]
pub mod shell;
pub mod sim;
#[cfg(feature = "disk")]
pub mod space;
pub mod stock;
pub mod store;
