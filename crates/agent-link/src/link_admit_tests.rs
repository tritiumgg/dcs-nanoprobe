//! The admission rows of spec/commands.md, "Sequence, epoch and revisions",
//! and the applied revisions' scopes.

use nanoprobe_transport::layout::{lifecycle, opcode, result};
use nanoprobe_transport::records::CommandHeader;

use super::tests::open;
use super::{Admit, Link};

const CAMPAIGN: u64 = 7;

fn header(seq: u64, epoch: u64, entity: u64, rev: u64, mission_gen: u32) -> CommandHeader {
    CommandHeader {
        seq,
        epoch,
        campaign: CAMPAIGN,
        entity,
        rev,
        mission_gen,
    }
}

/// A link with campaign 7 active in generation 1.
fn link(name: &str) -> (Link, impl Drop) {
    let (mut link, dir) = open(name);
    link.set_campaign(CAMPAIGN);
    link.new_generation();
    (link, dir)
}

fn generation(link: &Link) -> u32 {
    link.stamps().mission_gen
}

/// Admit and, on Run, report `code` as the handler's result.
fn run(link: &mut Link, h: &CommandHeader, op: u64, ext: Option<&[u8]>, code: u64) -> u64 {
    match link.admit(h, op as u16, ext) {
        Admit::Run => {
            link.ran(code);
            code
        }
        Admit::Answer(c) => c,
        Admit::Control => panic!("not a control command"),
    }
}

#[test]
fn the_rows_answer_in_table_order() {
    let (mut l, _d) = link("admit-order");
    let g = generation(&l);
    let spawn = opcode::GROUP_SPAWN;
    assert_eq!(
        run(&mut l, &header(1, 5, 9, 1, g), spawn, None, result::APPLIED),
        result::APPLIED
    );
    // Lower epoch first, even for a sequence number already handled.
    assert_eq!(
        run(&mut l, &header(1, 4, 9, 2, g), spawn, None, 0),
        result::STALE_EPOCH
    );
    assert_eq!(
        run(&mut l, &header(1, 5, 9, 2, g), spawn, None, 0),
        result::DUPLICATE
    );
    let mut other = header(2, 5, 9, 2, g);
    other.campaign = CAMPAIGN + 1;
    assert_eq!(run(&mut l, &other, spawn, None, 0), result::WRONG_CAMPAIGN);
    // WRONG_CAMPAIGN took seq 2.
    assert_eq!(
        run(&mut l, &header(2, 5, 9, 2, g), spawn, None, 0),
        result::DUPLICATE
    );
    assert_eq!(
        run(&mut l, &header(3, 5, 9, 2, g + 1), spawn, None, 0),
        result::WRONG_GENERATION
    );
    assert_eq!(
        run(&mut l, &header(4, 5, 9, 0, g), spawn, None, 0),
        result::BAD_PAYLOAD
    );
    assert_eq!(
        run(&mut l, &header(5, 5, 9, 1, g), spawn, None, 0),
        result::AT_REVISION
    );
    assert_eq!(l.watermarks(), (5, 5));
    // A higher epoch starts the sequence again.
    assert_eq!(
        run(&mut l, &header(1, 6, 9, 2, g), spawn, None, result::APPLIED),
        result::APPLIED
    );
    assert_eq!(l.watermarks(), (6, 1));
}

#[test]
fn no_campaign_comes_only_for_an_opcode_that_needs_one() {
    let (mut l, _d) = open("admit-no-campaign");
    let mut h = header(1, 1, 0, 0, 0);
    h.campaign = 0;
    assert_eq!(
        run(&mut l, &h, opcode::EXPLOSION, None, 0),
        result::NO_CAMPAIGN
    );
    h.seq = 2;
    assert_eq!(
        run(&mut l, &h, opcode::MESSAGE, None, result::APPLIED),
        result::APPLIED
    );
}

#[test]
fn an_unknown_opcode_passes_the_campaign_row_then_is_unknown() {
    let (mut l, _d) = link("admit-unknown");
    let g = generation(&l);
    assert_eq!(
        run(&mut l, &header(1, 1, 0, 0, g), 200, None, 0),
        result::UNKNOWN_OPCODE
    );
    assert_eq!(
        run(&mut l, &header(1, 1, 0, 0, g), 200, None, 0),
        result::DUPLICATE
    );
    let mut h = header(2, 1, 0, 0, g);
    h.campaign = 0;
    assert_eq!(run(&mut l, &h, 200, None, 0), result::WRONG_CAMPAIGN);
    // A control opcode sent with a sequence number.
    assert_eq!(
        run(&mut l, &header(3, 1, 0, 0, g), opcode::CAMPAIGN, None, 0),
        result::UNKNOWN_OPCODE
    );
    assert_eq!(l.watermarks(), (1, 3));
}

