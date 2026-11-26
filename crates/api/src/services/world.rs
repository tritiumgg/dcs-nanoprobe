//! `WorldService`: list, then watch (plan 4.5; etcd- and Kubernetes-style
//! list-then-watch with one monotonic revision per campaign).
//!
//! A list is a snapshot at one store revision. A watch sends, for every
//! revision after the one it starts from, the entities that changed as they
//! now are: changes to one entity between two wake-ups are conflated into one
//! upsert. A change of position alone is no upsert: positions come in
//! batches no more often than the subscription's period, which is never
//! shorter than the server's cap. A watch buffers at most `stream_buffer`
//! messages and is ended with `CONSUMER_TOO_SLOW` past that.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::world_service_server::WorldService;
use nanoprobe_proto::v1alpha1::{
    self as pb, ErrorReason, ListWorldRequest, ListWorldResponse, PositionBatch, QueryAreaRequest,
    QueryAreaResponse, RateClass, WatchWorldRequest, WatchWorldResponse, WorldChange, world_change,
};
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::store::{Group, Point, Store};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Request, Response, Status};

use super::{Out, end, push};
use crate::auth::{Access, require};
use crate::convert::{Filter, entity, invalid, marker_entity, removal_cause};
use crate::hub::{Hub, Inner};

/// The service.
pub struct World(pub Arc<Hub>);

/// Entities per `ListWorldResponse` message.
const PAGE: usize = 256;

/// The default position period: the agent's default tier rate
/// (spec/config.md, `position_s`).
const NORMAL_PERIOD: Duration = Duration::from_secs(1);

/// The period of `RATE_CLASS_SLOW`: the store's checkpoint (`checkpoint_s`).
const SLOW_PERIOD: Duration = Duration::from_secs(5);

fn snapshot(g: &Inner, campaign: u64, filter: &Filter) -> (u64, Vec<pb::Entity>) {
    let store = g.read_store(campaign);
    let mut entities: Vec<pb::Entity> = filter
        .candidates(store)
        .into_iter()
        .filter_map(|id| store.group(id))
        .filter(|grp| filter.passes(store, grp))
        .map(|grp| entity(store, grp))
        .collect();
    entities.extend(
        store
            .markers()
            .filter(|m| filter.marker_passes(m))
            .filter_map(|m| marker_of(g, campaign, store, m.id)),
    );
    (store.revision(), entities)
}

/// The API entity of a mark or drawing, with what the active campaign's
/// reconciler knows of it in DCS.
fn marker_of(g: &Inner, campaign: u64, store: &Store, id: EntityId) -> Option<pb::Entity> {
    let m = store.marker(id)?;
    let rec = g.rec.as_ref().filter(|_| g.active == Some(campaign));
    let applied = rec.map_or(0, |r| r.marker_applied(id));
    let refused = rec.is_some_and(|r| r.marker_refused(id, store));
    marker_entity(m, applied, refused)
}

/// The longest position period a watch may ask for.
const LONGEST_PERIOD: Duration = Duration::from_secs(24 * 3600);

fn period(req: &WatchWorldRequest, cap: Duration) -> Result<Duration, Status> {
    let asked = match &req.min_period {
        Some(d) => {
            if d.seconds < 0 || d.nanos < 0 {
                return Err(invalid("min_period", "a period is not negative"));
            }
            if u64::try_from(d.seconds).unwrap_or(u64::MAX) >= LONGEST_PERIOD.as_secs() {
                return Err(invalid("min_period", "a period is under one day"));
            }
            Some(Duration::new(
                u64::try_from(d.seconds).unwrap_or(0),
                u32::try_from(d.nanos).unwrap_or(0),
            ))
        }
        None => None,
    };
    let by_class = match RateClass::try_from(req.rate).unwrap_or(RateClass::Unspecified) {
        RateClass::Fast => cap,
        RateClass::Slow => SLOW_PERIOD,
        RateClass::Normal | RateClass::Unspecified => NORMAL_PERIOD,
    };
    Ok(asked.unwrap_or(by_class).max(cap))
}

