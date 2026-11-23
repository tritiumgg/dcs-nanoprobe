# Error model

An error is reported where it is found, with a fixed reason a program can match and a
text a person can read. No layer turns a failure into a success, and no layer retries
what the layer below refused.

## Layers

| Layer | How it reports | Reference |
|---|---|---|
| Agent | A `result` code and a message in a `RESULT` record; a log record; flags and counters in the status block | spec/commands.md, spec/records.md, spec/transport.md |
| Sidecar | A command outcome in the store; a health verdict; a log record | below |
| Public API | A gRPC status code with an `ErrorDetail` (`proto/nanoprobe/v1alpha1/common.proto`) | below |

## Inside DCS

- A Lua error in a handler, an extension or a DCS call is caught by `pcall` and becomes
  a result: `LUA_ERROR` or `DCS_REFUSED`, with the error text as the message.
- A Rust panic is caught at the entry point. The agent logs it, sets the lifecycle
  state to `DISABLED`, and does no more work in this DCS process.
- A native exception is recorded in `crash.log` with the breadcrumb (spec/install.md).
  The status block keeps the breadcrumb for the command-line inspector to read after
  DCS dies.
- Bytes from the sidecar that fail a check are refused: a bad payload is the result
  `BAD_PAYLOAD`; a bad ring position detaches the agent (spec/transport.md).

## Command outcomes

The sidecar journals every command and resolves it to one outcome:

| Outcome | From | Meaning |
|---|---|---|
| `pending` | no result yet | Journalled, and not yet written or not yet answered |
| `applied` | result `APPLIED` or `AT_REVISION`; result `TOO_LARGE` for a set-state command or an effect | DCS is in the commanded state. After `TOO_LARGE` the command ran and the value it returned is lost; the message says so. |
| `refused` | result `DCS_REFUSED`, `NOT_BOUND` or `LUA_ERROR`; result `TOO_LARGE` for a query | The command reached DCS and failed, or a query's value does not fit a record. The message says why. |
| `rejected` | result `STALE_EPOCH`, `WRONG_CAMPAIGN`, `WRONG_GENERATION`, `NOT_READY`, `NO_CAMPAIGN`, `BAD_PAYLOAD`, `UNKNOWN_OPCODE` or `RAW_LUA_OFF` | The agent did not try it |
| `unknown` | no result, and the agent's `in_head` is past the command, or the command's `agent_boot` ended before it answered (spec/commands.md, "Unanswered commands") | The agent may have taken it; its result is lost |
| `not_sent` | a command the agent never took | It did not happen. An effect or a client's query is not sent again. For a set-state command, or a query the sidecar itself needs (the `CAPTURE` of a dematerialization), what DCS still needs is derived from the store and sent as a new command. |

`DUPLICATE` says only that the agent handled the sequence number before. The command's
outcome is the one journalled from its first result, or `unknown` when there is none.
`DUPLICATE` never becomes `applied`.

What the sidecar does with an outcome:

| Class | `refused` | `rejected` | `unknown` |
|---|---|---|---|
| set-state | A spawn becomes `unspawnable`. A ruin's spawn (spec/world.md, "Ruins") leaves the static `dead`: the error is kept as its last error until a spawn of the ruin applies, and the spawn is not sent again at that revision in that generation by the same sidecar process (a restarted sidecar tries once more). A destroy is sent again (destroying nothing is `APPLIED`) up to `destroy_retries` (spec/config.md); a dematerializing entity then goes `lost` with the error. Any other is reported and not sent again until its revision changes. | After `STALE_EPOCH`, `WRONG_CAMPAIGN`, `WRONG_GENERATION` or `NOT_READY`, what DCS still needs is derived from the store and sent under the current epoch. Others are reported. | What DCS needs is derived again from the store's desired state and sent as new commands; the old payload is never sent again. For a spawn, `AT_REVISION` then shows it was applied. A destroy is sent again up to `destroy_retries`, as for `refused`. A capture-and-destroy is not sent again, nor after `refused` or `TOO_LARGE`: the sidecar takes the last checkpointed state and sends a destroy. |
| effect | Reported | Reported | Reported. Never sent again. |
| query | Reported, and not sent again | Reported | Sent again under a new sequence number |

The sidecar builds no command whose payload is over an eighth of the inbound ring. A
call that would need one fails with `INVALID_FIELD`.

`BAD_PAYLOAD` and `UNKNOWN_OPCODE` mean the sidecar and the agent disagree, which the
layout hash should have caught. The sidecar logs them as errors.

