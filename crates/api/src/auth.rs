//! Tokens and roles (spec/errors.md, "Roles"; spec/config.md, `tokens_path`).
//!
//! A loopback listener with no tokens file gives every caller `admin`. With
//! one, a caller sends `authorization: Bearer <token>` and gets the role the
//! file gives that token. The file holds the SHA-256 of each token, never the
//! token itself. A token is at least [`MIN_TOKEN_LEN`] characters of
//! printable ASCII with no spaces, and its hash is compared with every entry
//! in constant time.

use std::path::Path;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::{ErrorReason, Role};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tonic::{Code, Request, Status};

/// The fewest characters a token has (spec/config.md, "Tokens file").
pub const MIN_TOKEN_LEN: usize = 32;

/// The caller's role, put in each request's extensions by [`Tokens::check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Calls that change nothing.
    Observer,
    /// The above, the lease calls and most changing calls.
    Controller,
    /// Everything.
    Admin,
}

impl Access {
    /// The role as the API names it.
    pub fn role(self) -> Role {
        match self {
            Access::Observer => Role::Observer,
            Access::Controller => Role::Controller,
            Access::Admin => Role::Admin,
        }
    }

    fn parse(s: &str) -> Option<Access> {
        match s {
            "observer" => Some(Access::Observer),
            "controller" => Some(Access::Controller),
            "admin" => Some(Access::Admin),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    token: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    role: String,
    sha256: String,
}

/// The tokens a listener accepts. `None` inside means no file: everyone is
/// `admin`.
#[derive(Clone, Debug, Default)]
pub struct Tokens {
    entries: Option<Vec<([u8; 32], Access)>>,
}

/// The SHA-256 of a token, as the tokens file holds it: 64 lower-case hex
/// digits.
pub fn token_hash(token: &str) -> String {
    let d = Sha256::digest(token.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(h: &str) -> Option<[u8; 32]> {
    if h.len() != 64
        || !h
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Equal in time that does not depend on where the bytes differ.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let d = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(d) == 0
}

impl Tokens {
    /// No tokens file: every caller is `admin`. Only for a loopback listener.
    pub fn open() -> Tokens {
        Tokens { entries: None }
    }

    /// Whether a tokens file is in force.
    pub fn required(&self) -> bool {
        self.entries.is_some()
    }

    /// Parse a tokens file's text.
    pub fn parse(text: &str) -> Result<Tokens, String> {
        let f: File = toml::from_str(text).map_err(|e| format!("tokens file: {e}"))?;
        let mut entries: Vec<([u8; 32], Access)> = Vec::new();
        for e in f.token {
            let access = Access::parse(&e.role).ok_or_else(|| {
                format!(
                    "tokens file: token `{}` has unknown role `{}`",
                    e.name, e.role
                )
            })?;
            let hash = unhex(&e.sha256).ok_or_else(|| {
                format!(
                    "tokens file: token `{}` needs sha256 as 64 lower-case hex digits",
                    e.name
                )
            })?;
            if entries.iter().any(|(h, _)| *h == hash) {
                return Err(format!("tokens file: token `{}` repeats a hash", e.name));
            }
            entries.push((hash, access));
        }
        Ok(Tokens {
            entries: Some(entries),
        })
    }

    /// Read a tokens file.
    pub fn read(path: &Path) -> Result<Tokens, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Tokens::parse(&text)
    }

    /// The access a request's metadata gives.
    pub fn access(&self, authorization: Option<&str>) -> Result<Access, Status> {
        let Some(entries) = &self.entries else {
            return Ok(Access::Admin);
        };
        let refused = || {
            status(
                Code::Unauthenticated,
                ErrorReason::TokenRequired,
                format!(
                    "a valid token of at least {MIN_TOKEN_LEN} characters is required: send `authorization: Bearer <token>`"
                ),
                0,
                "",
            )
        };
        let token = authorization
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|t| t.len() >= MIN_TOKEN_LEN && t.bytes().all(|b| b.is_ascii_graphic()))
            .ok_or_else(refused)?;
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        // Every entry is compared, with no early exit.
        let mut found = None;
        for (h, a) in entries {
            if same(h, &presented) {
                found = Some(*a);
            }
        }
        found.ok_or_else(refused)
    }

    /// The interceptor: put the caller's [`Access`] in the request.
    pub fn check(&self, mut req: Request<()>) -> Result<Request<()>, Status> {
        let auth = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let access = self.access(auth.as_deref())?;
        req.extensions_mut().insert(access);
        Ok(req)
    }
}

/// The caller's access, which the interceptor put in the request.
pub fn access_of<T>(req: &Request<T>) -> Access {
    req.extensions()
        .get::<Access>()
        .copied()
        .unwrap_or(Access::Observer)
}

/// Fail with `ROLE_REQUIRED` unless the caller has at least `need`.
pub fn require<T>(req: &Request<T>, need: Access) -> Result<Access, Status> {
    let have = access_of(req);
    if have >= need {
        Ok(have)
    } else {
        Err(status(
            Code::PermissionDenied,
            ErrorReason::RoleRequired,
            format!("the call needs the {need:?} role; the token has {have:?}").to_lowercase(),
            0,
            "",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBS: &str = "observer-token-0123456789abcdef01";
    const CTL: &str = "controller-token-0123456789abcdef";

    #[test]
    fn roles_come_from_the_file() {
        let text = format!(
            "[[token]]\nname = \"watcher\"\nrole = \"observer\"\nsha256 = \"{}\"\n\n\
             [[token]]\nname = \"campaign\"\nrole = \"controller\"\nsha256 = \"{}\"\n",
            token_hash(OBS),
            token_hash(CTL)
        );
        let t = Tokens::parse(&text).expect("parse");
        assert!(t.required());
        assert_eq!(
            t.access(Some(&format!("Bearer {OBS}"))).ok(),
            Some(Access::Observer)
        );
        assert_eq!(
            t.access(Some(&format!("Bearer {CTL}"))).ok(),
            Some(Access::Controller)
        );
        // A listed token with a space in it is refused like any other.
        let spaced = "a token with spaces in it, longer than 32";
        let with_spaced = Tokens::parse(&format!(
            "[[token]]\nname=\"s\"\nrole=\"admin\"\nsha256=\"{}\"\n",
            token_hash(spaced)
        ))
        .expect("parse");
        assert!(
            with_spaced
                .access(Some(&format!("Bearer {spaced}")))
                .is_err()
        );
        let short = "short-token";
        let with_short = Tokens::parse(&format!(
            "[[token]]\nname=\"s\"\nrole=\"admin\"\nsha256=\"{}\"\n",
            token_hash(short)
        ))
        .expect("parse");
        // A token under the minimum is refused even when its hash is listed.
        assert!(with_short.access(Some(&format!("Bearer {short}"))).is_err());
        let wrong = format!("Bearer {}", "x".repeat(40));
        let bare = CTL.to_owned();
        for bad in [
            None,
            Some("Bearer "),
            Some(bare.as_str()),
            Some(wrong.as_str()),
        ] {
            let e = t.access(bad).expect_err("refused");
            assert_eq!(e.code(), Code::Unauthenticated);
            assert_eq!(
                nanoprobe_proto::errors::reason(&e),
                ErrorReason::TokenRequired
            );
        }
        assert_eq!(Tokens::open().access(None).ok(), Some(Access::Admin));
    }

    #[test]
    fn a_bad_file_is_refused() {
        let h = token_hash("x");
        let entry = |role: &str, hash: &str, more: &str| {
            format!("[[token]]\nname=\"a\"\nrole=\"{role}\"\nsha256=\"{hash}\"\n{more}")
        };
        assert!(Tokens::parse(&entry("admin", &h, "")).is_ok());
        // Each case differs from the good one in one way.
        assert!(Tokens::parse(&entry("root", &h, "")).is_err());
        assert!(Tokens::parse(&entry("admin", "00", "")).is_err());
        assert!(Tokens::parse(&entry("admin", &h.to_uppercase(), "")).is_err());
        assert!(Tokens::parse(&entry("admin", &h, "extra=1\n")).is_err());
        assert!(Tokens::parse(&format!("stray = 1\n{}", entry("admin", &h, ""))).is_err());
        let dup = format!("{}{}", entry("admin", &h, ""), entry("observer", &h, ""));
        assert!(Tokens::parse(&dup).is_err());
    }
}
