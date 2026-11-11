//! What differs between the Windows DLL and the host-native build.
//!
//! Everything Windows-only lives behind this module, so the agent's logic runs
//! in host tests under a stock Lua 5.1. The host side is enough for those
//! tests; it is not a second product.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod host;
#[cfg(unix)]
pub use host::*;
