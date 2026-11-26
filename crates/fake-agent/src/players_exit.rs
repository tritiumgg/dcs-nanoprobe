//! The S16 live test (players, moderation, menus, marks and extensions), run
//! by `command-probe --part players`: a sidecar over the real link
//! ([`LinkSim`]) that sends each command as the sidecar's wire body
//! ([`Body`]) and reads the agent's `RESULT`s and records. It has no store
//! and no reconciler: a command is a header and a body, so each step names the
//! one command it tests. Test tooling only: built from source, never
//! released.
//!
//! The probe needs no person. What only DCS itself can do or show is the
//! caller's: the probe prints `{"ev":"phase","name":N}` and waits for a
//! file `N`-named in `--control-dir`, whose text is the caller's answer. Every
//! step prints `{"ev":"step","name":..,"verdict":"pass|fail|skip|note",
//! "detail":..}`. A `fail` ends the run after the probe removes what it
//! added. The last line is `{"ev":"verdict","pass":B,...}`, and the exit status
//! is 0 on a pass. No ucid is printed or written: a player is shown by the
//! first eight hex digits of a keyed hash.
//!
//! # `--scenario functional`
//!
//! `--raw-lua on|off` says what the agent's `agent.toml` is expected to hold;
//! the probe reads the mode from the `HELLO`'s `RAW_LUA` flag and fails when
//! they differ. `--mission-path` is the loaded mission's path as DCS opens it
//! (for `MISSION_LOAD`, with raw Lua on). Steps, in order:
//!
//! 1. `link`: a `HELLO`, the mode, a campaign and a mission running.
//! 2. `menu_set`: `MENU_SET` of a submenu and a command item in it.
//!    Phase `menus_set`. The caller selects the command in the server state
//!    (`missionCommands.doAction(index)`, trying indexes) and answers
//!    `menus_picked`: `index=N`, or `unreachable: <why>`. `menu_select`: a `MENU`
//!    record with the command's `EntityId` (`skip` when unreachable).
//!    `menu_remove`: both items removed, command first.
//! 3. With raw Lua on, `extension_set`: an extension with `on_event`,
//!    `on_frame`, `invoke` and one `emit`. With it off, `extension_gate`:
//!    `EXTENSION_SET` answers `RAW_LUA_OFF`.
//! 4. `mark_set`: `MARK_SET` and `DRAWING_SET`. Phase `marks_set`; the
//!    caller reads `world.getMarkPanels()` in the server state and answers
//!    `marks_read` with the read of `exit-marks-read.lua`. `marks_present`: the
//!    mark's text is in the read (the drawing is judged apart, and a
//!    drawing `getMarkPanels` does not list is a `note`). `mark_remove`, phase
//!    `marks_removed`, answer `marks_gone`: `marks_gone`: no mark of the shim is left.
//! 5. `message_all`, `message_coalition`, `chat_all`: applied.
//! 6. With raw Lua on: `extension_invoke` (the extension's `on_frame` and
//!    `on_event` ran, the marks' events reached it), `extension_emit` (an
//!    `AUTHOR_EVENT` from `emit`), `extension_record` (an `EXTENSION`
//!    record with `calls` above 0), then `mission_load`: `MISSION_LOAD` of
//!    `--mission-path`, `LOADING`, then `RUNNING` or `PAUSED` with a new
//!    generation (phases `mission_load_sent`, `loaded`; the caller holds the
//!    simulation running after `loaded`), `running_after_load`,
//!    `extension_after_load` (the chunk ran again: `invoke`'s own counter
//!    restarted), `extension_frames_after_load`, `extension_record_after_load`,
//!    `extension_emit_after_load`, and `extension_remove` (and an
//!    invoke after it answers `LUA_ERROR`).
//! 7. `eval_gate`: with raw Lua off `EVAL` answers `RAW_LUA_OFF`; with it on it
//!    returns the value 42.
//!
//! # `--scenario moderation`
//!
//! A scripted client (`cargo xtask commands <server> --part moderation`)
//! joins, takes a slot and spawns, and the probe moderates it. The probe asks
//! the caller for each client action with a phase and a control file; the
//! steps are `players_query`, `connect`, `slot_record`, `spawned`,
//! `slot_to_spectators`, `e35_slot_race` (parts below), `kick`,
//! `reconnect_after_kick`, `ban`, `ban_refused`, `bans_lists_client`, `unban`,
//! `bans_forgot_client`, `rejoin_after_unban`, `leave`. The first failure
//! ends the run, the probe unbans the client if it banned it, and the phase
//! `failed` tells the caller to disconnect the client.
//!
//! E35: when the `SLOT` record that puts the client in a slot arrives, the
//! probe sends `MODERATE SLOT` to spectators at once. It prints the times, in
//! milliseconds from the record being read: `queued` (the command is in the
//! link's queue), `written` (the link wrote it), `result` (its `RESULT`), and
//! `slot` (the next `SLOT` record that shows spectators), with the `EVENT`
//! ids that came in the 10 s after the record and whether a `BIRTH` was among
//! them (the aircraft spawned).
//!
//! # `--scenario moves`
//!
//! Slot moves on the final agent (spec/commands.md, "Slot moves"), with a
//! scripted client (`cargo xtask commands <server> --part moves`) that joins
//! and holds no slot. The caller's `joined` answer is a blue slot ID the
//! mission lists. Each step sends one `MODERATE SLOT` and reads the client's
//! side and slot from `PLAYERS` until they match: `blue_no_slot`,
//! `blue_slot`, `keep_leaves_slot`, `other_coalition_refused`,
//! `missing_slot_refused`, `red_no_slot`, `keep_takes_slot_coalition` (from
//! red), `keep_race` and `spectators`. Each step prints the handler's `cost_us`. In `keep_race`
//! the caller makes the client change to red itself (`race_go`); the probe
//! sends `KEEP` with no slot every 100 ms around the change and checks the
//! client ends on red, where a side read before the change would have put it
//! back on blue.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nanoprobe_sidecar::link::Config;
use nanoprobe_sidecar::players::Keyring;
use nanoprobe_sidecar::sim::wire::{Body, Command as WireCommand, Input, moderation};
use nanoprobe_sidecar::sim::{RawRecord, SimAdapter};
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{flags, kind, lifecycle, result};
use nanoprobe_transport::records::{CommandHeader, CommandResult};
use serde_json::{Value, json};

use crate::linksim::LinkSim;
use crate::{arg, json_string, read_token};

/// `scope` codes (spec/commands.md, "Codes: scope").
const ALL: u32 = 1;
const COALITION: u32 = 2;
/// `player_event` codes (spec/records.md).
const CONNECT: u32 = 1;
const DISCONNECT: u32 = 2;
const SLOT: u32 = 3;
/// DCS's `S_EVENT_BIRTH`.
const BIRTH: u32 = 15;
/// Entity IDs the probe gives what it adds.
const SUBMENU: u64 = 0x5316_0001;
const COMMAND: u64 = 0x5316_0002;
const MARK: u64 = 0x5316_0003;
const DRAWING: u64 = 0x5316_0004;
/// The DCS mark IDs the probe gives its mark and drawing: the first of the
/// mark range and of drawing band 255, order 0 (spec/world.md, "DCS IDs").
const MARK_DCS_ID: u64 = 1 << 30;
const DRAWING_DCS_ID: u64 = (1 << 30) + (1 << 24) + 255 * (1 << 20);
/// The extension's name and the event it emits.
const EXTENSION: &str = "np_s16_ext";
const EMIT_NAME: &str = "np.s16.test";
/// The mark's text.
const MARK_TEXT: &str = "np-s16-mark";
/// How long a ban lasts, seconds: a ban the test cannot lift ends by itself.
const BAN_S: f64 = 180.0;

/// The extension: counts what its hooks see, in locals the chunk makes, so a
/// chunk that runs again after a load starts every count at 0.
const EXTENSION_SOURCE: &str = "local frames, events, invokes = 0, 0, 0\n\
    return {\n\
      on_event = function(e) events = events + 1 end,\n\
      on_frame = function(t) frames = frames + 1 end,\n\
      invoke = function(what)\n\
        invokes = invokes + 1\n\
        if what == 'emit' then\n\
          local ok, why = emit('np.s16.test', { n = invokes }, nil)\n\
          return 'emit=' .. tostring(ok) .. ' ' .. tostring(why or '')\n\
        end\n\
        return string.format('frames=%d;events=%d;invokes=%d', frames, events, invokes)\n\
      end,\n\
    }\n";

