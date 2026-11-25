//! `command-script --map PATH --port N --token-file PATH --script PATH
//! [--seconds N] [--linger-ms N]`
//!
//! A sidecar that sends the commands a JSON file lists, in order, over the
//! real link ([`LinkSim`]), and prints what the agent answers. It is the
//! host tests' way to write commands that `link-probe` does not: the tests
//! in `lua/tests/dispatch.lua` use it. Nothing it sends depends on a target.
//!
//! The script is one object:
//!
//! ```text
//! {
//!   "campaign": 7,
//!   "commands": [
//!     {"op": "EXTENSION_SET", "name": "x", "rev": 1, "source": "return {}"},
//!     {"op": "EXTENSION_INVOKE", "name": "x", "args": ["a", 1], "wait_result": true},
//!     {"op": 49, "wait_ms": 100}
//!   ]
//! }
//! ```
//!
//! `campaign`, when present, is announced first with `CAMPAIGN` and is the
//! campaign every command's header carries unless the command says
//! `"campaign"`. Each command has `op`, an opcode by name or number, and the
//! optional header fields `seq` (default: 1, 2, ... in the order of the
//! non-control commands), `epoch` (default: the `HELLO`'s epoch plus one),
//! `campaign`, `entity`, `rev` and `gen`. A control command (`CAMPAIGN`,
//! `ENUMERATE`, `BURST`) has a zero header. Payload fields by opcode:
//!
//! | Opcode | Fields |
//! |---|---|
//! | `CAMPAIGN` | `campaign` |
//! | `BURST` | `on` |
//! | `EXTENSION_SET` | `name`, `source` (text) |
//! | `EXTENSION_INVOKE` | `name`, `args` (an `lv` value), or `args_hex`, the argument's raw bytes, for a value the writer would cut |
//! | `GROUP_SPAWN` | `country`, `category`, `group` (an `lv`), `name` (text, default `""`) |
//! | `GROUP_ORDER` | `task` (an `lv`) |
//! | `GROUP_OPTION` | `option`, `value` (an `lv`) |
//! | `STATIC_SPAWN` | `country`, `static` (an `lv`), `name` |
//! | `AIRBASE_OWNER` | `coalition`, `airbase` (text) |
//! | `WAREHOUSE_SET` | `airbase`, `stock` (an `lv`) |
//! | `WAREHOUSE_ADJUST` | `airbase`, `delta` (an `lv`) |
//! | `DAMAGE` | `life_percent` |
//! | `SOUND` | `scope`, `target`, `file` (text) |
//! | `CLEAR_DEBRIS` | `volume`, `x`, `y`, `z`, `x2`, `y2`, `z2`, `radius` |
//! | `PICTURE` | `scope`, `target`, `file`, `seconds`, `clear_view`, `size`, `horizontal`, `vertical`, `size_units` |
//! | any | `payload_hex`, appended after what the opcode writes |
//!
//! An `lv` value is JSON: an object is a table with string keys (a key
//! `#<number>` is that number), an array a
//! table with keys 1 to n, `null` is nil. Three optional fields pace a command:
//! `wait_ms` waits that long before it is sent, `wait_result` waits until
//! every command sent before it has its `RESULT`, and `wait_file` waits until
//! the file it names exists. A test that must see a state before the command
//! goes creates the file then; `wait_ms` counts from there.
//!
//! Output is one JSON object per line, in the order the agent wrote the records:
//! one for each `RESULT`,
//! `{"ev":"result","seq":N,"epoch":N,"opcode":N,"code":N,"code_name":"..",
//! "message":"..","cost_us":N,"object":N,"depth":N,"value":<json>}`, where `depth` is how
//! deep the value's tables nest; one for each `OBJECT_GONE`,
//! `{"ev":"gone","object":N,"reason":N,"entity":N}`; one for each `OBJECT`,
//! `{"ev":"object","object":N,"kind":N,"category":N,"enumeration":N,"life0":N,"name":".."}`;
//! one for each `SEEN`, `{"ev":"seen","enumeration":N,"objects":[N,..]}`;
//! one for each `PLAYER`,
//! `{"ev":"player","event":N,"id":N,"side":N,"seat":N,"client":"..","name":"..","slot":"..","text":".."}`;
//! one for each `MENU`, `{"ev":"menu","entity":N,"target":N,"scope":N,"player":N}`;
//! one for each `AUTHOR_EVENT`, `{"ev":"author","name":"..","objects":[N,..],"payload":<json>}`;
//! one for each `EXTENSION`, `{"ev":"extension","name":"..","calls":N,"total_us":N,
//! "max_us":N,"rev":N,"skipped":N,"stopped":N}`; and last
//! `{"ev":"end","sent":N,"results":N,"complete":B,"duplicates":N}`. The tool ends
//! `--linger-ms` (default 300) after every command it sent is answered, or
//! after `--seconds` (default 60), and exits 0 only when all were answered.
//! A problem with the script prints `{"ev":"error","text":".."}` and exits 2.
//!
//! Test tooling only: built from source, never released.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::linksim::LinkSim;
use nanoprobe_fake_agent::{arg, json_string, read_token};
use nanoprobe_sidecar::link::Config;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_transport::codec::{Lv, Writer};
use nanoprobe_transport::layout::{kind, opcode, result};
use nanoprobe_transport::records::{
    AuthorEvent, CommandHeader, CommandResult, ExtensionStats, Hello, Menu, Object, ObjectGone,
    Player, Seen,
};
use serde_json::Value;