/// What one watch has sent.
struct Watch {
    campaign: u64,
    follows_active: bool,
    filter: Filter,
    revision: u64,
    /// The last position sent of each unit.
    sent: HashMap<EntityId, Point>,
    /// The entities the watcher holds: listed or sent, and not yet sent
    /// leaving the filter.
    held: HashSet<EntityId>,
    /// A digest of each entity as last sent, its positions left out.
    shapes: HashMap<EntityId, u64>,
    /// The changes from the starting revision have been read once. Until
    /// then the watcher may hold what has since left the filter.
    caught_up: bool,
}

/// A digest of an entity without its positions and attitudes, which reach
/// the watcher in position batches.
fn shape(e: &pb::Entity) -> u64 {
    use prost::Message;
    use std::hash::{Hash, Hasher};
    let mut e = e.clone();
    match &mut e.kind {
        Some(pb::entity::Kind::Group(g)) => {
            for u in &mut g.units {
                u.position = None;
                u.attitude = None;
                u.speed = 0.0;
                u.sampled = None;
            }
        }
        Some(pb::entity::Kind::Static(s)) => {
            s.position = None;
            s.heading = 0.0;
        }
        _ => {}
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    e.encode_to_vec().hash(&mut h);
    h.finish()
}

impl Watch {
    /// An upsert of the entity as it is, remembering its shape.
    fn send(&mut self, store: &Store, grp: &Group) -> WorldChange {
        let e = entity(store, grp);
        self.shapes.insert(grp.id, shape(&e));
        WorldChange {
            change: Some(world_change::Change::Upsert(e)),
        }
    }

    /// An upsert unless only positions changed since the last one sent.
    fn send_if_changed(&mut self, store: &Store, grp: &Group) -> Option<WorldChange> {
        let e = entity(store, grp);
        let d = shape(&e);
        (self.shapes.insert(grp.id, d) != Some(d)).then_some(WorldChange {
            change: Some(world_change::Change::Upsert(e)),
        })
    }

    /// The last state of an entity that left the filter, and for a removed
    /// one its removal. Its shape and its units' positions are forgotten,
    /// so it is sent in full if it comes back.
    fn leave(&mut self, store: &Store, grp: &Group, out: &mut Vec<WorldChange>) {
        self.shapes.remove(&grp.id);
        for m in &grp.members {
            self.sent.remove(m);
        }
        out.push(WorldChange {
            change: Some(world_change::Change::Upsert(entity(store, grp))),
        });
        if grp.removed {
            out.push(WorldChange {
                change: Some(world_change::Change::Removal(pb::Removal {
                    id: grp.id.0,
                    cause: if grp.expired {
                        pb::RemovalCause::Expired
                    } else {
                        pb::RemovalCause::Controller
                    } as i32,
                })),
            });
        }
    }

    /// The next message, if there is anything to send. An error ends the
    /// watch.
    fn next(&mut self, g: &Inner, positions: bool) -> Result<Option<WatchWorldResponse>, Status> {
        if self.follows_active && g.active != Some(self.campaign) {
            return Err(status(
                Code::FailedPrecondition,
                ErrorReason::CampaignNotActive,
                "the active campaign changed",
                0,
                "",
            ));
        }
        let slot = g.campaigns.get(&self.campaign).ok_or_else(|| {
            status(
                Code::NotFound,
                ErrorReason::CampaignNotFound,
                "the campaign is gone",
                0,
                "",
            )
        })?;
        let store = &slot.store;
        let mut changes = Vec::new();
        if store.revision() > self.revision {
            let oldest = store.changes().next().map_or(store.revision() + 1, |c| c.0);
            if oldest > self.revision + 1 {
                return Err(super::too_slow());
            }
            // Revisions are consecutive, so the changes after ours start
            // this far into the kept ones.
            let skip = usize::try_from(self.revision + 1 - oldest).unwrap_or(usize::MAX);
            let mut seen = HashSet::new();
            for (_, id) in store.changes().skip(skip) {
                // A unit's change shows as its group's.
                let gid = store.unit(*id).map_or(*id, |u| u.group);
                if !seen.insert(gid) {
                    continue;
                }
                if let Some(m) = store.marker(gid) {
                    if m.removed {
                        if self.held.remove(&gid) || !self.caught_up {
                            self.shapes.remove(&gid);
                            // A menu item is never served, so no watch
                            // holds it and a Removal would name an ID the
                            // client cannot resolve.
                            if let Some(e) = marker_of(g, self.campaign, store, gid) {
                                changes.push(WorldChange {
                                    change: Some(world_change::Change::Upsert(e)),
                                });
                                changes.push(WorldChange {
                                    change: Some(world_change::Change::Removal(pb::Removal {
                                        id: gid.0,
                                        cause: removal_cause(m.removed_cause) as i32,
                                    })),
                                });
                            }
                        }
                    } else if self.filter.marker_passes(m) {
                        if let Some(e) = marker_of(g, self.campaign, store, gid) {
                            let d = shape(&e);
                            self.held.insert(gid);
                            if self.shapes.insert(gid, d) != Some(d) {
                                changes.push(WorldChange {
                                    change: Some(world_change::Change::Upsert(e)),
                                });
                            }
                        }
                    } else if self.held.remove(&gid) || !self.caught_up {
                        self.shapes.remove(&gid);
                        changes.extend(marker_of(g, self.campaign, store, gid).map(|e| {
                            WorldChange {
                                change: Some(world_change::Change::Upsert(e)),
                            }
                        }));
                    }
                    continue;
                }
                let Some(grp) = store.group(gid) else {
                    // Purged: a watcher that held it, or one that resumed
                    // from before it may have, is told it left the store.
                    if store.was_purged(gid) && (self.held.remove(&gid) || !self.caught_up) {
                        changes.push(WorldChange {
                            change: Some(world_change::Change::RemovedId(gid.0)),
                        });
                        self.shapes.remove(&gid);
                        self.sent.retain(|u, _| store.unit(*u).is_some());
                    }
                    continue;
                };
                if self.filter.passes(store, grp) {
                    if self.held.insert(gid) {
                        changes.push(self.send(store, grp));
                    } else {
                        changes.extend(self.send_if_changed(store, grp));
                    }
                } else if self.held.remove(&gid) || !self.caught_up {
                    // It left the filter, by dying or moving out of the
                    // area, possibly between the list and this watch: the
                    // watcher gets its last state.
                    self.leave(store, grp, &mut changes);
                }
            }
            self.revision = store.revision();
        }
        self.caught_up = true;
        if positions {
            // Groups that moved into or out of the area with no store
            // change are sent before any position of theirs.
            let gone: Vec<EntityId> = self
                .held
                .iter()
                .copied()
                .filter(|id| {
                    // Marks and drawings leave through store changes only.
                    store.marker(*id).is_none()
                        && store
                            .group(*id)
                            .is_none_or(|grp| !self.filter.passes(store, grp))
                })
                .collect();
            let mut purged = false;
            for id in gone {
                self.held.remove(&id);
                match store.group(id) {
                    Some(grp) => self.leave(store, grp, &mut changes),
                    None => {
                        // Purged before the watch read its removal.
                        purged = true;
                        self.shapes.remove(&id);
                        changes.push(WorldChange {
                            change: Some(world_change::Change::RemovedId(id.0)),
                        });
                    }
                }
            }
            if purged {
                self.sent.retain(|u, _| store.unit(*u).is_some());
            }
            for gid in self.filter.candidates(store) {
                if let Some(grp) = store.group(gid)
                    && self.filter.passes(store, grp)
                    && self.held.insert(gid)
                {
                    changes.push(self.send(store, grp));
                }
            }
            let mut batches: BTreeMap<String, PositionBatch> = BTreeMap::new();
            for gid in self.filter.candidates(store) {
                let Some(grp) = store.group(gid) else {
                    continue;
                };
                if !self.filter.passes(store, grp) {
                    continue;
                }
                for u in grp.members.iter().filter_map(|m| store.unit(*m)) {
                    if u.dead || !self.filter.unit_in_area(u) {
                        continue;
                    }
                    if self.sent.get(&u.id) == Some(&u.state.at) {
                        continue;
                    }
                    self.sent.insert(u.id, u.state.at);
                    let b = batches
                        .entry(grp.theater.clone())
                        .or_insert_with(|| PositionBatch {
                            theater: grp.theater.clone(),
                            sampled: Some(g.link.stamp()),
                            ..PositionBatch::default()
                        });
                    b.unit_id.push(u.id.0);
                    b.north.push(u.state.at.x);
                    b.east.push(u.state.at.z);
                    b.alt.push(u.state.at.y);
                }
            }
            changes.extend(batches.into_values().map(|b| WorldChange {
                change: Some(world_change::Change::Positions(b)),
            }));
        }
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(WatchWorldResponse {
            revision: self.revision,
            changes,
            campaign_id: self.campaign,
        }))
    }
}

