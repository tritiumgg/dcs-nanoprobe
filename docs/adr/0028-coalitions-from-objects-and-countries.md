# ADR 0028: Coalitions come from OBJECT records and the COUNTRIES query

## Status

Accepted

## Context

A group or static spec carries a country, not a coalition. DCS takes the coalition
from the country, and the mission sets which coalition owns which country. A client of
the API needs a coalition for every entity, including those not yet in DCS: a group
that is `virtual`, or one still `materializing`.

The agent reports the coalition of each object it holds in an `OBJECT` record
([spec/records.md](../../spec/records.md)). It holds no object for a `virtual` entity.
The sidecar cannot compute the map from country to coalition itself, because the
mission sets it. DCS exposes it through `coalition.getCountryCoalition`.

## Decision

The API reports a materialized object's coalition from its `OBJECT` records. For a group
or static that is not materialized, it uses the country map from the `COUNTRIES` query.
Until the answer arrives, the coalition is `COALITION_UNSPECIFIED`.

- `COUNTRIES` is a query, opcode 65, in the `server` state. Its campaign is `any` and
  its generation is `bound` ([spec/commands.md](../../spec/commands.md)).
- The handler calls `coalition.getCountryCoalition` for each value of `country.id`,
  each under `pcall`. It leaves out a country whose read raises or returns no number.
  The result's `value` maps country number to coalition number.
- The sidecar sends the query whenever it holds no map for the current mission
  generation and a mission is loaded: a new generation, a sidecar restart or a lost
  answer. A resync `HELLO` within one generation keeps the map.
- The spec carries no coalition for a group or static. The store keeps the map, not
  the spec.
- A disagreement between an `OBJECT` coalition and the map is counted.

Rejected:

- A coalition field in `GroupSpec` and `StaticSpec`. DCS takes the coalition from the
  country, so a field could disagree with the mission.
- A sampled tier or a `HELLO` field for the map. The map is taken to change only with
  the mission (UNVERIFIED), so one query per generation is enough.

## Consequences

- A `virtual` entity shows `COALITION_UNSPECIFIED` until the agent answers. A server
  with no agent connected cannot answer.
- A mission whose country-to-coalition table the script changes mid-generation shows
  the old map. UNVERIFIED whether DCS allows such a change at run time.
- The code is `crates/sidecar/src/store.rs` (`country_coalition`),
  `crates/api/src/convert.rs`, and the `COUNTRIES` arm of
  `crates/sidecar/src/reconciler.rs`.
