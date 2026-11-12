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
  The status block keeps the breadcrumb for the sidecar to read after DCS dies.
- Bytes from the sidecar that fail a check are refused: a bad payload is the result
  `BAD_PAYLOAD`; a bad ring position detaches the agent (spec/transport.md).

## Command outcomes

The sidecar journals every command and resolves it to one outcome:

| Outcome | From | Meaning |
|---|---|---|
| `pending` | no result yet | Journalled, and not yet written or not yet answered |
| `applied` | result `APPLIED` or `AT_REVISION`; result `TOO_LARGE` for a set-state command or an effect | DCS is in the commanded state. After `TOO_LARGE` the command ran and the value it returned is lost; the message says so. |
| `refused` | result `DCS_REFUSED`, `NOT_BOUND` or `LUA_ERROR`; result `TOO_LARGE` for a query | The command reached DCS and failed, or a query's value does not fit a record. The message says why. |
| `rejected` | result `STALE_EPOCH`, `WRONG_CAMPAIGN`, `WRONG_GENERATION`, `NO_CAMPAIGN`, `BAD_PAYLOAD`, `UNKNOWN_OPCODE` or `RAW_LUA_OFF` | The agent did not try it |
| `unknown` | no result, and the agent's `in_head` is past the command (spec/commands.md, "Unanswered commands") | The agent took it, and its result was lost |
| `not_sent` | an effect the agent never took | It did not happen and is not sent again |

`DUPLICATE` says only that the agent handled the sequence number before. The command's
outcome is the one journalled from its first result, or `unknown` when there is none.
`DUPLICATE` never becomes `applied`.

What the sidecar does with an outcome:

| Class | `refused` | `rejected` | `unknown` |
|---|---|---|---|
| set-state | A spawn becomes `unspawnable`; any other is reported and not sent again until its revision changes | After `STALE_EPOCH`, `WRONG_CAMPAIGN` or `WRONG_GENERATION`, what DCS still needs is derived from the store and sent under the current epoch. Others are reported. | What DCS needs is derived again from the store's desired state and sent as new commands; the old payload is never sent again. For a spawn, `AT_REVISION` then shows it was applied. A capture-and-destroy is not sent again, nor after `TOO_LARGE`: the sidecar takes the last checkpointed state and sends a destroy. |
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
| `NOT_FOUND` | `ENTITY_NOT_FOUND`, `CAMPAIGN_NOT_FOUND`, `EXTENSION_NOT_FOUND`, `PLAYER_NOT_FOUND` | The named thing does not exist |
| `NOT_FOUND` | `OUTCOME_NOT_FOUND` | `GetOutcome` names an idempotency key that is unknown or no longer remembered |
| `ALREADY_EXISTS` | `CAMPAIGN_EXISTS` | A campaign with that name exists |
| `FAILED_PRECONDITION` | `NO_ACTIVE_CAMPAIGN` | The call needs a campaign, names campaign 0, and no campaign is active |
| `FAILED_PRECONDITION` | `CAMPAIGN_NOT_ACTIVE` | The call acts on DCS (an effect, a capture, materialize) and names a campaign that is not the active one |
| `FAILED_PRECONDITION` | `CAMPAIGN_READ_ONLY` | The campaign is finished |
| `FAILED_PRECONDITION` | `NOT_CONTROLLER` | The call needs the controller lease and carries none, or carries a wrong lease ID |
| `FAILED_PRECONDITION` | `LEASE_HELD` | Another controller holds the lease |
| `FAILED_PRECONDITION` | `RAW_LUA_OFF` | The call needs raw Lua, which the agent has off |
| `FAILED_PRECONDITION` | `THEATER_NOT_LOADED` | The entity's theater is not the loaded one |
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
is remembered in the server-scoped store, with the campaign the call resolved to, for
`idempotency_keep_h` hours (spec/config.md). A key is therefore one key on the whole
server: a repeat sent after the active campaign changed still finds the first call.
After the time passes a repeat is a new call.

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
| `admin` | The above, `EvalLua`, `RegisterLua`, `RemoveLua`, `DeleteCampaign` and `ErasePlayer` |

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
| 4 | A TCP connect to the handshake port is refused | `gone` |
| 5 | Otherwise | `hung` |

- `heartbeat_stale_s` applies in every lifecycle state but `LOADING`. No frame runs
  during a load, so the heartbeat stands still and the agent answers no handshake;
  only `loading_limit_s` bounds a load.
- Being attached is not part of the verdict. A DCS whose heartbeat advances is
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

The sidecar takes no action on a verdict. Starting, stopping and restarting DCS belong
to the host. The health command's exit code is 0 for `healthy` and differs for each
other verdict (spec/config.md).
