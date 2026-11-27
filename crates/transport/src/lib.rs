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
pub mod names;
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

/// The longest `PICTURE` file name, in bytes.
pub const PICTURE_FILE_MAX: usize = 255;

/// Whether a `PICTURE` file name is one the hook may put in Lua source:
/// 1 to [`PICTURE_FILE_MAX`] bytes of ASCII letters, digits, space and
/// `_.-/()+,#`. No quote, backslash or control character, so the name cannot
/// end the string it sits in (spec/commands.md, "Handler arguments").
pub fn picture_file_ok(file: &str) -> bool {
    !file.is_empty()
        && file.len() <= PICTURE_FILE_MAX
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" _.-/()+,#".contains(&b))
}

/// The longest `BROADCAST` name, in bytes.
pub const BROADCAST_NAME_MAX: usize = 64;

/// Whether a `BROADCAST` or `BROADCAST_STOP` name is valid: 1 to
/// [`BROADCAST_NAME_MAX`] bytes of ASCII letters, digits, space and `_.-`
/// (spec/commands.md, "Layout: BROADCAST").
pub fn broadcast_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= BROADCAST_NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" _.-".contains(&b))
}

#[cfg(test)]
mod broadcast_name_tests {
    use super::broadcast_name_ok;

    #[test]
    fn only_plain_names_pass() {
        for ok in ["a", "Beacon 1", "a_b.c-d", &"x".repeat(64)] {
            assert!(broadcast_name_ok(ok), "{ok}");
        }
        for bad in ["", "a/b", "a\"b", "\u{e9}", &"x".repeat(65)] {
            assert!(!broadcast_name_ok(bad), "{bad}");
        }
    }
}

#[cfg(test)]
mod picture_file_tests {
    use super::picture_file_ok;

    #[test]
    fn only_safe_names_pass() {
        for ok in ["ResKey_1", "a b.png", "l10n/DEFAULT/x-1(2).ogg", "#k,+"] {
            assert!(picture_file_ok(ok), "{ok}");
        }
        for bad in ["", "a\"b", "a'b", "a\\b", "a\nb", "a]]b", "é", "a;b", "a=b"] {
            assert!(!picture_file_ok(bad), "{bad:?}");
        }
        assert!(picture_file_ok(&"a".repeat(255)));
        assert!(!picture_file_ok(&"a".repeat(256)));
    }
}
