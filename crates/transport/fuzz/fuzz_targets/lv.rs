//! An `lv` decoder never panics, and what it accepts encodes to bytes that
//! decode to the same encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanoprobe_transport::codec::{Reader, Writer};

fuzz_target!(|data: &[u8]| {
    if let Ok(v) = Reader::new(data).lv() {
        let mut w = Writer::new();
        w.lv(&v);
        let once = w.finish();
        let again = Reader::new(&once).lv().expect("an encoded value decodes");
        let mut w = Writer::new();
        w.lv(&again);
        assert_eq!(w.finish(), once);
    }
    let _ = Reader::new(data).str();
});
