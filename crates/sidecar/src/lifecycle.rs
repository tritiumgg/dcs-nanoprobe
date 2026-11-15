//! The lifecycle of a managed group or static (spec/world.md, "Lifecycle").
//!
//! [`next`] is the transition table and nothing else: it says where a state
//! goes on a trigger, or that the trigger does not apply. The reconciler
//! decides when a trigger has happened.

/// A lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    /// Simulated by the campaign, not in DCS.
    Virtual,
    /// A spawn was sent and not yet acknowledged.
    Materializing,
    /// In DCS, at least one member alive.
    Live,
    /// Leaving DCS: a capture-and-destroy, or for a static a capture and
    /// then a destroy, was sent and DCS may still hold the entity.
    Dematerializing,
    /// Every member has a death fact. Final.
    Dead,
    /// Missing from DCS with no death fact, in the same mission generation.
    Lost,
    /// DCS refused the spawn. Not retried.
    Unspawnable,
}

impl State {
    /// Every state.
    pub const ALL: [State; 7] = [
        State::Virtual,
        State::Materializing,
        State::Live,
        State::Dematerializing,
        State::Dead,
        State::Lost,
        State::Unspawnable,
    ];

    /// The name the spec and the API use.
    pub fn name(self) -> &'static str {
        match self {
            State::Virtual => "virtual",
            State::Materializing => "materializing",
            State::Live => "live",
            State::Dematerializing => "dematerializing",
            State::Dead => "dead",
            State::Lost => "lost",
            State::Unspawnable => "unspawnable",
        }
    }

    /// Whether DCS should hold the entity in this state.
    pub fn wants_dcs(self) -> bool {
        matches!(self, State::Materializing | State::Live)
    }
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// The authority is DCS and the entity's theater is loaded: a level
    /// condition, checked on every plan.
    Materialize,
    /// The spawn's outcome is `applied`.
    SpawnApplied,
    /// The spawn's outcome is `refused`.
    SpawnRefused,
    /// The spawn's outcome is `unknown`: it is sent again.
    SpawnUnknown,
    /// Every member has a death fact. From `lost` it follows a death event
    /// that comes after the members left as `MISSING` or `REMOVED`.
    AllDead,
    /// The authority moved to the campaign.
    Dematerialize,
    /// The capture result arrived, or failed: the entity takes the captured
    /// or the last checkpointed state.
    CaptureDone,
    /// The mission generation changed. `same_theater` says whether the new
    /// mission's theater is the entity's.
    NewGeneration {
        /// The new mission is on the entity's theater.
        same_theater: bool,
    },
    /// A full enumeration found no member, and no death fact explains it.
    EnumMissing,
    /// A full enumeration found a member.
    EnumFound,
    /// The author's policy for a lost group is respawn.
    PolicyRespawn,
    /// The author's policy for a lost group is mark dead.
    PolicyDead,
    /// The author changed the spec.
    SpecChanged,
    /// The authority moved to the campaign, or the entity's theater is not
    /// loaded, and no spawn of it is unanswered.
    Withdraw,
    /// Every member without a death fact left DCS in an earlier generation
    /// with no death fact, so none may be spawned (spec/persistence.md,
    /// "Restore").
    HeldOut,
    /// The destroy that ends a dematerialization was refused or unknown as
    /// many times as the limit allows.
    DestroyFailed,
}

impl Trigger {
    /// One of each trigger, both values of `same_theater` included.
    pub const ALL: [Trigger; 16] = [
        Trigger::Materialize,
        Trigger::SpawnApplied,
        Trigger::SpawnRefused,
        Trigger::SpawnUnknown,
        Trigger::AllDead,
        Trigger::Dematerialize,
        Trigger::CaptureDone,
        Trigger::NewGeneration { same_theater: true },
        Trigger::NewGeneration {
            same_theater: false,
        },
        Trigger::EnumMissing,
        Trigger::EnumFound,
        Trigger::PolicyRespawn,
        Trigger::PolicyDead,
        Trigger::SpecChanged,
        Trigger::Withdraw,
        Trigger::DestroyFailed,
    ];
}

