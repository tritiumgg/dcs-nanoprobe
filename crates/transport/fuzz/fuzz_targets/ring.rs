//! A consumer over arbitrary ring bytes and positions never panics, never
//! reads past the ring, and never moves its head past the tail.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanoprobe_transport::mem::{Heap, Memory};
use nanoprobe_transport::ring::{Consumer, Ring};
use std::sync::atomic::Ordering;

const CAP: u64 = 256;

fuzz_target!(|data: &[u8]| {
    if data.len() < 16 {
        return;
    }
    let tail = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let head = u64::from_le_bytes(data[8..16].try_into().unwrap()) & !7;
    // Positions in the first 16 bytes, the ring after them.
    let m = Heap::new(16 + CAP as usize);
    m.write(16, &data[16..data.len().min(16 + CAP as usize)]);
    m.store(0, tail, Ordering::Relaxed);
    let mut c = Consumer::new(Ring::new(16, CAP, 0, 8), head);
    if c.refresh(&m).is_err() {
        return;
    }
    let mut buf = Vec::new();
    while let Ok(Some(r)) = c.next(&m, &mut buf) {
        assert!(r.next <= c.tail());
        assert!(buf.len() as u64 <= CAP / 8);
    }
});
