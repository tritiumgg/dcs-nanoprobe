//! Tests of `inspect` and `top` on files built in heap memory.

use std::sync::atomic::Ordering;
use std::time::Duration;

use nanoprobe_transport::codec::{Lv, Writer};
use nanoprobe_transport::layout::{kind, status as s};
use nanoprobe_transport::mem::{Heap, Memory};
use nanoprobe_transport::records::{CommandHeader, CommandResult, Frame, Gap, Hello, Lifecycle};
use nanoprobe_transport::ring::{Producer, Ring, record_size};
use nanoprobe_transport::status::{Header, Metrics, start, write_metrics};

use crate::inspect::{self, Options};
use crate::top::Top;

const CAP: u64 = 256 << 10;

fn header() -> Header {
    Header {
        interface: 2,
        agent_boot: 0xabc,
        build: 0x1234,
        created_ms: 1_700_000_000_000,
        clock_hz: 1_000_000_000,
        out_capacity: CAP,
        in_capacity: CAP,
    }
}

/// A started file with five outbound records and two inbound commands.
fn fixture() -> Heap {
    let h = header();
    let m = Heap::new(h.file_size() as usize);
    start(&m, &h, 3);
    let mut out = Producer::new(Ring::outbound(CAP), 0);
    let frame = Frame {
        frame: 41,
        model_time: 12.5,
        real_time: 13.5,
        campaign: 9,
        number: 1,
        mission_gen: 2,
        lifecycle: 3,
        replay: 0,
    };
    out.push(&m, kind::FRAME as u16, &[&frame.encode()])
        .expect("push");
    let hello = Hello {
        agent_boot: 0xabc,
        sidecar_boot: 0x77,
        flags: 1,
        dcs_version: "2.9.31".into(),
        ..Hello::default()
    };
    out.push(&m, kind::HELLO as u16, &[&hello.encode()])
        .expect("push");
    let life = Lifecycle {
        state: 3,
        mission_gen: 2,
        theater: "Caucasus".into(),
        mission: "test".into(),
    };
    out.push(&m, kind::LIFECYCLE as u16, &[&life.encode()])
        .expect("push");
    let result = CommandResult {
        seq: 5,
        opcode: 16,
        code: 19,
        message: "bad".into(),
        value: Lv::Nil,
        epoch: 0,
        campaign: 9,
        entity: 0,
        rev: 0,
        object: 0,
        cost_us: 0,
    };
    out.push(&m, kind::RESULT as u16, &[&result.encode()])
        .expect("push");
    let gap = Gap {
        class: 1,
        count: 4,
        ..Gap::default()
    };
    out.push(&m, kind::GAP as u16, &[&gap.encode()])
        .expect("push");
    out.publish(&m);

    let mut inbound = Producer::new(Ring::inbound(CAP, CAP), 0);
    for (seq, campaign) in [(0u64, 9u64), (0, 10)] {
        let mut w = Writer::new();
        CommandHeader {
            seq,
            campaign,
            ..CommandHeader::default()
        }
        .write(&mut w);
        w.u64(campaign);
        inbound.push(&m, 1, &[&w.finish()]).expect("push");
    }
    inbound.publish(&m);
    m.store(s::OUT_HEAD, 0, Ordering::Relaxed);
    m.store(s::SIDECAR_HEARTBEAT, 100, Ordering::Relaxed);
    m.store(s::FRAME, 41, Ordering::Release);
    m
}

fn inspect_text(m: &Heap, records: usize) -> String {
    inspect::render(m, m.len() as u64, &Options { records })
}

#[test]
fn inspect_names_the_records_and_fields() {
    let m = fixture();
    let mut metrics = Metrics {
        slice_us: 250.0,
        ..Metrics::default()
    };
    metrics.hist[0][4] = 99;
    metrics.hist[0][9] = 1;
    write_metrics(&m, &metrics);
    let text = inspect_text(&m, 50);
    for want in [
        "header check: ok",
        "lifecycle            RUNNING(3)",
        "agent_boot           0xabc",
        "sidecar_heartbeat    100",
        "slice_us             250",
        "hist_pump            100 samples  p50 <16us  p99 <16us  max <512us",
        "FRAME",
        "frame=41",
        "HELLO",
        "dcs_version=\"2.9.31\"",
        "flags=ATTACHED(0x1)",
        "LIFECYCLE",
        "theater=\"Caucasus\"",
        "RESULT",
        "code=BAD_PAYLOAD(19)",
        "opcode=GROUP_SPAWN(16)",
        "message=\"bad\"",
        "GAP",
        "class=EVENTS(1)",
        "inbound ring",
        "CAMPAIGN",
    ] {
        assert!(text.contains(want), "missing `{want}` in:\n{text}");
    }
    assert_eq!(text.matches("CAMPAIGN").count(), 2, "{text}");
}

