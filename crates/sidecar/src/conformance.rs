//! The conformance suite: what any simulation adapter must do, from
//! spec/commands.md and spec/records.md, checked through the adapter alone.
//! `FakeSim` runs it in CI; `RealSim` (step S17) runs it against DCS.
//!
//! Each check writes commands, lets frames pass, and reads what the agent
//! wrote. A check needs a mission running, an attached sidecar, a unit type
//! DCS accepts and a point on the loaded theater where it may spawn.

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{lifecycle as lc, result as rc};
use nanoprobe_transport::records::{CommandHeader, CommandResult};

use crate::ids::{self, CampaignId, EntityId, NameKind};
use crate::sim::SimAdapter;
use crate::sim::wire::{Body, Command, Input, gone_reason, lv};

/// What the suite needs beyond the adapter.
pub trait Target: SimAdapter {
    /// Let about one frame pass. A real target waits and polls.
    fn advance(&mut self);
    /// Load the running mission again. Returns once the load was asked for.
    fn reload(&mut self);
    /// Drop the link and attach again as the same sidecar (spec/transport.md,
    /// "Detaching" and "Attaching").
    fn reattach(&mut self);
    /// A ground unit type DCS accepts.
    fn unit_type(&self) -> &str;
    /// A point `x`, `z` on the loaded theater where a ground group may spawn.
    fn spawn_point(&self) -> (f64, f64);
    /// Frames to wait for an answer before a check fails.
    fn patience(&self) -> usize {
        200
    }
}

/// One check's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// The check.
    pub name: &'static str,
    /// `Ok`, or what went wrong.
    pub result: Result<(), String>,
}

/// The campaign the suite announces. Any ID works; this one is the
/// millisecond the suite was written.
pub const CAMPAIGN: CampaignId = CampaignId(1_763_020_000_000);

struct Session<'a> {
    t: &'a mut dyn Target,
    seen: Vec<Input>,
    epoch: u64,
    seq: u64,
    mission_gen: u32,
    next_entity: u64,
}

type R = Result<(), String>;

/// A named check.
type CheckFn = (&'static str, fn(&mut Session<'_>) -> R);

impl Session<'_> {
    fn pump(&mut self) {
        self.t.advance();
        for r in self.t.read() {
            if let Ok(i) = Input::decode(r.kind, &r.payload) {
                if let Input::Lifecycle(l) = &i
                    && u64::from(l.state) == lc::RUNNING
                {
                    self.mission_gen = l.mission_gen;
                }
                if let Input::Hello(h) = &i {
                    self.epoch = self.epoch.max(h.epoch) + 1;
                }
                self.seen.push(i);
            }
        }
    }

    /// Pump until `f` finds something in the records seen since `from`.
    fn until<T>(
        &mut self,
        from: usize,
        what: &str,
        mut f: impl FnMut(&[Input]) -> Option<T>,
    ) -> Result<T, String> {
        for _ in 0..self.t.patience() {
            if let Some(v) = f(&self.seen[from..]) {
                return Ok(v);
            }
            self.pump();
        }
        f(&self.seen[from..]).ok_or_else(|| format!("no {what} after {} frames", self.t.patience()))
    }

    fn header(&mut self, entity: EntityId, rev: u64) -> CommandHeader {
        self.seq += 1;
        CommandHeader {
            seq: self.seq,
            epoch: self.epoch,
            campaign: CAMPAIGN.0,
            entity: entity.0,
            rev,
            mission_gen: self.mission_gen,
        }
    }

    fn send(&mut self, header: CommandHeader, body: Body) -> Result<usize, String> {
        let from = self.seen.len();
        let (op, payload) = Command { header, body }.encode();
        self.t
            .send(op, &payload)
            .map_err(|_| "the adapter is detached".to_owned())?;
        Ok(from)
    }

    /// Send a sequenced command and wait for its result.
    fn call(&mut self, header: CommandHeader, body: Body) -> Result<CommandResult, String> {
        let (epoch, seq) = (header.epoch, header.seq);
        let from = self.send(header, body)?;
        self.until(from, "result", |s| {
            s.iter().find_map(|i| match i {
                Input::Result(r) if r.epoch == epoch && r.seq == seq => Some(r.clone()),
                _ => None,
            })
        })
    }

    fn control(&mut self, body: Body) -> R {
        let h = CommandHeader {
            campaign: CAMPAIGN.0,
            mission_gen: self.mission_gen,
            ..CommandHeader::default()
        };
        self.send(h, body).map(|_| ())
    }

    fn entity(&mut self) -> EntityId {
        self.next_entity += 1;
        EntityId(self.next_entity)
    }

    fn spawn_body(&self, group: EntityId, units: &[EntityId]) -> Body {
        let (x, z) = self.t.spawn_point();
        let name = ids::name(NameKind::Group, CAMPAIGN, group);
        let units = units
            .iter()
            .enumerate()
            .map(|(i, u)| {
                lv::obj(vec![
                    ("name", lv::s(&ids::name(NameKind::Unit, CAMPAIGN, *u))),
                    ("type", lv::s(self.t.unit_type())),
                    ("x", lv::n(x + 15.0 * i as f64)),
                    ("y", lv::n(z)),
                    ("heading", lv::n(0.0)),
                ])
            })
            .collect();
        Body::GroupSpawn {
            country: 2,
            category: 2,
            group: lv::obj(vec![("name", lv::s(&name)), ("units", lv::list(units))]),
            name,
        }
    }
}