## Public API

Every failed call returns a gRPC status whose details hold one `ErrorDetail`: a
`reason` from the table, a message, and the entity or field the error is about.

| gRPC code | Reason | When |
|---|---|---|
| `INVALID_ARGUMENT` | `INVALID_FIELD` | A field is missing, out of range or malformed |
| `INVALID_ARGUMENT` | `IDEMPOTENCY_MISMATCH` | An idempotency key was used before with a different request |
| `NOT_FOUND` | `ENTITY_NOT_FOUND`, `CAMPAIGN_NOT_FOUND`, `EXTENSION_NOT_FOUND`, `PLAYER_NOT_FOUND` | The named thing does not exist, or was removed and purged (spec/world.md, "Removal") |
| `NOT_FOUND` | `OUTCOME_NOT_FOUND` | `GetOutcome` names an idempotency key that is unknown or no longer remembered |
| `ALREADY_EXISTS` | `CAMPAIGN_EXISTS` | A campaign with that name exists |
| `FAILED_PRECONDITION` | `NO_ACTIVE_CAMPAIGN` | The call needs a campaign, names campaign 0, and no campaign is active |
| `FAILED_PRECONDITION` | `CAMPAIGN_NOT_ACTIVE` | The call acts on DCS (an effect, a capture, materialize) and names a campaign that is not the active one |
| `FAILED_PRECONDITION` | `CAMPAIGN_READ_ONLY` | The campaign is finished |
| `FAILED_PRECONDITION` | `NOT_CONTROLLER` | The call needs the controller lease and carries none, or carries a wrong lease ID |
| `FAILED_PRECONDITION` | `LEASE_HELD` | Another controller holds the lease |
| `FAILED_PRECONDITION` | `RAW_LUA_OFF` | The call needs raw Lua, which the agent has off |
| `FAILED_PRECONDITION` | `THEATER_NOT_LOADED` | The entity's theater is not the loaded one |
| `FAILED_PRECONDITION` | none (`UNSPECIFIED`), `field` `theater` | An activation, a `CreateCampaign` that activates, or a `Reload`, whose theater has no template (the message starts `no template for theater`), whose template does not parse (`the template for theater`), or whose shell does not render (`the shell`). Nothing is created, suspended, captured, opened or sent. A shell that renders but cannot be saved (`could not be written`) is the one refusal that comes after the capture: the groups are captured, and nothing is suspended, opened or sent. A `SetNextMission` or `SetCampaignTemplate` that lands during a `Reload`'s capture can refuse the reload the same way |
| `FAILED_PRECONDITION` | none (`UNSPECIFIED`) | A `Reload` while a mission load is owed. Nothing is captured |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `next.dynamic_spawn` | An activation, a `CreateCampaign` that activates, or a `Reload`, whose stored next-mission settings name an airport that is not in the `warehouses` of the theater's template. Nothing is created, suspended, captured, opened or sent |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `next.theater` | A `SetNextMission` whose theater is not one of the campaign's |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `theater` | A `SetCampaignTemplate` whose theater is not one of the campaign's. Nothing is changed |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `path` | A `SetCampaignTemplate` whose path is not absolute for the sidecar's system (the message has `is not an absolute path`); does not resolve to a file in a folder that `[missions] template_dirs` lists, which covers a missing file, a folder, a path outside the folders and an empty list alike (`is not a file in a folder`; the sidecar's log has the cause); cannot be read (`could not be read`); or names a file over 16 MiB (`at most`), one whose `theatre` member is not the theater, one whose `mission` or `warehouses` does not parse, or one that places a platform name. Nothing is changed: the campaign keeps the template it had |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `path` | A `PutGroupFromTemplate` whose path fails the path rules of `SetCampaignTemplate`, with the same messages; names a file over 1 MiB (`a group template file is at most`); or names a file that is not a group template file: one the reader refuses, with the line (`the file is not a group template file: line`), which covers every statement but one assignment and every value that is not a literal, or one that does not assign a table to `templates` (`does not assign`). No group is made |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `template` | A `PutGroupFromTemplate` whose template name is empty (`a template name is required`) or is not in the file (`has no template named`); or whose template is not a table (`is not a table`), has a `type` other than `"vehicle"` (`does not have the type`), has no units or units that are not tables numbered from 1 (`has no units`, `are not tables numbered`), has a unit with no `name` (`has no` `name`), has a unit whose `dx`, `dy` or `heading` is not a finite number (`is not a finite number`), has no country number while the request gives none (`has no country number`), puts a unit at a place that is not finite (`not at a finite place`), or makes a command over the agent's limit (`the agent takes`). No group is made |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `anchor`, `anchor.theater`, `anchor.north`, `anchor.east`, `anchor.alt` or `heading` | A `PutGroupFromTemplate` with no anchor, an anchor that names no theater or one the campaign does not use, or a number that is not finite. These are checked before the file's refusal is given. No group is made. A refused `order` is refused as `PutGroup` refuses it, with the same reason and field, and makes no group either |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `path` | A `PutFromStaticTemplate` whose path fails the path rules of `SetCampaignTemplate`, with the same messages; names a file over 1 MiB (`a static template file is at most`); or names a file that is not a static template file (`the file is not a static template file:`): one the reader refuses, with the line; one that does not assign a table to `staticTemplate` (`does not assign`); with no `coalition` table; with no vehicle group and no static (`holds no vehicle group and no static`); holding a plane, helicopter, ship or complex group (`only vehicle groups and statics are built`); with a country `id` that is not a DCS country number (`has no` `id`); a list with two entries under one number, or a coalition or country with two entries under one key (`has two entries`); a group with no units; a unit with no `type`; an `x`, `y` or `heading` that is not a finite number; a linked static (`a linked static is not built`); a heliport or grass airfield (`is a heliport or a grass airfield`); a static value the store cannot encode, such as a table nested too deep (`cannot be stored`). Each of these names where in the file it is, when it is about one entry. Also: without an anchor, a file with no `theatre` (`has no` `theatre`) or with a theater the campaign does not use (`the campaign does not use the theater`); with or without one, a place that is not finite (`is not at a finite place`) and a spawn command over the agent's limit (`the agent takes`). No entity is made |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `anchor`, `anchor.theater`, `anchor.north`, `anchor.east`, `anchor.alt` or `heading` | A `PutFromStaticTemplate` whose anchor names no theater or one the campaign does not use, or with a number that is not finite. Checked before the file's refusal is given. No entity is made |
| `INTERNAL` | `INTERNAL` | A `PutFromStaticTemplate` that needs more entity IDs than the campaign has left (`the campaign has used every entity ID`), as `PutGroup` gives it. No entity is made |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `next.start` | A `SetNextMission` whose start is not a date from year 1 to year 9999 |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `clock` | A `CreateCampaign` whose `clock` is not a date from year 1 to year 9999. Nothing is created |
| `FAILED_PRECONDITION` | none (`UNSPECIFIED`), `field` `campaign_id` | A `DeleteCampaign` of the active campaign or of one with a backup running |
| `INVALID_ARGUMENT` | `INVALID_FIELD`, `field` `confirm_name` | A `DeleteCampaign` whose `confirm_name` is not the campaign's name |
| `INTERNAL` | `INTERNAL` | A `DeleteCampaign` whose folder could not be removed; the campaign stays listed with what is left |
| `FAILED_PRECONDITION` | `UNMANAGED_ENTITY` | The call changes a spec or order of an unmanaged entity |
| `FAILED_PRECONDITION` | `CONFIRMATION_REQUIRED` | A delete without its confirmation field |
| `ABORTED` | `LEASE_LOST` | The caller's lease epoch is not the current one |
| `RESOURCE_EXHAUSTED` | `QUEUE_FULL` | The command queue to DCS is full |
| `RESOURCE_EXHAUSTED` | `CONSUMER_TOO_SLOW` | A stream's consumer fell behind; the stream ends |
| `UNAVAILABLE` | `DCS_NOT_READY` | An effect or a query needs a running DCS, and none is attached, the lifecycle state is `LOADING`, or the verdict is `hung` |
| `UNAUTHENTICATED` | `TOKEN_REQUIRED` | A network listener got no valid token |
| `PERMISSION_DENIED` | `ROLE_REQUIRED` | The token's role does not allow the call |
| `OUT_OF_RANGE` | `SEQUENCE_EXPIRED` | An event sequence number is older than the retained history |
| `OUT_OF_RANGE` | `REVISION_EXPIRED` | A watch starts at a revision older than the changes the store keeps. The consumer lists again. |
| `UNAVAILABLE` | none (`UNSPECIFIED`) | The server is stopping. A stream ends with it, and a changing call that arrives is refused with it; the client connects again and resumes from its last revision or sequence number |
| `UNIMPLEMENTED` | `UNVERIFIED` | A call path not yet verified on DCS; permanent until a release verifies it. The detail's `field` names the field |
| `UNIMPLEMENTED` | none (`UNSPECIFIED`) | The call is in the schema and this server does not build it yet. The detail's `field` names the method |
| `RESOURCE_EXHAUSTED` | `SPACE_LOW` | The store's file system has less free space than the campaign holds there (its store, the store's WAL and its events file), plus a template being set: a backup, a read copy and `SetCampaignTemplate` are refused. A store write that fails still stops the sidecar |
| `INTERNAL` | `INTERNAL` | A fault in the sidecar |

