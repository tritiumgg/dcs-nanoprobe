//! Platform names: the DCS names of the groups, units and statics the
//! platform creates (spec/world.md, "Names"). The agent builds them and the
//! sidecar parses them, so both use this one codec.

/// The base-62 digits, in value order.
pub const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// The largest entity a name holds: the largest identity that survives a trip
/// through a Lua number.
pub const MAX_ENTITY: u64 = (1 << 53) - 1;

/// The longest platform name, in bytes: `np`, the marker, a 3-character tag
/// and 9 base-62 digits.
pub const MAX_LEN: usize = 15;

/// The marker after `np`: which DCS namespace the name is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `!`
    Group,
    /// `.`
    Unit,
    /// `#`
    Static,
}

impl Kind {
    /// The marker character.
    pub fn marker(self) -> u8 {
        match self {
            Kind::Group => b'!',
            Kind::Unit => b'.',
            Kind::Static => b'#',
        }
    }

    fn of_marker(b: u8) -> Option<Kind> {
        match b {
            b'!' => Some(Kind::Group),
            b'.' => Some(Kind::Unit),
            b'#' => Some(Kind::Static),
            _ => None,
        }
    }
}

/// A campaign's tag: 3 base-62 characters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(pub [u8; 3]);

impl Tag {
    /// The tag of 3 bytes, if each is a base-62 digit.
    pub fn new(b: [u8; 3]) -> Option<Tag> {
        b.iter().all(|&c| digit(c).is_some()).then_some(Tag(b))
    }

    /// The tag as text.
    pub fn as_str(&self) -> &str {
        // Every byte is an ASCII digit of `DIGITS`.
        core::str::from_utf8(&self.0).unwrap_or("")
    }
}

impl core::fmt::Display for Tag {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The number of distinct tags, 62^3.
pub const TAGS: u64 = 62 * 62 * 62;

/// A campaign's tag, a fixed function of its ID (spec/world.md, "Names"):
/// the SplitMix64 finalizer of the ID, reduced modulo 62^3 and written as 3
/// base-62 digits, most significant first, leading zeros kept. Stored
/// campaigns depend on it: never change it.
pub fn tag_of(campaign: u64) -> Tag {
    let v = mix(campaign) % TAGS;
    Tag([
        DIGITS[(v / (62 * 62)) as usize],
        DIGITS[(v / 62 % 62) as usize],
        DIGITS[(v % 62) as usize],
    ])
}

/// SplitMix64's output for the state `x` before its increment.
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'Z' => Some(c - b'A' + 10),
        b'a'..=b'z' => Some(c - b'a' + 36),
        _ => None,
    }
}

/// Append `v` in base 62, no leading zeros (0 is `0`).
pub fn push_base62(mut v: u64, out: &mut String) {
    let mut buf = [0u8; 11];
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = DIGITS[(v % 62) as usize];
        v /= 62;
        if v == 0 {
            break;
        }
    }
    out.extend(buf[at..].iter().map(|&b| b as char));
}

/// A base-62 number of 1 to 9 digits with no leading zero, at most
/// [`MAX_ENTITY`] and not 0.
fn parse_entity(s: &[u8]) -> Option<u64> {
    if s.is_empty() || s.len() > 9 || s[0] == b'0' {
        return None;
    }
    let mut v: u64 = 0;
    for &c in s {
        v = v * 62 + u64::from(digit(c)?);
    }
    (v <= MAX_ENTITY).then_some(v)
}

/// The DCS name of a platform object: `np<marker><tag><entity>`.
pub fn name(kind: Kind, tag: Tag, entity: u64) -> String {
    let mut s = String::with_capacity(MAX_LEN);
    s.push_str("np");
    s.push(kind.marker() as char);
    s.push_str(tag.as_str());
    push_base62(entity, &mut s);
    s
}

/// A parsed platform name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The namespace marker.
    pub kind: Kind,
    /// The campaign's tag.
    pub tag: Tag,
    /// The entity, never 0.
    pub entity: u64,
}