fn expect(r: &CommandResult, code: u64, what: &str) -> R {
    if u64::from(r.code) == code {
        Ok(())
    } else {
        Err(format!(
            "{what}: result {} ({:?}), want {code}",
            r.code, r.message
        ))
    }
}

/// The agent writes a LIFECYCLE of its state right after every HELLO, so a
/// sidecar attaching to a running mission learns it (step S10 writes the
/// rule into spec/records.md). `at` is the HELLO's index in `s.seen`.
fn lifecycle_after_hello(s: &mut Session<'_>, at: usize) -> R {
    s.until(at + 1, "a record after the HELLO", |x| {
        x.first().map(|_| ())
    })?;
    match s.seen.get(at + 1) {
        Some(Input::Lifecycle(_)) => Ok(()),
        other => Err(format!(
            "the record after the HELLO is {other:?}, not a LIFECYCLE"
        )),
    }
}

fn attach(s: &mut Session<'_>) -> R {
    let at = s.until(0, "HELLO", |x| {
        x.iter().position(|i| matches!(i, Input::Hello(_)))
    })?;
    lifecycle_after_hello(s, at)?;
    s.until(0, "running mission", |x| {
        x.iter()
            .any(|i| matches!(i, Input::Lifecycle(l) if u64::from(l.state) == lc::RUNNING && !l.theater.is_empty()))
            .then_some(())
    })?;
    s.control(Body::Campaign(CAMPAIGN.0))
}

fn spawn_is_guarded_by_revision(s: &mut Session<'_>) -> R {
    let (g, u1, u2) = (s.entity(), s.entity(), s.entity());
    let body = s.spawn_body(g, &[u1, u2]);
    let h = s.header(g, 1);
    let from = s.seen.len();
    let r = s.call(h, body.clone())?;
    expect(&r, rc::APPLIED, "first spawn")?;
    if r.object == 0 {
        return Err("a spawn's result names no DCS group".into());
    }
    let objects = s.until(from, "OBJECT for each unit", |x| {
        let names: Vec<String> = x
            .iter()
            .filter_map(|i| match i {
                Input::Object(o) if o.enumeration == 0 && o.group == r.object => {
                    Some(o.name.clone())
                }
                _ => None,
            })
            .collect();
        (names.len() == 2).then_some(names)
    })?;
    for u in [u1, u2] {
        if !objects.contains(&ids::name(NameKind::Unit, CAMPAIGN, u)) {
            return Err(format!("no OBJECT names unit {u}"));
        }
    }
    let h = s.header(g, 1);
    expect(
        &s.call(h, body.clone())?,
        rc::AT_REVISION,
        "spawn again at its revision",
    )?;
    let h = s.header(g, 0);
    expect(&s.call(h, body)?, rc::BAD_PAYLOAD, "spawn with no revision")
}

fn sequence_epoch_campaign_and_generation(s: &mut Session<'_>) -> R {
    let g = s.entity();
    let h = s.header(g, 0);
    expect(
        &s.call(h, Body::GroupDestroy)?,
        rc::APPLIED,
        "destroy of nothing",
    )?;
    expect(
        &s.call(h, Body::GroupDestroy)?,
        rc::DUPLICATE,
        "the same sequence number",
    )?;
    let mut h = s.header(g, 0);
    h.epoch -= 1;
    expect(
        &s.call(h, Body::GroupDestroy)?,
        rc::STALE_EPOCH,
        "an older epoch",
    )?;
    let mut h = s.header(g, 0);
    h.campaign += 1;
    expect(
        &s.call(h, Body::GroupDestroy)?,
        rc::WRONG_CAMPAIGN,
        "another campaign",
    )?;
    let mut h = s.header(g, 0);
    h.mission_gen += 1;
    expect(
        &s.call(h, Body::GroupDestroy)?,
        rc::WRONG_GENERATION,
        "another generation",
    )?;
    // A higher epoch starts the sequence again.
    s.epoch += 1;
    s.seq = 0;
    let h = s.header(g, 0);
    expect(
        &s.call(h, Body::GroupDestroy)?,
        rc::APPLIED,
        "a new epoch from 1",
    )
}

