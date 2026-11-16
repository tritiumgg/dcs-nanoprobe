//! Idempotency keys (spec/errors.md, "Public API").
//!
//! Every changing call carries a key. The first call with a key runs; a
//! repeat with the same request returns the first call's response and does
//! nothing else, and a repeat while the first is still running waits for it.
//! A repeat with a different request is `IDEMPOTENCY_MISMATCH`. A call that
//! fails is not remembered, so a repeat after the cause is fixed (a new
//! lease, an active campaign) runs as a new call.
//!
//! The table is pure: time comes in as an argument, and a request is compared
//! by a digest of its method and encoding with the lease left out.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// A digest of one call: its method and its request bytes.
pub type Digest32 = [u8; 32];

/// The digest of a request to a method.
pub fn digest(method: &str, request: &[u8]) -> Digest32 {
    let mut h = Sha256::new();
    h.update(method.as_bytes());
    h.update([0]);
    h.update(request);
    h.finalize().into()
}

/// A finished call as remembered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    /// The method, such as `PutGroup`.
    pub method: &'static str,
    /// The encoded response.
    pub response: Vec<u8>,
    /// The campaign the call resolved to, 0 for none.
    pub campaign: u64,
    /// For an effect or a query that returned `pending`: the command to read
    /// the result of, and what to build from it.
    pub pending: Option<Pending>,
}

/// What an effect or a query call returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectKind {
    /// `AdjustWarehouseStock`.
    Adjust,
    /// `DoEffect`, whatever the effect.
    Damage,
    /// `Capture` of the entity.
    Capture(u64),
    /// `SendMessage`.
    Message,
    /// `SendChat`.
    Chat,
    /// `InvokeLua`.
    Invoke,
    /// `EvalLua`.
    Eval,
    /// `Kick`.
    Kick,
    /// `Ban`.
    Ban,
    /// `Unban`.
    Unban,
    /// `ListBans`.
    Bans,
    /// `MovePlayer`.
    Move,
}

/// A command whose result the call did not wait for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    /// The command's epoch and sequence number.
    pub cmd: (u64, u64),
    /// What to build.
    pub kind: EffectKind,
}

/// Every changing method's name, so an answer read back from a store gets
/// its `&'static str`. A test checks it against every method the services
/// name.
const METHODS: &[&str] = &[
    "ActivateCampaign",
    "AdjustWarehouseStock",
    "BackupCampaign",
    "Ban",
    "Capture",
    "CreateCampaign",
    "DeleteCampaign",
    "Dematerialize",
    "DoEffect",
    "EvalLua",
    "FinishCampaign",
    "InvokeLua",
    "Kick",
    "ListBans",
    "Materialize",
    "MovePlayer",
    "PutDrawing",
    "PutGroup",
    "PutMark",
    "PutMenuItem",
    "PutStatic",
    "RegisterLua",
    "Reload",
    "RemoveEntity",
    "RemoveLua",
    "SendChat",
    "SendMessage",
    "SetAirbaseOwner",
    "SetLostPolicy",
    "SetNextMission",
    "SetOptions",
    "SetOrder",
    "SetSampleRates",
    "SetWarehouseStock",
    "SuspendCampaign",
    "Unban",
];

/// An answer as a store keeps it (spec/persistence.md, "Idempotency keys"):
/// an `lv` table of the method, the response, the campaign and, for a
/// pending effect, the command and what to build.
pub fn encode_done(done: &Done) -> Vec<u8> {
    use nanoprobe_transport::codec::{Lv, Writer};
    let s = |v: &str| Lv::String(v.as_bytes().to_vec());
    let mut pairs = vec![
        (s("method"), s(done.method)),
        (s("response"), Lv::String(done.response.clone())),
        (s("campaign"), s(&done.campaign.to_string())),
    ];
    if let Some(p) = done.pending {
        let (kind, entity) = match p.kind {
            EffectKind::Adjust => ("adjust", 0),
            EffectKind::Damage => ("damage", 0),
            EffectKind::Capture(e) => ("capture", e),
            EffectKind::Message => ("message", 0),
            EffectKind::Chat => ("chat", 0),
            EffectKind::Invoke => ("invoke", 0),
            EffectKind::Eval => ("eval", 0),
            EffectKind::Kick => ("kick", 0),
            EffectKind::Ban => ("ban", 0),
            EffectKind::Unban => ("unban", 0),
            EffectKind::Bans => ("bans", 0),
            EffectKind::Move => ("move", 0),
        };
        pairs.push((s("epoch"), s(&p.cmd.0.to_string())));
        pairs.push((s("seq"), s(&p.cmd.1.to_string())));
        pairs.push((s("kind"), s(kind)));
        pairs.push((s("entity"), s(&entity.to_string())));
    }
    let mut w = Writer::new();
    w.lv(&Lv::Table(pairs));
    w.finish()
}

