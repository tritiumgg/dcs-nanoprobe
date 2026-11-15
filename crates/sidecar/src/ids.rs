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

pub use nanoprobe_transport::names::Tag;
use nanoprobe_transport::names::{self, Kind};

/// The kind of a platform name: its DCS namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameKind {
    /// `np!`
    Group,
    /// `np.`
    Unit,
    /// `np#`
    Static,
}

impl NameKind {
    fn kind(self) -> Kind {
        match self {
            NameKind::Group => Kind::Group,
            NameKind::Unit => Kind::Unit,
            NameKind::Static => Kind::Static,
        }
    }
}

/// A parsed platform name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformName {
    /// The namespace marker.
    pub kind: NameKind,
    /// The tag of the campaign it names.
    pub tag: Tag,
    /// The entity it names.
    pub entity: EntityId,
}

/// A campaign's tag (spec/world.md, "Names").
pub fn tag(campaign: CampaignId) -> Tag {
    names::tag_of(campaign.0)
}

/// The ID of a new campaign (spec/world.md, "Identities" and "Names"): the
/// creation time `now_ms`, or one more than the newest of `existing` if that
/// is larger, then moved forward until no campaign of `existing` has its tag.
pub fn new_campaign(now_ms: u64, existing: &[CampaignId]) -> CampaignId {
    let newest = existing.iter().map(|c| c.0).max().unwrap_or(0);
    let mut id = now_ms.max(newest + 1).max(1);
    let taken: std::collections::HashSet<Tag> = existing.iter().map(|&c| tag(c)).collect();
    // Each step lands on a free tag with probability at least
    // 1 - existing/62^3, so this ends after a few steps.
    while taken.contains(&names::tag_of(id)) {
        id += 1;
    }
    debug_assert!(id <= MAX_ID);
    CampaignId(id)
}

/// The DCS name of a platform object: `np<marker><tag><entity>`.
pub fn name(kind: NameKind, campaign: CampaignId, entity: EntityId) -> String {
    debug_assert!(campaign.0 != 0 && campaign.0 <= MAX_ID);
    debug_assert!(entity.0 != 0 && entity.0 <= MAX_ID);
    names::name(kind.kind(), tag(campaign), entity.0)
}

/// Parse a platform name. Anything not exactly in the form, including a zero
/// or a leading zero, is not a platform name.
pub fn parse(name: &str) -> Option<PlatformName> {
    let p = names::parse(name)?;
    Some(PlatformName {
        kind: match p.kind {
            Kind::Group => NameKind::Group,
            Kind::Unit => NameKind::Unit,
            Kind::Static => NameKind::Static,
        },
        tag: p.tag,
        entity: EntityId(p.entity),
    })
}

/// Whether a parsed name belongs to `campaign`: its tag is the campaign's.
/// No name belongs to campaign 0, which means none.
pub fn of_campaign(p: &PlatformName, campaign: CampaignId) -> bool {
    campaign.0 != 0 && p.tag == tag(campaign)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_carry_the_campaign_tag() {
        let c = CampaignId(1_763_000_000_000);
        assert_eq!(tag(c).as_str(), "jqW");
        assert_eq!(name(NameKind::Unit, c, EntityId(73)), "np.jqW1B");
        assert_eq!(name(NameKind::Group, c, EntityId(72)), "np!jqW1A");
        assert_eq!(name(NameKind::Static, c, EntityId(74)), "np#jqW1C");
        for (k, e) in [
            (NameKind::Group, 1),
            (NameKind::Unit, MAX_ID),
            (NameKind::Static, 62),
        ] {
            let p = parse(&name(k, c, EntityId(e))).unwrap();
            assert_eq!((p.kind, p.entity), (k, EntityId(e)));
            assert!(of_campaign(&p, c));
        }
    }

    #[test]
    fn a_name_of_another_tag_is_foreign() {
        let (a, b) = (CampaignId(1_763_000_000_000), CampaignId(1_700_000_000_000));
        let p = parse(&name(NameKind::Unit, b, EntityId(5))).unwrap();
        assert!(!of_campaign(&p, a));
        assert!(of_campaign(&p, b));
        // Campaign 0's hash is `nwl`, and still no name is campaign 0's.
        assert!(!of_campaign(&parse("np.nwl1").unwrap(), CampaignId(0)));
    }

    #[test]
    fn a_new_campaign_never_shares_a_tag_or_an_id() {
        assert_eq!(
            new_campaign(1_763_000_000_000, &[]),
            CampaignId(1_763_000_000_000)
        );
        // A clock behind the newest campaign still gives a newer ID.
        assert_eq!(new_campaign(5, &[CampaignId(9)]), CampaignId(10));
        // The tag of the creation millisecond is taken: the ID moves forward.
        let now = 1_763_000_000_000;
        let mut clash = 1;
        while tag(CampaignId(clash)) != tag(CampaignId(now)) {
            clash += 1;
        }
        let id = new_campaign(now, &[CampaignId(clash)]);
        assert!(id.0 > now);
        assert_ne!(tag(id), tag(CampaignId(clash)));
        // Many campaigns: every new one gets a tag none of them has.
        let mut all: Vec<CampaignId> = Vec::new();
        for _ in 0..2_000 {
            let id = new_campaign(now, &all);
            assert!(all.iter().all(|c| tag(*c) != tag(id) && *c != id));
            all.push(id);
        }
    }
}
