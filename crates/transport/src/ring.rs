//! The two single-producer, single-consumer rings (spec/transport.md, "Rings").
//!
//! A position is a byte count that only grows; it maps to the offset
//! `position & (capacity - 1)`. The producer owns the tail and the consumer the
//! head. Each side keeps its own position in private memory and stores it once
//! per batch, and keeps a checked copy of the other side's.

use core::fmt;
use core::sync::atomic::Ordering;

use crate::layout::{kind, record, status};
use crate::mem::Memory;

/// Where one ring lives in the memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ring {
    /// Offset of the ring's first byte.
    pub data: usize,
    /// Bytes in the ring, a power of two, at least 32.
    pub capacity: u64,
    /// Offset of the producer's position in the status block.
    pub tail: usize,
    /// Offset of the consumer's position in the status block.
    pub head: usize,
}

impl Ring {
    /// The outbound ring: agent to sidecar.
    pub fn outbound(out_capacity: u64) -> Ring {
        Ring::new(
            status::SIZE,
            out_capacity,
            status::OUT_TAIL,
            status::OUT_HEAD,
        )
    }

    /// The inbound ring: sidecar to agent.
    pub fn inbound(out_capacity: u64, in_capacity: u64) -> Ring {
        Ring::new(
            status::SIZE + out_capacity as usize,
            in_capacity,
            status::IN_TAIL,
            status::IN_HEAD,
        )
    }

    /// A ring at `data` of `capacity` bytes with its positions at `tail` and
    /// `head`. The file's rings are at least 256 KiB; the tests use smaller.
    pub fn new(data: usize, capacity: u64, tail: usize, head: usize) -> Ring {
        assert!(
            capacity.is_power_of_two() && capacity >= 32,
            "ring capacity"
        );
        Ring {
            data,
            capacity,
            tail,
            head,
        }
    }

    /// The largest payload a record may have: an eighth of the ring.
    pub fn max_payload(&self) -> usize {
        (self.capacity / 8) as usize
    }

    fn offset(&self, position: u64) -> usize {
        (position & (self.capacity - 1)) as usize
    }
}

/// Bytes a record with `len` payload bytes takes in a ring.
pub fn record_size(len: usize) -> u64 {
    (record::HEADER + len).next_multiple_of(8) as u64
}

/// A failed check: the other side is broken (spec/transport.md, "Consuming").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RingError {
    /// The consumer's position is impossible for the producer's.
    Head {
        /// The head read.
        head: u64,
        /// The producer's tail.
        tail: u64,
    },
    /// The producer's position is impossible for the consumer's, or moved back.
    Tail {
        /// The tail read.
        tail: u64,
        /// The consumer's head.
        head: u64,
        /// The check that failed.
        rule: &'static str,
    },
    /// A record header that breaks a rule.
    Record {
        /// The record's position.
        position: u64,
        /// Its `len`.
        len: u32,
        /// Its `kind`.
        kind: u16,
        /// The rule it breaks.
        rule: &'static str,
    },
    /// A payload over an eighth of the ring. Never written.
    TooLarge {
        /// The payload's length.
        len: usize,
    },
}

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RingError::Head { head, tail } => {
                write!(f, "head {head:#x} impossible for tail {tail:#x}")
            }
            RingError::Tail { tail, head, rule } => {
                write!(f, "tail {tail:#x} impossible for head {head:#x}: {rule}")
            }
            RingError::Record {
                position,
                len,
                kind,
                rule,
            } => {
                write!(
                    f,
                    "record at {position:#x} (len {len}, kind {kind}): {rule}"
                )
            }
            RingError::TooLarge { len } => write!(f, "payload of {len} bytes is over the limit"),
        }
    }
}

impl std::error::Error for RingError {}

/// The producer's side of a ring. A clone saves the private state: restoring
/// it drops what was written since, which is safe until a publish.
#[derive(Clone, Debug)]
pub struct Producer {
    ring: Ring,
    tail: u64,
    head: u64,
    records: u64,
}