fn num<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> Result<T, String> {
    arg(args, flag).map_or(Ok(default), |v| {
        v.parse().map_err(|_| format!("{flag} takes a number"))
    })
}

/// The name of a `result` code.
fn code_name(code: u16) -> String {
    result::ALL
        .iter()
        .find(|(_, c)| *c == u64::from(code))
        .map_or_else(|| code.to_string(), |(n, _)| (*n).to_owned())
}

/// A table field of an `lv` value.
fn lv_field<'a>(v: &'a Lv, key: &str) -> Option<&'a Lv> {
    let Lv::Table(pairs) = v else { return None };
    pairs.iter().find_map(|(k, x)| match k {
        Lv::String(s) if s == key.as_bytes() => Some(x),
        _ => None,
    })
}

/// A string `lv` value as text.
fn lv_text(v: &Lv) -> Option<String> {
    match v {
        Lv::String(s) => Some(String::from_utf8_lossy(s).into_owned()),
        _ => None,
    }
}

/// An `lv` array's items.
fn lv_items(v: &Lv) -> Vec<&Lv> {
    match v {
        Lv::Table(pairs) => pairs.iter().map(|(_, x)| x).collect(),
        _ => Vec::new(),
    }
}

fn lv_array(items: Vec<Lv>) -> Lv {
    Lv::Table(
        items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Lv::Number((i + 1) as f64), v))
            .collect(),
    )
}

fn lv_object(fields: Vec<(&str, Lv)>) -> Lv {
    Lv::Table(
        fields
            .into_iter()
            .map(|(k, v)| (Lv::String(k.as_bytes().to_vec()), v))
            .collect(),
    )
}

fn lv_str(s: &str) -> Lv {
    Lv::String(s.as_bytes().to_vec())
}

/// A point table.
fn point(x: f64, y: f64, z: f64) -> Lv {
    lv_object(vec![
        ("x", Lv::Number(x)),
        ("y", Lv::Number(y)),
        ("z", Lv::Number(z)),
    ])
}