fn order_is_guarded_by_revision(s: &mut Session<'_>) -> R {
    let (g, u) = (s.entity(), s.entity());
    let body = s.spawn_body(g, &[u]);
    let h = s.header(g, 1);
    expect(&s.call(h, body)?, rc::APPLIED, "spawn")?;
    let task = lv::obj(vec![("id", lv::s("Hold")), ("params", lv::obj(vec![]))]);
    let h = s.header(g, 1);
    expect(
        &s.call(h, Body::GroupOrder(task.clone()))?,
        rc::APPLIED,
        "order",
    )?;
    let h = s.header(g, 1);
    expect(
        &s.call(h, Body::GroupOrder(task.clone()))?,
        rc::AT_REVISION,
        "order again",
    )?;
    let h = s.header(g, 2);
    expect(
        &s.call(h, Body::GroupOrder(task))?,
        rc::APPLIED,
        "a newer order",
    )
}

fn enumeration_counts_its_objects(s: &mut Session<'_>) -> R {
    let from = s.seen.len();
    s.control(Body::Enumerate)?;
    let id = s.until(from, "ENUM_BEGIN", |x| {
        x.iter().find_map(|i| match i {
            Input::EnumBegin(e) => Some(e.enumeration),
            _ => None,
        })
    })?;
    let (end, found) = s.until(from, "ENUM_END", |x| {
        let end = x.iter().find_map(|i| match i {
            Input::EnumEnd(e) if e.enumeration == id => Some((e.objects, e.seen)),
            _ => None,
        })?;
        let objects = x
            .iter()
            .filter(|i| matches!(i, Input::Object(o) if o.enumeration == id))
            .count();
        let seen: usize = x
            .iter()
            .map(|i| match i {
                Input::Seen(r) if r.enumeration == id => r.objects.len(),
                _ => 0,
            })
            .sum();
        Some((end, (objects, seen)))
    })?;
    if (end.0 as usize, end.1 as usize) != found {
        return Err(format!(
            "ENUM_END says {} OBJECT and {} SEEN, the enumeration wrote {} and {}",
            end.0, end.1, found.0, found.1
        ));
    }
    if id == 0 {
        return Err("enumeration 0".into());
    }
    Ok(())
}

fn capture_and_destroy(s: &mut Session<'_>) -> R {
    let (g, u1, u2) = (s.entity(), s.entity(), s.entity());
    let body = s.spawn_body(g, &[u1, u2]);
    let h = s.header(g, 1);
    let spawned = s.call(h, body)?;
    expect(&spawned, rc::APPLIED, "spawn")?;
    let from = s.seen.len();
    let h = s.header(g, 0);
    let r = s.call(h, Body::GroupCaptureDestroy)?;
    expect(&r, rc::APPLIED, "capture and destroy")?;
    let names: Vec<String> = lv::items(&r.value)
        .into_iter()
        .filter_map(|t| lv::str_at(t, "name"))
        .collect();
    for u in [u1, u2] {
        if !names.contains(&ids::name(NameKind::Unit, CAMPAIGN, u)) {
            return Err(format!("the capture does not name unit {u}: {names:?}"));
        }
    }
    let objects: Vec<u64> = s.seen[..from]
        .iter()
        .filter_map(|i| match i {
            Input::Object(o) if o.group == spawned.object => Some(o.object),
            _ => None,
        })
        .collect();
    s.until(from, "OBJECT_GONE for each unit", |x| {
        objects
            .iter()
            .all(|o| {
                x.iter()
                    .any(|i| matches!(i, Input::ObjectGone(g) if g.object == *o && g.reason == gone_reason::DESTROYED))
            })
            .then_some(())
    })?;
    let h = s.header(g, 0);
    let again = s.call(h, Body::GroupCaptureDestroy)?;
    expect(&again, rc::APPLIED, "capture and destroy of nothing")?;
    if again.value != Lv::Nil {
        return Err("a capture of nothing returns a value".into());
    }
    Ok(())
}