impl Producer {
    /// A producer whose tail is `tail`, the value it last stored (0 after a
    /// reset). It assumes the ring is empty until [`Producer::refresh`].
    pub fn new(ring: Ring, tail: u64) -> Producer {
        Producer {
            ring,
            tail,
            head: tail,
            records: 0,
        }
    }

    /// The ring.
    pub fn ring(&self) -> Ring {
        self.ring
    }

    /// Records this producer wrote, padding records included.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Records the next push of `len` bytes writes: 2 when a padding record
    /// goes first, else 1.
    pub fn records_for(&self, len: usize) -> u64 {
        if self.cost(len) > record_size(len) {
            2
        } else {
            1
        }
    }

    /// The private tail: where the next record goes.
    pub fn tail(&self) -> u64 {
        self.tail
    }

    /// The consumer's head as last read.
    pub fn head(&self) -> u64 {
        self.head
    }

    /// Free bytes by the last head read.
    pub fn free(&self) -> u64 {
        self.ring.capacity - (self.tail - self.head)
    }

    /// Load the head (acquire) and check it.
    pub fn refresh(&mut self, m: &impl Memory) -> Result<(), RingError> {
        let head = m.load(self.ring.head, Ordering::Acquire);
        if !head.is_multiple_of(8)
            || head > self.tail
            || head < self.tail.saturating_sub(self.ring.capacity)
        {
            return Err(RingError::Head {
                head,
                tail: self.tail,
            });
        }
        self.head = head;
        Ok(())
    }

    /// Bytes a record of `len` would take at the tail, with the padding record
    /// in front of it if it would not fit before the end.
    pub fn cost(&self, len: usize) -> u64 {
        let size = record_size(len);
        let to_end = self.ring.capacity - self.ring.offset(self.tail) as u64;
        if size > to_end { to_end + size } else { size }
    }

    /// Write a record made of `parts` at the private tail. Returns `Ok(false)`
    /// and writes nothing when the ring has no room even after a fresh head.
    pub fn push(&mut self, m: &impl Memory, kind: u16, parts: &[&[u8]]) -> Result<bool, RingError> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if len > self.ring.max_payload() {
            return Err(RingError::TooLarge { len });
        }
        let cost = self.cost(len);
        if cost > self.free() {
            self.refresh(m)?;
            if cost > self.free() {
                return Ok(false);
            }
        }
        let size = record_size(len);
        let mut off = self.ring.offset(self.tail);
        let to_end = self.ring.capacity as usize - off;
        if size as usize > to_end {
            self.header(
                m,
                off,
                (to_end - record::HEADER) as u32,
                kind::PADDING as u16,
            );
            self.tail += to_end as u64;
            self.records += 1;
            off = 0;
        }
        self.header(m, off, len as u32, kind);
        let mut at = self.ring.data + off + record::HEADER;
        for p in parts {
            m.write(at, p);
            at += p.len();
        }
        let pad = size as usize - record::HEADER - len;
        m.zero(at, pad);
        self.tail += size;
        self.records += 1;
        Ok(true)
    }

    fn header(&self, m: &impl Memory, off: usize, len: u32, kind: u16) {
        let mut h = [0u8; record::HEADER];
        h[record::LEN..record::LEN + 4].copy_from_slice(&len.to_le_bytes());
        h[record::KIND..record::KIND + 2].copy_from_slice(&kind.to_le_bytes());
        m.write(self.ring.data + off, &h);
    }

    /// Store the private tail (release): the records written since the last
    /// publish become visible together.
    pub fn publish(&self, m: &impl Memory) {
        m.store(self.ring.tail, self.tail, Ordering::Release);
    }
}

/// The consumer's side of a ring.
#[derive(Clone, Debug)]
pub struct Consumer {
    ring: Ring,
    head: u64,
    tail: u64,
}