/// One command of the script.
struct Step {
    opcode: u16,
    control: bool,
    header: CommandHeader,
    /// Fields of the header the script left to the tool.
    epoch: Option<u64>,
    seq: Option<u64>,
    body: Vec<u8>,
    wait: Duration,
    wait_result: bool,
    /// The command is held until this file exists.
    wait_file: Option<PathBuf>,
}

fn name_of(table: &[(&str, u64)], code: u64) -> String {
    table
        .iter()
        .find(|(_, c)| *c == code)
        .map_or_else(|| code.to_string(), |(n, _)| (*n).to_owned())
}

fn opcode_of(v: &Value) -> Result<u16, String> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| format!("op {n} is not an opcode")),
        Value::String(s) => opcode::ALL
            .iter()
            .find(|(n, _)| n == s)
            .map(|(_, c)| *c as u16)
            .ok_or_else(|| format!("no opcode named {s}")),
        _ => Err("op is a name or a number".into()),
    }
}

/// A JSON value as an `lv` value.
fn lv_of(v: &Value) -> Lv {
    match v {
        Value::Null => Lv::Nil,
        Value::Bool(b) => Lv::Bool(*b),
        Value::Number(n) => Lv::Number(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Lv::String(s.as_bytes().to_vec()),
        Value::Array(a) => Lv::Table(
            a.iter()
                .enumerate()
                .map(|(i, v)| (Lv::Number((i + 1) as f64), lv_of(v)))
                .collect(),
        ),
        Value::Object(o) => Lv::Table(
            o.iter()
                .map(|(k, v)| {
                    // A key `#<number>` is a number key.
                    let key = k
                        .strip_prefix('#')
                        .and_then(|n| n.parse::<f64>().ok())
                        .map_or_else(|| Lv::String(k.as_bytes().to_vec()), Lv::Number);
                    (key, lv_of(v))
                })
                .collect(),
        ),
    }
}

/// The bytes a string of hex digit pairs names.
fn hex_bytes(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) || !hex.is_ascii() {
        return Err("a hex field is an even number of hex digits".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("hex: {e}")))
        .collect()
}

fn u64_field(step: &Value, name: &str) -> Result<Option<u64>, String> {
    match step.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{name} is a whole number")),
    }
}

fn text_field<'a>(step: &'a Value, name: &str) -> Result<&'a str, String> {
    step.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} is required, as text"))
}

