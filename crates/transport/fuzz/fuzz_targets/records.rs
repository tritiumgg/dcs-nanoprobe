//! Every record decoder never panics on any payload.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanoprobe_transport::codec::Reader;
use nanoprobe_transport::records::{CommandHeader, CommandResult, Frame, Gap, Hello, Lifecycle, Log};

fuzz_target!(|data: &[u8]| {
    let _ = Frame::decode(data);
    let _ = Hello::decode(data);
    let _ = Lifecycle::decode(data);
    let _ = Gap::decode(data);
    if let Ok(l) = Log::decode(data) {
        assert!(Log::decode(&l.encode()).is_ok());
    }
    if let Ok(r) = CommandResult::decode(data) {
        assert!(CommandResult::decode(&r.encode()).is_ok());
    }
    let _ = CommandHeader::read(&mut Reader::new(data));
});