/// spec/transport.md, "Attaching", and spec/commands.md, "Unanswered
/// commands": after a reattach the `HELLO`'s `in_head` covers exactly the
/// commands the agent took, each with its `RESULT` before the `HELLO`; a
/// command after it was discarded and is never answered.
fn a_reattach_answers_what_was_taken(s: &mut Session<'_>) -> R {
    let g = s.entity();
    let mut sent = Vec::new();
    for _ in 0..8 {
        let h = s.header(g, 0);
        let (op, payload) = Command {
            header: h,
            body: Body::GroupDestroy,
        }
        .encode();
        let pos =
            s.t.send(op, &payload)
                .map_err(|_| "the adapter is detached".to_owned())?;
        sent.push((h.epoch, h.seq, pos));
    }
    s.pump();
    let from = s.seen.len();
    s.t.reattach();
    let (hello_at, in_head) = s.until(from, "HELLO after the reattach", |x| {
        x.iter().enumerate().find_map(|(i, r)| match r {
            Input::Hello(h) => Some((i, h.in_head)),
            _ => None,
        })
    })?;
    lifecycle_after_hello(s, from + hello_at)?;
    for _ in 0..5 {
        s.pump();
    }
    let answered = |x: &[Input], e: u64, q: u64| {
        x.iter()
            .any(|i| matches!(i, Input::Result(r) if r.epoch == e && r.seq == q))
    };
    let before: Vec<Input> = s.seen[..from + hello_at].to_vec();
    let after: Vec<Input> = s.seen[from + hello_at..].to_vec();
    for (e, q, pos) in sent {
        let taken = pos <= in_head;
        if taken && !answered(&before, e, q) {
            return Err(format!(
                "seq {q} at {pos} is at or before in_head {in_head} and has no RESULT before the HELLO"
            ));
        }
        if !taken && (answered(&before, e, q) || answered(&after, e, q)) {
            return Err(format!(
                "seq {q} at {pos} is after in_head {in_head} and was answered"
            ));
        }
    }
    Ok(())
}

fn an_order_for_a_group_dcs_lacks_is_not_bound(s: &mut Session<'_>) -> R {
    let g = s.entity();
    let task = lv::obj(vec![("id", lv::s("Hold")), ("params", lv::obj(vec![]))]);
    let h = s.header(g, 1);
    expect(
        &s.call(h, Body::GroupOrder(task))?,
        rc::NOT_BOUND,
        "an order for no group",
    )
}

fn a_mission_load_forgets_revisions(s: &mut Session<'_>) -> R {
    let (g, u) = (s.entity(), s.entity());
    let body = s.spawn_body(g, &[u]);
    let h = s.header(g, 1);
    expect(&s.call(h, body.clone())?, rc::APPLIED, "spawn")?;
    let old = s.mission_gen;
    let from = s.seen.len();
    s.t.reload();
    s.until(from, "the new generation running", |x| {
        x.iter()
            .any(|i| matches!(i, Input::Lifecycle(l) if l.mission_gen > old && u64::from(l.state) == lc::RUNNING))
            .then_some(())
    })?;
    if s.seen[from..]
        .iter()
        .any(|i| matches!(i, Input::ObjectGone(_)))
    {
        return Err("a mission load wrote OBJECT_GONE".into());
    }
    let mut h = s.header(g, 1);
    h.mission_gen = old;
    expect(
        &s.call(h, body.clone())?,
        rc::WRONG_GENERATION,
        "spawn for the old generation",
    )?;
    let h = s.header(g, 1);
    expect(&s.call(h, body)?, rc::APPLIED, "spawn at revision 1 again")
}

/// Run every check, in order. A check that fails does not stop the others,
/// but a failed attach does.
pub fn run(t: &mut dyn Target) -> Vec<Check> {
    let mut s = Session {
        t,
        seen: Vec::new(),
        epoch: 0,
        seq: 0,
        mission_gen: 0,
        next_entity: 0,
    };
    let mut out = vec![Check {
        name: "attach",
        result: attach(&mut s),
    }];
    if out[0].result.is_err() {
        return out;
    }
    let checks: [CheckFn; 8] = [
        ("spawn_is_guarded_by_revision", spawn_is_guarded_by_revision),
        (
            "sequence_epoch_campaign_and_generation",
            sequence_epoch_campaign_and_generation,
        ),
        ("order_is_guarded_by_revision", order_is_guarded_by_revision),
        (
            "enumeration_counts_its_objects",
            enumeration_counts_its_objects,
        ),
        ("capture_and_destroy", capture_and_destroy),
        (
            "a_reattach_answers_what_was_taken",
            a_reattach_answers_what_was_taken,
        ),
        (
            "an_order_for_a_group_dcs_lacks_is_not_bound",
            an_order_for_a_group_dcs_lacks_is_not_bound,
        ),
        (
            "a_mission_load_forgets_revisions",
            a_mission_load_forgets_revisions,
        ),
    ];
    for (name, f) in checks {
        out.push(Check {
            name,
            result: f(&mut s),
        });
    }
    out
}