fn parse_step(step: &Value, campaign: u64) -> Result<Step, String> {
    let op = opcode_of(step.get("op").ok_or("a command has no op")?)?;
    let control = [opcode::CAMPAIGN, opcode::ENUMERATE, opcode::BURST].contains(&u64::from(op));
    let mut w = Writer::new();
    let mut raw = Vec::new();
    match u64::from(op) {
        opcode::CAMPAIGN => {
            w.u64(u64_field(step, "campaign")?.ok_or("CAMPAIGN needs campaign")?);
        }
        opcode::BURST => {
            let on = step.get("on").and_then(Value::as_bool).unwrap_or(false);
            w.u32(u32::from(on)).pad(4);
        }
        opcode::EXTENSION_SET => {
            let source = lv_of(&Value::String(text_field(step, "source")?.into()));
            w.str(text_field(step, "name")?).lv(&source);
        }
        opcode::EXTENSION_INVOKE => {
            w.str(text_field(step, "name")?);
            if let Some(hex) = step.get("args_hex").and_then(Value::as_str) {
                raw = hex_bytes(hex)?;
            } else {
                let args = step.get("args").map_or(Lv::Table(vec![]), lv_of);
                w.lv(&args);
            }
        }
        opcode::GROUP_SPAWN => {
            w.u32(u64_field(step, "country")?.unwrap_or(0) as u32)
                .u32(u64_field(step, "category")?.unwrap_or(0) as u32)
                .str(step.get("name").and_then(Value::as_str).unwrap_or(""))
                .lv(&step.get("group").map_or(Lv::Nil, lv_of));
        }
        opcode::GROUP_ORDER => {
            w.lv(&step.get("task").map_or(Lv::Nil, lv_of));
        }
        opcode::GROUP_OPTION => {
            w.u32(u64_field(step, "option")?.unwrap_or(0) as u32)
                .pad(4)
                .lv(&step.get("value").map_or(Lv::Nil, lv_of));
        }
        opcode::STATIC_SPAWN => {
            w.u32(u64_field(step, "country")?.unwrap_or(0) as u32)
                .pad(4)
                .str(step.get("name").and_then(Value::as_str).unwrap_or(""))
                .lv(&step.get("static").map_or(Lv::Nil, lv_of));
        }
        opcode::AIRBASE_OWNER => {
            w.u32(u64_field(step, "coalition")?.unwrap_or(0) as u32)
                .pad(4)
                .str(text_field(step, "airbase")?);
        }
        opcode::WAREHOUSE_SET | opcode::WAREHOUSE_ADJUST => {
            let key = if u64::from(op) == opcode::WAREHOUSE_SET {
                "stock"
            } else {
                "delta"
            };
            w.str(text_field(step, "airbase")?)
                .lv(&step.get(key).map_or(Lv::Nil, lv_of));
        }
        opcode::DAMAGE => {
            w.f64(
                step.get("life_percent")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            );
        }
        opcode::CLEAR_DEBRIS => {
            let real = |name: &str| step.get(name).and_then(Value::as_f64).unwrap_or(0.0);
            w.u32(u64_field(step, "volume")?.unwrap_or(0) as u32).pad(4);
            for name in ["x", "y", "z", "x2", "y2", "z2", "radius"] {
                w.f64(real(name));
            }
        }
        opcode::SOUND => {
            w.u32(u64_field(step, "scope")?.unwrap_or(0) as u32)
                .pad(4)
                .u64(u64_field(step, "target")?.unwrap_or(0))
                .str(text_field(step, "file")?);
        }
        opcode::PICTURE => {
            let real = |name: &str| step.get(name).and_then(Value::as_f64).unwrap_or(0.0);
            w.u32(u64_field(step, "scope")?.unwrap_or(0) as u32)
                .u32(u64_field(step, "clear_view")?.unwrap_or(0) as u32)
                .u64(u64_field(step, "target")?.unwrap_or(0))
                .f64(real("seconds"))
                .f64(real("size"))
                .u32(u64_field(step, "horizontal")?.unwrap_or(0) as u32)
                .u32(u64_field(step, "vertical")?.unwrap_or(0) as u32)
                .u32(u64_field(step, "size_units")?.unwrap_or(0) as u32)
                .pad(4)
                .str(text_field(step, "file")?);
        }
        _ => {}
    }
    let mut body = w.finish();
    body.extend(raw);
    if let Some(hex) = step.get("payload_hex").and_then(Value::as_str) {
        body.extend(hex_bytes(hex)?);
    }
    let header = if control {
        CommandHeader::default()
    } else {
        CommandHeader {
            seq: 0,
            epoch: 0,
            campaign: u64_field(step, "campaign")?.unwrap_or(campaign),
            entity: u64_field(step, "entity")?.unwrap_or(0),
            rev: u64_field(step, "rev")?.unwrap_or(0),
            mission_gen: u64_field(step, "gen")?.unwrap_or(0) as u32,
        }
    };
    Ok(Step {
        opcode: op,
        control,
        header,
        epoch: u64_field(step, "epoch")?,
        seq: u64_field(step, "seq")?,
        body,
        wait: Duration::from_millis(u64_field(step, "wait_ms")?.unwrap_or(0)),
        wait_result: step
            .get("wait_result")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        wait_file: match step.get("wait_file") {
            None => None,
            Some(Value::String(f)) => Some(PathBuf::from(f)),
            Some(_) => return Err("wait_file takes a path".into()),
        },
    })
}