#[test]
fn a_revision_moves_only_on_applied_or_too_large() {
    let (mut l, _d) = link("admit-moves");
    let g = generation(&l);
    let order = opcode::GROUP_ORDER;
    assert_eq!(
        run(
            &mut l,
            &header(1, 1, 9, 3, g),
            order,
            None,
            result::DCS_REFUSED
        ),
        result::DCS_REFUSED
    );
    // Not applied: the same revision runs again under a new sequence number.
    assert_eq!(
        run(
            &mut l,
            &header(2, 1, 9, 3, g),
            order,
            None,
            result::LUA_ERROR
        ),
        result::LUA_ERROR
    );
    assert_eq!(
        run(
            &mut l,
            &header(3, 1, 9, 3, g),
            order,
            None,
            result::TOO_LARGE
        ),
        result::TOO_LARGE
    );
    assert_eq!(
        run(&mut l, &header(4, 1, 9, 3, g), order, None, 0),
        result::AT_REVISION
    );
    assert_eq!(
        run(&mut l, &header(5, 1, 9, 2, g), order, None, 0),
        result::AT_REVISION
    );
    // The order and the spec revisions of an entity are apart.
    assert_eq!(
        run(
            &mut l,
            &header(6, 1, 9, 1, g),
            opcode::GROUP_SPAWN,
            None,
            result::APPLIED
        ),
        result::APPLIED
    );
    // Another entity has its own.
    assert_eq!(
        run(
            &mut l,
            &header(7, 1, 10, 1, g),
            order,
            None,
            result::APPLIED
        ),
        result::APPLIED
    );
}

#[test]
fn an_opcode_with_no_rev_column_is_deduplicated_by_sequence_only() {
    let (mut l, _d) = link("admit-no-rev");
    let g = generation(&l);
    for seq in 1..=3 {
        let h = header(seq, 1, 9, 0, g);
        assert_eq!(
            run(&mut l, &h, opcode::GROUP_DESTROY, None, result::APPLIED),
            result::APPLIED
        );
    }
    assert!(l.revisions().is_empty());
}

#[test]
fn extension_revisions_are_kept_by_name() {
    let (mut l, _d) = link("admit-extension");
    let set = opcode::EXTENSION_SET;
    // EXTENSION_SET is not bound to a generation.
    let h = |seq, rev| header(seq, 1, 0, rev, 0);
    assert_eq!(
        run(&mut l, &h(1, 2), set, Some(b"a"), result::APPLIED),
        result::APPLIED
    );
    assert_eq!(
        run(&mut l, &h(2, 2), set, Some(b"a"), 0),
        result::AT_REVISION
    );
    assert_eq!(
        run(&mut l, &h(3, 1), set, Some(b"b"), result::APPLIED),
        result::APPLIED
    );
    assert_eq!(run(&mut l, &h(4, 3), set, None, 0), result::BAD_PAYLOAD);
    l.forget_extension(b"a");
    assert_eq!(
        run(&mut l, &h(5, 2), set, Some(b"a"), result::APPLIED),
        result::APPLIED
    );
}

#[test]
fn a_generation_drops_entity_revisions_and_a_campaign_switch_drops_all() {
    let (mut l, _d) = link("admit-scopes");
    let g = generation(&l);
    run(
        &mut l,
        &header(1, 1, 9, 1, g),
        opcode::GROUP_SPAWN,
        None,
        result::APPLIED,
    );
    run(
        &mut l,
        &header(2, 1, 0, 1, 0),
        opcode::EXTENSION_SET,
        Some(b"x"),
        result::APPLIED,
    );
    assert_eq!(l.revisions().len(), 2);
    let g = l.new_generation();
    assert_eq!(l.revisions().len(), 1);
    assert_eq!(
        run(
            &mut l,
            &header(3, 1, 9, 1, g),
            opcode::GROUP_SPAWN,
            None,
            result::APPLIED
        ),
        result::APPLIED
    );
    // The same campaign again is no switch.
    l.set_campaign(CAMPAIGN);
    assert_eq!(l.revisions().len(), 2);
    l.set_campaign(CAMPAIGN + 1);
    assert!(l.revisions().is_empty());
    // The epoch and the sequence last for the process.
    assert_eq!(l.watermarks(), (1, 3));
}

#[test]
fn the_burst_holds_only_while_paused() {
    let (mut l, _d) = link("admit-burst");
    l.lifecycle(lifecycle::RUNNING as u32, "", "");
    l.set_burst(true);
    assert!(!l.unbudgeted());
    l.lifecycle(lifecycle::PAUSED as u32, "", "");
    assert!(!l.unbudgeted());
    l.set_burst(true);
    assert!(l.unbudgeted());
    l.lifecycle(lifecycle::RUNNING as u32, "", "");
    assert!(!l.unbudgeted());
    l.lifecycle(lifecycle::PAUSED as u32, "", "");
    assert!(!l.unbudgeted(), "leaving PAUSED turned it off");
    l.set_burst(true);
    l.set_burst(false);
    assert!(!l.unbudgeted());
}

