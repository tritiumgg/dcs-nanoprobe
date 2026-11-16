//! The line-per-record export of a campaign (spec/persistence.md,
//! "Export"): one JSON object per line, read from the store file and the
//! events file through read-only connections, so it works on a campaign in
//! any state without opening its store.

use std::path::Path;

use nanoprobe_transport::codec::Lv;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};

use crate::eventfile::PACKAGE;
use crate::store::{DiskError, schema_of};

/// The export's own format version, in its first line.
pub const FORMAT: u32 = 1;

/// Rows read per query.
const PAGE: i64 = 1000;

fn ro(path: &Path) -> Result<Connection, DiskError> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

/// Write campaign `campaign`'s export, line by line, each line ending in a
/// newline, to `out`. A line is passed whole; `out` may stop the export by
/// returning `false`.
pub fn export(
    campaign: u64,
    store: &Path,
    events: &Path,
    out: &mut dyn FnMut(Vec<u8>) -> bool,
) -> Result<(), DiskError> {
    let schema = schema_of(store)?;
    let mut emit = |v: Value| {
        let mut line = serde_json::to_vec(&v).expect("a JSON value encodes");
        line.push(b'\n');
        out(line)
    };
    if !emit(json!({
        "kind": "header",
        "format": FORMAT,
        "campaign_id": campaign,
        "schema": schema,
        "events_package": PACKAGE,
    })) {
        return Ok(());
    }
    let s = ro(store)?;
    // One read transaction: every row of the store comes from one snapshot,
    // while the driver goes on writing through its own connection.
    s.execute_batch("BEGIN DEFERRED")?;
    // The events up to the newest at the start; later ones are not exported.
    let events_end: Option<(Connection, i64)> = if events.exists() {
        let e = ro(events)?;
        e.execute_batch("BEGIN DEFERRED")?;
        let end: Option<i64> = e.query_row("SELECT max(seq) FROM event", [], |r| r.get(0))?;
        Some((e, end.unwrap_or(0)))
    } else {
        None
    };
    // Each query reads by key in pages, so a long journal is never held whole.
    let mut meta = s.prepare("SELECT key, value FROM meta ORDER BY key")?;
    let rows = meta.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    for row in rows {
        let (key, value) = row?;
        if !emit(json!({"kind": "meta", "key": key, "value": body(&value)})) {
            return Ok(());
        }
    }
    let mut last = (i64::MIN, String::new());
    loop {
        let mut q = s.prepare(
            "SELECT id, kind, body FROM entity WHERE (id, kind) > (?1, ?2) ORDER BY id, kind LIMIT ?3",
        )?;
        let page: Vec<(i64, String, Vec<u8>)> = q
            .query_map(rusqlite::params![last.0, last.1, PAGE], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        let Some(end) = page.last() else { break };
        last = (end.0, end.1.clone());
        for (id, kind, b) in page {
            if !emit(json!({"kind": "entity", "id": id, "entity_kind": kind, "body": body(&b)})) {
                return Ok(());
            }
        }
    }
    let mut ext = s.prepare("SELECT name, body FROM extension ORDER BY name")?;
    let rows = ext.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    for row in rows {
        let (name, b) = row?;
        if !emit(json!({"kind": "extension", "name": name, "body": body(&b)})) {
            return Ok(());
        }
    }
    let mut seq = 0i64;
    loop {
        let mut q = s.prepare(
            "SELECT seq, at_ms, kind, entity, body FROM journal WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let page: Vec<(i64, i64, String, i64, Vec<u8>)> = q
            .query_map(rusqlite::params![seq, PAGE], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        let Some(end) = page.last() else { break };
        seq = end.0;
        for (seq, at_ms, kind, entity, b) in page {
            if !emit(json!({
                "kind": "journal", "seq": seq, "at_ms": at_ms, "entry": kind,
                "entity": entity, "body": body(&b),
            })) {
                return Ok(());
            }
        }
    }
    let mut inflight = s.prepare("SELECT epoch, seq, body FROM inflight ORDER BY epoch, seq")?;
    let rows = inflight.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    for row in rows {
        let (epoch, seq, b) = row?;
        if !emit(json!({"kind": "inflight", "epoch": epoch, "seq": seq, "body": body(&b)})) {
            return Ok(());
        }
    }
    let mut cp = s.prepare("SELECT seq, body FROM checkpoint ORDER BY seq")?;
    let rows = cp.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
    for row in rows {
        let (seq, b) = row?;
        if !emit(json!({"kind": "checkpoint", "seq": seq, "body": body(&b)})) {
            return Ok(());
        }
    }
    let Some((e, last)) = events_end else {
        return Ok(());
    };
    let mut seq = 0i64;
    loop {
        let mut q = e.prepare(
            "SELECT seq, wall_ms, source, body FROM event WHERE seq > ?1 AND seq <= ?2 \
             ORDER BY seq LIMIT ?3",
        )?;
        let page: Vec<(i64, i64, Option<String>, Vec<u8>)> = q
            .query_map(rusqlite::params![seq, last, PAGE], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_, _>>()?;
        let Some(end) = page.last() else { break };
        seq = end.0;
        for (seq, wall_ms, source, b) in page {
            if !emit(json!({
                "kind": "event", "seq": seq, "wall_ms": wall_ms, "source": source,
                "event_b64": base64(&b),
            })) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// A stored body as JSON: the store's encoding of a Lua value, or, for a
/// body that is not one, its bytes in base 64.
fn body(b: &[u8]) -> Value {
    match nanoprobe_transport::codec::Reader::new(b).lv() {
        Ok(v) => lv_json(&v),
        Err(_) => json!({ "bytes_b64": base64(b) }),
    }
}

/// A Lua value as JSON: a table whose keys are exactly 1 to n is a list,
/// any other table an object with number keys in decimal; a string that is
/// not UTF-8 is `{"bytes_b64": ...}`; nil is null.
pub fn lv_json(v: &Lv) -> Value {
    match v {
        Lv::Nil => Value::Null,
        Lv::Bool(b) => Value::Bool(*b),
        Lv::Number(n) => serde_json::Number::from_f64(*n)
            .map_or_else(|| Value::String(n.to_string()), Value::Number),
        Lv::String(s) => match std::str::from_utf8(s) {
            Ok(t) => Value::String(t.to_owned()),
            Err(_) => json!({ "bytes_b64": base64(s) }),
        },
        Lv::Table(pairs) => {
            let list = pairs
                .iter()
                .enumerate()
                .all(|(i, (k, _))| *k == Lv::Number((i + 1) as f64));
            if list && !pairs.is_empty() {
                Value::Array(pairs.iter().map(|(_, v)| lv_json(v)).collect())
            } else {
                let mut m = Map::new();
                for (k, v) in pairs {
                    let key = match k {
                        Lv::String(s) => String::from_utf8_lossy(s).into_owned(),
                        Lv::Number(n) if n.fract() == 0.0 => format!("{}", *n as i64),
                        other => match lv_json(other) {
                            Value::String(s) => s,
                            j => j.to_string(),
                        },
                    };
                    m.insert(key, lv_json(v));
                }
                Value::Object(m)
            }
        }
        Lv::Other(t) => json!({ "other": t }),
    }
}

/// Standard base 64 with padding (RFC 4648).
pub fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaigns::Campaigns;
    use crate::eventfile::{EventFile, EventRow};
    use crate::ids::CampaignId;
    use crate::store::{CampaignRecord, CampaignState};

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn exports_every_table_one_json_object_per_line() {
        let d = tempfile::tempdir().expect("dir");
        let c = Campaigns::new(d.path(), 10, 2).expect("campaigns");
        let id = CampaignId(3);
        let record = CampaignRecord {
            name: "c".into(),
            state: CampaignState::Suspended,
            theaters: vec!["Caucasus".into()],
            theater: "Caucasus".into(),
            clock_ms: None,
            created_ms: 1,
            component_schemas: Default::default(),
            outcome: None,
        };
        let mut s = c.create(id, record).expect("create");
        s.adopt_id().expect("id");
        s.commit().expect("commit");
        let mut ev = EventFile::open(&c.events_path(id), 3).expect("events");
        ev.push(EventRow {
            seq: ev.next_seq(),
            wall_ms: 9,
            source: Some("1:2".into()),
            body: vec![1, 2, 3],
        });
        ev.commit().expect("commit");
        let mut lines = Vec::new();
        export(3, &c.store_path(id), &c.events_path(id), &mut |l| {
            lines.push(l);
            true
        })
        .expect("export");
        let parsed: Vec<Value> = lines
            .iter()
            .map(|l| {
                assert!(l.ends_with(b"\n") && !l[..l.len() - 1].contains(&b'\n'));
                serde_json::from_slice(l).expect("json")
            })
            .collect();
        assert_eq!(parsed[0]["kind"], "header");
        assert_eq!(parsed[0]["campaign_id"], 3);
        let kinds: Vec<&str> = parsed.iter().map(|v| v["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"meta"));
        let campaign = parsed
            .iter()
            .find(|v| v["kind"] == "meta" && v["key"] == "campaign")
            .expect("record");
        assert_eq!(campaign["value"]["name"], "c");
        let event = parsed.last().expect("event");
        assert_eq!(event["kind"], "event");
        assert_eq!(event["event_b64"], "AQID");
        assert_eq!(event["source"], "1:2");
        // A consumer that stops early gets only what it asked for.
        let mut n = 0;
        export(3, &c.store_path(id), &c.events_path(id), &mut |_| {
            n += 1;
            n < 2
        })
        .expect("export");
        assert_eq!(n, 2);
    }
}