#[test]
fn inspect_lists_only_the_unread_records_up_to_the_limit() {
    let m = fixture();
    let first = record_size(Frame::default().encode().len());
    m.store(s::OUT_HEAD, first, Ordering::Release);
    let text = inspect_text(&m, 2);
    assert!(!text.contains("FRAME "), "{text}");
    assert!(text.contains("HELLO"), "{text}");
    assert!(text.contains("LIFECYCLE"), "{text}");
    assert!(!text.contains("class=EVENTS"), "{text}");
    assert!(text.contains("... 2 more unread records"), "{text}");
}

#[test]
fn inspect_survives_a_corrupt_record_header() {
    let m = fixture();
    let first = record_size(Frame::default().encode().len()) as usize;
    // The second record's len becomes absurd.
    m.write(s::SIZE + first, &u32::MAX.to_le_bytes());
    let text = inspect_text(&m, 50);
    assert!(text.contains("FRAME"), "{text}");
    assert!(text.contains("ring check failed: record at"), "{text}");
    assert!(text.contains("the listing ends here"), "{text}");
    assert!(!text.contains("LIFECYCLE"), "{text}");
    assert!(
        text.contains("inbound ring"),
        "the other ring is still listed"
    );
    assert!(text.contains("CAMPAIGN"), "{text}");
}

#[test]
fn inspect_survives_a_bad_ring_position() {
    let m = fixture();
    m.store(s::OUT_TAIL, 12, Ordering::Release);
    m.store(s::IN_HEAD, 4, Ordering::Release);
    let text = inspect_text(&m, 50);
    assert!(text.contains("ring check failed"), "{text}");
    assert!(text.contains("not a multiple of 8"), "{text}");
}

#[test]
fn inspect_survives_a_bad_header() {
    let m = fixture();
    m.store(s::MAGIC, 3, Ordering::Release);
    let text = inspect_text(&m, 50);
    assert!(
        text.contains("header check: FAILED: magic is 0x3"),
        "{text}"
    );
    assert!(text.contains("lifecycle"), "{text}");
    assert!(
        text.contains("FRAME"),
        "rings are listed when the geometry is sane"
    );

    m.store(s::MAGIC, s::MAGIC_VALUE, Ordering::Release);
    m.store(s::OUT_CAPACITY, 1000, Ordering::Release);
    let text = inspect_text(&m, 50);
    assert!(text.contains("out_capacity is 0x3e8"), "{text}");
    assert!(text.contains("rings: not listed"), "{text}");

    let blank = Heap::new(s::SIZE);
    let text = inspect::render(&blank, s::SIZE as u64, &Options::default());
    assert!(
        text.contains("header check: FAILED: the header is not ready"),
        "{text}"
    );
    assert!(text.contains("rings: not listed"), "{text}");
}

#[test]
fn inspect_decodes_an_unknown_kind_as_hex() {
    let m = fixture();
    let mut out = Producer::new(Ring::outbound(CAP), 0);
    out.push(&m, 77, &[&[1, 2, 3]]).expect("push");
    out.publish(&m);
    let text = inspect_text(&m, 50);
    assert!(text.contains("kind 77"), "{text}");
    assert!(text.contains("layout unknown"), "{text}");
}

#[test]
fn top_renders_one_screen() {
    let m = fixture();
    m.store32(s::FLAGS, 1, Ordering::Relaxed);
    m.store(s::AGENT_BOOT, 0xabc, Ordering::Relaxed);
    m.store(s::CRUMB, 0x20, Ordering::Relaxed);
    let mut top = Top::new("test-file");
    let text = top.render(&m, m.len() as u64, Duration::ZERO);
    for want in [
        "nanoprobe top  test-file",
        "lifecycle        RUNNING(3)",
        "attached         yes",
        "ATTACHED(0x1)",
        "agent boot       0xabc",
        "frame            41",
        "pump time",
        "frame interval",
        "outbound ring",
        "inbound ring",
        "samples_dropped",
        "commands_applied",
        "control queued",
        "event queue",
        "lua heap",
        "heartbeat        100   first sample",
        "breadcrumb       0x20",
    ] {
        assert!(text.contains(want), "missing `{want}` in:\n{text}");
    }
    assert!(text.contains('%'), "{text}");
}

