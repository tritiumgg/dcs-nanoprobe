# Eval channel

The eval channel runs a Lua file in a chosen Lua state of a running DCS and writes what
it returned as JSON. It is development tooling: `cargo xtask deploy --dev` installs it,
and a release never carries it. It is pure Lua, and depends on no other part of the
platform, so it works when the agent does not.

## Files

```
<profile>/Scripts/Hooks/nanoprobe-eval.lua   the eval hook
<profile>/nanoprobe/dev/lua/prelude.lua      the prelude the hook sends to each state
<profile>/nanoprobe/dev/lua/<name>.lua       libraries a snippet loads with E.lib
<profile>/nanoprobe/dev/eval/in/             requests
<profile>/nanoprobe/dev/eval/taken/          requests the hook has taken
<profile>/nanoprobe/dev/eval/out/            results
```

## Request

A request is a file `in/<id>.<state>.lua`. `<id>` is 1 to 64 characters of
`A-Z a-z 0-9 _ -`. The writer creates the file under another name and renames it into
place, so the hook never reads half a file.

`<state>` is one of:

| State | Where the file runs |
|---|---|
| `hook` | The hook's own state, where DCS runs the files in `Scripts/Hooks` |
| `gui`, `server`, `scripting`, `mission`, `export`, `config` | That state, through `net.dostring_in` |
| `sse` | The sandboxed mission scripting environment, through `a_do_script` in the `mission` state |
| `reset` | Nowhere. The hook runs every cleanup in every state that holds the prelude. The file's text is ignored. |
| `uninstall` | Nowhere. As `reset`, and then the hook removes the prelude from every state. |

Any other name is passed to `net.dostring_in` as it is, so a probe can record what an
unknown name does. `hook` and `gui` are kept apart because on a dedicated server a hook's
globals are not the `gui` state's (LIVE).

The hook looks in `in/` four times a second of real time and takes the request whose
name sorts first. It renames the file into `taken/` before it runs it. A request that
crashes DCS is therefore not run again at the next start. The hook runs one request at
a time.

## Result

The hook writes `out/<id>.json`, under another name first and then renamed into place.
It is one JSON object:

| Key | Type | Meaning |
|---|---|---|
| `id` | string | The request's ID |
| `state` | string | The request's state |
| `ok` | boolean | Whether the request reached the state and a reply came back. A snippet that raised still gives `true` here. |
| `error` | string | Present when `ok` is false: what went wrong between the hook and the state |
| `reply` | object or array | Present when `ok` is true: the state's reply, below. For `reset` and `uninstall`, an array with one cleanup report per state. |
| `hook` | object | `real_start`, `real_end` (seconds of DCS real time), `frames` (frames the request spanned), `gen` (mission generation), `paused`, `model` (model time at the end), `resumes` (how often the hook resumed the simulation for the request) |

The reply to a snippet:

| Key | Type | Meaning |
|---|---|---|
| `ok` | boolean | False when the snippet failed to compile, raised, or timed out in `E.wait` |
| `error` | string | Present when `ok` is false |
| `n` | integer | How many values the snippet returned |
| `values` | array | The returned values, `n` of them, encoded as below |
| `observations` | array | What the snippet recorded with `E.observe`, in order |
| `notes` | array of string | What the snippet recorded with `E.note` |

An observation is an object: `name`; `value`; `kind`, one of `exact`, `timing` or
`info`; `expect`, an object with an expected value per platform column (`windows`,
`wine`, `mock`), absent when the snippet gave none; `within`, the allowed absolute
difference for a number, when given; and `verdict`, an object with `match` or `differ`
per platform column that has an expected value. The `mock` column falls back to the
`wine` expectation and then the `windows` one, because the mock is built from a listing
taken under Wine.

A cleanup report: `state`; `cleaned`, an object counting what was removed by kind;
`errors`, an array of strings; `new_globals`, the names of globals that exist now and
did not when the prelude was installed; `handlers`, the change in the number of event
handlers since then, or null where the state has none.

## Encoding

Values are encoded with fixed limits so that no value can raise or run away:

- A table whose keys are exactly `1..n` is an array. Any other table is an object; a
  key that is not a string is written with `tostring`. Keys are written in sorted order.
- A table nested deeper than 8 levels is the string `<depth>`; a table already being
  written is `<cycle>`; after 2,000 entries a table ends with `<more>`.
- A function is `<function>`, a userdata `<userdata>`, a thread `<thread>`. No method is
  called on any of them.
- A number that is not finite is the string `<nan>`, `<inf>` or `<-inf>`.

## The snippet's interface

A snippet is a Lua chunk. It runs with its own global table that reads through to the
state's globals, so a global it assigns does not leak into the state. It receives one
argument, `E`, which is also the global `E`:

| Member | What it does |
|---|---|
| `E.state`, `E.id` | The request's state and ID |
| `E.observe(name, value, expect)` | Record an observation. `expect` may hold `windows`, `wine`, `mock`, `timing = true`, `within = n`, `info = true`. |
| `E.note(text)` | Record a line of text |
| `E.bench(fn, n, rounds)` | Call `fn` `n` times, `rounds` times over; return a table with `ns` (best time per call, loop cost removed), `raw_ns`, `n`, `clock` |
| `E.clock()` | Seconds from the best clock the state has, or nil |
| `E.track.global(name, value)` | Set a real global, removed at reset |
| `E.track.timer(fn, arg, time)` | `timer.scheduleFunction`, removed at reset. The function runs under `pcall`. |
| `E.track.handler(handler)` | `world.addEventHandler`, removed at reset. `onEvent` runs under `pcall`. |
| `E.track.defer(label, fn)` | Run `fn` at reset |
| `E.store` | A table that lasts until reset |
| `E.lib(name)` | Load `<profile>/nanoprobe/dev/lua/<name>.lua` once and return what it returns. Needs `io` in the state. |
| `E.wait(fn, seconds, 'running')` | Returned by the snippet to finish later: the hook calls `fn` every frame until it returns a value or `seconds` of real time pass. With `'running'`, the hook also resumes the simulation whenever it finds it paused. |
| `E.cross(state, source)` | `hook` only. Run `source` as a snippet in another state now, and return `true` and its first value as a string, or `false` and the error |
| `E.hook` | `hook` only. The hook's callback log: `E.hook.seq()`, `E.hook.since(seq)`, `E.hook.counts()` |

Cleanups run last in, first out, each under `pcall`. A timer registered before the
latest mission load is not removed, because the load cleared it and its ID may belong
to another timer since.

## Kill switch

The hook checks `<profile>/nanoprobe/disable` once a second and at start. Once it
exists, the hook takes no more requests until DCS restarts.

## Probes

A probe is a snippet in `lua/probes/` that records observations. Its first lines hold
its settings as comments:

```
-- states: hook server
-- timeout: 60
```

`states` lists the states to run it in, in order. `timeout` is how many seconds the
runner waits for each result; the default is 30. The runner takes the probes in name
order.
