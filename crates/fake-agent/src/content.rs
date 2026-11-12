//! What the fake agent writes and the stub sends, made so the verifier can
//! check every byte: each value is a function of the agent's boot and a
//! sequence number or frame.

use nanoprobe_transport::codec::{DecodeError, Lv, Reader, Writer};
use nanoprobe_transport::records::Log;

/// The opcode the stub sends: `MESSAGE`, an effect that runs with no campaign.
pub const OPCODE: u64 = 49;

/// Wall time, milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A 64-bit mix (SplitMix64's finalizer).
pub fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A check value that an `f64` holds exactly.
pub fn check(boot: u64, seq: u64) -> f64 {
    (mix(boot ^ mix(seq)) >> 11) as f64
}

/// A digest of a command payload that an `f64` holds exactly (FNV-1a).
pub fn digest(bytes: &[u8]) -> f64 {
    let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    (h >> 11) as f64
}

fn table(seq: u64, boot: u64) -> Lv {
    Lv::Table(vec![
        (Lv::String(b"seq".to_vec()), Lv::Number(seq as f64)),
        (Lv::String(b"check".to_vec()), Lv::Number(check(boot, seq))),
    ])
}

/// The `AUTHOR_EVENT` payload of event `seq`.
pub fn event(boot: u64, seq: u64) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(0).pad(4).str("fake.event").lv(&table(seq, boot));
    w.finish()
}

/// The `LOG` of log line `seq`.
pub fn log(boot: u64, seq: u64) -> Log {
    Log {
        level: 3,
        target: "fake-agent".into(),
        msg: format!("line {seq}"),
        data: table(seq, !boot),
    }
}

/// The `POSITIONS` payload of `frame`: 1 to 128 rows, 2 KiB on average, so
/// that a boot that lives a few seconds wraps a 256 KiB ring.
pub fn positions(boot: u64, frame: u64) -> Vec<u8> {
    let n = 1 + frame % 128;
    let mut w = Writer::new();
    w.u32(n as u32).pad(4);
    for i in 0..n {
        w.u64(frame * 16 + i);
    }
    for i in 0..n {
        w.f64((frame + i) as f64);
    }
    for _ in 0..n {
        w.f64((boot >> 11) as f64);
    }
    for i in 0..n {
        w.f64(-((frame + i) as f64));
    }
    w.finish()
}

fn seq_of(v: &Lv, boot: u64) -> Result<u64, String> {
    let Lv::Table(pairs) = v else {
        return Err("the value is not a table".into());
    };
    let [
        (Lv::String(k1), Lv::Number(seq)),
        (Lv::String(k2), Lv::Number(c)),
    ] = &pairs[..]
    else {
        return Err("the table does not hold seq and check".into());
    };
    if k1 != b"seq" || k2 != b"check" {
        return Err("the table's keys are wrong".into());
    }
    let seq = *seq as u64;
    if *c != check(boot, seq) {
        return Err(format!("check value wrong for seq {seq}"));
    }
    Ok(seq)
}

fn whole(r: &Reader<'_>) -> Result<(), String> {
    if r.remaining() == 0 {
        Ok(())
    } else {
        Err(format!("{} bytes after the payload", r.remaining()))
    }
}

fn de(e: DecodeError) -> String {
    e.to_string()
}

/// Check an `AUTHOR_EVENT` payload; its sequence number.
pub fn check_event(boot: u64, payload: &[u8]) -> Result<u64, String> {
    let mut r = Reader::new(payload);
    if r.u32().map_err(de)? != 0 {
        return Err("objects in a fake event".into());
    }
    r.skip(4).map_err(de)?;
    if r.str().map_err(de)? != "fake.event" {
        return Err("the event's name is wrong".into());
    }
    let seq = seq_of(&r.lv().map_err(de)?, boot)?;
    whole(&r)?;
    Ok(seq)
}

/// Check a `LOG` payload; its sequence number.
pub fn check_log(boot: u64, payload: &[u8]) -> Result<u64, String> {
    let l = Log::decode(payload).map_err(de)?;
    let seq = seq_of(&l.data, !boot)?;
    let want = log(boot, seq);
    if l != want {
        return Err(format!("log line {seq} differs"));
    }
    if l.encode() != payload {
        return Err(format!("log line {seq} has bytes after it"));
    }
    Ok(seq)
}

/// Check a `POSITIONS` payload against its frame.
pub fn check_positions(boot: u64, frame: u64, payload: &[u8]) -> Result<(), String> {
    if payload == positions(boot, frame) {
        Ok(())
    } else {
        Err(format!("positions of frame {frame} differ"))
    }
}

/// A command's payload after its header: 8 to 4,103 bytes from `seed`.
pub fn command_body(seed: u64) -> Vec<u8> {
    // Up to 4 KiB, so that a boot that lives a few seconds wraps a 256 KiB
    // inbound ring.
    let n = 8 + (mix(seed) % 4096) as usize;
    (0..n).map(|i| mix(seed ^ (i as u64) << 32) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents_check_and_tampering_is_found() {
        let e = event(7, 3);
        assert_eq!(check_event(7, &e), Ok(3));
        assert!(check_event(8, &e).is_err(), "another boot");
        let mut bad = e.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(check_event(7, &bad).is_err());
        let l = log(7, 9).encode();
        assert_eq!(check_log(7, &l), Ok(9));
        assert!(check_log(7, &e).is_err());
        let p = positions(7, 12);
        assert!(check_positions(7, 12, &p).is_ok());
        assert!(check_positions(7, 13, &p).is_err());
    }
}
