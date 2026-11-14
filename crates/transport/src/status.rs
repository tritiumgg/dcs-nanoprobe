//! The status block: the agent's Start, the sidecar's header check, snapshots
//! and the metrics sequence lock (spec/transport.md).

use core::fmt;
use core::sync::atomic::Ordering;

use crate::golden::LAYOUT_HASH;
use crate::layout::{CAPACITY, Writer, status as s};
use crate::mem::Memory;

/// What the agent writes in the header when it opens in a new DCS process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// The agent DLL's interface version.
    pub interface: u32,
    /// A random non-zero value for this DCS process.
    pub agent_boot: u64,
    /// The first 16 hex digits of the install's build, as a number.
    pub build: u64,
    /// Wall time, milliseconds since the Unix epoch.
    pub created_ms: u64,
    /// Ticks per second of the agent's clock.
    pub clock_hz: u64,
    /// Bytes in the outbound ring.
    pub out_capacity: u64,
    /// Bytes in the inbound ring.
    pub in_capacity: u64,
}

impl Header {
    /// Bytes in use: the status block and the two rings.
    pub fn file_size(&self) -> u64 {
        s::SIZE as u64 + self.out_capacity + self.in_capacity
    }
}

/// The agent's Start (spec/transport.md, "Start"): `ready` 0, every header
/// field, every other agent field and pad zeroed, then `ready` 1. No sidecar field is
/// touched. The memory must hold at least `header.file_size()` bytes.
pub fn start(m: &impl Memory, header: &Header, lifecycle: u32) {
    assert!(
        m.len() as u64 >= header.file_size(),
        "the mapping holds the rings"
    );
    m.store(s::READY, 0, Ordering::Relaxed);
    m.fence(Ordering::Release);
    m.store(s::MAGIC, s::MAGIC_VALUE, Ordering::Relaxed);
    m.store32(
        s::LAYOUT_VERSION,
        s::LAYOUT_VERSION_VALUE,
        Ordering::Relaxed,
    );
    m.store32(s::INTERFACE, header.interface, Ordering::Relaxed);
    m.store(s::LAYOUT_HASH, LAYOUT_HASH, Ordering::Relaxed);
    m.store(s::FILE_SIZE, header.file_size(), Ordering::Relaxed);
    m.store(s::AGENT_BOOT, header.agent_boot, Ordering::Relaxed);
    m.store(s::BUILD, header.build, Ordering::Relaxed);
    m.store(s::CREATED_MS, header.created_ms, Ordering::Relaxed);
    m.store(s::CLOCK_HZ, header.clock_hz, Ordering::Relaxed);
    m.store(s::OUT_OFFSET, s::SIZE as u64, Ordering::Relaxed);
    m.store(s::OUT_CAPACITY, header.out_capacity, Ordering::Relaxed);
    m.store(
        s::IN_OFFSET,
        s::SIZE as u64 + header.out_capacity,
        Ordering::Relaxed,
    );
    m.store(s::IN_CAPACITY, header.in_capacity, Ordering::Relaxed);
    for &(_, off, width, count, writer) in s::FIELDS {
        if writer == Writer::Agent && off >= s::FRAME {
            for i in 0..count {
                match width {
                    8 => m.store(off + 8 * i, 0, Ordering::Relaxed),
                    _ => m.store32(off + 4 * i, 0, Ordering::Relaxed),
                }
            }
        }
    }
    for &(off, n) in s::AGENT_PADS {
        m.zero(off, n);
    }
    m.store32(s::LIFECYCLE, lifecycle, Ordering::Relaxed);
    m.store(s::READY, 1, Ordering::Release);
}

/// The file's geometry, from a header that passed [`check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// The agent's boot.
    pub agent_boot: u64,
    /// The interface version.
    pub interface: u32,
    /// Bytes in the outbound ring.
    pub out_capacity: u64,
    /// Bytes in the inbound ring.
    pub in_capacity: u64,
    /// Bytes in use.
    pub file_size: u64,
}

/// A header that fails the sidecar's check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// `ready` is not 1: the agent is writing the header, or none was written.
    NotReady,
    /// A field holds a value this sidecar does not accept.
    Field {
        /// The field.
        field: &'static str,
        /// Its value.
        value: u64,
    },
    /// `ready` or `agent_boot` changed while the header was read.
    Changed,
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderError::NotReady => write!(f, "the header is not ready"),
            HeaderError::Field { field, value } => write!(f, "{field} is {value:#x}"),
            HeaderError::Changed => write!(f, "the header changed while it was read"),
        }
    }
}