A set-state call succeeds when the desired state is journalled. It does not wait for
DCS, and it succeeds while DCS is absent. What DCS then does is reported through the
World service: every entity the controller sets shows its spec revision, the revision
DCS has applied, and the last error; a group or static also shows its lifecycle
state.

Every command result in a response carries the agent's `result` code
(spec/commands.md) beside the outcome, so a program can tell `WRONG_GENERATION` from
`RAW_LUA_OFF` without reading the message.

An effect or a query call returns once the agent's result arrives, with the outcome in
the response. If none arrives within `result_wait_s` (spec/config.md) the call returns
with the outcome `pending`; the caller reads the final outcome and the value the call
would have returned later, with the call's idempotency key (`GetOutcome`). A repeat of
the call after that returns the current outcome too, not the `pending` one. `QUEUE_FULL` is for these calls only: a set-state call
changes the store and is never queued. `refused`, `unknown` and `not_sent` are outcomes in a successful
response, not gRPC errors: the call was valid and the platform did what it could.

Every changing call names its campaign. Campaign 0 is resolved when the call arrives:
to the active campaign, or to none when no campaign is active. A call that needs a
campaign then fails with `NO_ACTIVE_CAMPAIGN`. A call that does not (a message, a
sound, a chat line, a reload, and the Session, Mission and Campaign calls) goes ahead
with no campaign, which is how a server is run between campaigns. The World service
needs a campaign: while none is active the sidecar's mirror of DCS is not served. The
Events service then serves the server log. A stream that names
campaign 0 is tied to the campaign active when it starts, and ends with
`CAMPAIGN_NOT_ACTIVE` when the active campaign changes.

