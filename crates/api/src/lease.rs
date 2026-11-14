//! The controller lease with a fencing epoch (plan 4.5; spec/errors.md).
//!
//! One controller holds the lease at a time. Each grant has a new epoch, and
//! a call that presents an older one fails with `LEASE_LOST`, so a controller
//! that paused, lost its lease and came back cannot act after another took
//! over. The epoch is never below the wall clock in milliseconds, so it keeps
//! growing across server restarts without a store.
//!
//! The state machine is pure: time and the lease ID come in as arguments.

use std::time::{Duration, Instant};

/// A lease as the server granted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The fencing epoch.
    pub epoch: u64,
    /// The holder's proof.
    pub lease_id: String,
}

/// Why a lease was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    /// Another controller holds a lease that has not expired: `LEASE_HELD`.
    Held,
    /// The presented epoch is not the current one, or the lease expired or
    /// was released: `LEASE_LOST`.
    Lost,
    /// No lease, an epoch never granted, or the current epoch with another
    /// lease ID: `NOT_CONTROLLER`.
    NotController,
}

#[derive(Clone, Debug)]
struct Held {
    lease_id: String,
    expires: Instant,
}

/// The lease table of one server.
#[derive(Clone, Debug)]
pub struct Leases {
    ttl: Duration,
    epoch: u64,
    /// The wall clock in milliseconds when this server started. Every epoch
    /// it grants is at least this, so a lower one is from before.
    floor: u64,
    held: Option<Held>,
}

impl Leases {
    /// No lease granted yet. `ttl` is `lease_ttl_s`; `start_ms` is the wall
    /// clock in milliseconds now.
    pub fn new(ttl: Duration, start_ms: u64) -> Leases {
        Leases {
            ttl,
            epoch: 0,
            floor: start_ms,
            held: None,
        }
    }

    /// The time a lease lasts without a renewal.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The epoch of the last grant, 0 before the first.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether a lease is held and not expired at `now`.
    pub fn held(&self, now: Instant) -> bool {
        self.held.as_ref().is_some_and(|h| now < h.expires)
    }

    /// Grant the lease under a new epoch, above both the last one and
    /// `wall_ms`. Fails with `Held` while an unexpired lease exists.
    pub fn acquire(
        &mut self,
        now: Instant,
        wall_ms: u64,
        lease_id: String,
    ) -> Result<Grant, LeaseError> {
        if self.held(now) {
            return Err(LeaseError::Held);
        }
        self.epoch = (self.epoch + 1).max(wall_ms).max(self.floor);
        self.held = Some(Held {
            lease_id: lease_id.clone(),
            expires: now + self.ttl,
        });
        Ok(Grant {
            epoch: self.epoch,
            lease_id,
        })
    }

    /// Check a presented lease. `None` is a call without one.
    pub fn check(&self, now: Instant, lease: Option<(u64, &str)>) -> Result<(), LeaseError> {
        let Some((epoch, id)) = lease else {
            return Err(LeaseError::NotController);
        };
        if epoch == 0 {
            return Err(LeaseError::NotController);
        }
        if epoch < self.epoch || (epoch < self.floor && epoch != self.epoch) {
            // Granted earlier, by this server or one before a restart.
            return Err(LeaseError::Lost);
        }
        if epoch > self.epoch {
            return Err(LeaseError::NotController);
        }
        match &self.held {
            None => Err(LeaseError::Lost),
            Some(h) if h.lease_id != id => Err(LeaseError::NotController),
            Some(h) if now >= h.expires => Err(LeaseError::Lost),
            Some(_) => Ok(()),
        }
    }

    /// Extend a valid lease by the time it lasts.
    pub fn renew(&mut self, now: Instant, lease: Option<(u64, &str)>) -> Result<(), LeaseError> {
        self.check(now, lease)?;
        if let Some(h) = &mut self.held {
            h.expires = now + self.ttl;
        }
        Ok(())
    }