/// One record read from a ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Read {
    /// The record's kind or opcode.
    pub kind: u16,
    /// The record's position.
    pub position: u64,
    /// The position after it: the head once it is consumed.
    pub next: u64,
    /// Padding records skipped just before it.
    pub padding: u32,
}

impl Consumer {
    /// A consumer whose head is `head`.
    pub fn new(ring: Ring, head: u64) -> Consumer {
        Consumer {
            ring,
            head,
            tail: head,
        }
    }

    /// The ring.
    pub fn ring(&self) -> Ring {
        self.ring
    }

    /// The private head: the next record to read.
    pub fn head(&self) -> u64 {
        self.head
    }

    /// The producer's tail as last read.
    pub fn tail(&self) -> u64 {
        self.tail
    }

    /// Bytes between the head and the tail as last read.
    pub fn pending(&self) -> u64 {
        self.tail - self.head
    }

    /// Load the tail (acquire) and check it. A tail behind the last one read
    /// is a failed check.
    pub fn refresh(&mut self, m: &impl Memory) -> Result<(), RingError> {
        let tail = m.load(self.ring.tail, Ordering::Acquire);
        let rule = if !tail.is_multiple_of(8) || !self.head.is_multiple_of(8) {
            Some("not a multiple of 8")
        } else if tail < self.tail {
            Some("behind the last tail read")
        } else if tail - self.head > self.ring.capacity {
            Some("more than the capacity ahead of the head")
        } else {
            None
        };
        if let Some(rule) = rule {
            return Err(RingError::Tail {
                tail,
                head: self.head,
                rule,
            });
        }
        self.tail = tail;
        Ok(())
    }

    /// Read the next record before the tail into `payload`, skipping padding.
    /// Advances the private head. `Ok(None)` when the ring is empty.
    pub fn next(
        &mut self,
        m: &impl Memory,
        payload: &mut Vec<u8>,
    ) -> Result<Option<Read>, RingError> {
        let mut padding = 0;
        loop {
            if self.head == self.tail {
                return Ok(None);
            }
            let position = self.head;
            let off = self.ring.offset(position);
            let mut h = [0u8; record::HEADER];
            m.read(self.ring.data + off, &mut h);
            let len = u32::from_le_bytes(h[0..4].try_into().expect("4 bytes"));
            let kind = u16::from_le_bytes(h[4..6].try_into().expect("2 bytes"));
            let fail = |rule| RingError::Record {
                position,
                len,
                kind,
                rule,
            };
            if len as usize > self.ring.max_payload() {
                return Err(fail("len over an eighth of the ring"));
            }
            let size = record_size(len as usize);
            if size > self.tail - position {
                return Err(fail("record passes the tail"));
            }
            if off as u64 + size > self.ring.capacity {
                return Err(fail("record passes the end of the ring"));
            }
            if u64::from(kind) == kind::PADDING {
                if off as u64 + size != self.ring.capacity {
                    return Err(fail("padding does not end at the end of the ring"));
                }
                self.head += size;
                padding += 1;
                continue;
            }
            payload.clear();
            payload.resize(len as usize, 0);
            m.read(self.ring.data + off + record::HEADER, payload);
            self.head += size;
            return Ok(Some(Read {
                kind,
                position,
                next: self.head,
                padding,
            }));
        }
    }

    /// Store `head` (release). It is at most the private head and at least the
    /// last head stored.
    pub fn commit_at(&self, m: &impl Memory, head: u64) {
        debug_assert!(head <= self.head);
        m.store(self.ring.head, head, Ordering::Release);
    }

