# Test templates

Empty missions saved from the DCS Mission Editor, one per map, for the tests and for
`cargo xtask`. They are test fixtures: no release archive, image or installer carries
them. A server's administrator saves their own per-map templates
(docs/mission-templates.md).

| File | Map |
|---|---|
| `caucasus.miz` | Caucasus |
| `afghanistan.miz` | Afghanistan |

Each holds the six members the Mission Editor writes, in its order: `mission`,
`l10n/DEFAULT/dictionary`, `l10n/DEFAULT/mapResource`, `warehouses`, `options`,
`theatre`. All but `options` are as the editor saved them.

The editor copies the author's own game options into `options`: player name, graphics,
VR, sound, plugins, views and client preferences. A fixture keeps only `difficulty` and
`format` from it. A test in `crates/xtask/src/miz.rs` fails if any other key returns.