impl std::error::Error for HeaderError {}

/// The sidecar's check of the header after `OK` (spec/transport.md,
/// "Attaching", step 1), against a file of `file_len` bytes.
pub fn check(m: &impl Memory, file_len: u64) -> Result<Geometry, HeaderError> {
    let ready = m.load(s::READY, Ordering::Acquire);
    let boot = m.load(s::AGENT_BOOT, Ordering::Relaxed);
    if ready != 1 {
        return Err(HeaderError::NotReady);
    }
    let field = |field, value| Err(HeaderError::Field { field, value });
    let magic = m.load(s::MAGIC, Ordering::Relaxed);
    if magic != s::MAGIC_VALUE {
        return field("magic", magic);
    }
    let version = m.load32(s::LAYOUT_VERSION, Ordering::Relaxed);
    if version != s::LAYOUT_VERSION_VALUE {
        return field("layout_version", version.into());
    }
    let hash = m.load(s::LAYOUT_HASH, Ordering::Relaxed);
    if hash != LAYOUT_HASH {
        return field("layout_hash", hash);
    }
    let out_capacity = m.load(s::OUT_CAPACITY, Ordering::Relaxed);
    let in_capacity = m.load(s::IN_CAPACITY, Ordering::Relaxed);
    for (name, c) in [("out_capacity", out_capacity), ("in_capacity", in_capacity)] {
        if !c.is_power_of_two() || !CAPACITY.contains(&c) {
            return field(name, c);
        }
    }
    let out_offset = m.load(s::OUT_OFFSET, Ordering::Relaxed);
    if out_offset != s::SIZE as u64 {
        return field("out_offset", out_offset);
    }
    let in_offset = m.load(s::IN_OFFSET, Ordering::Relaxed);
    if in_offset != s::SIZE as u64 + out_capacity {
        return field("in_offset", in_offset);
    }
    let file_size = m.load(s::FILE_SIZE, Ordering::Relaxed);
    if file_size != s::SIZE as u64 + out_capacity + in_capacity
        || file_size > file_len
        || file_size > m.len() as u64
    {
        return field("file_size", file_size);
    }
    if boot == 0 {
        return field("agent_boot", 0);
    }
    let interface = m.load32(s::INTERFACE, Ordering::Relaxed);
    m.fence(Ordering::Acquire);
    if m.load(s::READY, Ordering::Relaxed) != 1 || m.load(s::AGENT_BOOT, Ordering::Relaxed) != boot
    {
        return Err(HeaderError::Changed);
    }
    Ok(Geometry {
        agent_boot: boot,
        interface,
        out_capacity,
        in_capacity,
        file_size,
    })
}

/// A private copy of the whole status block, read field by field with atomic
/// loads of each field's width.
#[derive(Clone)]
pub struct Snapshot {
    bytes: Box<[u8; s::SIZE]>,
}

