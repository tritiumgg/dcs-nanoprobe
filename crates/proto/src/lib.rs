//! Rust types and gRPC stubs for the nanoprobe public API.
//!
//! The schema is `proto/nanoprobe/v1alpha1/` in the repository. The code in
//! `src/gen/` is generated from it by `cargo xtask proto` and committed; do
//! not edit it by hand.

/// Schema version `v1alpha1`.
#[allow(missing_docs, clippy::all, clippy::pedantic)]
pub mod v1alpha1 {
    include!("gen/nanoprobe.v1alpha1.rs");
}

/// The schema version this crate was generated from, as `Hello` reports it.
pub const SCHEMA_VERSION: &str = "v1alpha1";

/// The compiled schema with its imports, as a `FileDescriptorSet`. The
/// server's reflection service serves it.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("gen/descriptor.binpb");

pub mod errors;