A spec or an order may be written to a suspended campaign, which is how a new campaign
gets its first specs before it is activated; nothing is sent to DCS for it.

Every changing call carries an idempotency key; the lease calls are the exception. An
empty key is `INVALID_FIELD`. A repeated call with the same key and the same request,
the lease left out of the comparison, returns the first call's response and does
nothing else; while the first call is still waiting, the repeat waits with it. A key
is remembered for `idempotency_keep_h` hours (spec/config.md): a call that resolved to a
campaign in that campaign's store, written with its change, and in the server store's
index of keys; a call on no campaign in the server store (spec/persistence.md,
"Idempotency keys"). A key is therefore one key on the whole server: a repeat sent
after the active campaign changed still finds the first call.
After the time passes a repeat is a new call. A call that fails is not remembered: a
repeat of it after the cause is fixed, such as a new lease or an active campaign, runs
as a new call.

`GetOutcome` reads a remembered call by its key and returns the campaign the call acted
on. For an effect or a query it also returns the outcome and the value. A set-state call
has no outcome of its own, so for one `result` is absent: what DCS did with it shows in
the World service as the entity's applied revisions and last error. Repeating a
set-state call with the same key returns its first response, revisions included. A key
that is unknown or no longer remembered is `OUTCOME_NOT_FOUND`.

A subscription that asks for positions faster than the cap gets them at the cap; that
is not an error. A stream that falls behind is ended with `CONSUMER_TOO_SLOW`, and the
consumer lists or subscribes again from its last revision or sequence number.

### Roles

A loopback listener with no `tokens_path` gives every caller the `admin` role. With
tokens, a caller sends `authorization: Bearer <token>` as gRPC metadata.

| Role | May call |
|---|---|
| `observer` | Every call that changes nothing: Session `Hello`; the World, Events and Players lists and streams; `GetServerStatus`; `ListLua`; campaign list, get and export; `GetData` |
| `controller` | The above, the lease calls, and every changing call except those below |
| `admin` | The above, `EvalLua`, `RegisterLua`, `RemoveLua`, `DeleteCampaign`, `ErasePlayer`, and every Moderation call (`Kick`, `Ban`, `Unban`, `ListBans`, `MovePlayer`) |