fn parse_script(text: &str) -> Result<Vec<Step>, String> {
    let script: Value = serde_json::from_str(text).map_err(|e| format!("script: {e}"))?;
    let campaign = u64_field(&script, "campaign")?;
    let mut steps = Vec::new();
    if let Some(c) = campaign {
        steps.push(parse_step(
            &serde_json::json!({"op": "CAMPAIGN", "campaign": c}),
            c,
        )?);
    }
    let list = script
        .get("commands")
        .and_then(Value::as_array)
        .ok_or("the script has no commands array")?;
    for s in list {
        steps.push(parse_step(s, campaign.unwrap_or(0))?);
    }
    Ok(steps)
}

/// How deep the tables of `v` nest: 0 for a value that is not a table, 1 for
/// a table with no table in it.
fn depth(v: &Lv) -> usize {
    match v {
        Lv::Table(pairs) => {
            1 + pairs
                .iter()
                .map(|(k, v)| depth(k).max(depth(v)))
                .max()
                .unwrap_or(0)
        }
        _ => 0,
    }
}

fn lv_json(v: &Lv) -> String {
    match v {
        Lv::Nil => "null".into(),
        Lv::Bool(b) => b.to_string(),
        Lv::Number(n) if n.is_finite() => n.to_string(),
        Lv::Number(_) => "null".into(),
        Lv::String(s) => json_string(&String::from_utf8_lossy(s)),
        Lv::Other(t) => json_string(&format!("<{t}>")),
        Lv::Table(pairs) => {
            let array = pairs
                .iter()
                .enumerate()
                .all(|(i, (k, _))| matches!(k, Lv::Number(n) if *n == (i + 1) as f64));
            if array {
                let items: Vec<String> = pairs.iter().map(|(_, v)| lv_json(v)).collect();
                format!("[{}]", items.join(","))
            } else {
                let fields: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| {
                        let key = match k {
                            Lv::String(s) => String::from_utf8_lossy(s).into_owned(),
                            other => lv_json(other),
                        };
                        format!("{}:{}", json_string(&key), lv_json(v))
                    })
                    .collect();
                format!("{{{}}}", fields.join(","))
            }
        }
    }
}