impl Snapshot {
    /// Copy every field. The copy is not consistent by itself; see
    /// [`Snapshot::read`].
    pub fn copy(m: &impl Memory) -> Snapshot {
        let mut bytes = Box::new([0u8; s::SIZE]);
        for &(_, off, width, count, _) in s::FIELDS {
            for i in 0..count {
                let at = off + width * i;
                if width == 8 {
                    let v = m.load(at, Ordering::Relaxed);
                    bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
                } else {
                    let v = m.load32(at, Ordering::Relaxed);
                    bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        Snapshot { bytes }
    }

    /// The sidecar's snapshot (spec/transport.md, "Detaching"): load `ready`
    /// and `agent_boot`, copy, fence, load them again. `None` unless both loads
    /// agree and `ready` is 1. The metrics fields in it are not consistent;
    /// read them with [`read_metrics`].
    pub fn read(m: &impl Memory) -> Option<Snapshot> {
        let ready = m.load(s::READY, Ordering::Acquire);
        let boot = m.load(s::AGENT_BOOT, Ordering::Acquire);
        let snap = Snapshot::copy(m);
        m.fence(Ordering::Acquire);
        let again = (
            m.load(s::READY, Ordering::Relaxed),
            m.load(s::AGENT_BOOT, Ordering::Relaxed),
        );
        (ready == 1 && again == (ready, boot)).then_some(snap)
    }

    /// The `u64` at `off`.
    pub fn u64(&self, off: usize) -> u64 {
        u64::from_le_bytes(self.bytes[off..off + 8].try_into().expect("8 bytes"))
    }

    /// The `u32` at `off`.
    pub fn u32(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.bytes[off..off + 4].try_into().expect("4 bytes"))
    }

    /// The `f64` at `off`.
    pub fn f64(&self, off: usize) -> f64 {
        f64::from_bits(self.u64(off))
    }

    /// The raw bytes.
    pub fn bytes(&self) -> &[u8; s::SIZE] {
        &self.bytes
    }
}

/// The metrics fields as the agent writes them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    /// The time slice for the current frame, µs.
    pub slice_us: f64,
    /// Running average interval between pump calls, µs.
    pub frame_interval_us: f64,
    /// Achieved periods: position, vitals, ammunition, warehouse, enumeration.
    pub periods: [f64; 5],
    /// Rows the sampling loop passes between clock reads.
    pub rows_per_clock: u64,
    /// The histograms: pump, interval, command, event, system-call frame.
    pub hist: [[u64; s::HIST_BUCKETS]; 5],
}

const PERIODS: [usize; 5] = [
    s::PERIOD_POSITION,
    s::PERIOD_VITALS,
    s::PERIOD_AMMO,
    s::PERIOD_WAREHOUSE,
    s::PERIOD_ENUMERATION,
];
const HISTS: [usize; 5] = [
    s::HIST_PUMP,
    s::HIST_INTERVAL,
    s::HIST_COMMAND,
    s::HIST_EVENT,
    s::HIST_SYSCALL,
];

/// Write under a sequence lock at `seq_off` (spec/transport.md, "Metrics"):
/// store the sequence plus one (odd), a release fence, `write`, then the
/// sequence plus two (even) with release ordering. One writer only.
pub fn seqlock_write<M: Memory>(m: &M, seq_off: usize, write: impl FnOnce(&M)) {
    let seq = m.load(seq_off, Ordering::Relaxed);
    m.store(seq_off, seq.wrapping_add(1), Ordering::Relaxed);
    m.fence(Ordering::Release);
    write(m);
    m.store(seq_off, seq.wrapping_add(2), Ordering::Release);
}

/// Read under a sequence lock at `seq_off`: load the sequence (acquire),
/// `read` with relaxed loads, an acquire fence, and load the sequence again.
/// `None` unless both loads are the same even value.
pub fn seqlock_read<M: Memory, T>(m: &M, seq_off: usize, read: impl FnOnce(&M) -> T) -> Option<T> {
    let seq = m.load(seq_off, Ordering::Acquire);
    if seq % 2 == 1 {
        return None;
    }
    let out = read(m);
    m.fence(Ordering::Acquire);
    (m.load(seq_off, Ordering::Relaxed) == seq).then_some(out)
}

/// Write the metrics under their sequence lock.
pub fn write_metrics(m: &impl Memory, metrics: &Metrics) {
    seqlock_write(m, s::METRICS_SEQ, |m| {
        m.store(s::SLICE_US, metrics.slice_us.to_bits(), Ordering::Relaxed);
        m.store(
            s::FRAME_INTERVAL_US,
            metrics.frame_interval_us.to_bits(),
            Ordering::Relaxed,
        );
        for (off, v) in PERIODS.iter().zip(metrics.periods) {
            m.store(*off, v.to_bits(), Ordering::Relaxed);
        }
        m.store(s::ROWS_PER_CLOCK, metrics.rows_per_clock, Ordering::Relaxed);
        for (off, h) in HISTS.iter().zip(&metrics.hist) {
            for (i, v) in h.iter().enumerate() {
                m.store(off + 8 * i, *v, Ordering::Relaxed);
            }
        }
    });
}

/// Write the metrics that differ from `last`, under their sequence lock, and
/// bring `last` up to date. `last` is the writer's own copy of what the block
/// holds: one made with [`Metrics::default`] matches a block just started,
/// and the same copy goes to every call. A frame changes a few of the 170
/// words, and each word stored is a cache line the reader's core must fetch
/// again.
pub fn write_metrics_changed(m: &impl Memory, metrics: &Metrics, last: &mut Metrics) {
    // By bits: a period may be NaN, which is not equal to itself.
    let bits = |a: f64, b: f64| a.to_bits() == b.to_bits();
    if bits(metrics.slice_us, last.slice_us)
        && bits(metrics.frame_interval_us, last.frame_interval_us)
        && metrics
            .periods
            .iter()
            .zip(last.periods)
            .all(|(a, b)| bits(*a, b))
        && metrics.rows_per_clock == last.rows_per_clock
        && metrics.hist == last.hist
    {
        return;
    }
    seqlock_write(m, s::METRICS_SEQ, |m| {
        let float = |off, new: f64, old: f64| {
            if new.to_bits() != old.to_bits() {
                m.store(off, new.to_bits(), Ordering::Relaxed);
            }
        };
        float(s::SLICE_US, metrics.slice_us, last.slice_us);
        float(
            s::FRAME_INTERVAL_US,
            metrics.frame_interval_us,
            last.frame_interval_us,
        );
        for ((off, new), old) in PERIODS.iter().zip(metrics.periods).zip(last.periods) {
            float(*off, new, old);
        }
        if metrics.rows_per_clock != last.rows_per_clock {
            m.store(s::ROWS_PER_CLOCK, metrics.rows_per_clock, Ordering::Relaxed);
        }
        for ((off, new), old) in HISTS.iter().zip(&metrics.hist).zip(&last.hist) {
            for (i, (new, old)) in new.iter().zip(old).enumerate() {
                if new != old {
                    m.store(off + 8 * i, *new, Ordering::Relaxed);
                }
            }
        }
    });
    last.clone_from(metrics);
}

/// Read the metrics under their sequence lock. `None` when the agent was
/// writing them; try again on the next poll.
pub fn read_metrics(m: &impl Memory) -> Option<Metrics> {
    seqlock_read(m, s::METRICS_SEQ, |m| {
        let f = |off| f64::from_bits(m.load(off, Ordering::Relaxed));
        let mut out = Metrics {
            slice_us: f(s::SLICE_US),
            frame_interval_us: f(s::FRAME_INTERVAL_US),
            periods: PERIODS.map(f),
            rows_per_clock: m.load(s::ROWS_PER_CLOCK, Ordering::Relaxed),
            hist: [[0; s::HIST_BUCKETS]; 5],
        };
        for (off, h) in HISTS.iter().zip(&mut out.hist) {
            for (i, v) in h.iter_mut().enumerate() {
                *v = m.load(off + 8 * i, Ordering::Relaxed);
            }
        }
        out
    })
}

/// The histogram bucket of a value in µs (spec/transport.md, "Metrics").
pub fn bucket(us: f64) -> usize {
    if us.is_nan() || us < 1.0 {
        return 0;
    }
    // 2^(i-1) <= us < 2^i is bucket i.
    let i = us.log2().floor() as usize + 1;
    i.min(s::HIST_BUCKETS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::Heap;

    fn header() -> Header {
        Header {
            interface: 3,
            agent_boot: 0x1234,
            build: 7,
            created_ms: 9,
            clock_hz: 1_000_000_000,
            out_capacity: 256 << 10,
            in_capacity: 256 << 10,
        }
    }

    #[test]
    fn a_started_header_passes_the_check() {
        let h = header();
        let m = Heap::new(h.file_size() as usize);
        m.store(s::OUT_HEAD, 99, Ordering::Relaxed);
        m.store(s::FRAME, 5, Ordering::Relaxed);
        m.write(0x680, &[7; 16]);
        start(&m, &h, 1);
        let mut pad = [1u8; 16];
        m.read(0x680, &mut pad);
        assert_eq!(pad, [0; 16], "an agent pad is zeroed");
        assert_eq!(
            m.load(s::OUT_HEAD, Ordering::Relaxed),
            99,
            "a sidecar field is untouched"
        );
        assert_eq!(m.load(s::FRAME, Ordering::Relaxed), 0);
        assert_eq!(m.load32(s::LIFECYCLE, Ordering::Relaxed), 1);
        let g = check(&m, h.file_size()).expect("a good header");
        assert_eq!(g.agent_boot, 0x1234);
        assert_eq!(g.file_size, h.file_size());
        assert!(matches!(
            check(&m, h.file_size() - 1),
            Err(HeaderError::Field {
                field: "file_size",
                ..
            })
        ));
    }

    #[test]
    fn a_bad_header_fails_the_check() {
        let h = header();
        let m = Heap::new(h.file_size() as usize);
        assert_eq!(check(&m, h.file_size()), Err(HeaderError::NotReady));
        for (off, name) in [
            (s::MAGIC, "magic"),
            (s::LAYOUT_HASH, "layout_hash"),
            (s::OUT_CAPACITY, "out_capacity"),
            (s::IN_OFFSET, "in_offset"),
            (s::FILE_SIZE, "file_size"),
            (s::AGENT_BOOT, "agent_boot"),
        ] {
            start(&m, &h, 1);
            m.store(
                off,
                if off == s::AGENT_BOOT { 0 } else { 3 },
                Ordering::Relaxed,
            );
            match check(&m, h.file_size()) {
                Err(HeaderError::Field { field, .. }) => assert_eq!(field, name),
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    #[test]
    fn metrics_round_trip_and_an_odd_sequence_is_refused() {
        let m = Heap::new(s::SIZE);
        let mut metrics = Metrics {
            slice_us: 250.0,
            periods: [1.0, 5.0, 30.0, 60.0, 30.0],
            ..Metrics::default()
        };
        metrics.hist[2][7] = 11;
        write_metrics(&m, &metrics);
        assert_eq!(read_metrics(&m), Some(metrics));
        m.store(s::METRICS_SEQ, 3, Ordering::Relaxed);
        assert_eq!(read_metrics(&m), None);
    }

    #[test]
    fn storing_only_the_changed_metrics_leaves_the_block_as_a_full_store_does() {
        let (changed, full) = (Heap::new(s::SIZE), Heap::new(s::SIZE));
        let mut last = Metrics::default();
        let mut metrics = Metrics::default();
        // A seeded walk over every field, each step compared with a full
        // store of the same metrics.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for step in 0..500 {
            match next() % 5 {
                0 => metrics.slice_us = (next() % 250) as f64,
                1 => metrics.frame_interval_us = (next() % 20_000) as f64,
                2 => metrics.periods[(next() % 5) as usize] = (next() % 600) as f64 / 7.0,
                3 => metrics.rows_per_clock = next() % 64,
                _ => metrics.hist[(next() % 5) as usize][(next() % 32) as usize] += 1,
            }
            if step % 7 == 0 {
                // A period may be infinite or NaN; both are stored as given.
                metrics.periods[0] = if step % 14 == 0 {
                    f64::INFINITY
                } else {
                    f64::NAN
                };
            }
            let before = changed.load(s::METRICS_SEQ, Ordering::Relaxed);
            write_metrics_changed(&changed, &metrics, &mut last);
            write_metrics(&full, &metrics);
            // One write, or none when the step changed nothing.
            let after = changed.load(s::METRICS_SEQ, Ordering::Relaxed);
            assert!(
                after == before || after == before + 2,
                "{before} then {after}"
            );
            for off in (0..s::SIZE).step_by(8).filter(|o| *o != s::METRICS_SEQ) {
                assert_eq!(
                    changed.load(off, Ordering::Relaxed),
                    full.load(off, Ordering::Relaxed),
                    "the word at {off:#x} after step {step}"
                );
            }
        }
        // Nothing changed: nothing is stored, the sequence included.
        let before = changed.load(s::METRICS_SEQ, Ordering::Relaxed);
        write_metrics_changed(&changed, &metrics, &mut last);
        assert_eq!(changed.load(s::METRICS_SEQ, Ordering::Relaxed), before);
    }

    #[test]
    fn a_snapshot_needs_ready() {
        let h = header();
        let m = Heap::new(h.file_size() as usize);
        assert!(Snapshot::read(&m).is_none());
        start(&m, &h, 3);
        let snap = Snapshot::read(&m).expect("ready");
        assert_eq!(snap.u64(s::AGENT_BOOT), 0x1234);
        assert_eq!(snap.u32(s::LIFECYCLE), 3);
    }

    #[test]
    fn buckets_follow_the_powers_of_two() {
        assert_eq!(bucket(0.5), 0);
        assert_eq!(bucket(1.0), 1);
        assert_eq!(bucket(1.9), 1);
        assert_eq!(bucket(2.0), 2);
        assert_eq!(bucket(1023.0), 10);
        assert_eq!(bucket(1e12), 31);
        assert_eq!(bucket(f64::NAN), 0);
    }
}
