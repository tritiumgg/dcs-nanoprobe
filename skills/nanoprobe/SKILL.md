---
name: nanoprobe
description: Write, debug or run a nanoprobe controller, a program that runs a persistent DCS World campaign through nanoprobe's gRPC API (package nanoprobe.v1alpha1), or install and operate nanoprobe on a DCS server. Use when code calls SessionService, CommandsService, WorldService, EventsService or another nanoprobe service, when handling nanoprobe errors, leases, idempotency keys, revisions or events, or when running nanoprobe doctor, nanoprobe-sidecar or a nanoprobe upgrade.
license: MIT
---

# nanoprobe

nanoprobe runs a persistent campaign on a DCS World server. The agent, a DLL inside
DCS, reads state and applies commands. The sidecar, beside DCS, stores the world, makes
DCS match it, and serves a gRPC API on `127.0.0.1:47311` by default. A controller calls
only the sidecar.

The documents below win over this file. Read them from a local checkout when one is
present, else from GitHub.

| Document | Path in the repository | On GitHub |
|---|---|---|
| Controller guide | `docs/controller-guide.md` | <https://github.com/tritiumgg/dcs-nanoprobe/blob/main/docs/controller-guide.md> |
| Operator guide | `docs/operator-guide.md` | <https://github.com/tritiumgg/dcs-nanoprobe/blob/main/docs/operator-guide.md> |
| Every setting | `docs/configuration.md` | <https://github.com/tritiumgg/dcs-nanoprobe/blob/main/docs/configuration.md> |
| Generating a client | `docs/api-clients.md` | <https://github.com/tritiumgg/dcs-nanoprobe/blob/main/docs/api-clients.md> |
| Schema | `proto/nanoprobe/v1alpha1/` | <https://github.com/tritiumgg/dcs-nanoprobe/tree/main/proto/nanoprobe/v1alpha1> |
| Errors and health | `spec/errors.md` | <https://github.com/tritiumgg/dcs-nanoprobe/blob/main/spec/errors.md> |
| World model | `spec/world.md` | <https://github.com/tritiumgg/dcs-nanoprobe/blob/main/spec/world.md> |
| Hello examples | `examples/hello-{rust,python,kotlin}` | <https://github.com/tritiumgg/dcs-nanoprobe/tree/main/examples> |

Use the schema of the release the server runs. Each GitHub release carries
it as `nanoprobe-<version>-schema.tar.gz` and `-schema.binpb`. `Hello` returns the
server's version.

## Writing a controller

1. Read the controller guide whole before writing code. Read the proto file of each
   service you call for exact fields.
2. Start from the hello example in the user's language, or the one closest to it.
3. Follow these rules.
   - Call `Hello` first. Take the lease with `AcquireLease` and renew it at a third of
     its `ttl`. Every changing call carries the lease and an idempotency key, most in a
     `Write` with a campaign ID (0 is the active campaign). CampaignService calls carry
     `lease` and `idempotency` as their own fields.
   - Give every change its own idempotency key, and resend a change only with its own
     key. A new key sends an effect twice. Store the key durably before the call, so a
     restarted controller resends with it.
   - A set-state call (`Put*`, `SetOrder`, `Materialize`, ...) returns once the store
     holds the change. DCS may not hold it yet. Watch the entity's
     `applied_spec_revision` or `applied_order_revision`, its state, and `last_error` in
     `WatchWorld`.
   - An effect or a query can return `PENDING`. Read the final outcome with
     `GetOutcome` and the same key. Every outcome comes in a successful response; only a
     call the server refused is a gRPC error.
   - Match errors on `ErrorDetail.reason`. The message text can change. The guide's
     "Errors and retries" table says which reasons to retry and how.
   - Read the world with `ListWorld`, then `WatchWorld` from the listed revision. On
     `CONSUMER_TOO_SLOW` or a dropped stream, watch again from the last revision you
     processed, on the `campaign_id` the list returned. On `REVISION_EXPIRED`, list
     again.
   - Store the last event sequence you processed and subscribe after it. A `Gap` event
     means events were lost: treat the span as incomplete.
   - Compare `Stamp.agent_boot` for equality only, and only together with
     `mission_generation`. An `agent_boot` of 0 matches nothing.
   - Code for missing event fields. DCS does not always send the same fields for one
     event type.
   - Moving groups between DCS and the campaign (`Materialize`, `Dematerialize`) is the
     controller's policy. The sidecar never does it on its own.
4. Test against the fake sidecar before a real server, when a checkout is present:
   `cargo run -p nanoprobe-fake-sidecar -- --listen 127.0.0.1:47311`
   (`docs/first-campaign.md`).

## Running nanoprobe on a server

Read the operator guide, then `docs/install.md` for the steps.

- Nothing goes into the DCS install. Install writes only into the Saved Games profile.
- Run `nanoprobe doctor` after every install, upgrade and DCS update.
- `nanoprobe-sidecar health --config sidecar.toml` exits 0 `healthy`, 1 `hung`,
  2 `gone`, 3 `disabled`, 4 no answer. nanoprobe never restarts DCS; the host does.
- Never read or copy `serverSettings.lua`, `network.vault`, token files, or
  `autoexec.cfg` beyond its two `net.*` keys.
- Uninstall deletes the profile's `nanoprobe/` folder, a mission folder there
  included. Move templates out first.
- Back up every campaign before an upgrade. A newer sidecar migrates the store, and
  there is no downgrade.

## Contributing to nanoprobe itself

Read `AGENTS.md` at the repository root. It holds the project's rules and the
contributor workflow.
