//! Loom models of the ring and the metrics sequence lock.
//!
//! Run with `mise run loom`. The data bytes are modeled as relaxed atomic
//! words, so a read that the tail's release and acquire do not order after
//! its write may see an old value, and the checks below catch it. Loom does
//! not model a load that reads a later store, so it cannot find a head
//! stored or loaded too weakly; `ring::tests` pins those orderings.
#![cfg(loom)]

use std::sync::atomic::Ordering;

use loom::sync::Arc;
use loom::sync::atomic::AtomicU64;
use loom::thread;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::ring::{Consumer, Producer, Ring};

struct LoomMem {
    words: Vec<AtomicU64>,
}

impl LoomMem {
    fn new(len: usize) -> LoomMem {
        LoomMem {
            words: (0..len / 8).map(|_| AtomicU64::new(0)).collect(),
        }
    }
}

impl Memory for LoomMem {
    fn len(&self) -> usize {
        self.words.len() * 8
    }
    fn load(&self, off: usize, order: Ordering) -> u64 {
        self.words[off / 8].load(order)
    }
    fn store(&self, off: usize, value: u64, order: Ordering) {
        self.words[off / 8].store(value, order)
    }
    fn load32(&self, _: usize, _: Ordering) -> u32 {
        unimplemented!("the models use only 64-bit words")
    }
    fn store32(&self, _: usize, _: u32, _: Ordering) {
        unimplemented!("the models use only 64-bit words")
    }
    fn fence(&self, order: Ordering) {
        loom::sync::atomic::fence(order)
    }
    fn read(&self, off: usize, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            let at = off + i;
            *b = self.words[at / 8].load(Ordering::Relaxed).to_le_bytes()[at % 8];
        }
    }
    fn write(&self, off: usize, data: &[u8]) {
        // Only the producer writes the data words, so a relaxed
        // read-modify-write of its own word is enough.
        for (i, b) in data.iter().enumerate() {
            let at = off + i;
            let w = &self.words[at / 8];
            let mut bytes = w.load(Ordering::Relaxed).to_le_bytes();
            bytes[at % 8] = *b;
            w.store(u64::from_le_bytes(bytes), Ordering::Relaxed);
        }
    }
    fn zero(&self, off: usize, n: usize) {
        self.write(off, &vec![0; n]);
    }
}

const CAP: u64 = 32;

/// Positions in the first 16 bytes; the 32-byte ring after them.
fn ring() -> Ring {
    Ring::new(16, CAP, 0, 8)
}

/// Payloads of 0, 4 and 4 bytes take 8, 16 and 16 bytes: the third does not
/// fit before the end, so the padding record and the wrap are modeled too.
const PAYLOADS: [&[u8]; 3] = [b"", b"\x01\x02\x03\x04", b"\x05\x06\x07\x08"];

#[test]
fn every_record_arrives_whole_and_in_order() {
    loom::model(|| {
        let m = Arc::new(LoomMem::new(16 + CAP as usize));
        let producer = {
            let m = m.clone();
            thread::spawn(move || {
                let mut p = Producer::new(ring(), 0);
                let mut sent = 0;
                while sent < PAYLOADS.len() {
                    match p.push(&*m, 9, &[PAYLOADS[sent]]) {
                        Ok(true) => {
                            p.publish(&*m);
                            sent += 1;
                        }
                        Ok(false) => thread::yield_now(),
                        Err(e) => panic!("producer: {e}"),
                    }
                }
                assert_eq!(p.records(), 4, "three records and one padding record");
            })
        };
        let mut c = Consumer::new(ring(), 0);
        let mut buf = Vec::new();
        let mut got = 0;
        while got < PAYLOADS.len() {
            c.refresh(&*m).expect("a good tail");
            match c.next(&*m, &mut buf).expect("a good record") {
                Some(r) => {
                    assert_eq!(r.kind, 9);
                    assert_eq!(buf, PAYLOADS[got], "record {got} arrived whole");
                    assert_eq!(r.padding, u32::from(got == 2));
                    c.commit(&*m);
                    got += 1;
                }
                None => thread::yield_now(),
            }
        }
        producer.join().expect("producer");
    });
}

/// The sequence lock that `write_metrics` and `read_metrics` use, over two
/// fields: the whole metrics block is too many relaxed words for Loom.
#[test]
fn the_sequence_lock_never_yields_a_torn_copy() {
    use nanoprobe_transport::status::{seqlock_read, seqlock_write};
    const SEQ: usize = 0;
    const A: usize = 8;
    const B: usize = 16;
    loom::model(|| {
        let m = Arc::new(LoomMem::new(24));
        let writer = {
            let m = m.clone();
            thread::spawn(move || {
                for v in 1..=2u64 {
                    seqlock_write(&*m, SEQ, |m| {
                        m.store(A, v, Ordering::Relaxed);
                        m.store(B, v, Ordering::Relaxed);
                    });
                }
            })
        };
        let got = seqlock_read(&*m, SEQ, |m| {
            (m.load(A, Ordering::Relaxed), m.load(B, Ordering::Relaxed))
        });
        if let Some((a, b)) = got {
            assert_eq!(a, b, "a kept copy is from one write");
            KEPT.fetch_add(1, Ordering::Relaxed);
        }
        writer.join().expect("writer");
        let last = seqlock_read(&*m, SEQ, |m| {
            (m.load(A, Ordering::Relaxed), m.load(B, Ordering::Relaxed))
        });
        assert_eq!(
            last,
            Some((2, 2)),
            "a read after the writer finished is kept"
        );
    });
    // A reader that never keeps a copy would pass the check above.
    assert!(
        KEPT.load(Ordering::Relaxed) > 0,
        "some interleaving kept a copy"
    );
}

/// Interleavings in which the concurrent read kept a copy. A std atomic, so
/// it counts across Loom's executions.
static KEPT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