/// [`encode_done`] read back; `None` for bytes it did not write.
pub fn decode_done(bytes: &[u8]) -> Option<Done> {
    use nanoprobe_transport::codec::{Lv, Reader};
    let Lv::Table(pairs) = Reader::new(bytes).lv().ok()? else {
        return None;
    };
    let get = |k: &str| {
        pairs
            .iter()
            .find(|(key, _)| matches!(key, Lv::String(t) if t == k.as_bytes()))
            .and_then(|(_, v)| match v {
                Lv::String(t) => Some(t.clone()),
                _ => None,
            })
    };
    let text = |k: &str| get(k).and_then(|t| String::from_utf8(t).ok());
    let num = |k: &str| text(k).and_then(|t| t.parse::<u64>().ok());
    let method = text("method")?;
    let method = METHODS.iter().copied().find(|m| *m == method)?;
    let pending = match text("kind").as_deref() {
        None => None,
        Some(k) => Some(Pending {
            cmd: (num("epoch")?, num("seq")?),
            kind: match k {
                "adjust" => EffectKind::Adjust,
                "damage" => EffectKind::Damage,
                "capture" => EffectKind::Capture(num("entity")?),
                "message" => EffectKind::Message,
                "chat" => EffectKind::Chat,
                "invoke" => EffectKind::Invoke,
                "eval" => EffectKind::Eval,
                "kick" => EffectKind::Kick,
                "ban" => EffectKind::Ban,
                "unban" => EffectKind::Unban,
                "bans" => EffectKind::Bans,
                "move" => EffectKind::Move,
                _ => return None,
            },
        }),
    };
    Some(Done {
        method,
        response: get("response")?,
        campaign: num("campaign")?,
        pending,
    })
}

/// What a call with a key should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Begin {
    /// Run the call, then [`Idem::finish`] or [`Idem::fail`] it.
    Run,
    /// Return this response.
    Done(Done),
    /// The first call with this key is still running: wait and ask again.
    Wait,
    /// The key was used with another request.
    Mismatch,
}

#[derive(Clone, Debug)]
enum Slot {
    Running(Digest32),
    /// Finished at the instant, which is also its entry in `order`.
    Done(Digest32, Done, Instant),
}

/// The keys of one server.
#[derive(Clone, Debug)]
pub struct Idem {
    keep: Duration,
    slots: HashMap<String, Slot>,
    /// Finished keys in the order they finished, to forget them in time.
    order: VecDeque<(Instant, String)>,
}

