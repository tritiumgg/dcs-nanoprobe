# Mission templates

One empty mission per map, saved from the DCS Mission Editor, for `cargo xtask miz` to
write shell missions from.

| File | Map |
|---|---|
| `caucasus.miz` | Caucasus |
| `afghanistan.miz` | Afghanistan |

Each holds the six members the Mission Editor writes, in its order: `mission`,
`l10n/DEFAULT/dictionary`, `l10n/DEFAULT/mapResource`, `warehouses`, `options`,
`theatre`. All but `options` are as the editor saved them.

The editor copies the author's own game options into `options`: player name, graphics,
VR, sound, plugins, views and client preferences. A template keeps only `difficulty`
and `format` from it. A test in `crates/xtask/src/miz.rs` fails if any other key
returns.

Whether a server loads a mission with this reduced `options` is UNVERIFIED. The first
server session that loads a generated shell settles it.