fn print_result(r: &CommandResult) {
    println!(
        "{{\"ev\":\"result\",\"seq\":{},\"epoch\":{},\"opcode\":{},\"code\":{},\"code_name\":{},\
         \"message\":{},\"cost_us\":{},\"object\":{},\"depth\":{},\"value\":{}}}",
        r.seq,
        r.epoch,
        r.opcode,
        r.code,
        json_string(&name_of(result::ALL, u64::from(r.code))),
        json_string(&r.message),
        r.cost_us,
        r.object,
        depth(&r.value),
        lv_json(&r.value),
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            println!("{{\"ev\":\"error\",\"text\":{}}}", json_string(&e));
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<bool, String> {
    let need = |name: &str| arg(args, name).ok_or(format!("{name} is required"));
    let token = read_token(need("--token-file")?.as_ref()).map_err(|e| format!("token: {e}"))?;
    let port: u16 = need("--port")?
        .parse()
        .map_err(|_| "--port takes a number")?;
    let seconds: u64 = arg(args, "--seconds")
        .map_or(Ok(60), |v| v.parse())
        .map_err(|_| "--seconds takes a number")?;
    let linger: u64 = arg(args, "--linger-ms")
        .map_or(Ok(300), |v| v.parse())
        .map_err(|_| "--linger-ms takes a number")?;
    let script = need("--script")?;
    let mut steps =
        parse_script(&std::fs::read_to_string(&script).map_err(|e| format!("{script}: {e}"))?)?
            .into_iter()
            .peekable();
    let cfg = Config {
        map_path: need("--map")?.into(),
        handshake: ([127, 0, 0, 1], port).into(),
        token,
        poll: Duration::from_millis(2),
        handshake_timeout: Duration::from_secs(15),
    };
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut sim = LinkSim::start(cfg, Default::default());
    let mut epoch: Option<u64> = None;
    let mut next_seq = 1u64;
    let mut sent = 0u64;
    let mut results = 0u64;
    // Commands sent and RESULTs read, per (epoch, seq): a duplicate result
    // does not stand in for a missing one.
    let mut sends: BTreeMap<(u64, u64), u64> = BTreeMap::new();
    let mut got: BTreeMap<(u64, u64), u64> = BTreeMap::new();
    let outstanding = |sends: &BTreeMap<(u64, u64), u64>, got: &BTreeMap<(u64, u64), u64>| {
        sends
            .iter()
            .map(|(k, n)| n.saturating_sub(got.get(k).copied().unwrap_or(0)))
            .sum::<u64>()
    };
    let mut due: Option<Instant> = None;
    let mut done_at: Option<Instant> = None;
    while Instant::now() < deadline {
        for r in sim.read() {
            match u64::from(r.kind) {
                kind::HELLO if epoch.is_none() => {
                    epoch = Hello::decode(&r.payload).ok().map(|h| h.epoch + 1);
                }
                kind::OBJECT => {
                    if let Ok(o) = Object::decode(&r.payload) {
                        println!(
                            "{{\"ev\":\"object\",\"object\":{},\"kind\":{},\"category\":{},\
                             \"enumeration\":{},\"life0\":{},\"name\":{}}}",
                            o.object,
                            o.kind,
                            o.category,
                            o.enumeration,
                            o.life0,
                            json_string(&o.name)
                        );
                    }
                }
                kind::SEEN => {
                    if let Ok(v) = Seen::decode(&r.payload) {
                        let ids: Vec<String> = v.objects.iter().map(u64::to_string).collect();
                        println!(
                            "{{\"ev\":\"seen\",\"enumeration\":{},\"objects\":[{}]}}",
                            v.enumeration,
                            ids.join(",")
                        );
                    }
                }
                kind::OBJECT_GONE => {
                    if let Ok(g) = ObjectGone::decode(&r.payload) {
                        println!(
                            "{{\"ev\":\"gone\",\"object\":{},\"reason\":{},\"entity\":{}}}",
                            g.object, g.reason, g.entity
                        );
                    }
                }
                kind::PLAYER => {
                    if let Ok(p) = Player::decode(&r.payload) {
                        println!(
                            "{{\"ev\":\"player\",\"event\":{},\"id\":{},\"side\":{},\"seat\":{},\
                             \"client\":{},\"name\":{},\"slot\":{},\"text\":{}}}",
                            p.event,
                            p.player,
                            p.side,
                            p.seat,
                            json_string(&p.client),
                            json_string(&p.name),
                            json_string(&p.slot),
                            json_string(&p.text)
                        );
                    }
                }
                kind::MENU => {
                    if let Ok(m) = Menu::decode(&r.payload) {
                        println!(
                            "{{\"ev\":\"menu\",\"entity\":{},\"target\":{},\"scope\":{},\"player\":{}}}",
                            m.entity, m.target, m.scope, m.player
                        );
                    }
                }
                kind::AUTHOR_EVENT => {
                    if let Ok(a) = AuthorEvent::decode(&r.payload) {
                        let objects: Vec<String> =
                            a.objects.iter().map(ToString::to_string).collect();
                        println!(
                            "{{\"ev\":\"author\",\"name\":{},\"objects\":[{}],\"payload\":{}}}",
                            json_string(&a.name),
                            objects.join(","),
                            lv_json(&a.payload)
                        );
                    }
                }
                kind::EXTENSION => {
                    if let Ok(e) = ExtensionStats::decode(&r.payload) {
                        println!(
                            "{{\"ev\":\"extension\",\"name\":{},\"calls\":{},\"total_us\":{},\
                             \"max_us\":{},\"rev\":{},\"skipped\":{},\"stopped\":{}}}",
                            json_string(&e.name),
                            e.calls,
                            e.total_us,
                            e.max_us,
                            e.rev,
                            e.events_skipped,
                            e.stopped
                        );
                    }
                }
                kind::RESULT => {
                    if let Ok(c) = CommandResult::decode(&r.payload) {
                        print_result(&c);
                        results += 1;
                        *got.entry((c.epoch, c.seq)).or_default() += 1;
                    }
                }
                _ => {}
            }
        }
        if let (true, Some(epoch)) = (sim.ready(), epoch) {
            while let Some(step) = steps.peek() {
                if step.wait_result && outstanding(&sends, &got) > 0 {
                    break;
                }
                if step.wait_file.as_ref().is_some_and(|f| !f.exists()) {
                    break;
                }
                let at = *due.get_or_insert_with(|| Instant::now() + step.wait);
                if Instant::now() < at {
                    break;
                }
                due = None;
                let Some(mut step) = steps.next() else { break };
                if !step.control {
                    step.header.seq = step.seq.unwrap_or(next_seq);
                    next_seq = step.header.seq + 1;
                    step.header.epoch = step.epoch.unwrap_or(epoch);
                    *sends
                        .entry((step.header.epoch, step.header.seq))
                        .or_default() += 1;
                }
                let mut w = Writer::new();
                step.header.write(&mut w);
                let mut payload = w.finish();
                payload.extend_from_slice(&step.body);
                if sim.send(step.opcode, &payload).is_err() {
                    return Err("the link detached before a command was sent".into());
                }
                // A script has no store to commit first: let it through.
                sim.flush();
                sent += 1;
            }
            if steps.peek().is_none() && outstanding(&sends, &got) == 0 && sim.queued() == 0 {
                let t = *done_at.get_or_insert_with(Instant::now);
                if t.elapsed() >= Duration::from_millis(linger) {
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let complete = steps.peek().is_none() && outstanding(&sends, &got) == 0;
    // Results beyond the number of commands sent under the same numbers.
    let duplicates: u64 = got
        .iter()
        .map(|(k, n)| n.saturating_sub(sends.get(k).copied().unwrap_or(0)))
        .sum();
    println!(
        "{{\"ev\":\"end\",\"sent\":{sent},\"results\":{results},\"complete\":{complete},\"duplicates\":{duplicates}}}"
    );
    Ok(complete)
}
