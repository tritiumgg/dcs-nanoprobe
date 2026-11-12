//! What the agent and the sidecar share: the mapped file, its status block and
//! two rings, the record encodings, the handshake lines, and the log record.
//!
//! Layouts follow `spec/transport.md`, `spec/records.md` and
//! `spec/commands.md`. Every offset here is checked against
//! `spec/transport.golden`, which is built into the crate ([`golden`]).

pub mod codec;
pub mod golden;
pub mod handshake;
pub mod layout;
pub mod log;
pub mod map;
pub mod mem;
pub mod records;
pub mod ring;
pub mod status;

/// A random non-zero value, for a boot or an attach ID.
pub fn random_nonzero() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    loop {
        // `RandomState` is keyed from the operating system's random source.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        let v = h.finish();
        if v != 0 {
            return v;
        }
    }
}
