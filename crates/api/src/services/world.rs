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
use crate::convert::{Filter, entity, invalid};
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
    let slot = &g.campaigns[&campaign];
    let store = &slot.store;
    let entities = filter
        .candidates(store)
        .into_iter()
        .filter_map(|id| store.group(id))
        .filter(|grp| filter.passes(store, grp))
        .map(|grp| entity(store, grp))
        .collect();
    (store.revision(), entities)
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

    /// The last state of an entity that left the filter. Its shape is
    /// forgotten, so it is sent in full if it comes back.
    fn leave(&mut self, store: &Store, grp: &Group) -> WorldChange {
        self.shapes.remove(&grp.id);
        WorldChange {
            change: Some(world_change::Change::Upsert(entity(store, grp))),
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
                let Some(grp) = store.group(gid) else {
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
                    changes.push(self.leave(store, grp));
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
                    store
                        .group(*id)
                        .is_none_or(|grp| !self.filter.passes(store, grp))
                })
                .collect();
            for id in gone {
                self.held.remove(&id);
                if let Some(grp) = store.group(id) {
                    changes.push(self.leave(store, grp));
                }
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
        let (revision, entities) = snapshot(&g, campaign, &filter);
        Ok(Response::new(QueryAreaResponse {
            revision,
            entities,
            campaign_id: campaign,
        }))
    }
}