    /// Give a valid lease up. The next acquire takes a new epoch at once.
    pub fn release(&mut self, now: Instant, lease: Option<(u64, &str)>) -> Result<(), LeaseError> {
        self.check(now, lease)?;
        self.held = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const TTL: Duration = Duration::from_secs(30);

    #[test]
    fn a_stale_controller_after_takeover_is_fenced() {
        let t0 = Instant::now();
        let mut l = Leases::new(TTL, 0);
        let a = l.acquire(t0, 1_000, "a".into()).expect("first grant");
        assert_eq!(a.epoch, 1_000);
        // A holds; B is refused.
        assert_eq!(l.acquire(t0, 1_001, "b".into()), Err(LeaseError::Held));
        // A pauses past its time; B takes over under a higher epoch.
        let t1 = t0 + TTL + Duration::from_millis(1);
        let b = l.acquire(t1, 1_001, "b".into()).expect("takeover");
        assert!(b.epoch > a.epoch);
        // A comes back: every call it makes is LEASE_LOST, renewal too.
        assert_eq!(l.check(t1, Some((a.epoch, "a"))), Err(LeaseError::Lost));
        assert_eq!(l.renew(t1, Some((a.epoch, "a"))), Err(LeaseError::Lost));
        assert_eq!(l.release(t1, Some((a.epoch, "a"))), Err(LeaseError::Lost));
        // A forging B's epoch without its ID is not the controller.
        assert_eq!(
            l.check(t1, Some((b.epoch, "a"))),
            Err(LeaseError::NotController)
        );
        assert_eq!(l.check(t1, Some((b.epoch, "b"))), Ok(()));
    }

    #[test]
    fn an_expired_lease_is_lost_even_before_a_takeover() {
        let t0 = Instant::now();
        let mut l = Leases::new(TTL, 0);
        let a = l.acquire(t0, 5, "a".into()).expect("grant");
        assert_eq!(l.renew(t0 + TTL / 2, Some((a.epoch, "a"))), Ok(()));
        assert_eq!(l.check(t0 + TTL, Some((a.epoch, "a"))), Ok(()));
        let late = t0 + TTL / 2 + TTL;
        assert_eq!(l.check(late, Some((a.epoch, "a"))), Err(LeaseError::Lost));
        assert!(!l.held(late));
    }

    #[test]
    fn the_epoch_grows_when_the_clock_goes_back() {
        let t0 = Instant::now();
        let mut l = Leases::new(TTL, 0);
        let a = l.acquire(t0, 2_000, "a".into()).expect("grant");
        l.release(t0, Some((a.epoch, "a"))).expect("release");
        let b = l.acquire(t0, 1_000, "b".into()).expect("grant");
        assert_eq!(b.epoch, 2_001);
        assert_eq!(l.check(t0, None), Err(LeaseError::NotController));
        assert_eq!(l.check(t0, Some((0, "b"))), Err(LeaseError::NotController));
        assert_eq!(
            l.check(t0, Some((b.epoch + 1, "b"))),
            Err(LeaseError::NotController)
        );
    }

    #[test]
    fn a_lease_from_before_a_restart_is_lost() {
        let t0 = Instant::now();
        let mut before = Leases::new(TTL, 1_000);
        let old = before.acquire(t0, 1_000, "a".into()).expect("grant");
        // The server restarts later; nothing is granted yet.
        let after = Leases::new(TTL, 5_000);
        assert_eq!(
            after.check(t0, Some((old.epoch, "a"))),
            Err(LeaseError::Lost)
        );
        assert_eq!(
            after.check(t0, Some((9_000, "a"))),
            Err(LeaseError::NotController)
        );
    }

    #[derive(Clone, Debug)]
    enum Op {
        Advance(u64),
        Acquire(u8),
        Renew(u8, u16),
        Release(u8, u16),
        Call(u8, u16),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u64..40_000).prop_map(Op::Advance),
            (0u8..3).prop_map(Op::Acquire),
            (0u8..3, any::<u16>()).prop_map(|(c, g)| Op::Renew(c, g)),
            (0u8..3, any::<u16>()).prop_map(|(c, g)| Op::Release(c, g)),
            (0u8..3, any::<u16>()).prop_map(|(c, g)| Op::Call(c, g)),
        ]
    }

    /// The model: the newest grant, when it expires, whether it was given up.
    #[derive(Clone, Debug)]
    struct Model {
        epoch: u64,
        id: String,
        owner: u8,
        expires: Instant,
        released: bool,
    }

    proptest! {
        /// Against a model of expiry: acquire succeeds exactly when no lease
        /// is held; the owner of the newest grant passes exactly while it
        /// is held; any other presented lease fails, an older epoch with
        /// LEASE_LOST; epochs only grow.
        #[test]
        fn fencing_holds(ops in proptest::collection::vec(op(), 1..200)) {
            let t0 = Instant::now();
            let mut now = t0;
            let mut l = Leases::new(TTL, 0);
            let mut grants: Vec<(u8, u64, String)> = Vec::new();
            let mut m: Option<Model> = None;
            let held = |m: &Option<Model>, now: Instant| {
                m.as_ref().is_some_and(|m| !m.released && now < m.expires)
            };
            for o in ops {
                match o {
                    Op::Advance(ms) => now += Duration::from_millis(ms),
                    Op::Acquire(c) => {
                        let wall = (now - t0).as_millis() as u64;
                        let id = format!("{c}-{}", grants.len());
                        let r = l.acquire(now, wall, id.clone());
                        if held(&m, now) {
                            prop_assert_eq!(r, Err(LeaseError::Held));
                        } else {
                            let g = r.expect("free");
                            prop_assert!(m.as_ref().is_none_or(|m| g.epoch > m.epoch));
                            m = Some(Model {
                                epoch: g.epoch,
                                id: id.clone(),
                                owner: c,
                                expires: now + TTL,
                                released: false,
                            });
                            grants.push((c, g.epoch, id));
                        }
                    }
                    Op::Renew(c, g) | Op::Release(c, g) | Op::Call(c, g) => {
                        if grants.is_empty() {
                            continue;
                        }
                        let (owner, epoch, id) = grants[usize::from(g) % grants.len()].clone();
                        let presented = if owner == c { id.clone() } else { format!("forged-{c}") };
                        let r = match o {
                            Op::Renew(..) => l.renew(now, Some((epoch, &presented))),
                            Op::Release(..) => l.release(now, Some((epoch, &presented))),
                            _ => l.check(now, Some((epoch, &presented))),
                        };
                        let model = m.clone().expect("a grant exists");
                        let valid = epoch == model.epoch
                            && owner == c
                            && presented == model.id
                            && owner == model.owner
                            && held(&m, now);
                        prop_assert_eq!(r.is_ok(), valid, "{:?}", r);
                        if epoch < model.epoch {
                            prop_assert_eq!(r, Err(LeaseError::Lost));
                        }
                        if valid {
                            let mm = m.as_mut().expect("model");
                            match o {
                                Op::Renew(..) => mm.expires = now + TTL,
                                Op::Release(..) => mm.released = true,
                                _ => {}
                            }
                        }
                    }
                }
                prop_assert_eq!(l.epoch(), m.as_ref().map_or(0, |m| m.epoch));
            }
        }
    }
}
