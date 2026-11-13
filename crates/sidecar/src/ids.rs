//! Identities and platform names (spec/world.md, "Identities" and "Names").

use core::fmt;

/// The largest identity that survives a trip through a Lua number.
pub const MAX_ID: u64 = (1 << 53) - 1;

/// An entity of one campaign. Never reused within the campaign; 0 means none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(pub u64);

/// A campaign. Never 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CampaignId(pub u64);

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The `<kind>` part of a platform name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameKind {
    /// `g`
    Group,
    /// `u`
    Unit,
    /// `s`
    Static,
}

impl NameKind {
    fn letter(self) -> char {
        match self {
            NameKind::Group => 'g',
            NameKind::Unit => 'u',
            NameKind::Static => 's',
        }
    }
}

/// A parsed platform name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformName {
    /// The kind letter.
    pub kind: NameKind,
    /// The campaign it names.
    pub campaign: CampaignId,
    /// The entity it names.
    pub entity: EntityId,
}

fn base36(mut v: u64, out: &mut String) {
    let mut digits = [0u8; 13];
    let mut n = 0;
    loop {
        let d = (v % 36) as u8;
        digits[n] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        n += 1;
        v /= 36;
        if v == 0 {
            break;
        }
    }
    for &d in digits[..n].iter().rev() {
        out.push(d as char);
    }
}

fn parse_base36(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 11 || s.starts_with('0') {
        return None;
    }
    let mut v: u64 = 0;
    for c in s.bytes() {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'z' => c - b'a' + 10,
            _ => return None,
        };
        v = v.checked_mul(36)?.checked_add(u64::from(d))?;
    }
    (v <= MAX_ID).then_some(v)
}

/// The DCS name of a platform object: `np.<kind>.<campaign>.<entity>`.
pub fn name(kind: NameKind, campaign: CampaignId, entity: EntityId) -> String {
    debug_assert!(campaign.0 != 0 && campaign.0 <= MAX_ID);
    debug_assert!(entity.0 != 0 && entity.0 <= MAX_ID);
    let mut s = String::with_capacity(28);
    s.push_str("np.");
    s.push(kind.letter());
    s.push('.');
    base36(campaign.0, &mut s);
    s.push('.');
    base36(entity.0, &mut s);
    s
}

/// Parse a platform name. Anything not exactly in the form, including a zero
/// or a leading zero, is not a platform name.
pub fn parse(name: &str) -> Option<PlatformName> {
    let mut parts = name.split('.');
    if parts.next()? != "np" {
        return None;
    }
    let kind = match parts.next()? {
        "g" => NameKind::Group,
        "u" => NameKind::Unit,
        "s" => NameKind::Static,
        _ => return None,
    };
    let campaign = parse_base36(parts.next()?)?;
    let entity = parse_base36(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(PlatformName {
        kind,
        campaign: CampaignId(campaign),
        entity: EntityId(entity),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_example_parses() {
        let n = parse("np.u.m3k9x2a1.1z").unwrap();
        assert_eq!(n.kind, NameKind::Unit);
        assert_eq!(n.entity, EntityId(71));
        assert_eq!(
            name(NameKind::Unit, n.campaign, n.entity),
            "np.u.m3k9x2a1.1z"
        );
    }

    #[test]
    fn names_round_trip_and_fit() {
        for (c, e) in [(1, 1), (MAX_ID, MAX_ID), (1_763_000_000_000, 36), (35, 37)] {
            for k in [NameKind::Group, NameKind::Unit, NameKind::Static] {
                let s = name(k, CampaignId(c), EntityId(e));
                assert!(s.len() <= 28, "{s}");
                assert!(
                    s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.')
                );
                let p = parse(&s).unwrap();
                assert_eq!((p.kind, p.campaign.0, p.entity.0), (k, c, e));
            }
        }
    }

    #[test]
    fn other_names_do_not_parse() {
        for s in [
            "",
            "np",
            "np.g.1",
            "np.g.1.0",
            "np.g.0.1",
            "np.g.01.1",
            "np.x.1.1",
            "np.g.1.1.1",
            "NP.g.1.1",
            "np.g.1.A",
            "np.g.1.-1",
            "np.g.zzzzzzzzzzz.1",
            "Ground-1",
        ] {
            assert_eq!(parse(s), None, "{s}");
        }
    }
}