impl Idem {
    /// An empty table that remembers a finished key for `keep`
    /// (`idempotency_keep_h`).
    pub fn new(keep: Duration) -> Idem {
        Idem {
            keep,
            slots: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn expire(&mut self, now: Instant) {
        while let Some((at, _)) = self.order.front() {
            if now.duration_since(*at) < self.keep {
                break;
            }
            let (at, key) = self.order.pop_front().expect("front");
            // The slot is this entry's unless the key failed and ran again
            // since, in which case it has another finish time or none.
            if matches!(self.slots.get(&key), Some(Slot::Done(_, _, t)) if *t == at) {
                self.slots.remove(&key);
            }
        }
    }

    /// Start a call with `key` and the request's digest.
    pub fn begin(&mut self, now: Instant, key: &str, d: Digest32) -> Begin {
        self.expire(now);
        match self.slots.get(key) {
            None => {
                self.slots.insert(key.to_owned(), Slot::Running(d));
                Begin::Run
            }
            Some(Slot::Running(x)) if *x == d => Begin::Wait,
            Some(Slot::Done(x, done, _)) if *x == d => Begin::Done(done.clone()),
            Some(_) => Begin::Mismatch,
        }
    }

    /// The call with `key` succeeded with this response.
    pub fn finish(&mut self, now: Instant, key: &str, done: Done) {
        if let Some(slot) = self.slots.get_mut(key)
            && let Slot::Running(d) = *slot
        {
            *slot = Slot::Done(d, done, now);
            self.order.push_back((now, key.to_owned()));
        }
    }

    /// The call with `key` failed: forget the key.
    pub fn fail(&mut self, key: &str) {
        if matches!(self.slots.get(key), Some(Slot::Running(_))) {
            self.slots.remove(key);
        }
    }

    /// Replace the remembered response of a finished call: a pending effect
    /// that has its result now.
    pub fn update(&mut self, key: &str, done: Done) {
        if let Some(Slot::Done(_, d, _)) = self.slots.get_mut(key) {
            *d = done;
        }
    }

    /// A finished call by its key, for `GetOutcome`.
    pub fn get(&mut self, now: Instant, key: &str) -> Option<Done> {
        self.expire(now);
        match self.slots.get(key) {
            Some(Slot::Done(_, d, _)) => Some(d.clone()),
            _ => None,
        }
    }

    /// Put back a call a store remembered, answered `age` ago, in place of
    /// whatever the table holds for `key`. A key past `keep` is not put back.
    pub fn restore(&mut self, now: Instant, key: &str, d: Digest32, done: Done, age: Duration) {
        if age >= self.keep {
            return;
        }
        let at = now.checked_sub(age).unwrap_or(now);
        self.slots.insert(key.to_owned(), Slot::Done(d, done, at));
        // `order` is kept sorted: the entry goes where its time belongs.
        let i = self.order.partition_point(|(t, _)| *t <= at);
        self.order.insert(i, (at, key.to_owned()));
    }

    /// The request digest a key was used with.
    pub fn digest_of(&self, key: &str) -> Option<Digest32> {
        match self.slots.get(key)? {
            Slot::Running(d) | Slot::Done(d, _, _) => Some(*d),
        }
    }

    /// Whether a call with `key` is running.
    pub fn running(&self, key: &str) -> bool {
        matches!(self.slots.get(key), Some(Slot::Running(_)))
    }

    /// Keys remembered or running.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no key is remembered or running.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    /// Every method name a service passes to `changing!`, `moderating!` or
    /// a `Call`, read from the services' source.
    fn service_methods() -> Vec<String> {
        let sources = [
            include_str!("services/campaign.rs"),
            include_str!("services/commands.rs"),
            include_str!("services/extension.rs"),
            include_str!("services/mission.rs"),
            include_str!("services/moderation.rs"),
            include_str!("services/players.rs"),
        ];
        let mut out = Vec::new();
        for src in sources {
            for start in ["changing!(", "moderating!(", "method: \""] {
                for (i, _) in src.match_indices(start) {
                    let rest = &src[i + start.len()..];
                    // A field's literal starts right away; a macro's is the
                    // first one after its name, unless the call ends first.
                    let lit = if start.ends_with('"') {
                        rest
                    } else {
                        let Some(q) = rest.find('"') else { continue };
                        if rest[..q].contains(')') {
                            continue;
                        }
                        &rest[q + 1..]
                    };
                    let Some(end) = lit.find('"') else { continue };
                    let name = &lit[..end];
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()) {
                        out.push(name.to_owned());
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn every_service_method_round_trips_through_a_store() {
        let methods = service_methods();
        assert!(methods.len() >= 30, "found only {methods:?}");
        for m in &methods {
            let method = METHODS
                .iter()
                .copied()
                .find(|x| x == m)
                .unwrap_or_else(|| panic!("{m} is not in METHODS"));
            let done = Done {
                method,
                response: vec![1, 2],
                campaign: 3,
                pending: None,
            };
            assert_eq!(decode_done(&encode_done(&done)), Some(done), "{m}");
        }
        let kinds = [
            EffectKind::Adjust,
            EffectKind::Damage,
            EffectKind::Capture(9),
            EffectKind::Message,
            EffectKind::Chat,
            EffectKind::Invoke,
            EffectKind::Eval,
            EffectKind::Kick,
            EffectKind::Ban,
            EffectKind::Unban,
            EffectKind::Bans,
            EffectKind::Move,
        ];
        for kind in kinds {
            let done = Done {
                method: "DoEffect",
                response: Vec::new(),
                campaign: 1,
                pending: Some(Pending { cmd: (4, 5), kind }),
            };
            assert_eq!(decode_done(&encode_done(&done)), Some(done), "{kind:?}");
        }
    }

    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap as Map;

    const KEEP: Duration = Duration::from_secs(3600);

    fn done(n: u8) -> Done {
        Done {
            method: "PutGroup",
            response: vec![n],
            campaign: 1,
            pending: None,
        }
    }

    #[test]
    fn a_repeat_returns_the_first_response() {
        let t = Instant::now();
        let mut i = Idem::new(KEEP);
        let d = digest("PutGroup", b"a");
        assert_eq!(i.begin(t, "k", d), Begin::Run);
        assert_eq!(i.begin(t, "k", d), Begin::Wait);
        assert_eq!(i.begin(t, "k", digest("PutGroup", b"b")), Begin::Mismatch);
        assert_eq!(i.begin(t, "k", digest("SetOrder", b"a")), Begin::Mismatch);
        i.finish(t, "k", done(7));
        assert_eq!(i.begin(t, "k", d), Begin::Done(done(7)));
        assert_eq!(i.get(t, "k"), Some(done(7)));
        // Forgotten after the keep time: a repeat is a new call.
        assert_eq!(i.get(t + KEEP, "k"), None);
        assert_eq!(i.begin(t + KEEP, "k", d), Begin::Run);
    }

    #[test]
    fn a_failed_call_is_not_remembered() {
        let t = Instant::now();
        let mut i = Idem::new(KEEP);
        let d = digest("PutGroup", b"a");
        assert_eq!(i.begin(t, "k", d), Begin::Run);
        i.fail("k");
        assert_eq!(i.get(t, "k"), None);
        assert_eq!(i.begin(t, "k", digest("PutGroup", b"other")), Begin::Run);
    }

    #[derive(Clone, Debug)]
    enum Op {
        Call(u8, u8),
        Finish(u8),
        Fail(u8),
        Advance(u16),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..4, 0u8..3).prop_map(|(k, r)| Op::Call(k, r)),
            (0u8..4).prop_map(Op::Finish),
            (0u8..4).prop_map(Op::Fail),
            (0u16..2000).prop_map(Op::Advance),
        ]
    }

    proptest! {
        /// Against a model: a key runs at most once until it fails or is
        /// forgotten; a remembered key returns its first response for the
        /// same request and a mismatch for any other.
        #[test]
        fn matches_the_model(ops in proptest::collection::vec(op(), 1..300)) {
            let keep = Duration::from_secs(1000);
            let t0 = Instant::now();
            let mut now = t0;
            let mut i = Idem::new(keep);
            // key -> (request, finished at and response)
            let mut model: Map<u8, (u8, Option<(Instant, u8)>)> = Map::new();
            let mut next = 0u8;
            for o in ops {
                // Forget what the model says is past its time.
                model.retain(|_, (_, f)| f.is_none_or(|(at, _)| now.duration_since(at) < keep));
                match o {
                    Op::Advance(s) => now += Duration::from_secs(u64::from(s)),
                    Op::Call(k, r) => {
                        let got = i.begin(now, &k.to_string(), digest("M", &[r]));
                        let want = match model.get(&k) {
                            None => { model.insert(k, (r, None)); Begin::Run }
                            Some((r0, _)) if *r0 != r => Begin::Mismatch,
                            Some((_, None)) => Begin::Wait,
                            Some((_, Some((_, n)))) => Begin::Done(Done { method: "M", response: vec![*n], campaign: 0, pending: None }),
                        };
                        prop_assert_eq!(got, want);
                    }
                    Op::Finish(k) => {
                        if let Some((_, f @ None)) = model.get_mut(&k) {
                            next = next.wrapping_add(1);
                            *f = Some((now, next));
                            i.finish(now, &k.to_string(), Done { method: "M", response: vec![next], campaign: 0, pending: None });
                        }
                    }
                    Op::Fail(k) => {
                        if matches!(model.get(&k), Some((_, None))) {
                            model.remove(&k);
                            i.fail(&k.to_string());
                        }
                    }
                }
            }
        }
    }
}