/// A value as short JSON for a step's detail; a table is cut at a depth.
pub fn lv_json(v: &Lv, depth: usize) -> String {
    match v {
        Lv::Nil => "null".into(),
        Lv::Bool(b) => b.to_string(),
        Lv::Number(n) if n.is_finite() => n.to_string(),
        Lv::Number(_) => "null".into(),
        Lv::String(s) => json_string(&String::from_utf8_lossy(s)),
        Lv::Other(t) => json_string(&format!("<{t}>")),
        Lv::Table(_) if depth >= 3 => "\"{...}\"".into(),
        Lv::Table(pairs) => {
            let parts: Vec<String> = pairs
                .iter()
                .map(|(k, x)| format!("{}:{}", lv_json(k, depth + 1), lv_json(x, depth + 1)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// What `exit-marks-read.lua` returns, parsed: `count|id=text,...|panels=N`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarksRead {
    /// Panels the shim's mark IDs (2^30 and up) cover.
    pub count: usize,
    /// Their texts.
    pub texts: Vec<String>,
    /// Every panel DCS listed.
    pub panels: usize,
}

/// Parse a read of the mark panels.
pub fn parse_marks_read(text: &str) -> Option<MarksRead> {
    let mut parts = text.trim().splitn(3, '|');
    let count = parts.next()?.parse().ok()?;
    let list = parts.next()?;
    let panels = parts.next()?.strip_prefix("panels=")?.parse().ok()?;
    let texts = list
        .split(',')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').map_or(p, |(_, t)| t).to_owned())
        .collect();
    Some(MarksRead {
        count,
        texts,
        panels,
    })
}

/// What the extension's `invoke` returns for `status`, parsed:
/// `(frames, events, invokes)`.
pub fn parse_status(text: &str) -> Option<(u64, u64, u64)> {
    let field = |key: &str| {
        text.split(';')
            .find_map(|p| p.strip_prefix(key))
            .and_then(|v| v.trim().parse().ok())
    };
    Some((field("frames=")?, field("events=")?, field("invokes=")?))
}

/// A player as the probe holds it. It holds the ucid, which is never printed.
#[derive(Clone)]
struct Seen {
    at: Instant,
    event: u32,
    id: u32,
    side: u32,
    slot: String,
    ucid: String,
    name: String,
    text_len: usize,
}

/// A client's identity, for a command that names it.
#[derive(Clone)]
struct Who {
    id: u32,
    ucid: String,
}

struct Opts {
    scenario: String,
    campaign: u64,
    raw: Option<bool>,
    mission_path: Option<String>,
    seconds: u64,
    control: PathBuf,
}

fn opts(args: &[String]) -> Result<Opts, String> {
    let raw = match arg(args, "--raw-lua").as_deref() {
        None => None,
        Some("on") => Some(true),
        Some("off") => Some(false),
        Some(other) => return Err(format!("--raw-lua is on or off, not {other}")),
    };
    let o = Opts {
        scenario: arg(args, "--scenario").unwrap_or_else(|| "functional".into()),
        campaign: num(args, "--campaign", 16u64)?,
        raw,
        mission_path: arg(args, "--mission-path"),
        seconds: num(args, "--seconds", 900u64)?,
        control: arg(args, "--control-dir")
            .map(PathBuf::from)
            .ok_or("--control-dir is required")?,
    };
    if o.scenario == "functional" && o.raw.is_none() {
        return Err("--scenario functional needs --raw-lua on|off".into());
    }
    Ok(o)
}

/// The sidecar side: the link, the commands' numbers and what the agent wrote.
struct Rig {
    sim: LinkSim,
    o: Opts,
    deadline: Instant,
    keys: Keyring,
    epoch: Option<u64>,
    hello_flags: Option<u32>,
    next_seq: u64,
    generation: u32,
    life: u32,
    life_log: Vec<(Instant, u32, u32)>,
    results: HashMap<u64, CommandResult>,
    menus: Vec<u64>,
    players: Vec<Seen>,
    authors: Vec<(Instant, String)>,
    extensions: Vec<(Instant, String, u64, u64, u32)>,
    events: Vec<(Instant, u32)>,
    steps: Vec<Value>,
    /// What to remove when the run ends or fails: `(entity, body)`.
    cleanup: Vec<(u64, Body)>,
    /// The ucid of a client the probe banned and has not unbanned.
    banned: Option<String>,
}

impl Rig {
    fn pump(&mut self) {
        for r in self.sim.read() {
            self.take(&r);
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    fn take(&mut self, r: &RawRecord) {
        if u64::from(r.kind) == kind::LOG {
            return;
        }
        let now = Instant::now();
        let Ok(input) = Input::decode(r.kind, &r.payload) else {
            return;
        };
        match input {
            Input::Hello(h) => {
                if self.epoch.is_none() {
                    self.epoch = Some(h.epoch + 1);
                    self.hello_flags = Some(h.flags);
                }
            }
            Input::Frame(f) if f.replay == 0 => {
                self.generation = f.mission_gen;
                self.note_life(now, f.lifecycle, f.mission_gen);
            }
            Input::Lifecycle(l) => {
                self.generation = l.mission_gen;
                self.note_life(now, l.state, l.mission_gen);
            }
            Input::Result(c) => {
                self.results.insert(c.seq, c);
            }
            Input::Menu(m) => {
                println!(
                    "{}",
                    json!({"ev": "menu", "entity": m.entity, "scope": m.scope, "player": m.player})
                );
                self.menus.push(m.entity);
            }
            Input::AuthorEvent(a) => self.authors.push((now, a.name)),
            Input::Extension(e) => self
                .extensions
                .push((now, e.name, e.calls, e.total_us, e.stopped)),
            Input::Event(e) => self.events.push((now, e.id)),
            Input::Player(p) => {
                let seen = Seen {
                    at: now,
                    event: p.event,
                    id: p.player,
                    side: p.side,
                    slot: p.slot,
                    ucid: p.client,
                    name: p.name,
                    text_len: p.text.len(),
                };
                let line = json!({
                    "ev": "player", "event": seen.event, "id": seen.id,
                    "side": seen.side, "slot": seen.slot,
                    "player": self.tag(&seen.ucid), "text_len": seen.text_len,
                });
                println!("{line}");
                self.players.push(seen);
            }
            _ => {}
        }
    }

    fn note_life(&mut self, now: Instant, state: u32, generation: u32) {
        if self
            .life_log
            .last()
            .is_none_or(|l| l.1 != state || l.2 != generation)
        {
            self.life_log.push((now, state, generation));
        }
        self.life = state;
    }

    /// A player's tag: eight hex digits of a keyed hash of the ucid.
    fn tag(&self, ucid: &str) -> String {
        self.keys.key_of(ucid)[..8].to_owned()
    }

    fn check_deadline(&self, what: &str) -> Result<(), String> {
        if Instant::now() > self.deadline {
            return Err(format!("timed out waiting for {what}"));
        }
        if self.o.control.join("abort").exists() {
            return Err(format!("the caller aborted while waiting for {what}"));
        }
        Ok(())
    }

    /// Step the link until `done`, for at most `secs`; whether it was done.
    fn until(
        &mut self,
        what: &str,
        secs: u64,
        mut done: impl FnMut(&Rig) -> bool,
    ) -> Result<bool, String> {
        let by = Instant::now() + Duration::from_secs(secs);
        while !done(self) {
            if Instant::now() > by {
                return Ok(false);
            }
            self.check_deadline(what)?;
            self.pump();
        }
        Ok(true)
    }

    /// Send a command; its sequence number.
    fn send(&mut self, entity: u64, rev: u64, body: Body) -> Result<u64, String> {
        let header = if body.is_control() {
            CommandHeader::default()
        } else {
            let seq = self.next_seq;
            self.next_seq += 1;
            CommandHeader {
                seq,
                epoch: self.epoch.ok_or("no HELLO yet")?,
                campaign: self.o.campaign,
                entity,
                rev,
                mission_gen: self.generation,
            }
        };
        let seq = header.seq;
        let (op, payload) = WireCommand { header, body }.encode();
        self.sim
            .send(op, &payload)
            .map_err(|_| "the link detached before a command was sent".to_owned())?;
        // A command is held until the flush; this tool keeps no store.
        self.sim.flush();
        Ok(seq)
    }

    /// Send and wait for the `RESULT`.
    fn call(&mut self, entity: u64, rev: u64, body: Body) -> Result<CommandResult, String> {
        let seq = self.send(entity, rev, body)?;
        self.result_of(seq, 30)
    }

    fn result_of(&mut self, seq: u64, secs: u64) -> Result<CommandResult, String> {
        if !self.until("a RESULT", secs, |r| r.results.contains_key(&seq))? {
            return Err(format!("no RESULT for command {seq} in {secs} s"));
        }
        Ok(self.results[&seq].clone())
    }

    /// Send, and require `want` as the result code.
    fn expect(
        &mut self,
        what: &str,
        entity: u64,
        rev: u64,
        body: Body,
        want: u64,
    ) -> Result<CommandResult, String> {
        let r = self.call(entity, rev, body)?;
        if u64::from(r.code) != want {
            return Err(format!(
                "{what}: answered {} {}, wanted {}",
                code_name(r.code),
                r.message,
                code_name(want as u16)
            ));
        }
        Ok(r)
    }

    fn step(&mut self, name: &str, verdict: &str, detail: &str) {
        let line = json!({"ev": "step", "name": name, "verdict": verdict, "detail": detail});
        println!("{line}");
        self.steps.push(line);
    }

    fn pass(&mut self, name: &str, detail: &str) {
        self.step(name, "pass", detail);
    }

    fn note(&mut self, name: &str, detail: &str) {
        self.step(name, "note", detail);
    }

    fn skip(&mut self, name: &str, detail: &str) {
        self.step(name, "skip", detail);
    }

    /// A failed step: printed, and returned as the run's error.
    fn fail(&mut self, name: &str, detail: &str) -> String {
        self.step(name, "fail", detail);
        format!("{name}: {detail}")
    }

    fn phase(&self, name: &str, extra: Value) {
        let mut line = json!({"ev": "phase", "name": name});
        if let (Some(l), Some(e)) = (line.as_object_mut(), extra.as_object()) {
            l.extend(e.clone());
        }
        println!("{line}");
    }

    /// Wait for the caller's answer: the text of the control file `file`.
    fn wait_file(&mut self, file: &str, secs: u64) -> Result<String, String> {
        let path = self.o.control.join(file);
        let by = Instant::now() + Duration::from_secs(secs);
        while !path.exists() {
            if Instant::now() > by {
                return Err(format!("no {file} answer from the caller in {secs} s"));
            }
            self.check_deadline(file)?;
            self.pump();
        }
        std::thread::sleep(Duration::from_millis(50));
        Ok(std::fs::read_to_string(&path)
            .unwrap_or_default()
            .trim()
            .to_owned())
    }

    /// Wait for the link, the mode and a running mission, and announce the
    /// campaign.
    fn ready(&mut self) -> Result<(), String> {
        self.until("a ready link", 120, |r| r.epoch.is_some() && r.sim.ready())
            .and_then(|ok| ok.then_some(()).ok_or("no HELLO in 120 s".to_owned()))?;
        let want = self.o.campaign;
        self.send(0, 0, Body::Campaign(want))?;
        let live =
            |r: &Rig| r.life == lifecycle::RUNNING as u32 || r.life == lifecycle::PAUSED as u32;
        if !self.until("a running mission", 180, live)? {
            return Err("the mission did not run in 180 s".into());
        }
        Ok(())
    }

    /// Remove what the run added, best effort, and unban a banned client.
    fn clean_up(&mut self) {
        while let Some((entity, body)) = self.cleanup.pop() {
            if let Ok(seq) = self.send(entity, 0, body) {
                let _ = self.result_of(seq, 5);
            }
        }
        if let Some(ucid) = self.banned.take() {
            let body = Body::Moderate {
                action: moderation::UNBAN,
                player: 0,
                side: 0,
                seconds: 0.0,
                ucid,
                slot: String::new(),
                reason: String::new(),
            };
            if let Ok(seq) = self.send(0, 0, body) {
                let _ = self.result_of(seq, 10);
            }
        }
    }

    fn untrack(&mut self, entity: u64) {
        self.cleanup.retain(|(e, _)| *e != entity);
    }

    fn verdict(&self, pass: bool, error: Option<&str>) -> Value {
        let count = |v: &str| {
            self.steps
                .iter()
                .filter(|s| s["verdict"] == json!(v))
                .count()
        };
        json!({
            "ev": "verdict",
            "pass": pass,
            "scenario": self.o.scenario,
            "raw_lua_seen": self.hello_flags.map(|f| u64::from(f) & flags::RAW_LUA != 0),
            "raw_lua_expected": self.o.raw,
            "pass_steps": count("pass"),
            "fail_steps": count("fail"),
            "skip_steps": count("skip"),
            "note_steps": count("note"),
            "error": error,
            "steps": self.steps,
        })
    }
}

/// Run the scenario `--scenario` names; whether it passed.
pub fn run(args: &[String]) -> Result<bool, String> {
    let o = opts(args)?;
    let need = |flag: &str| arg(args, flag).ok_or(format!("{flag} is required"));
    let token = read_token(need("--token-file")?.as_ref()).map_err(|e| format!("token: {e}"))?;
    let port: u16 = need("--port")?
        .parse()
        .map_err(|_| "--port takes a number")?;
    let cfg = Config {
        map_path: need("--map")?.into(),
        handshake: ([127, 0, 0, 1], port).into(),
        token,
        poll: Duration::from_millis(2),
        handshake_timeout: Duration::from_secs(15),
    };
    let mut r = Rig {
        sim: LinkSim::start(cfg, Default::default()),
        deadline: Instant::now() + Duration::from_secs(o.seconds),
        keys: Keyring::generate(),
        epoch: None,
        hello_flags: None,
        next_seq: 1,
        generation: 0,
        life: 0,
        life_log: Vec::new(),
        results: HashMap::new(),
        menus: Vec::new(),
        players: Vec::new(),
        authors: Vec::new(),
        extensions: Vec::new(),
        events: Vec::new(),
        steps: Vec::new(),
        cleanup: Vec::new(),
        banned: None,
        o,
    };
    let outcome = match r.o.scenario.as_str() {
        "functional" => functional(&mut r),
        "moderation" => moderated(&mut r),
        "moves" => moves(&mut r),
        other => Err(format!(
            "--scenario is functional, moderation or moves, not {other}"
        )),
    };
    if outcome.is_err() {
        r.phase("failed", json!({}));
    }
    r.clean_up();
    let pass = outcome.is_ok() && r.steps.iter().all(|s| s["verdict"] != json!("fail"));
    let v = r.verdict(pass, outcome.as_ref().err().map(String::as_str));
    println!("{v}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// The functional scenario.

fn functional(r: &mut Rig) -> Result<(), String> {
    r.ready()?;
    let raw = r.o.raw.expect("checked");
    let seen = r
        .hello_flags
        .is_some_and(|f| u64::from(f) & flags::RAW_LUA != 0);
    if seen != raw {
        return Err(r.fail(
            "link",
            &format!(
                "the agent runs with raw Lua {}, and the run expects {}: set `raw` in \
                 agent.toml and restart DCS",
                if seen { "on" } else { "off" },
                if raw { "on" } else { "off" }
            ),
        ));
    }
    r.pass(
        "link",
        &format!(
            "raw Lua {}, campaign {}, generation {}, lifecycle {}",
            if seen { "on" } else { "off" },
            r.o.campaign,
            r.generation,
            r.life
        ),
    );
    menus(r)?;
    if raw {
        extension_set(r)?;
    } else {
        extension_gate(r)?;
    }
    marks(r)?;
    messages(r)?;
    if raw {
        extension_run(r)?;
        mission_load(r)?;
        extension_after_load(r)?;
        extension_remove(r)?;
    }
    eval_gate(r, raw)?;
    Ok(())
}

fn menus(r: &mut Rig) -> Result<(), String> {
    let set = |parent, submenu: bool, text: &str| Body::MenuSet {
        parent,
        target: 0,
        scope: ALL,
        submenu,
        text: text.to_owned(),
    };
    for (entity, parent, submenu, text) in [
        (SUBMENU, 0, true, "np-s16"),
        (COMMAND, SUBMENU, false, "np-s16 pick"),
    ] {
        r.cleanup.push((entity, Body::MenuRemove));
        if let Err(e) = r.expect("MENU_SET", entity, 1, set(parent, submenu, text), 0) {
            return Err(r.fail("menu_set", &e));
        }
    }
    r.pass(
        "menu_set",
        "a submenu and a command item in it, both applied",
    );
    let before = r.menus.len();
    r.phase("menus_set", json!({"submenu": SUBMENU, "command": COMMAND}));
    let answer = r.wait_file("menus_picked", 300)?;
    if let Some(why) = answer.strip_prefix("unreachable") {
        r.skip(
            "menu_select",
            &format!("the caller could not select the item{why}"),
        );
    } else {
        let got = r.until("a MENU record", 10, |r| {
            r.menus[before..].contains(&COMMAND)
        })?;
        if got {
            r.pass(
                "menu_select",
                &format!("a MENU record for the item ({answer})"),
            );
        } else {
            let others = r.menus.len() - before;
            return Err(r.fail(
                "menu_select",
                &format!("no MENU record for the item ({answer}); {others} for others"),
            ));
        }
    }
    for entity in [COMMAND, SUBMENU] {
        if let Err(e) = r.expect("MENU_REMOVE", entity, 0, Body::MenuRemove, 0) {
            return Err(r.fail("menu_remove", &e));
        }
        r.untrack(entity);
    }
    r.pass("menu_remove", "the command, then its submenu, removed");
    Ok(())
}

fn extension_set(r: &mut Rig) -> Result<(), String> {
    let body = Body::ExtensionSet {
        name: EXTENSION.into(),
        source: lv_str(EXTENSION_SOURCE),
    };
    r.cleanup.push((
        0,
        Body::ExtensionRemove {
            name: EXTENSION.into(),
        },
    ));
    match r.expect("EXTENSION_SET", 0, 1, body, 0) {
        Ok(_) => {
            r.pass(
                "extension_set",
                "an extension with on_event, on_frame, invoke and emit",
            );
            Ok(())
        }
        Err(e) => Err(r.fail("extension_set", &e)),
    }
}

fn extension_gate(r: &mut Rig) -> Result<(), String> {
    let body = Body::ExtensionSet {
        name: EXTENSION.into(),
        source: lv_str(EXTENSION_SOURCE),
    };
    match r.expect("EXTENSION_SET", 0, 1, body, result::RAW_LUA_OFF) {
        Ok(_) => {
            r.pass("extension_gate", "EXTENSION_SET answered RAW_LUA_OFF");
            Ok(())
        }
        Err(e) => Err(r.fail("extension_gate", &e)),
    }
}

fn marks(r: &mut Rig) -> Result<(), String> {
    let mark = Body::MarkSet {
        scope: ALL,
        read_only: true,
        target: 0,
        x: 1000.0,
        y: 0.0,
        z: 1000.0,
        dcs_id: MARK_DCS_ID,
        text: MARK_TEXT.into(),
    };
    r.cleanup.push((MARK, Body::MarkRemove));
    if let Err(e) = r.expect("MARK_SET", MARK, 1, mark, 0) {
        return Err(r.fail("mark_set", &e));
    }
    // A line: two points, a color, a fill color, a line type, read only and
    // a message (`trigger.action.markupToAll`, shape 1). DCS refused a circle's
    // radius at parameter 5 (LIVE): it takes points there.
    let args = lv_array(vec![
        point(1500.0, 0.0, 1000.0),
        point(1800.0, 0.0, 1300.0),
        lv_array(vec![
            Lv::Number(1.0),
            Lv::Number(0.0),
            Lv::Number(0.0),
            Lv::Number(1.0),
        ]),
        lv_array(vec![
            Lv::Number(1.0),
            Lv::Number(0.0),
            Lv::Number(0.0),
            Lv::Number(0.3),
        ]),
        Lv::Number(1.0),
        Lv::Bool(true),
        lv_str("np-s16-drawing"),
    ]);
    r.cleanup.push((DRAWING, Body::DrawingRemove));
    let drawing = Body::DrawingSet {
        scope: ALL,
        shape: 1,
        target: 0,
        dcs_id: DRAWING_DCS_ID,
        args,
    };
    if let Err(e) = r.expect("DRAWING_SET", DRAWING, 1, drawing, 0) {
        return Err(r.fail("mark_set", &e));
    }
    r.pass("mark_set", "MARK_SET and DRAWING_SET applied");
    r.phase("marks_set", json!({"text": MARK_TEXT}));
    let read = r.wait_file("marks_read", 120)?;
    let Some(m) = parse_marks_read(&read) else {
        return Err(r.fail(
            "marks_present",
            &format!("the read is not a mark list: {read}"),
        ));
    };
    if !m.texts.iter().any(|t| t == MARK_TEXT) {
        return Err(r.fail(
            "marks_present",
            &format!(
                "the server state's panels hold {} of the shim's ({} in all) and not the mark",
                m.count, m.panels
            ),
        ));
    }
    r.pass(
        "marks_present",
        &format!("the mark is on the map ({} panels of the shim's)", m.count),
    );
    if m.count >= 2 {
        r.pass(
            "drawing_present",
            "a second panel of the shim's is on the map",
        );
    } else {
        r.note(
            "drawing_present",
            "world.getMarkPanels lists the mark and no drawing: whether it lists drawings is UNVERIFIED",
        );
    }
    for (entity, body, what) in [
        (MARK, Body::MarkRemove, "MARK_REMOVE"),
        (DRAWING, Body::DrawingRemove, "DRAWING_REMOVE"),
    ] {
        if let Err(e) = r.expect(what, entity, 0, body, 0) {
            return Err(r.fail("mark_remove", &e));
        }
        r.untrack(entity);
    }
    r.pass("mark_remove", "both removed");
    r.phase("marks_removed", json!({}));
    let gone = r.wait_file("marks_gone", 120)?;
    match parse_marks_read(&gone) {
        Some(m) if m.count == 0 => {
            r.pass(
                "marks_gone",
                &format!("no panel of the shim's is left ({} in all)", m.panels),
            );
            Ok(())
        }
        Some(m) => Err(r.fail(
            "marks_gone",
            &format!("{} panel(s) of the shim's are still on the map", m.count),
        )),
        None => Err(r.fail(
            "marks_gone",
            &format!("the read is not a mark list: {gone}"),
        )),
    }
}

fn messages(r: &mut Rig) -> Result<(), String> {
    let message = |scope, target, text: &str| Body::Message {
        scope,
        clear: false,
        target,
        seconds: 5.0,
        text: text.to_owned(),
    };
    for (name, body) in [
        ("message_all", message(ALL, 0, "np-s16 message to all")),
        (
            "message_coalition",
            message(COALITION, 2, "np-s16 message to blue"),
        ),
        (
            "chat_all",
            Body::Chat {
                player: 0,
                text: "np-s16 chat to everyone".into(),
            },
        ),
    ] {
        if let Err(e) = r.expect(name, 0, 0, body, 0) {
            return Err(r.fail(name, &e));
        }
        r.pass(name, "applied");
    }
    Ok(())
}

fn invoke(r: &mut Rig, what: &str) -> Result<String, String> {
    let body = Body::ExtensionInvoke {
        name: EXTENSION.into(),
        args: lv_array(vec![lv_str(what)]),
    };
    let res = r.expect("EXTENSION_INVOKE", 0, 0, body, 0)?;
    lv_text(&res.value).ok_or_else(|| format!("invoke returned {}", lv_json(&res.value, 0)))
}

/// Invoke `status` until its frames count is above `frames` or `secs` pass.
fn status_until(
    r: &mut Rig,
    secs: u64,
    more: impl Fn(u64, u64, u64) -> bool,
) -> Result<(u64, u64, u64), String> {
    let by = Instant::now() + Duration::from_secs(secs);
    loop {
        let text = invoke(r, "status")?;
        let Some(s) = parse_status(&text) else {
            return Err(format!(
                "status is not frames=..;events=..;invokes=..: {text}"
            ));
        };
        if more(s.0, s.1, s.2) || Instant::now() > by {
            return Ok(s);
        }
        r.until("the extension's hooks", 2, |_| false)?;
    }
}

fn extension_run(r: &mut Rig) -> Result<(), String> {
    let first = match invoke(r, "status") {
        Ok(t) => t,
        Err(e) => return Err(r.fail("extension_invoke", &e)),
    };
    let (frames, events, invokes) = match status_until(r, 30, |f, e, _| f > 0 && e > 0) {
        Ok(s) => s,
        Err(e) => return Err(r.fail("extension_invoke", &e)),
    };
    if frames == 0 || events == 0 {
        return Err(r.fail(
            "extension_invoke",
            &format!(
                "invoke works ({first}) but the hooks did not run: on_frame {frames}, on_event {events} \
                 (the simulation must be running)"
            ),
        ));
    }
    r.pass(
        "extension_invoke",
        &format!(
            "invoke answered; on_frame ran {frames} times, on_event {events}, invokes {invokes}"
        ),
    );
    let authors = r.authors.len();
    match invoke(r, "emit") {
        Ok(t) if t.starts_with("emit=true") => {}
        Ok(t) => return Err(r.fail("extension_emit", &format!("emit answered: {t}"))),
        Err(e) => return Err(r.fail("extension_emit", &e)),
    }
    if !r.until("an AUTHOR_EVENT", 10, |r| {
        r.authors[authors..].iter().any(|(_, n)| n == EMIT_NAME)
    })? {
        return Err(r.fail(
            "extension_emit",
            "emit returned true and no AUTHOR_EVENT came",
        ));
    }
    r.pass(
        "extension_emit",
        &format!("an AUTHOR_EVENT named {EMIT_NAME}"),
    );
    let by = Instant::now();
    if !r.until("an EXTENSION record", 40, |r| {
        r.extensions
            .iter()
            .any(|(t, n, c, us, _)| *t >= by && n == EXTENSION && *c > 0 && *us > 0)
    })? {
        return Err(r.fail(
            "extension_record",
            "no EXTENSION record with calls and time above 0 in 40 s",
        ));
    }
    r.pass(
        "extension_record",
        "an EXTENSION record with calls and time above 0",
    );
    Ok(())
}

fn mission_load(r: &mut Rig) -> Result<(), String> {
    let Some(path) = r.o.mission_path.clone() else {
        return Err(r.fail("mission_load", "--mission-path is required with raw Lua on"));
    };
    let gen0 = r.generation;
    let sent = Instant::now();
    if let Err(e) = r.expect("MISSION_LOAD", 0, 0, Body::MissionLoad { path }, 0) {
        return Err(r.fail("mission_load", &e));
    }
    r.phase("mission_load_sent", json!({"generation": gen0}));
    let loading = |r: &Rig| {
        r.life_log
            .iter()
            .any(|(t, s, _)| *t >= sent && *s == lifecycle::LOADING as u32)
    };
    if !r.until("LOADING", 60, loading)? {
        return Err(r.fail(
            "mission_load",
            "the lifecycle never went to LOADING in 60 s",
        ));
    }
    let back = |r: &Rig| {
        r.life_log.iter().any(|(t, s, g)| {
            *t >= sent
                && (*s == lifecycle::RUNNING as u32 || *s == lifecycle::PAUSED as u32)
                && *g > gen0
        })
    };
    if !r.until("the mission again", 600, back)? {
        return Err(r.fail("mission_load", "the mission did not run again in 600 s"));
    }
    r.pass(
        "mission_load",
        &format!(
            "LOADING, then running again in {:.0} s, generation {} to {}",
            sent.elapsed().as_secs_f64(),
            gen0,
            r.generation
        ),
    );
    r.phase("loaded", json!({"generation": r.generation}));
    Ok(())
}

fn extension_after_load(r: &mut Rig) -> Result<(), String> {
    if !r.until("RUNNING", 120, |r| r.life == lifecycle::RUNNING as u32)? {
        return Err(r.fail(
            "running_after_load",
            "the lifecycle did not reach RUNNING in 120 s (the caller holds the simulation running)",
        ));
    }
    r.pass("running_after_load", "RUNNING");
    // The chunk ran again, so invoke's own counter starts over: this is the
    // first invoke of a fresh environment.
    let text = match invoke(r, "status") {
        Ok(t) => t,
        Err(e) => return Err(r.fail("extension_after_load", &e)),
    };
    let Some((_, _, invokes)) = parse_status(&text) else {
        return Err(r.fail(
            "extension_after_load",
            &format!("unreadable status: {text}"),
        ));
    };
    if invokes != 1 {
        return Err(r.fail(
            "extension_after_load",
            &format!("the invoke count is {invokes}, not 1: the chunk did not run again"),
        ));
    }
    r.pass(
        "extension_after_load",
        "invoke works and its counters started over: the chunk ran again",
    );
    let (frames, events, _) = match status_until(r, 30, |f, _, _| f > 0) {
        Ok(s) => s,
        Err(e) => return Err(r.fail("extension_frames_after_load", &e)),
    };
    if frames == 0 {
        return Err(r.fail(
            "extension_frames_after_load",
            "on_frame did not run after the load",
        ));
    }
    r.pass(
        "extension_frames_after_load",
        &format!("on_frame ran {frames} times, on_event {events}"),
    );
    let by = Instant::now();
    if !r.until("an EXTENSION record", 40, |r| {
        r.extensions
            .iter()
            .any(|(t, n, c, us, _)| *t >= by && n == EXTENSION && *c > 0 && *us > 0)
    })? {
        return Err(r.fail(
            "extension_record_after_load",
            "no EXTENSION record with calls and time above 0 after the load in 40 s",
        ));
    }
    r.pass(
        "extension_record_after_load",
        "an EXTENSION record with calls and time above 0 after the load",
    );
    let authors = r.authors.len();
    match invoke(r, "emit") {
        Ok(t) if t.starts_with("emit=true") => {}
        Ok(t) => return Err(r.fail("extension_emit_after_load", &format!("emit answered: {t}"))),
        Err(e) => return Err(r.fail("extension_emit_after_load", &e)),
    }
    if !r.until("an AUTHOR_EVENT", 10, |r| {
        r.authors[authors..].iter().any(|(_, n)| n == EMIT_NAME)
    })? {
        return Err(r.fail(
            "extension_emit_after_load",
            "no AUTHOR_EVENT after the load",
        ));
    }
    r.pass(
        "extension_emit_after_load",
        "an AUTHOR_EVENT after the load",
    );
    Ok(())
}

fn extension_remove(r: &mut Rig) -> Result<(), String> {
    let body = Body::ExtensionRemove {
        name: EXTENSION.into(),
    };
    if let Err(e) = r.expect("EXTENSION_REMOVE", 0, 0, body, 0) {
        return Err(r.fail("extension_remove", &e));
    }
    r.untrack(0);
    let after = Body::ExtensionInvoke {
        name: EXTENSION.into(),
        args: lv_array(vec![lv_str("status")]),
    };
    match r.expect("EXTENSION_INVOKE", 0, 0, after, result::LUA_ERROR) {
        Ok(_) => {
            r.pass("extension_remove", "removed; an invoke answers LUA_ERROR");
            Ok(())
        }
        Err(e) => Err(r.fail("extension_remove", &e)),
    }
}

fn eval_gate(r: &mut Rig, raw: bool) -> Result<(), String> {
    let body = Body::Eval {
        source: lv_str("return 6 * 7"),
    };
    if raw {
        match r.expect("EVAL", 0, 0, body, 0) {
            Ok(res) if res.value == Lv::Number(42.0) => {
                r.pass("eval_gate", "EVAL returned 42");
                Ok(())
            }
            Ok(res) => Err(r.fail(
                "eval_gate",
                &format!("EVAL returned {}, not 42", lv_json(&res.value, 0)),
            )),
            Err(e) => Err(r.fail("eval_gate", &e)),
        }
    } else {
        match r.expect("EVAL", 0, 0, body, result::RAW_LUA_OFF) {
            Ok(_) => {
                r.pass("eval_gate", "EVAL answered RAW_LUA_OFF");
                Ok(())
            }
            Err(e) => Err(r.fail("eval_gate", &e)),
        }
    }
}

// ---------------------------------------------------------------------------
// The moderation scenario.

impl Rig {
    /// A player row after the `from`-th, matching `pick`.
    fn player_after(&self, from: usize, pick: impl Fn(&Seen) -> bool) -> Option<Seen> {
        self.players.iter().skip(from).find(|p| pick(p)).cloned()
    }

    /// The connected players the agent lists, by ucid tag.
    fn listed(&mut self) -> Result<Vec<(u32, String)>, String> {
        let res = self.expect("PLAYERS", 0, 0, Body::Players, 0)?;
        Ok(lv_items(&res.value)
            .into_iter()
            .filter_map(|p| {
                let id = match lv_field(p, "id")? {
                    Lv::Number(n) => *n as u32,
                    _ => return None,
                };
                let ucid = lv_text(lv_field(p, "ucid")?)?;
                Some((id, self.tag(&ucid)))
            })
            .collect())
    }

    /// The tags of the banned ucids `BANS` lists.
    fn banned_tags(&mut self) -> Result<Vec<String>, String> {
        let res = self.expect("BANS", 0, 0, Body::Bans, 0)?;
        Ok(lv_items(&res.value)
            .into_iter()
            .filter_map(|b| lv_text(lv_field(b, "ucid")?).map(|u| self.tag(&u)))
            .collect())
    }
}

/// A `MODERATE` body that names `who`.
fn moderate(who: &Who, action: u32, side: u32, slot: &str, reason: &str) -> Body {
    Body::Moderate {
        action,
        player: who.id,
        side,
        seconds: if action == moderation::BAN {
            BAN_S
        } else {
            0.0
        },
        ucid: who.ucid.clone(),
        slot: slot.to_owned(),
        reason: reason.to_owned(),
    }
}

/// A `CONNECT` row after `from`, as the client.
fn wait_connect(r: &mut Rig, from: usize, secs: u64) -> Result<Option<Seen>, String> {
    if r.until("a CONNECT record", secs, |r| {
        r.player_after(from, |p| p.event == CONNECT).is_some()
    })? {
        Ok(r.player_after(from, |p| p.event == CONNECT))
    } else {
        Ok(None)
    }
}

fn moderated(r: &mut Rig) -> Result<(), String> {
    r.ready()?;
    r.pass(
        "link",
        &format!(
            "campaign {}, generation {}, lifecycle {}",
            r.o.campaign, r.generation, r.life
        ),
    );
    // The server holds no player yet.
    match r.listed() {
        Ok(l) if l.is_empty() => r.pass("players_query", "PLAYERS answered with no player"),
        Ok(l) => {
            return Err(r.fail(
                "players_query",
                &format!("{} player(s) are connected", l.len()),
            ));
        }
        Err(e) => return Err(r.fail("players_query", &e)),
    }
    r.phase("ready", json!({}));
    r.wait_file("joined", 300)?;
    let Some(first) = wait_connect(r, 0, 60)? else {
        return Err(r.fail("connect", "no CONNECT record after the client joined"));
    };
    let tag = r.tag(&first.ucid);
    let mut who = Who {
        id: first.id,
        ucid: first.ucid.clone(),
    };
    r.pass(
        "connect",
        &format!(
            "player {tag}, connection {}, name of {} bytes",
            first.id,
            first.name.len()
        ),
    );
    match r.listed() {
        Ok(l) if l.iter().any(|(id, t)| *id == who.id && *t == tag) => {
            r.pass("players_query_client", "PLAYERS lists the client")
        }
        Ok(l) => {
            return Err(r.fail(
                "players_query_client",
                &format!("PLAYERS lists {} players, not the client", l.len()),
            ));
        }
        Err(e) => return Err(r.fail("players_query_client", &e)),
    }

    // The client takes a slot and spawns.
    let mark = r.players.len();
    let events = r.events.len();
    r.phase("connected", json!({"player": tag}));
    r.wait_file("slot_taken", 120)?;
    // The record may follow the caller's answer by a moment.
    let taken = |p: &Seen| p.event == SLOT && !p.slot.is_empty();
    let got = r.until("a SLOT record", 30, |r| {
        r.player_after(mark, taken).is_some()
    })?;
    let Some(slot) = got.then(|| r.player_after(mark, taken)).flatten() else {
        return Err(r.fail(
            "slot_record",
            "no SLOT record with a slot after the client took one",
        ));
    };
    r.pass(
        "slot_record",
        &format!("side {}, slot {}", slot.side, slot.slot),
    );
    r.wait_file("spawned", 120)?;
    r.until("events", 3, |_| false)?;
    let ids = event_ids(&r.events[events..]);
    if ids.contains(&BIRTH) {
        r.pass("spawned", &format!("a BIRTH event came; event ids {ids:?}"));
    } else {
        r.note(
            "spawned",
            &format!("no BIRTH event came; event ids {ids:?}"),
        );
    }

    // E34, SLOT: the client moves to spectators.
    r.phase("slot_move_armed", json!({}));
    r.wait_file("slot_move_ready", 60)?;
    let mark = r.players.len();
    let body = moderate(&who, moderation::SLOT, 0, "", "");
    if let Err(e) = r.expect("MODERATE SLOT", 0, 0, body, 0) {
        return Err(r.fail("slot_to_spectators", &e));
    }
    let moved = |p: &Seen| p.event == SLOT && (p.slot.is_empty() || p.side == 0);
    if !r.until("a SLOT record", 20, |r| {
        r.player_after(mark, moved).is_some()
    })? {
        return Err(r.fail(
            "slot_to_spectators",
            "no SLOT record showed spectators in 20 s",
        ));
    }
    r.pass(
        "slot_to_spectators",
        "MODERATE SLOT applied and a SLOT record shows spectators",
    );
    r.phase("slot_moved", json!({}));
    r.wait_file("slot_checked", 60)?;

    e35(r, &who)?;

    // KICK.
    r.phase("kick_armed", json!({}));
    r.wait_file("kick_ready", 60)?;
    let mark = r.players.len();
    let body = moderate(&who, moderation::KICK, 0, "", "np-s16-kick");
    if let Err(e) = r.expect("MODERATE KICK", 0, 0, body, 0) {
        return Err(r.fail("kick", &e));
    }
    if !r.until("a DISCONNECT record", 30, |r| {
        r.player_after(mark, |p| p.event == DISCONNECT && p.id == who.id)
            .is_some()
    })? {
        return Err(r.fail("kick", "KICK applied and no DISCONNECT record came in 30 s"));
    }
    r.pass("kick", "KICK applied and a DISCONNECT record came");
    r.phase("kicked", json!({}));
    let mark = r.players.len();
    r.wait_file("rejoined", 300)?;
    let Some(again) = wait_connect(r, mark, 60)? else {
        return Err(r.fail(
            "reconnect_after_kick",
            "the client rejoined and no CONNECT record came",
        ));
    };
    if r.tag(&again.ucid) != tag {
        return Err(r.fail(
            "reconnect_after_kick",
            "a CONNECT record names another player",
        ));
    }
    who = Who {
        id: again.id,
        ucid: again.ucid.clone(),
    };
    r.pass(
        "reconnect_after_kick",
        &format!("the same player, connection {}", again.id),
    );

    // BAN, the refusal, the ban list, UNBAN.
    r.phase("ban_armed", json!({}));
    r.wait_file("ban_ready", 60)?;
    let mark = r.players.len();
    let body = moderate(&who, moderation::BAN, 0, "", "np-s16-ban");
    if let Err(e) = r.expect("MODERATE BAN", 0, 0, body, 0) {
        return Err(r.fail("ban", &e));
    }
    r.banned = Some(who.ucid.clone());
    if !r.until("a DISCONNECT record", 30, |r| {
        r.player_after(mark, |p| p.event == DISCONNECT && p.id == who.id)
            .is_some()
    })? {
        return Err(r.fail("ban", "BAN applied and no DISCONNECT record came in 30 s"));
    }
    r.pass(
        "ban",
        &format!("BAN applied for {BAN_S:.0} s and a DISCONNECT record came"),
    );
    let mark = r.players.len();
    r.phase("banned", json!({}));
    let answer = r.wait_file("rejoin_tried", 300)?;
    let connects = r.players[mark..]
        .iter()
        .filter(|p| p.event == CONNECT)
        .count();
    if answer != "refused" || connects > 0 {
        return Err(r.fail(
            "ban_refused",
            &format!(
                "the caller reports the rejoin {answer:?}, and {connects} CONNECT record(s) came"
            ),
        ));
    }
    r.pass(
        "ban_refused",
        "the rejoin was refused and no CONNECT record came",
    );
    match r.banned_tags() {
        Ok(t) if t.contains(&tag) => r.pass(
            "bans_lists_client",
            &format!("BANS lists the client among {}", t.len()),
        ),
        Ok(t) => {
            return Err(r.fail(
                "bans_lists_client",
                &format!("BANS lists {} bans, not the client", t.len()),
            ));
        }
        Err(e) => return Err(r.fail("bans_lists_client", &e)),
    }
    let body = moderate(&who, moderation::UNBAN, 0, "", "");
    if let Err(e) = r.expect("MODERATE UNBAN", 0, 0, body, 0) {
        return Err(r.fail("unban", &e));
    }
    r.banned = None;
    r.pass("unban", "UNBAN applied");
    match r.banned_tags() {
        Ok(t) if !t.contains(&tag) => {
            r.pass("bans_forgot_client", "BANS no longer lists the client")
        }
        Ok(_) => {
            return Err(r.fail(
                "bans_forgot_client",
                "BANS still lists the client after UNBAN",
            ));
        }
        Err(e) => return Err(r.fail("bans_forgot_client", &e)),
    }
    let mark = r.players.len();
    r.phase("unbanned", json!({}));
    r.wait_file("rejoined_after_unban", 300)?;
    match wait_connect(r, mark, 60)? {
        Some(p) if r.tag(&p.ucid) == tag => {
            who.id = p.id;
            r.pass(
                "rejoin_after_unban",
                &format!("the same player, connection {}", p.id),
            );
        }
        _ => {
            return Err(r.fail(
                "rejoin_after_unban",
                "no CONNECT record of the client after UNBAN",
            ));
        }
    }

    // The client leaves.
    let mark = r.players.len();
    r.phase("finishing", json!({}));
    r.wait_file("left", 120)?;
    if r.until("a DISCONNECT record", 30, |r| {
        r.player_after(mark, |p| p.event == DISCONNECT && p.id == who.id)
            .is_some()
    })? {
        r.pass("leave", "a DISCONNECT record after the client left");
    } else {
        r.note(
            "leave",
            "no DISCONNECT record in 30 s after the client left",
        );
    }
    Ok(())
}

/// The distinct `EVENT` ids, sorted.
fn event_ids(events: &[(Instant, u32)]) -> Vec<u32> {
    let mut ids: Vec<u32> = events.iter().map(|(_, id)| *id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// E35: how fast a `MODERATE SLOT` follows the `SLOT` record that put the
/// client in a slot, and whether the aircraft spawned.
fn e35(r: &mut Rig, who: &Who) -> Result<(), String> {
    let mark = r.players.len();
    let events = r.events.len();
    r.phase("e35_armed", json!({}));
    let by = Instant::now() + Duration::from_secs(120);
    let slot = loop {
        if let Some(p) = r.player_after(mark, |p| {
            p.event == SLOT && !p.slot.is_empty() && p.id == who.id
        }) {
            break p;
        }
        if Instant::now() > by {
            return Err(r.fail("e35_slot_race", "no SLOT record with a slot in 120 s"));
        }
        r.check_deadline("the E35 SLOT record")?;
        r.pump();
    };
    // The record was read in `pump`; `slot.at` is when. The command goes out
    // now.
    let read_at = slot.at;
    let body = moderate(who, moderation::SLOT, 0, "", "");
    let seq = r.send(0, 0, body)?;
    let queued = Instant::now();
    let mut written = None;
    while written.is_none() && queued.elapsed() < Duration::from_secs(5) {
        if r.sim.queued() == 0 {
            written = Some(Instant::now());
        } else {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    let res = r.result_of(seq, 30)?;
    let result_at = Instant::now();
    if res.code != result::APPLIED as u16 {
        return Err(r.fail(
            "e35_slot_race",
            &format!(
                "MODERATE SLOT answered {} {}",
                code_name(res.code),
                res.message
            ),
        ));
    }
    let after = r.players.len();
    let moved = |p: &Seen| p.event == SLOT && p.id == who.id && (p.slot.is_empty() || p.side == 0);
    if !r.until("a SLOT record", 20, |r| {
        r.player_after(mark, |p| moved(p) && p.at > read_at)
            .is_some()
    })? {
        return Err(r.fail("e35_slot_race", "no SLOT record showed spectators in 20 s"));
    }
    let _ = after;
    let spectators = r
        .player_after(mark, |p| moved(p) && p.at > read_at)
        .expect("checked");
    r.until("events", 10, |_| false)?;
    let ms = |t: Instant| (t.saturating_duration_since(read_at)).as_secs_f64() * 1000.0;
    let ids = event_ids(&r.events[events..]);
    let birth = ids.contains(&BIRTH);
    let detail = json!({
        "queued_ms": ms(queued),
        "written_ms": written.map(ms),
        "result_ms": ms(result_at),
        "slot_ms": ms(spectators.at),
        "event_ids": ids,
        "birth_after_record": r.events[events..].iter().any(|(t, id)| *id == BIRTH && *t >= read_at),
        "aircraft_spawned": birth,
    });
    r.pass("e35_slot_race", &detail.to_string());
    r.phase("e35_done", json!({}));
    r.wait_file("e35_checked", 120)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The moves scenario.

/// Where the client is: its side and slot as `PLAYERS` lists them.
fn where_is(r: &mut Rig, who: &Who) -> Result<Option<(u32, String)>, String> {
    let res = r.expect("PLAYERS", 0, 0, Body::Players, 0)?;
    Ok(lv_items(&res.value).into_iter().find_map(|p| {
        let id = match lv_field(p, "id")? {
            Lv::Number(n) => *n as u32,
            _ => return None,
        };
        if id != who.id {
            return None;
        }
        let side = match lv_field(p, "side") {
            Some(Lv::Number(n)) => *n as u32,
            _ => u32::MAX,
        };
        let slot = lv_field(p, "slot").and_then(lv_text).unwrap_or_default();
        Some((side, slot))
    }))
}

/// Wait up to `secs` for the client to be at `want`; where it was last.
fn settle(
    r: &mut Rig,
    who: &Who,
    want: &(u32, String),
    secs: u64,
) -> Result<Option<(u32, String)>, String> {
    let by = Instant::now() + Duration::from_secs(secs);
    loop {
        let at = where_is(r, who)?;
        if at.as_ref() == Some(want) || Instant::now() > by {
            return Ok(at);
        }
        r.until("a moment", 1, |_| false)?;
    }
}

/// One move: `MODERATE SLOT` with `side` and `slot`, the answer `code` with
/// `message` for a refusal, and the client at `want` afterwards.
fn move_step(
    r: &mut Rig,
    who: &Who,
    name: &str,
    (side, slot): (u32, &str),
    (code, message): (u16, &str),
    want: (u32, &str),
) -> Result<(), String> {
    let body = moderate(who, moderation::SLOT, side, slot, "");
    let res = r.call(0, 0, body)?;
    if res.code != code || (code != result::APPLIED as u16 && res.message != message) {
        return Err(r.fail(
            name,
            &format!(
                "answered {} {:?}, wanted {} {message:?}",
                code_name(res.code),
                res.message,
                code_name(code)
            ),
        ));
    }
    let want = (want.0, want.1.to_owned());
    // A refusal changes nothing, so its read is the state the step wants.
    let at = settle(r, who, &want, 10)?;
    if at.as_ref() != Some(&want) {
        return Err(r.fail(name, &format!("the client is at {at:?}, not {want:?}")));
    }
    r.pass(
        name,
        &format!(
            "{} {:?}; side {}, slot {:?}; cost_us {}",
            code_name(res.code),
            res.message,
            want.0,
            want.1,
            res.cost_us
        ),
    );
    Ok(())
}

fn moves(r: &mut Rig) -> Result<(), String> {
    let applied = result::APPLIED as u16;
    let refused = result::DCS_REFUSED as u16;
    let keep = moderation::KEEP;
    r.ready()?;
    r.pass(
        "link",
        &format!(
            "campaign {}, generation {}, lifecycle {}",
            r.o.campaign, r.generation, r.life
        ),
    );
    // The moves act on one player: the server must hold no one else.
    match r.listed() {
        Ok(l) if l.is_empty() => r.pass("players_query", "PLAYERS answered with no player"),
        Ok(l) => {
            return Err(r.fail(
                "players_query",
                &format!("{} player(s) are connected", l.len()),
            ));
        }
        Err(e) => return Err(r.fail("players_query", &e)),
    }
    r.phase("ready", json!({}));
    let blue = r.wait_file("joined", 300)?;
    if blue.is_empty() {
        return Err(r.fail("connect", "the caller gave no blue slot"));
    }
    let Some(first) = wait_connect(r, 0, 60)? else {
        return Err(r.fail("connect", "no CONNECT record after the client joined"));
    };
    let who = Who {
        id: first.id,
        ucid: first.ucid.clone(),
    };
    let tag = r.tag(&first.ucid);
    match r.listed() {
        Ok(l) if l.len() == 1 && l[0].0 == who.id && l[0].1 == tag => r.pass(
            "players_query_client",
            "PLAYERS lists the client and no one else",
        ),
        Ok(l) => {
            return Err(r.fail(
                "players_query_client",
                &format!("PLAYERS lists {} players, not the client alone", l.len()),
            ));
        }
        Err(e) => return Err(r.fail("players_query_client", &e)),
    }
    r.pass(
        "connect",
        &format!("connection {}, blue slot {blue}", who.id),
    );
    let blue = blue.as_str();
    move_step(r, &who, "blue_no_slot", (2, ""), (applied, ""), (2, ""))?;
    move_step(r, &who, "blue_slot", (2, blue), (applied, ""), (2, blue))?;
    move_step(
        r,
        &who,
        "keep_leaves_slot",
        (keep, ""),
        (applied, ""),
        (2, ""),
    )?;
    move_step(
        r,
        &who,
        "other_coalition_refused",
        (1, blue),
        (refused, "the slot belongs to the other coalition"),
        (2, ""),
    )?;
    move_step(
        r,
        &who,
        "missing_slot_refused",
        (2, "99999999"),
        (refused, "the mission lists no slot with this ID"),
        (2, ""),
    )?;
    // From red, so KEEP can only reach blue from the slot's list.
    move_step(r, &who, "red_no_slot", (1, ""), (applied, ""), (1, ""))?;
    move_step(
        r,
        &who,
        "keep_takes_slot_coalition",
        (keep, blue),
        (applied, ""),
        (2, blue),
    )?;

    // The race: the client changes to red itself while KEEP moves go out.
    r.phase("race_armed", json!({}));
    r.wait_file("race_go", 60)?;
    // Only a record after this mark is the client's own change.
    let mark = r.players.len();
    let (mut before, mut after, mut changed) = (0u32, 0u32, None::<Instant>);
    let by = Instant::now() + Duration::from_secs(60);
    while after < 5 {
        if Instant::now() > by {
            return Err(r.fail("keep_race", "the client did not change to red in 60 s"));
        }
        let res = r.call(0, 0, moderate(&who, moderation::SLOT, keep, "", ""))?;
        if res.code != applied {
            return Err(r.fail(
                "keep_race",
                &format!("KEEP answered {} {:?}", code_name(res.code), res.message),
            ));
        }
        if changed.is_none()
            && r.players
                .iter()
                .skip(mark)
                .any(|p| p.id == who.id && p.event == SLOT && p.side == 1)
        {
            changed = Some(Instant::now());
        }
        if changed.is_some() {
            after += 1;
        } else {
            before += 1;
        }
        r.until("a moment", 0, |_| false)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    if before == 0 {
        return Err(r.fail(
            "keep_race",
            "no KEEP move went out before the change, so the run shows nothing of the race",
        ));
    }
    let want = (1, String::new());
    let at = settle(r, &who, &want, 10)?;
    if at.as_ref() != Some(&want) {
        return Err(r.fail(
            "keep_race",
            &format!("after {before} KEEP moves before and {after} after the change the client is at {at:?}, not red"),
        ));
    }
    r.pass(
        "keep_race",
        &format!("{before} KEEP moves before the change and {after} after; the client stays on red with no slot"),
    );
    r.phase("race_done", json!({}));
    r.wait_file("race_checked", 60)?;
    move_step(r, &who, "spectators", (0, ""), (applied, ""), (0, ""))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_read_parses() {
        let m = parse_marks_read("2|1073741824=np-s16-mark,1073741825=|panels=5").unwrap();
        assert_eq!(m.count, 2);
        assert_eq!(m.texts, vec!["np-s16-mark".to_owned(), String::new()]);
        assert_eq!(m.panels, 5);
        let none = parse_marks_read("0||panels=0").unwrap();
        assert_eq!((none.count, none.texts.len(), none.panels), (0, 0, 0));
        assert!(parse_marks_read("two").is_none());
        assert!(parse_marks_read("1|a=b|5").is_none());
    }

    #[test]
    fn the_extension_status_parses() {
        assert_eq!(
            parse_status("frames=12;events=3;invokes=1"),
            Some((12, 3, 1))
        );
        assert_eq!(parse_status("frames=12;events=3"), None);
        assert_eq!(parse_status("emit=true "), None);
    }

    #[test]
    fn code_names_come_from_the_table() {
        assert_eq!(code_name(0), "APPLIED");
        assert_eq!(code_name(21), "RAW_LUA_OFF");
        assert_eq!(code_name(999), "999");
    }

    #[test]
    fn lv_helpers_read_fields_and_items() {
        let v = lv_array(vec![lv_object(vec![
            ("id", Lv::Number(2.0)),
            ("ucid", lv_str("abc")),
        ])]);
        let items = lv_items(&v);
        assert_eq!(items.len(), 1);
        assert_eq!(lv_field(items[0], "id"), Some(&Lv::Number(2.0)));
        assert_eq!(
            lv_text(lv_field(items[0], "ucid").unwrap()).as_deref(),
            Some("abc")
        );
        assert_eq!(lv_json(&v, 0), "{1:{\"id\":2,\"ucid\":\"abc\"}}");
    }

    #[test]
    fn event_ids_are_sorted_and_distinct() {
        let t = Instant::now();
        assert_eq!(event_ids(&[(t, 15), (t, 20), (t, 15)]), vec![15, 20]);
    }

    #[test]
    fn a_tag_never_holds_the_ucid() {
        let keys = Keyring::generate();
        let ucid = "0123456789abcdef0123456789abcdef";
        let tag = keys.key_of(ucid)[..8].to_owned();
        assert_eq!(tag.len(), 8);
        assert!(!ucid.contains(&tag));
    }
}
