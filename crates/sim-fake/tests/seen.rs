//! The fake sim's enumerations write `SEEN` as the agent does
//! (spec/records.md, "Layout: SEEN"): the first enumeration after a `HELLO`
//! is whole, later ones name unchanged rows in `SEEN` records, and `ENUM_END`
//! counts both.

use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sidecar::sim::wire::Input;
use nanoprobe_sim_fake::{Config, FakeSim};

/// `(enumeration, OBJECT records, SEEN IDs, ENUM_END's objects, its seen)` of
/// each enumeration that ended in `inputs`.
fn enumerations(inputs: &[Input]) -> Vec<(u64, u32, u32, u32, u32)> {
    let mut out = Vec::new();
    for i in inputs {
        if let Input::EnumEnd(e) = i {
            let objects = inputs
                .iter()
                .filter(|i| matches!(i, Input::Object(o) if o.enumeration == e.enumeration))
                .count() as u32;
            let seen: u32 = inputs
                .iter()
                .map(|i| match i {
                    Input::Seen(s) if s.enumeration == e.enumeration => s.objects.len() as u32,
                    _ => 0,
                })
                .sum();
            out.push((e.enumeration, objects, seen, e.objects, e.seen));
        }
    }
    out
}

fn read(sim: &mut FakeSim) -> Vec<Input> {
    sim.read()
        .iter()
        .map(|r| Input::decode(r.kind, &r.payload).expect("decodes"))
        .collect()
}

fn running_sim() -> FakeSim {
    let mut sim = FakeSim::new(Config::default());
    while !sim.running() {
        sim.frame();
    }
    sim.add_campaign_group(1, 1, &[1, 2, 3]);
    sim
}

#[test]
fn the_first_enumeration_after_a_hello_is_whole_and_later_ones_use_seen() {
    let mut sim = running_sim();
    sim.attach(1);
    for _ in 0..200 {
        sim.frame();
    }
    let all = read(&mut sim);
    let e = enumerations(&all);
    assert!(e.len() >= 3, "{e:?}");
    for (n, objects, seen, end_objects, end_seen) in &e {
        assert_eq!((objects, seen), (end_objects, end_seen), "enumeration {n}");
    }
    let rows = e[0].1;
    assert!(rows >= 3, "{e:?}");
    assert_eq!((e[0].1, e[0].2), (rows, 0), "whole after the HELLO");
    assert_eq!((e[1].1, e[1].2), (0, rows), "SEEN after that");
    // A SEEN comes before the ENUM_END of its enumeration.
    let end = all
        .iter()
        .position(|i| matches!(i, Input::EnumEnd(x) if x.enumeration == e[1].0))
        .unwrap();
    let seen = all
        .iter()
        .position(|i| matches!(i, Input::Seen(x) if x.enumeration == e[1].0))
        .unwrap();
    assert!(seen < end);
}

#[test]
fn an_attach_makes_the_next_enumeration_whole() {
    let mut sim = running_sim();
    sim.attach(1);
    for _ in 0..120 {
        sim.frame();
    }
    read(&mut sim);
    sim.detach();
    sim.attach(2);
    for _ in 0..120 {
        sim.frame();
    }
    let e = enumerations(&read(&mut sim));
    let rows = e[0].1;
    assert!(rows >= 3, "{e:?}");
    assert_eq!((e[0].1, e[0].2), (rows, 0), "{e:?}");
    assert_eq!((e[1].1, e[1].2), (0, rows), "{e:?}");
}
