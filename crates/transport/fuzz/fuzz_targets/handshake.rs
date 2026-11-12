//! A handshake line parser never panics, and what it accepts encodes back to
//! a line that parses the same.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanoprobe_transport::handshake::{Line, LineBuffer};

fuzz_target!(|data: &[u8]| {
    if let Ok(l) = Line::parse(data)
        && let Some(text) = l.encode()
    {
        assert_eq!(Line::parse(text.as_bytes()), Ok(l));
    }
    let mut b = LineBuffer::new();
    for chunk in data.chunks(7) {
        b.push(chunk);
        while let Ok(Some(line)) = b.line() {
            assert!(line.len() <= 1024 && line.ends_with(b"\n"));
        }
    }
});
