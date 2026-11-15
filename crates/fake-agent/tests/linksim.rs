//! The command-path adapter over the real link, against the fake agent: its
//! predicted ring positions hold across ring wraps, and a sidecar restart
//! drops what it had not written.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nanoprobe_fake_agent::agent::{Agent, Options};
use nanoprobe_fake_agent::linksim::LinkSim;
use nanoprobe_sidecar::link::Config;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_transport::codec::Writer;
use nanoprobe_transport::layout::{kind, opcode, result};
use nanoprobe_transport::records::{CommandHeader, CommandResult};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn message(seq: u64, epoch: u64, filler: usize) -> Vec<u8> {
    let mut w = Writer::new();
    CommandHeader {
        seq,
        epoch,
        campaign: 0,
        entity: 0,
        rev: 0,
        mission_gen: 0,
    }
    .write(&mut w);
    let mut b = w.finish();
    // The fake agent answers MESSAGE without reading its body.
    b.extend(std::iter::repeat_n(0x5a, filler));
    b
}

/// Read until `want` results arrive or 20 s pass; results by (epoch, seq).
fn results(sim: &mut LinkSim, into: &mut BTreeMap<(u64, u64), Vec<u16>>, want: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while into.values().map(Vec::len).sum::<usize>() < want && Instant::now() < deadline {
        for r in sim.read() {
            if u64::from(r.kind) == kind::RESULT {
                let c = CommandResult::decode(&r.payload).expect("a RESULT");
                into.entry((c.epoch, c.seq)).or_default().push(c.code);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_ready(sim: &LinkSim) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !sim.ready() {
        assert!(
            Instant::now() < deadline,
            "no HELLO: {:?}",
            sim.take_reports()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A scratch folder, removed when the test ends, whether it passes or fails.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn predictions_hold_across_wraps_and_a_restart_drops_the_unwritten() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("nanoprobe-linksim-{}", std::process::id())));
    let dir = scratch.0.clone();
    std::fs::create_dir_all(&dir).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let map = dir.join("link.map");
    let mut opts = Options::new(map.clone(), port, TOKEN.into());
    opts.commands_per_frame = 64;
    let mut agent = Agent::open(opts).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let agent_thread = std::thread::spawn(move || agent.run(&s, |_| {}));
    let cfg = Config {
        map_path: map,
        handshake: ([127, 0, 0, 1], port).into(),
        token: TOKEN.into(),
        poll: Duration::from_millis(1),
        handshake_timeout: Duration::from_secs(15),
    };

    // 3,000 commands of about 600 bytes pass the 256 KiB ring several times.
    let mut got = BTreeMap::new();
    let mut sim = LinkSim::start(cfg.clone(), Default::default());
    wait_ready(&sim);
    for seq in 1..=3000u64 {
        let p = message(seq, 1, 520 + (seq as usize % 97));
        let want = sim.position_after(opcode::MESSAGE as u16, &p);
        let end = sim.send(opcode::MESSAGE as u16, &p).expect("attached");
        sim.flush();
        assert_eq!(want, end);
        if seq % 200 == 0 {
            results(&mut sim, &mut got, seq as usize);
        }
    }
    results(&mut sim, &mut got, 3000);
    assert_eq!(sim.mispredicted(), 0, "{:?}", sim.take_reports());
    assert_eq!(got.len(), 3000);
    assert!(got.values().all(|c| c == &[result::APPLIED as u16]));

    // Queue more than the ring holds, then restart the sidecar at once.
    for seq in 3001..=4000u64 {
        sim.send(opcode::MESSAGE as u16, &message(seq, 1, 600))
            .expect("attached");
        sim.flush();
    }
    let left = sim.queued();
    let last = sim.last_numbers();
    // Read again at the drop: what is still queued then must be lost.
    let queued_at_drop = sim.queued();
    drop(sim);
    let mut sim = LinkSim::start(cfg, last);
    wait_ready(&sim);
    assert_eq!(sim.queued(), 0);
    for seq in 1..=50u64 {
        sim.send(opcode::MESSAGE as u16, &message(seq, 2, 64))
            .expect("attached");
        sim.flush();
    }
    // Wait for the 50 epoch-2 results: they come after every written epoch-1
    // result, and more than `1000 - left` of those may have been written.
    let deadline = Instant::now() + Duration::from_secs(20);
    while got.range((2, 0)..(3, 0)).count() < 50 && Instant::now() < deadline {
        let total = got.values().map(Vec::len).sum::<usize>();
        results(&mut sim, &mut got, total + 1);
    }
    stop.store(true, Ordering::Relaxed);
    agent_thread.join().unwrap();
    assert_eq!(sim.mispredicted(), 0, "{:?}", sim.take_reports());
    // Every command has at most one result; the new epoch's all came.
    assert!(got.values().all(|c| c.len() == 1));
    assert_eq!(got.range((2, 0)..(3, 0)).count(), 50);
    // The restart drops what the adapter had not written and nothing else.
    // The epoch-1 results the old adapter read before the restart died with
    // it, so they are a gap at the start. The rest are contiguous (a dropped
    // command in the middle would leave a gap), and they reach at least the
    // `1000 - left` commands written when `left` was read: the adapter kept
    // writing after that, so more may follow. The epoch-2 results come after
    // every written epoch-1 result in the ring, so the count is final.
    let seqs: Vec<u64> = got
        .range((1, 3001)..(1, 4001))
        .map(|((_, q), _)| *q)
        .collect();
    let last = *seqs.last().expect("results after the restart's start");
    assert!(last >= 4000 - left as u64, "last {last}, {left} left");
    // What was queued at the drop did not reach the ring. The adapter may
    // write a few commands between the read and the drop, so the check
    // needs a margin of 100: with that many queued, seq 4000 cannot have
    // been written.
    if queued_at_drop >= 100 {
        assert!(
            last < 4000,
            "last {last}: {queued_at_drop} were queued at the drop and none was lost"
        );
    }
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "a gap in {seqs:?}"
    );
}