#[tonic::async_trait]
impl WorldService for World {
    type ListWorldStream = Out<ListWorldResponse>;
    type WatchWorldStream = Out<WatchWorldResponse>;

    async fn list_world(
        &self,
        req: Request<ListWorldRequest>,
    ) -> Result<Response<Self::ListWorldStream>, Status> {
        require(&req, Access::Observer)?;
        let f = req.get_ref().filter.as_ref();
        let filter = Filter::new(f, false)?;
        let (campaign, revision, entities) = {
            let mut g = self.0.lock();
            let campaign = g.resolve_read(f.map_or(0, |f| f.campaign_id))?;
            g.open_archive(campaign)?;
            let (revision, entities) = snapshot(&g, campaign, &filter);
            (campaign, revision, entities)
        };
        let mut pages: Vec<Result<ListWorldResponse, Status>> = entities
            .chunks(PAGE)
            .map(|c| {
                Ok(ListWorldResponse {
                    revision,
                    entities: c.to_vec(),
                    campaign_id: campaign,
                })
            })
            .collect();
        if pages.is_empty() {
            pages.push(Ok(ListWorldResponse {
                revision,
                entities: Vec::new(),
                campaign_id: campaign,
            }));
        }
        Ok(Response::new(Box::pin(tokio_stream::iter(pages))))
    }