`GetOutcome` needs the role of the call whose outcome it reads.

## Health

The sidecar publishes one verdict about DCS, with the time since the last heartbeat.
It cannot see the DCS process: it judges from the mapped file, which it reads whether
or not it is attached, and from its own handshake attempts. The first row that
matches decides:

| # | When | Verdict |
|---|---|---|
| 1 | The file is ready, its lifecycle state is `DISABLED`, and a TCP connect to the handshake port is not refused | `disabled` |
| 2 | The heartbeat advanced within `heartbeat_stale_s` | `healthy` |
| 3 | The lifecycle state is `LOADING` and the heartbeat advanced within `loading_limit_s` | `healthy` |
| 4 | No connect to the handshake port tried in this stall has finished, and no session is attached | the verdict before; `gone` before the sidecar's first verdict |
| 5 | A TCP connect to the handshake port is refused | `gone` |
| 6 | Otherwise | `hung` |

- A stall begins when the sidecar reads a heartbeat that advanced, or finds a session
  attached, and lasts until the next such read. Rows 1, 5 and 6 read the last connect
  tried in the current stall that finished; row 4 holds while there is none.
- A connect finishes when it is refused, accepted, or reaches the probe's timeout. One
  that reaches the timeout counts as not refused. A try that fails in any other way,
  or whose connect was never made, is no connect; the sidecar tries again a second
  later and logs repeated failures.
- The probe's timeout is 100 ms on Unix and 5 s on Windows (`REFUSAL_WAIT` in the
  sidecar), whatever `handshake_timeout_s` says. Windows reports a refusal only after
  about 2 s (PRIOR).
- The verdict can read `healthy` while the heartbeat's age is past
  `heartbeat_stale_s`, until a connect of the new stall finishes. Operators act on the
  verdict, not on the heartbeat's age.
- `heartbeat_stale_s` applies in every lifecycle state but `LOADING`. No frame runs
  during a load, so the heartbeat stands still and the agent answers no handshake;
  only `loading_limit_s` bounds a load.
- Being attached decides no verdict of its own; it only keeps row 4 from holding the
  verdict before. A DCS whose heartbeat advances is
  `healthy` even when the sidecar cannot attach to it. The status reports that apart:
  whether the sidecar is attached, and why the last attach failed: one of the agent's
  reasons (`version`, `token`, `disabled`, `no_file`) or one of the sidecar's own
  (`layout_hash`, `boot_mismatch`, `bad_file`, `no_answer`, `no_listener`).
- A disabled agent does no work and stores no heartbeat, and its process still holds
  the listening socket. So `disabled` needs that socket: once the process is gone the
  file still says `DISABLED`, the connect is refused, and the verdict is `gone`.
- `gone` means nothing listens: no DCS process holds a running agent. A DCS whose
  agent never ran, because the kill switch was there at start, has no fresh heartbeat
  and no listener, and is also `gone`.
- An agent that could not open its socket still runs: its heartbeat advances and its
  flags say `NO_HANDSHAKE`. DCS is `healthy`, and the status reports that the sidecar
  cannot attach, with the reason `no_listener`.
- A DCS that crashed stays alive in its crash-report window and runs no frames (LIVE,
  docs/results/2026-11-11-first-dll-and-spikes.md, "Crash capture"). That its
  handshake port still accepts a connect, which makes it `hung` and not `gone`, is
  UNVERIFIED until step S19 (E22).

While the verdict is `hung`, the status reports the state `SIM_STATE_HUNG`, and while it
is `gone`, `SIM_STATE_GONE`. When the verdict enters `hung`, the sidecar publishes a
lifecycle event with `SIM_STATE_HUNG`; when it leaves, one with the state the status
then reports. The event goes to the active campaign's history, or to the server log.

The sidecar takes no action on a verdict. Starting, stopping and restarting DCS belong
to the host. The health command's exit code is 0 for `healthy` and differs for each
other verdict (spec/config.md).

The status also reports the free bytes on the store's file system, and whether they
are below what the active campaign holds there (spec/persistence.md, "Free space").
Space is not part of the verdict or of the health command's exit code: a full disk must
not make a watchdog restart DCS. Nor are scheduled backups: the status reports the last
one that succeeded, the last one that failed and why (`last_backup`,
`last_backup_failure`, `backup_error`; spec/persistence.md, "Backups").
