# ADR 0009: Platform names: a base-62 tag hashed from the campaign ID

## Status

Accepted

## Context

DCS names every group, unit and static. After a DCS restart the sidecar rebuilds its
binding from names alone, so a platform object's name must say which campaign and
which entity it is. The agent builds names from the command header's campaign ID. It
cannot read a tag stored only in the sidecar.

Players read unit and group names in the unit dialog (LIVE), so shorter names are
better. These facts hold (LIVE, DCS 2.9.31.29434, a dedicated server under Wine 10.0
and a Windows client; [the first spikes](../results/2026-11-11-first-dll-and-spikes.md),
[world commands](../results/2026-11-15-s15-world-commands.md),
[the FARP name](../results/2026-11-15-e21c-farp-name.md),
[the base-62 check](../results/2026-11-15-naming-base62.md)):

- DCS names are case-sensitive.
- Unit and static names share one namespace. A new object with a taken name removes
  the old holder without an event. Group names are a separate namespace.
- Names in the character set `0-9A-Za-z`, with a leading `.`, `#` or `!` after `np`,
  are accepted, found by name and read back exactly.

## Decision

A platform name is `np`, a marker character, the campaign's 3-character base-62 tag,
and the entity ID in base 62. The tag is a fixed hash of the campaign ID.

- The forms are `np!<tag><entity>` for a group, `np.<tag><entity>` for a unit and
  `np#<tag><entity>` for a static. The alphabet is `0-9A-Za-z`. The entity has no
  leading zeros, the tag keeps them, and nothing separates them.
- The tag is `SplitMix64(campaign ID) mod 62^3`, written as 3 base-62 digits. The
  function never changes, because stored campaigns depend on it. The spec holds test
  vectors.
- The sidecar creates a campaign by taking its ID from the Identities rule, then
  moving it forward by one until no campaign in its stores has the same tag.
- One codec in the transport crate builds and parses names for both the agent and the
  sidecar.
- An object whose name parses with another campaign's tag is never bound to the
  active campaign. It is reported as unmanaged with the warning `foreign_name`.
- A name holds no player name or label. An author's label is stored in the sidecar and
  served through the API.

Spec: [world](../../spec/world.md), "Names" and "The campaign tag".

Rejected:

- A tag stored in the sidecar and sent to the agent in the `CAMPAIGN` control payload.
  It changes the transport.
- A longer base-36 name. Base 62 holds a name in about half the characters.

## Consequences

- A tag can equal the tag of a campaign the stores have never held, with probability
  1 in 238,328 per pair. That campaign's objects, in a mission DCS still runs, read as
  the active campaign's.
- A campaign folder removed by hand, or lost with the server store, leaves no trace,
  and its tag can be given to a new campaign.
- A campaign ID is later than its creation millisecond by the number of tag
  collisions it skipped.
- The shell writer never puts a name in this form into a mission. A campaign's own
  template that places one is refused.