    async fn watch_world(
        &self,
        req: Request<WatchWorldRequest>,
    ) -> Result<Response<Self::WatchWorldStream>, Status> {
        require(&req, Access::Observer)?;
        let r = req.into_inner();
        let filter = Filter::new(r.filter.as_ref(), false)?;
        let every = period(&r, self.0.cfg.position_period_min)?;
        let named = r.filter.as_ref().map_or(0, |f| f.campaign_id);
        let mut watch = {
            let mut g = self.0.lock();
            let campaign = g.resolve_read(named)?;
            if g.active != Some(campaign) {
                return Err(invalid(
                    "filter.campaign_id",
                    "an archived or suspended campaign is listed, never watched",
                ));
            }
            let store = &g.campaigns[&campaign].store;
            if r.since_revision > store.revision() {
                return Err(invalid(
                    "since_revision",
                    format!(
                        "revision {} is ahead of the store's {}",
                        r.since_revision,
                        store.revision()
                    ),
                ));
            }
            let oldest = store.changes().next().map_or(store.revision() + 1, |c| c.0);
            if r.since_revision + 1 < oldest {
                return Err(status(
                    Code::OutOfRange,
                    ErrorReason::RevisionExpired,
                    format!("changes before revision {oldest} are no longer kept; list again"),
                    0,
                    "since_revision",
                ));
            }
            // The watcher holds what passes now; changes after its revision
            // update that.
            let held = store
                .groups()
                .filter(|grp| filter.passes(store, grp))
                .map(|grp| grp.id)
                .collect();
            Watch {
                campaign,
                follows_active: named == 0,
                filter,
                revision: r.since_revision,
                sent: HashMap::new(),
                held,
                shapes: HashMap::new(),
                caught_up: false,
            }
        };
        let (tx, rx) = mpsc::channel(self.0.cfg.stream_buffer.max(2));
        let hub = Arc::clone(&self.0);
        tokio::spawn(async move {
            let mut tick = hub.tick();
            let mut next_positions = tokio::time::Instant::now();
            loop {
                let due = tokio::time::Instant::now() >= next_positions;
                if due {
                    next_positions = tokio::time::Instant::now()
                        .checked_add(every)
                        .unwrap_or_else(tokio::time::Instant::now);
                }
                if hub.closing() {
                    end(&tx, super::stopping());
                    return;
                }
                let msg = {
                    let g = hub.lock();
                    watch.next(&g, due)
                };
                match msg {
                    Ok(Some(m)) => {
                        if !push(&tx, m) {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        end(&tx, e);
                        return;
                    }
                }
                tokio::select! {
                    changed = tick.changed() => if changed.is_err() { return },
                    () = tokio::time::sleep_until(next_positions) => {}
                    () = tx.closed() => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn query_area(
        &self,
        req: Request<QueryAreaRequest>,
    ) -> Result<Response<QueryAreaResponse>, Status> {
        require(&req, Access::Observer)?;
        let f = req.get_ref().filter.as_ref();
        let filter = Filter::new(f, true)?;
        let mut g = self.0.lock();
        let campaign = g.resolve_read(f.map_or(0, |f| f.campaign_id))?;
        g.open_archive(campaign)?;
        let (revision, entities) = snapshot(&g, campaign, &filter);
        Ok(Response::new(QueryAreaResponse {
            revision,
            entities,
            campaign_id: campaign,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::hub::Backend;
    use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
    use nanoprobe_sidecar::store::{Authority, NewGroup, NewUnit, UnitState};

    /// A DCS that never answers: the store is the test's.
    struct Silent;

    impl SimAdapter for Silent {
        fn position_after(&self, _opcode: u16, _payload: &[u8]) -> u64 {
            0
        }
        fn send(&mut self, _opcode: u16, _payload: &[u8]) -> Result<u64, Detached> {
            Err(Detached)
        }
        fn read(&mut self) -> Vec<RawRecord> {
            Vec::new()
        }
        fn flush(&mut self) {}
    }

    impl Backend for Silent {
        fn sim(&mut self) -> &mut dyn SimAdapter {
            self
        }
        fn frame(&mut self) {}
        fn attach(&mut self, _sidecar_boot: u64) {}
        fn detach(&mut self) {}
        fn load_mission(&mut self, _theater: &str) {}
        fn health(&self) -> pb::Health {
            pb::Health::Gone
        }
        fn heartbeat_age(&self) -> Option<Duration> {
            None
        }
        fn attach_error(&self) -> String {
            String::new()
        }
    }

    fn new_group() -> NewGroup {
        NewGroup {
            theater: "Caucasus".into(),
            country: 2,
            category: 2,
            coalition: 2,
            units: (0..2)
                .map(|_| NewUnit::new("M-113", UnitState::default()))
                .collect(),
            authority: Authority::Campaign,
            label: String::new(),
            extra: nanoprobe_transport::codec::Lv::Nil,
        }
    }

    fn watch_from(revision: u64, caught_up: bool) -> Watch {
        Watch {
            campaign: 1,
            follows_active: false,
            filter: Filter::new(None, false).expect("filter"),
            revision,
            sent: HashMap::new(),
            held: HashSet::new(),
            shapes: HashMap::new(),
            caught_up,
        }
    }

    fn removed_ids(m: &WatchWorldResponse) -> Vec<u64> {
        m.changes
            .iter()
            .filter_map(|c| match c.change {
                Some(world_change::Change::RemovedId(id)) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// A hub holding an in-memory campaign 1, active.
    fn hub() -> Arc<Hub> {
        let hub = Hub::new(Config::default(), Box::new(Silent));
        {
            let mut g = hub.lock();
            let meta = pb::Campaign {
                id: 1,
                name: "c1".into(),
                ..pb::Campaign::default()
            };
            g.add_campaign(&Config::default(), meta, "Caucasus".into())
                .expect("add");
            g.active = Some(1);
        }
        hub
    }

    /// A watch that holds a group, then sees it removed and purged, is told
    /// by `removed_id`, and keeps no position of its members.
    #[test]
    fn a_purged_group_leaves_held_and_sent() {
        let hub = hub();
        let mut g = hub.lock();
        let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
        store.set_now(1_000);
        let id = store.create_group(new_group()).expect("group");
        let members = store.group(id).expect("group").members.clone();
        let mut w = watch_from(0, false);
        let m = w.next(&g, true).expect("next").expect("a message");
        assert!(w.held.contains(&id));
        assert_eq!(w.sent.len(), members.len(), "positions sent");
        assert!(removed_ids(&m).is_empty());
        let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
        store.remove_entity(id).expect("remove");
        assert!(store.purge(id).is_some(), "purged");
        let m = w.next(&g, false).expect("next").expect("a message");
        assert_eq!(removed_ids(&m), vec![id.0]);
        assert!(!w.held.contains(&id));
        assert!(
            members.iter().all(|u| !w.sent.contains_key(u)),
            "sent holds a purged member"
        );
        // Told once.
        assert!(
            w.next(&g, true)
                .expect("next")
                .is_none_or(|m| removed_ids(&m).is_empty())
        );
    }

    /// A watch that resumes from a revision at which the group was listed,
    /// after it was removed and purged with no watch running, gets
    /// `removed_id` for it.
    #[test]
    fn a_resume_from_before_a_purge_gets_removed_id() {
        let hub = hub();
        let mut g = hub.lock();
        let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
        store.set_now(1_000);
        let id = store.create_group(new_group()).expect("group");
        let listed_at = store.revision();
        store.remove_entity(id).expect("remove");
        assert!(store.purge(id).is_some(), "purged");
        // Nothing is held: the group is not in the store when the watch
        // starts, as `watch_world` builds it.
        let mut w = watch_from(listed_at, false);
        let m = w.next(&g, false).expect("next").expect("a message");
        assert_eq!(removed_ids(&m), vec![id.0]);
        assert_eq!(w.revision, g.campaigns[&1].store.revision());
    }

    /// A purged group's shape digest leaves the watch too.
    #[test]
    fn a_purged_group_leaves_shapes() {
        let hub = hub();
        let mut g = hub.lock();
        let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
        store.set_now(1_000);
        let id = store.create_group(new_group()).expect("group");
        let mut w = watch_from(0, false);
        w.next(&g, true).expect("next");
        assert!(w.shapes.contains_key(&id));
        let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
        store.remove_entity(id).expect("remove");
        assert!(store.purge(id).is_some(), "purged");
        w.next(&g, true).expect("next");
        assert!(!w.shapes.contains_key(&id), "shapes holds a purged group");
    }

    /// A watch that resumes past a removed menu item gets no Removal for
    /// it: menu items are never served.
    #[test]
    fn a_resume_past_a_removed_menu_item_gets_no_removal() {
        use nanoprobe_sidecar::store::MarkerSpec;
        let hub = hub();
        let mut g = hub.lock();
        let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
        store.set_now(1_000);
        let before = store.revision();
        let (id, _) = store
            .put_marker(
                EntityId(0),
                1,
                0,
                MarkerSpec::MenuItem {
                    parent: EntityId(0),
                    submenu: false,
                    text: "go".into(),
                },
            )
            .expect("menu item");
        store.remove_marker(id).expect("remove");
        assert!(store.purge_marker(id));
        let mut w = watch_from(before, false);
        let m = w.next(&g, false).expect("next");
        let removals = m.map_or(0, |m| {
            m.changes
                .iter()
                .filter(|c| {
                    matches!(
                        c.change,
                        Some(world_change::Change::Removal(_) | world_change::Change::RemovedId(_))
                    )
                })
                .count()
        });
        assert_eq!(removals, 0, "a removal for a menu item");
    }
}