#[test]
fn top_computes_rates_from_two_refreshes() {
    let m = fixture();
    let mut top = Top::new("f");
    let mut metrics = Metrics::default();
    metrics.hist[0][3] = 10;
    write_metrics(&m, &metrics);
    top.render(&m, m.len() as u64, Duration::from_secs(1));

    m.store(s::FRAME, 141, Ordering::Release);
    m.store(s::SAMPLES_DROPPED, 20, Ordering::Relaxed);
    m.store(s::SIDECAR_HEARTBEAT, 101, Ordering::Relaxed);
    metrics.hist[0][3] = 10;
    metrics.hist[0][8] = 5;
    write_metrics(&m, &metrics);
    let text = top.render(&m, m.len() as u64, Duration::from_secs(3));
    assert!(text.contains("141   50.0 frames/s"), "{text}");
    assert!(text.contains("samples_dropped  20   10.0/s"), "{text}");
    // Only the five new samples are in the window.
    assert!(
        text.contains("p50 <256us  p99 <256us  max <256us"),
        "{text}"
    );
    assert!(text.contains("changed since the last refresh"), "{text}");

    let text = top.render(&m, m.len() as u64, Duration::from_millis(5500));
    assert!(text.contains("unchanged for 2.5 s"), "{text}");
    assert!(text.contains("0.0 frames/s"), "{text}");
}

#[test]
fn top_resets_when_the_agent_restarts() {
    let m = fixture();
    let mut top = Top::new("f");
    top.render(&m, m.len() as u64, Duration::from_secs(1));
    m.store(s::AGENT_BOOT, 0x999, Ordering::Release);
    m.store(s::FRAME, 3, Ordering::Release);
    let text = top.render(&m, m.len() as u64, Duration::from_secs(2));
    assert!(text.contains("3   - frames/s"), "{text}");
}

#[test]
fn top_reports_a_bad_header_and_still_renders() {
    let m = Heap::new(s::SIZE);
    let mut top = Top::new("f");
    let text = top.render(&m, s::SIZE as u64, Duration::ZERO);
    assert!(text.contains("header check: FAILED"), "{text}");
    assert!(text.contains("lifecycle"), "{text}");
    assert!(text.contains("capacity 0"), "{text}");
}

#[test]
fn run_reports_usage_and_help() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(crate::run(&[], &mut out, &mut err), 2);
    assert!(String::from_utf8_lossy(&err).contains("Usage:"));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(crate::run(&["--help".into()], &mut out, &mut err), 0);
    assert!(String::from_utf8_lossy(&out).contains("nanoprobe inspect"));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let args = ["inspect".to_string(), "/no/such/file".to_string()];
    assert_eq!(crate::run(&args, &mut out, &mut err), 1);
    assert!(String::from_utf8_lossy(&err).contains("cannot open"));
}

/// A file whose producer runs a whole ring ahead while the rings are copied.
struct Racing {
    heap: Heap,
    bumped: std::cell::Cell<bool>,
}

impl Memory for Racing {
    fn len(&self) -> usize {
        self.heap.len()
    }
    fn load(&self, off: usize, order: Ordering) -> u64 {
        self.heap.load(off, order)
    }
    fn store(&self, off: usize, value: u64, order: Ordering) {
        self.heap.store(off, value, order)
    }
    fn load32(&self, off: usize, order: Ordering) -> u32 {
        self.heap.load32(off, order)
    }
    fn store32(&self, off: usize, value: u32, order: Ordering) {
        self.heap.store32(off, value, order)
    }
    fn fence(&self, order: Ordering) {
        self.heap.fence(order)
    }
    fn read(&self, off: usize, out: &mut [u8]) {
        if off >= s::SIZE && !self.bumped.replace(true) {
            let head = self.heap.load(s::OUT_HEAD, Ordering::Relaxed);
            self.heap
                .store(s::OUT_TAIL, head + CAP + 8, Ordering::Relaxed);
        }
        self.heap.read(off, out)
    }
    fn write(&self, off: usize, data: &[u8]) {
        self.heap.write(off, data)
    }
    fn zero(&self, off: usize, n: usize) {
        self.heap.zero(off, n)
    }
}

#[test]
fn inspect_does_not_list_records_written_over_while_copied() {
    // The snapshot is sane; the producer moves a ring ahead during the copy.
    let racing = Racing {
        heap: fixture(),
        bumped: std::cell::Cell::new(false),
    };
    let text = inspect::render(&racing, racing.len() as u64, &Options { records: 50 });
    assert!(text.contains("wrote over unread records"), "{text}");
    // The same file with no race lists its records.
    let text = inspect_text(&fixture(), 50);
    assert!(
        !text.contains("wrote over") && text.contains("FRAME"),
        "{text}"
    );
}