/// The state `state` moves to on `trigger`, or `None` when the trigger does
/// not apply to it.
pub fn next(state: State, trigger: Trigger) -> Option<State> {
    use State as S;
    use Trigger as T;
    Some(match (state, trigger) {
        (S::Virtual, T::Materialize) => S::Materializing,
        (S::Materializing, T::SpawnApplied) => S::Live,
        (S::Materializing, T::SpawnRefused) => S::Unspawnable,
        (S::Materializing, T::SpawnUnknown) => S::Materializing,
        (S::Live | S::Materializing | S::Dematerializing | S::Lost | S::Virtual, T::AllDead) => {
            S::Dead
        }
        (S::Live, T::Dematerialize) => S::Dematerializing,
        (S::Dematerializing, T::CaptureDone) => S::Virtual,
        (S::Dematerializing, T::NewGeneration { .. }) => S::Virtual,
        (S::Live | S::Materializing | S::Lost, T::NewGeneration { same_theater: true }) => {
            S::Materializing
        }
        (
            S::Live | S::Materializing | S::Lost,
            T::NewGeneration {
                same_theater: false,
            },
        ) => S::Virtual,
        (S::Live, T::EnumMissing) => S::Lost,
        (S::Lost, T::EnumFound) => S::Live,
        (S::Lost, T::PolicyRespawn) => S::Materializing,
        (S::Lost, T::PolicyDead) => S::Dead,
        (S::Unspawnable, T::SpecChanged) => S::Virtual,
        (S::Materializing, T::Withdraw) => S::Virtual,
        (S::Materializing, T::HeldOut) => S::Lost,
        (S::Dematerializing, T::DestroyFailed) => S::Lost,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's transition table, row by row.
    const TABLE: &[(State, Trigger, State)] = {
        use State as S;
        use Trigger as T;
        &[
            (S::Virtual, T::Materialize, S::Materializing),
            (S::Materializing, T::SpawnApplied, S::Live),
            (S::Materializing, T::SpawnRefused, S::Unspawnable),
            (S::Materializing, T::SpawnUnknown, S::Materializing),
            (S::Materializing, T::HeldOut, S::Lost),
            (
                S::Materializing,
                T::NewGeneration { same_theater: true },
                S::Materializing,
            ),
            (S::Live, T::AllDead, S::Dead),
            (S::Live, T::Dematerialize, S::Dematerializing),
            (S::Dematerializing, T::CaptureDone, S::Virtual),
            (
                S::Dematerializing,
                T::NewGeneration { same_theater: true },
                S::Virtual,
            ),
            (
                S::Dematerializing,
                T::NewGeneration {
                    same_theater: false,
                },
                S::Virtual,
            ),
            (
                S::Live,
                T::NewGeneration {
                    same_theater: false,
                },
                S::Virtual,
            ),
            (
                S::Materializing,
                T::NewGeneration {
                    same_theater: false,
                },
                S::Virtual,
            ),
            (S::Live, T::EnumMissing, S::Lost),
            (
                S::Live,
                T::NewGeneration { same_theater: true },
                S::Materializing,
            ),
            (S::Lost, T::EnumFound, S::Live),
            (
                S::Lost,
                T::NewGeneration { same_theater: true },
                S::Materializing,
            ),
            (
                S::Lost,
                T::NewGeneration {
                    same_theater: false,
                },
                S::Virtual,
            ),
            (S::Lost, T::PolicyRespawn, S::Materializing),
            (S::Lost, T::PolicyDead, S::Dead),
            (S::Unspawnable, T::SpecChanged, S::Virtual),
            (S::Materializing, T::AllDead, S::Dead),
            (S::Dematerializing, T::AllDead, S::Dead),
            (S::Lost, T::AllDead, S::Dead),
            (S::Virtual, T::AllDead, S::Dead),
            (S::Materializing, T::Withdraw, S::Virtual),
            (S::Dematerializing, T::DestroyFailed, S::Lost),
        ]
    };

    #[test]
    fn every_pair_matches_the_table() {
        for s in State::ALL {
            for t in Trigger::ALL {
                let want = TABLE.iter().find(|r| r.0 == s && r.1 == t).map(|r| r.2);
                assert_eq!(next(s, t), want, "{s:?} on {t:?}");
            }
        }
    }

    #[test]
    fn dead_is_final() {
        for t in Trigger::ALL {
            assert_eq!(next(State::Dead, t), None);
        }
    }
}