#[test]
fn an_invoke_of_an_extension_the_campaign_lacks_is_refused() {
    let (mut l, _d) = link("admit-invoke");
    let g = generation(&l);
    let set = header(1, 5, 0, 1, g);
    assert_eq!(
        run(
            &mut l,
            &set,
            opcode::EXTENSION_SET,
            Some(b"x"),
            result::APPLIED
        ),
        result::APPLIED
    );
    let invoke = |seq| header(seq, 5, 0, 0, g);
    assert_eq!(
        run(
            &mut l,
            &invoke(2),
            opcode::EXTENSION_INVOKE,
            Some(b"x"),
            result::APPLIED
        ),
        result::APPLIED
    );
    // An unknown name is the driver's check, after the raw-Lua and
    // not-ready rows: admission runs it, and the link reports no revision.
    assert!(matches!(
        l.admit(&invoke(3), opcode::EXTENSION_INVOKE as u16, Some(b"y")),
        Admit::Run
    ));
    assert!(l.has_extension(b"x"));
    assert!(!l.has_extension(b"y"));
    l.set_campaign(CAMPAIGN + 1);
    let mut h = invoke(4);
    h.campaign = CAMPAIGN + 1;
    assert!(matches!(
        l.admit(&h, opcode::EXTENSION_INVOKE as u16, Some(b"x")),
        Admit::Run
    ));
    assert!(!l.has_extension(b"x"));
}

#[test]
fn a_burst_outside_paused_is_noted_once() {
    let (mut l, _d) = link("admit-burst-note");
    l.lifecycle(lifecycle::RUNNING as u32, "", "");
    l.take_notes();
    l.set_burst(true);
    l.set_burst(true);
    let notes = l.take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(!l.unbudgeted());
}

#[test]
fn a_peek_with_a_full_control_queue_sets_commands_held() {
    use nanoprobe_transport::layout::flags;
    let (mut l, _d) = link("peek-held");
    l.flags |= flags::ATTACHED as u32;
    l.reading_commands = true;
    l.inbound_fresh = true;
    l.control_count = l.opts.control_queue;
    let mut buf = Vec::new();
    assert_eq!(l.peek_command(&mut buf), None);
    assert_ne!(l.flags() & flags::COMMANDS_HELD as u32, 0);
    l.control_count = 0;
    assert_eq!(l.peek_command(&mut buf), None, "the ring is empty");
    assert_eq!(l.flags() & flags::COMMANDS_HELD as u32, 0);
}

/// The RESULT `answer` queued last.
fn last_result(l: &Link) -> nanoprobe_transport::records::CommandResult {
    let item = l.queue.back().expect("a queued record");
    assert_eq!(
        u64::from(item.kind),
        nanoprobe_transport::layout::kind::RESULT
    );
    nanoprobe_transport::records::CommandResult::decode(&item.payload).unwrap()
}

#[test]
fn an_oversized_result_is_too_large_only_when_it_was_applied() {
    use nanoprobe_transport::codec::Lv;
    let (mut l, _d) = link("oversized-result");
    let max = l.out.ring().max_payload();
    let big = || Lv::String(vec![b'x'; max + 1]);
    let g = generation(&l);
    let h = header(1, 5, 9, 1, g);
    let op = opcode::EXTENSION_INVOKE as u16;
    l.answer(op, &h, result::APPLIED, 0, 0, "", big());
    let r = last_result(&l);
    assert_eq!(u64::from(r.code), result::TOO_LARGE);
    assert_eq!(r.value, Lv::Nil);
    assert!(r.message.contains("record limit"), "{}", r.message);
    for code in [result::LUA_ERROR, result::DCS_REFUSED] {
        l.answer(op, &h, code, 0, 0, "m", big());
        let r = last_result(&l);
        assert_eq!(u64::from(r.code), code);
        assert_eq!(r.value, Lv::Nil);
        assert!(r.message.contains("record limit"), "{}", r.message);
    }
}

#[test]
fn a_spawn_clears_the_order_and_a_destroy_clears_both() {
    use crate::revisions::RevKey;
    let (mut l, _d) = link("admit-clear");
    let g = generation(&l);
    let ok = result::APPLIED;
    run(
        &mut l,
        &header(1, 5, 9, 3, g),
        opcode::GROUP_SPAWN,
        None,
        ok,
    );
    run(
        &mut l,
        &header(2, 5, 9, 2, g),
        opcode::GROUP_ORDER,
        None,
        ok,
    );
    assert_eq!(l.revisions().get(&RevKey::Order(9)), 2);
    l.clear_order(9);
    assert_eq!(l.revisions().get(&RevKey::Order(9)), 0);
    assert_eq!(l.revisions().get(&RevKey::Spec(9)), 3);
    // The order is sent again at its own revision.
    assert_eq!(
        run(
            &mut l,
            &header(3, 5, 9, 1, g),
            opcode::GROUP_ORDER,
            None,
            result::APPLIED
        ),
        result::APPLIED
    );
    l.clear_entity(9);
    assert_eq!(l.revisions().get(&RevKey::Spec(9)), 0);
    assert_eq!(l.revisions().get(&RevKey::Order(9)), 0);
}