    /// Store the private head (release).
    pub fn commit(&self, m: &impl Memory) {
        self.commit_at(m, self.head);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::Heap;

    const CAP: u64 = 256;

    fn setup() -> (Heap, Ring) {
        // Positions in the first 16 bytes, the ring after them.
        (Heap::new(16 + CAP as usize), Ring::new(16, CAP, 0, 8))
    }

    #[test]
    fn records_round_trip_across_the_end() {
        let (m, ring) = setup();
        let mut p = Producer::new(ring, 0);
        let mut c = Consumer::new(ring, 0);
        let mut buf = Vec::new();
        for i in 0..200u32 {
            let body = vec![i as u8; (i % 28) as usize];
            assert!(p.push(&m, 7, &[&i.to_le_bytes(), &body]).expect("push"));
            p.publish(&m);
            c.refresh(&m).expect("tail");
            let r = c.next(&m, &mut buf).expect("next").expect("a record");
            assert_eq!(r.kind, 7);
            assert_eq!(buf[..4], i.to_le_bytes());
            assert_eq!(&buf[4..], &body[..]);
            c.commit(&m);
        }
        assert!(p.tail() > 4 * CAP, "the test wrapped");
    }

    #[test]
    fn a_full_ring_refuses_and_writes_nothing() {
        let (m, ring) = setup();
        let mut p = Producer::new(ring, 0);
        let mut n = 0;
        while p.push(&m, 1, &[&[0u8; 24]]).expect("push") {
            n += 1;
        }
        assert_eq!(n, CAP / 32);
        assert_eq!(p.tail(), CAP);
        assert_eq!(p.push(&m, 1, &[&[0u8; 32]]), Ok(false));
        assert_eq!(p.tail(), CAP, "a refused push moves nothing");
        p.publish(&m);
        let mut c = Consumer::new(ring, 0);
        c.refresh(&m).expect("tail");
        let mut buf = Vec::new();
        let mut read = 0;
        while c.next(&m, &mut buf).expect("next").is_some() {
            read += 1;
        }
        assert_eq!(read, n);
    }

    /// Two threads, as the agent and the sidecar are two processes. Under Miri
    /// this checks the data copies against the positions' release and acquire.
    #[test]
    fn a_producer_and_a_consumer_on_two_threads() {
        let (m, ring) = setup();
        let m = std::sync::Arc::new(m);
        let n: u32 = if cfg!(miri) { 60 } else { 20_000 };
        let producer = {
            let m = m.clone();
            std::thread::spawn(move || {
                let mut p = Producer::new(ring, 0);
                let mut i = 0u32;
                while i < n {
                    let body = vec![i as u8; (i % 25) as usize];
                    if p.push(&*m, 3, &[&i.to_le_bytes(), &body]).expect("push") {
                        i += 1;
                        if i.is_multiple_of(3) {
                            p.publish(&*m);
                        }
                    } else {
                        p.publish(&*m);
                        std::thread::yield_now();
                    }
                }
                p.publish(&*m);
            })
        };
        let mut c = Consumer::new(ring, 0);
        let mut buf = Vec::new();
        let mut want = 0u32;
        while want < n {
            c.refresh(&*m).expect("tail");
            while let Some(r) = c.next(&*m, &mut buf).expect("next") {
                assert_eq!(r.kind, 3);
                assert_eq!(buf[..4], want.to_le_bytes());
                assert!(buf[4..].iter().all(|&b| b == want as u8));
                assert_eq!(buf.len(), 4 + (want % 25) as usize);
                want += 1;
            }
            c.commit(&*m);
            std::thread::yield_now();
        }
        producer.join().expect("producer");
    }

    #[test]
    fn an_oversized_payload_is_refused() {
        let (m, ring) = setup();
        let mut p = Producer::new(ring, 0);
        assert_eq!(
            p.push(&m, 1, &[&[0u8; 33]]),
            Err(RingError::TooLarge { len: 33 })
        );
    }

    #[test]
    fn impossible_positions_fail_their_checks() {
        let (m, ring) = setup();
        let mut p = Producer::new(ring, 64);
        m.store(8, 72, Ordering::Release);
        assert!(matches!(p.refresh(&m), Err(RingError::Head { .. })));
        m.store(8, 4, Ordering::Release);
        assert!(matches!(p.refresh(&m), Err(RingError::Head { .. })));
        let mut c = Consumer::new(ring, 0);
        let rule = |c: &mut Consumer| match c.refresh(&m) {
            Err(RingError::Tail { rule, .. }) => rule,
            other => panic!("{other:?}"),
        };
        m.store(0, CAP + 8, Ordering::Release);
        assert_eq!(rule(&mut c), "more than the capacity ahead of the head");
        m.store(0, 12, Ordering::Release);
        assert_eq!(rule(&mut c), "not a multiple of 8");
        m.store(0, 16, Ordering::Release);
        c.refresh(&m).expect("a good tail");
        m.store(0, 8, Ordering::Release);
        assert_eq!(rule(&mut c), "behind the last tail read");
    }

    #[test]
    fn bad_records_fail_their_checks() {
        let bad = |len: u32, kind: u16, at: u64, tail: u64| {
            let (m, ring) = setup();
            let mut h = [0u8; 8];
            h[..4].copy_from_slice(&len.to_le_bytes());
            h[4..6].copy_from_slice(&kind.to_le_bytes());
            m.write(16 + (at % CAP) as usize, &h);
            m.store(0, tail, Ordering::Release);
            let mut c = Consumer::new(ring, at);
            c.refresh(&m).expect("tail");
            c.next(&m, &mut Vec::new())
        };
        assert!(bad(33, 1, 0, 64).is_err(), "len over the limit");
        assert!(bad(16, 1, 0, 16).is_err(), "passes the tail");
        assert!(bad(32, 1, 224, 288).is_err(), "passes the end");
        assert!(
            bad(8, 0, 0, 64).is_err(),
            "padding that does not end at the end"
        );
        assert!(bad(24, 0, 224, 256).expect("padding at the end").is_none());
        assert!(bad(u32::MAX, 1, 0, 64).is_err(), "a huge len");
    }

    /// Memory that records the ordering of each word access.
    struct Recorder {
        heap: Heap,
        log: std::cell::RefCell<Vec<(&'static str, usize, Ordering)>>,
    }

    impl Memory for Recorder {
        fn len(&self) -> usize {
            self.heap.len()
        }
        fn load(&self, off: usize, order: Ordering) -> u64 {
            self.log.borrow_mut().push(("load", off, order));
            self.heap.load(off, order)
        }
        fn store(&self, off: usize, value: u64, order: Ordering) {
            self.log.borrow_mut().push(("store", off, order));
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
            self.heap.read(off, out)
        }
        fn write(&self, off: usize, data: &[u8]) {
            self.heap.write(off, data)
        }
        fn zero(&self, off: usize, n: usize) {
            self.heap.zero(off, n)
        }
    }

    /// The Loom model checks the tail's release and acquire; it cannot check
    /// the head's, so this test pins both sides' orderings.
    #[test]
    fn positions_are_stored_with_release_and_loaded_with_acquire() {
        let ring = Ring::new(16, CAP, 0, 8);
        let m = Recorder {
            heap: Heap::new(16 + CAP as usize),
            log: Default::default(),
        };
        let mut p = Producer::new(ring, 0);
        let mut c = Consumer::new(ring, 0);
        p.refresh(&m).expect("head");
        assert!(p.push(&m, 1, &[&[0u8; 8]]).expect("push"));
        p.publish(&m);
        c.refresh(&m).expect("tail");
        c.next(&m, &mut Vec::new()).expect("next");
        c.commit(&m);
        let log = m.log.borrow();
        assert_eq!(
            *log,
            [
                ("load", 8, Ordering::Acquire),
                ("store", 0, Ordering::Release),
                ("load", 0, Ordering::Acquire),
                ("store", 8, Ordering::Release),
            ]
        );
    }
}
