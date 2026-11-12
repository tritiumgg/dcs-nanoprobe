//! The sidecar's header check and a snapshot never panic on any status block,
//! and a header they accept describes a file that holds its rings.
//!
//! Each input starts from a valid header written by `status::start` and
//! changes bytes of it, so the fuzzer reaches the checks past `ready`,
//! `magic` and `layout_hash` as well as the success path.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanoprobe_transport::layout::CAPACITY;
use nanoprobe_transport::mem::{Heap, Memory};
use nanoprobe_transport::status::{Header, Snapshot, check, read_metrics, start};

/// The smallest valid file: the block and two 256 KiB rings.
const LEN: usize = 4096 + 2 * (256 << 10);

fuzz_target!(|data: &[u8]| {
    let m = Heap::new(LEN);
    let header = Header {
        interface: 1,
        agent_boot: 7,
        build: 1,
        created_ms: 1,
        clock_hz: 1,
        out_capacity: 256 << 10,
        in_capacity: 256 << 10,
    };
    start(&m, &header, 1);
    // The first 8 bytes are the file length the check is given; each
    // following 3 bytes set one byte of the block: a u16 offset, a value.
    let file_len = data.get(..8).map_or(LEN as u64, |b| u64::from_le_bytes(b.try_into().unwrap()));
    for edit in data.get(8..).unwrap_or(&[]).chunks_exact(3) {
        let off = usize::from(u16::from_le_bytes([edit[0], edit[1]])) % 4096;
        m.write(off, &[edit[2]]);
    }
    if let Ok(g) = check(&m, file_len) {
        assert_eq!(g.file_size, 4096 + g.out_capacity + g.in_capacity);
        assert!(CAPACITY.contains(&g.out_capacity) && CAPACITY.contains(&g.in_capacity));
        assert!(g.file_size <= file_len && g.file_size <= m.len() as u64);
        assert_ne!(g.agent_boot, 0);
    }
    let _ = Snapshot::read(&m);
    let _ = read_metrics(&m);
});
