//! Every record decoder never panics on any payload.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanoprobe_transport::codec::Reader;
use nanoprobe_transport::records::{
    Airbase, Ammo, AuthorEvent, ExtensionStats, Menu, Player, Attitude, Bind, CommandHeader, CommandResult, EnumBegin, EnumEnd, Event, Frame,
    Gap, Hello, Lifecycle, Log, Object, ObjectGone, Positions, Rates, Seen, Subscribe, Vitals,
    Warehouse,
};

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
    if let Ok(e) = Event::decode(data) {
        assert_eq!(Event::decode(&e.encode()).as_ref().map(|d| d.fields.len()), Ok(e.fields.len()));
        let _ = e.cut().encode();
        for (code, _) in &e.fields {
            let _ = (e.object(u64::from(*code)), e.text(u64::from(*code)));
        }
    }
    let _ = (ObjectGone::decode(data), EnumBegin::decode(data), EnumEnd::decode(data));
    let _ = (Rates::decode(data), Airbase::decode(data));
    if let Ok(m) = Menu::decode(data) {
        assert_eq!(Menu::decode(&m.encode()).as_ref(), Ok(&m));
    }
    if let Ok(p) = Player::decode(data) {
        assert_eq!(Player::decode(&p.encode()).as_ref(), Ok(&p));
    }
    if let Ok(x) = ExtensionStats::decode(data) {
        assert_eq!(ExtensionStats::decode(&x.encode()).as_ref(), Ok(&x));
    }
    // The payload may hold a NaN, which is not equal to itself, so the
    // re-encoded bytes are compared.
    if let Ok(a) = AuthorEvent::decode(data) {
        let again = AuthorEvent::decode(&a.encode()).expect("a record decodes again");
        assert_eq!(again.encode(), a.encode());
        assert_eq!((again.objects, again.name), (a.objects, a.name));
    }
    if let Ok(s) = Seen::decode(data) {
        assert_eq!(Seen::decode(&s.encode()).as_ref(), Ok(&s));
    }
    if let Ok(o) = Object::decode(data) {
        assert_eq!(Object::decode(&o.encode()).as_ref(), Ok(&o));
    }
    if let Ok(p) = Positions::decode(data) {
        assert_eq!(p.object.len(), p.z.len());
        assert!(Positions::decode(&p.encode()).is_ok());
    }
    if let Ok(v) = Vitals::decode(data) {
        assert_eq!(v.object.len(), v.fuel.len());
    }
    if let Ok(a) = Attitude::decode(data) {
        assert_eq!(a.object.len(), a.roll.len());
    }
    if let Ok(a) = Ammo::decode(data) {
        assert!(Ammo::decode(&a.encode()).is_ok());
    }
    if let Ok(w) = Warehouse::decode(data) {
        assert!(Warehouse::decode(&w.encode()).is_ok());
    }
    if let Ok(s) = Subscribe::decode(data) {
        assert_eq!(Subscribe::decode(&s.encode()).map(|d| d.entity.len()), Ok(s.entity.len()));
    }
    if let Ok(b) = Bind::decode(data) {
        assert_eq!(b.entity.len(), b.object.len());
    }
    let _ = CommandHeader::read(&mut Reader::new(data));
});