/// Parse a platform name. Anything not exactly in the form, including an
/// entity of 0, a leading zero or an entity above [`MAX_ENTITY`], is not one.
pub fn parse(name: &str) -> Option<Parsed> {
    let b = name.as_bytes();
    if b.len() < 7 || b.len() > MAX_LEN || &b[..2] != b"np" {
        return None;
    }
    let kind = Kind::of_marker(b[2])?;
    let tag = Tag::new([b[3], b[4], b[5]])?;
    let entity = parse_entity(&b[6..])?;
    Some(Parsed { kind, tag, entity })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(s: &str) -> Tag {
        let b = s.as_bytes();
        Tag::new([b[0], b[1], b[2]]).unwrap()
    }

    /// The vectors in spec/world.md, "Names". A change here renames every
    /// stored campaign's objects.
    #[test]
    fn tags_match_the_golden_vectors() {
        // SplitMix64's first output from state 0, its published reference.
        assert_eq!(mix(0), 0xE220_A839_7B1D_CDAF);
        for (c, t) in [
            (0, "nwl"),
            (1, "blp"),
            (7, "hGx"),
            (1_700_000_000_000, "ncp"),
            (1_763_000_000_000, "jqW"),
            (1_763_020_000_000, "nEd"),
            (MAX_ENTITY, "pzz"),
        ] {
            assert_eq!(tag_of(c).as_str(), t, "{c}");
        }
    }

    #[test]
    fn the_spec_example_parses() {
        let t = tag_of(1_763_000_000_000);
        assert_eq!(t, tag("jqW"));
        assert_eq!(name(Kind::Group, t, 72), "np!jqW1A");
        assert_eq!(name(Kind::Unit, t, 73), "np.jqW1B");
        assert_eq!(name(Kind::Static, t, 74), "np#jqW1C");
        let p = parse("np.jqW1B").unwrap();
        assert_eq!((p.kind, p.tag, p.entity), (Kind::Unit, t, 73));
    }

    #[test]
    fn the_entity_above_the_range_is_refused() {
        for k in [Kind::Group, Kind::Unit, Kind::Static] {
            let mut s = format!("np{}abc", k.marker() as char);
            push_base62(MAX_ENTITY + 1, &mut s);
            assert_eq!(s.len(), MAX_LEN);
            assert_eq!(parse(&s), None, "{s}");
        }
    }

    #[test]
    fn names_round_trip_at_the_ends_of_the_range() {
        for e in [
            1,
            9,
            10,
            35,
            36,
            61,
            62,
            63,
            3843,
            3844,
            MAX_ENTITY - 1,
            MAX_ENTITY,
        ] {
            for k in [Kind::Group, Kind::Unit, Kind::Static] {
                for t in ["000", "zzz", "aZ9"] {
                    let s = name(k, tag(t), e);
                    assert!(s.len() <= MAX_LEN, "{s}");
                    assert_eq!(
                        parse(&s),
                        Some(Parsed {
                            kind: k,
                            tag: tag(t),
                            entity: e
                        })
                    );
                }
            }
        }
        assert_eq!(name(Kind::Unit, tag("000"), MAX_ENTITY).len(), MAX_LEN);
    }

    #[test]
    fn every_digit_is_its_value() {
        for (v, &c) in DIGITS.iter().enumerate() {
            assert_eq!(digit(c), Some(v as u8));
            let s = format!("np.abc{}", c as char);
            let p = parse(&s);
            if v == 0 {
                assert_eq!(p, None, "{s}");
            } else {
                assert_eq!(p.map(|p| p.entity), Some(v as u64), "{s}");
                assert_eq!(name(Kind::Unit, tag("abc"), v as u64), s);
            }
            assert!(Tag::new([c, c, c]).is_some());
        }
        assert_eq!(parse("np.abc10").map(|p| p.entity), Some(62));
    }

    #[test]
    fn names_that_differ_only_by_case_are_different() {
        for (a, b) in [
            ("np.abcA", "np.abca"),
            ("np.Abc1", "np.abc1"),
            ("np!xYz1", "np!XyZ1"),
        ] {
            let (pa, pb) = (parse(a).unwrap(), parse(b).unwrap());
            assert_ne!(pa, pb, "{a} {b}");
        }
        assert_eq!(parse("np.abcA").unwrap().entity, 10);
        assert_eq!(parse("np.abca").unwrap().entity, 36);
    }

    #[test]
    fn the_kind_is_the_marker() {
        assert_eq!(parse("np!abc1").unwrap().kind, Kind::Group);
        assert_eq!(parse("np.abc1").unwrap().kind, Kind::Unit);
        assert_eq!(parse("np#abc1").unwrap().kind, Kind::Static);
    }

    #[test]
    fn other_names_do_not_parse() {
        let over = format!("np.abc{}", "z".repeat(9)); // above MAX_ENTITY
        let long = format!("np.abc1{}", "0".repeat(9)); // 16 bytes
        for s in [
            "",
            "np",
            "np.abc",
            "np.abc0",
            "np.abc01",
            "np-abc1",
            "nP.abc1",
            "NP.abc1",
            "np.ab-1",
            "np.ab_1",
            "np.abc1.",
            "np.abc-1",
            "np.abc 1",
            "np.ab\u{e9}1",
            "np.u.m3k9x2a1.1z",
            "Ground-1",
            &over,
            &long,
        ] {
            assert_eq!(parse(s), None, "{s}");
        }
    }
}
