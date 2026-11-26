//! A blocking client of the nanoprobe API for the persistence test: each
//! call runs on the command's runtime, and a task on it renews the lease.

use std::collections::BTreeMap;
use std::time::Duration;

use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{self as pb, entity};
use tokio::runtime::Runtime;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

/// Adds the token to every call.
#[derive(Clone)]
struct Auth(Option<MetadataValue<Ascii>>);

impl Interceptor for Auth {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(v) = &self.0 {
            req.metadata_mut().insert("authorization", v.clone());
        }
        Ok(req)
    }
}

type Svc = InterceptedService<Channel, Auth>;

fn fail(what: &'static str) -> impl Fn(Status) -> String {
    move |s| format!("{what}: {:?} {}", s.code(), s.message())
}

/// A connection with the controller lease.
pub struct Api<'r> {
    rt: &'r Runtime,
    session: SessionServiceClient<Svc>,
    campaigns: CampaignServiceClient<Svc>,
    commands: CommandsServiceClient<Svc>,
    world: WorldServiceClient<Svc>,
    events: EventsServiceClient<Svc>,
    mission: MissionServiceClient<Svc>,
    pub lease: Option<pb::Lease>,
    renew: Option<tokio::task::JoinHandle<()>>,
    /// How long one call waits for its answer.
    call_timeout: Duration,
    /// Makes each idempotency key unique in the run.
    keys: u64,
    run: String,
}

impl<'r> Api<'r> {
    /// Connect, say hello and take the lease, retrying for `patience`: the
    /// sidecar may still be starting, or the lease of an earlier connection
    /// of this run still held until it times out.
    pub fn connect(
        rt: &'r Runtime,
        endpoint: &str,
        token: Option<&str>,
        run: &str,
        patience: Duration,
    ) -> Result<Api<'r>, String> {
        Api::connect_with(rt, endpoint, token, run, patience, Duration::from_secs(120))
    }

    /// [`Api::connect`], with how long one call waits for its answer.
    pub fn connect_with(
        rt: &'r Runtime,
        endpoint: &str,
        token: Option<&str>,
        run: &str,
        patience: Duration,
        call_timeout: Duration,
    ) -> Result<Api<'r>, String> {
        let deadline = std::time::Instant::now() + patience;
        loop {
            match Api::try_connect(rt, endpoint, token, run, call_timeout) {
                Ok(a) => return Ok(a),
                Err(e) if std::time::Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_secs(2)),
            }
        }
    }

    fn try_connect(
        rt: &'r Runtime,
        endpoint: &str,
        token: Option<&str>,
        run: &str,
        call_timeout: Duration,
    ) -> Result<Api<'r>, String> {
        let channel = rt.block_on(async {
            Endpoint::from_shared(endpoint.to_owned())
                .map_err(|e| format!("endpoint: {e}"))?
                .connect_timeout(Duration::from_secs(10))
                .timeout(call_timeout)
                .connect()
                .await
                .map_err(|e| format!("cannot connect to {endpoint}: {e}"))
        })?;
        let auth = Auth(
            token
                .map(|t| format!("Bearer {t}").parse())
                .transpose()
                .map_err(|_| "the token is not header text".to_owned())?,
        );
        let mut api = Api {
            rt,
            session: SessionServiceClient::with_interceptor(channel.clone(), auth.clone()),
            campaigns: CampaignServiceClient::with_interceptor(channel.clone(), auth.clone()),
            commands: CommandsServiceClient::with_interceptor(channel.clone(), auth.clone()),
            world: WorldServiceClient::with_interceptor(channel.clone(), auth.clone()),
            events: EventsServiceClient::with_interceptor(channel.clone(), auth.clone()),
            mission: MissionServiceClient::with_interceptor(channel, auth),
            lease: None,
            renew: None,
            call_timeout,
            keys: 0,
            // A connection of its own in the keys: a key repeated across
            // connections would be taken as a repeat of the first call.
            run: format!(
                "{run}/{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ),
        };
        let mut s = api.session.clone();
        let granted = rt.block_on(async {
            s.hello(pb::HelloRequest {
                client_name: "xtask-persistence".into(),
                schema_version: nanoprobe_proto::SCHEMA_VERSION.into(),
            })
            .await
            .map_err(fail("Hello"))?;
            s.acquire_lease(pb::AcquireLeaseRequest {
                client_name: "xtask-persistence".into(),
            })
            .await
            .map_err(fail("AcquireLease"))
        })?;
        let granted = granted.into_inner();
        let ttl = granted
            .ttl
            .map(|d| Duration::new(d.seconds.max(0) as u64, d.nanos.max(0) as u32))
            .ok_or("AcquireLease gave no lease time")?;
        api.lease = granted.lease;
        let lease = api.lease.clone();
        let mut s = api.session.clone();
        api.renew = Some(rt.spawn(async move {
            let mut every = tokio::time::interval((ttl / 3).max(Duration::from_millis(100)));
            every.tick().await;
            loop {
                every.tick().await;
                let _ = s
                    .renew_lease(pb::RenewLeaseRequest {
                        lease: lease.clone(),
                    })
                    .await;
            }
        }));
        Ok(api)
    }

    /// A new connection to `endpoint`, for the same run.
    pub fn reconnect(&self, endpoint: &str, token: Option<&str>) -> Result<Api<'r>, String> {
        let run = self
            .run
            .rsplit_once('/')
            .map_or(self.run.as_str(), |(r, _)| r);
        Api::connect_with(
            self.rt,
            endpoint,
            token,
            run,
            Duration::from_secs(120),
            self.call_timeout,
        )
    }

    /// Give the lease up.
    pub fn release(mut self) {
        if let Some(r) = self.renew.take() {
            r.abort();
        }
        let mut s = self.session.clone();
        let lease = self.lease.take();
        let _ = self
            .rt
            .block_on(async move { s.release_lease(pb::ReleaseLeaseRequest { lease }).await });
    }

    fn key(&mut self, what: &str) -> Option<pb::Idempotency> {
        self.keys += 1;
        Some(pb::Idempotency {
            key: format!("xtask/{}/{what}/{}", self.run, self.keys),
        })
    }

    fn write(&mut self, campaign: u64, what: &str) -> Option<pb::Write> {
        Some(pb::Write {
            lease: self.lease.clone(),
            idempotency: self.key(what),
            campaign_id: campaign,
        })
    }

    pub fn create(&mut self, name: &str, theater: &str) -> Result<pb::Campaign, String> {
        self.create_as(name, theater, true)
    }

    /// Create a campaign and leave it inactive; its ID.
    pub fn create_inactive(&mut self, name: &str, theater: &str) -> Result<u64, String> {
        Ok(self.create_as(name, theater, false)?.id)
    }

    fn create_as(
        &mut self,
        name: &str,
        theater: &str,
        activate: bool,
    ) -> Result<pb::Campaign, String> {
        let req = pb::CreateCampaignRequest {
            lease: self.lease.clone(),
            name: name.into(),
            theaters: vec![theater.into()],
            first_theater: theater.into(),
            clock: None,
            activate,
            idempotency: self.key("create"),
        };
        let mut c = self.campaigns.clone();
        self.rt
            .block_on(c.create_campaign(req))
            .map_err(fail("CreateCampaign"))?
            .into_inner()
            .campaign
            .ok_or_else(|| "CreateCampaign gave no campaign".into())
    }

    pub fn activate(&mut self, campaign: u64) -> Result<pb::Campaign, String> {
        let req = pb::ActivateCampaignRequest {
            lease: self.lease.clone(),
            campaign_id: campaign,
            idempotency: self.key("activate"),
        };
        let mut c = self.campaigns.clone();
        self.rt
            .block_on(c.activate_campaign(req))
            .map_err(fail("ActivateCampaign"))?
            .into_inner()
            .campaign
            .ok_or_else(|| "ActivateCampaign gave no campaign".into())
    }

    pub fn suspend(&mut self, campaign: u64) -> Result<pb::Campaign, String> {
        let req = pb::SuspendCampaignRequest {
            lease: self.lease.clone(),
            campaign_id: campaign,
            idempotency: self.key("suspend"),
        };
        let mut c = self.campaigns.clone();
        self.rt
            .block_on(c.suspend_campaign(req))
            .map_err(fail("SuspendCampaign"))?
            .into_inner()
            .campaign
            .ok_or_else(|| "SuspendCampaign gave no campaign".into())
    }

    pub fn campaign(&mut self, campaign: u64) -> Result<pb::Campaign, String> {
        let mut c = self.campaigns.clone();
        self.rt
            .block_on(c.get_campaign(pb::GetCampaignRequest {
                campaign_id: campaign,
            }))
            .map_err(fail("GetCampaign"))?
            .into_inner()
            .campaign
            .ok_or_else(|| "GetCampaign gave no campaign".into())
    }

    pub fn put_group(
        &mut self,
        campaign: u64,
        spec: pb::GroupSpec,
        order: Option<pb::Order>,
    ) -> Result<pb::PutGroupResponse, String> {
        let req = pb::PutGroupRequest {
            write: self.write(campaign, "put"),
            group_id: 0,
            spec: Some(spec),
            order,
        };
        let mut c = self.commands.clone();
        Ok(self
            .rt
            .block_on(c.put_group(req))
            .map_err(fail("PutGroup"))?
            .into_inner())
    }

    pub fn set_options(
        &mut self,
        campaign: u64,
        group: u64,
        options: BTreeMap<u32, prost_types::Value>,
    ) -> Result<(), String> {
        let req = pb::SetOptionsRequest {
            write: self.write(campaign, "options"),
            group_id: group,
            options: options.into_iter().collect(),
        };
        let mut c = self.commands.clone();
        self.rt
            .block_on(c.set_options(req))
            .map_err(fail("SetOptions"))?;
        Ok(())
    }

    pub fn materialize(&mut self, campaign: u64, ids: &[u64], on: bool) -> Result<(), String> {
        let mut c = self.commands.clone();
        if on {
            let req = pb::MaterializeRequest {
                write: self.write(campaign, "materialize"),
                entity_ids: ids.to_vec(),
            };
            self.rt
                .block_on(c.materialize(req))
                .map_err(fail("Materialize"))?;
        } else {
            let req = pb::DematerializeRequest {
                write: self.write(campaign, "dematerialize"),
                entity_ids: ids.to_vec(),
            };
            self.rt
                .block_on(c.dematerialize(req))
                .map_err(fail("Dematerialize"))?;
        }
        Ok(())
    }

    pub fn capture(&mut self, campaign: u64, id: u64) -> Result<pb::CaptureResponse, String> {
        let req = pb::CaptureRequest {
            write: self.write(campaign, "capture"),
            entity_id: id,
        };
        let mut c = self.commands.clone();
        Ok(self
            .rt
            .block_on(c.capture(req))
            .map_err(fail("Capture"))?
            .into_inner())
    }

    pub fn sample_rates(
        &mut self,
        campaign: u64,
        position: f64,
        vitals: f64,
    ) -> Result<(), String> {
        let req = pb::SetSampleRatesRequest {
            write: self.write(campaign, "rates"),
            position_period: position,
            vitals_period: vitals,
            ..pb::SetSampleRatesRequest::default()
        };
        let mut c = self.commands.clone();
        self.rt
            .block_on(c.set_sample_rates(req))
            .map_err(fail("SetSampleRates"))?;
        Ok(())
    }

    /// Every group of `campaign` the sidecar holds, by ID.
    pub fn groups(&mut self, campaign: u64) -> Result<BTreeMap<u64, pb::Group>, String> {
        let mut w = self.world.clone();
        let filter = pb::WorldFilter {
            campaign_id: campaign,
            ..pb::WorldFilter::default()
        };
        self.rt.block_on(async move {
            let mut s = w
                .list_world(pb::ListWorldRequest {
                    filter: Some(filter),
                })
                .await
                .map_err(fail("ListWorld"))?
                .into_inner();
            let mut out = BTreeMap::new();
            while let Some(page) = s.message().await.map_err(fail("ListWorld"))? {
                for e in page.entities {
                    if let Some(entity::Kind::Group(g)) = e.kind {
                        out.insert(g.id, g);
                    }
                }
            }
            Ok(out)
        })
    }

    /// Put a static of `type_name` at a place, alive or destroyed.
    pub fn put_static(
        &mut self,
        campaign: u64,
        label: &str,
        type_name: &str,
        position: pb::Position,
        dead: bool,
    ) -> Result<u64, String> {
        let req = pb::PutStaticRequest {
            write: self.write(campaign, "put-static"),
            static_id: 0,
            spec: Some(pb::StaticSpec {
                further_units: Vec::new(),
                label: label.into(),
                country: super::scenarios::COUNTRY,
                r#type: type_name.into(),
                position: Some(position),
                heading: 0.0,
                extra: None,
                dead,
            }),
        };
        let mut c = self.commands.clone();
        Ok(self
            .rt
            .block_on(c.put_static(req))
            .map_err(fail("PutStatic"))?
            .into_inner()
            .static_id)
    }

    /// Remove a managed group or static from the campaign.
    pub fn remove_entity(&mut self, campaign: u64, id: u64) -> Result<(), String> {
        let req = pb::RemoveEntityRequest {
            write: self.write(campaign, "remove"),
            entity_id: id,
        };
        let mut c = self.commands.clone();
        self.rt
            .block_on(c.remove_entity(req))
            .map_err(fail("RemoveEntity"))?;
        Ok(())
    }

    /// Every static of `campaign` the sidecar lists, by ID.
    pub fn statics(&mut self, campaign: u64) -> Result<BTreeMap<u64, pb::Static>, String> {
        let mut w = self.world.clone();
        let filter = pb::WorldFilter {
            campaign_id: campaign,
            ..pb::WorldFilter::default()
        };
        self.rt.block_on(async move {
            let mut s = w
                .list_world(pb::ListWorldRequest {
                    filter: Some(filter),
                })
                .await
                .map_err(fail("ListWorld"))?
                .into_inner();
            let mut out = BTreeMap::new();
            while let Some(page) = s.message().await.map_err(fail("ListWorld"))? {
                for e in page.entities {
                    if let Some(entity::Kind::Static(x)) = e.kind {
                        out.insert(x.id, x);
                    }
                }
            }
            Ok(out)
        })
    }

    /// The groups of `campaign` with a unit within `radius` meters of a
    /// point: the interest query.
    pub fn query_area(
        &mut self,
        campaign: u64,
        center: pb::Position,
        radius: f64,
    ) -> Result<Vec<pb::Group>, String> {
        let mut w = self.world.clone();
        let req = pb::QueryAreaRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: campaign,
                center: Some(center),
                radius,
                kinds: vec![pb::EntityKind::Group as i32],
                ..pb::WorldFilter::default()
            }),
        };
        Ok(self
            .rt
            .block_on(w.query_area(req))
            .map_err(fail("QueryArea"))?
            .into_inner()
            .entities
            .into_iter()
            .filter_map(|e| match e.kind {
                Some(entity::Kind::Group(g)) => Some(g),
                _ => None,
            })
            .collect())
    }

    pub fn server_status(&mut self) -> Result<pb::ServerStatus, String> {
        let mut m = self.mission.clone();
        self.rt
            .block_on(m.get_server_status(pb::GetServerStatusRequest {}))
            .map_err(fail("GetServerStatus"))?
            .into_inner()
            .status
            .ok_or_else(|| "GetServerStatus gave no status".into())
    }

    /// Set a unit's life in DCS, as a percentage of its initial life
    /// (`DoEffect` with `Damage`). The effect's outcome.
    pub fn damage(&mut self, campaign: u64, unit: u64, life_percent: f64) -> Result<i32, String> {
        let req = pb::DoEffectRequest {
            write: self.write(campaign, "damage"),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::Damage(pb::Damage {
                    unit_id: unit,
                    life_percent,
                })),
            }),
        };
        let mut c = self.commands.clone();
        Ok(self
            .rt
            .block_on(c.do_effect(req))
            .map_err(fail("DoEffect"))?
            .into_inner()
            .result
            .map_or(0, |r| r.outcome))
    }

    /// Clear debris in a box from `min` to `max`. Returns the outcome and
    /// how many objects DCS and the handler removed.
    pub fn clear_debris_box(
        &mut self,
        campaign: u64,
        min: pb::Position,
        max: pb::Position,
    ) -> Result<(i32, Option<u64>), String> {
        let req = pb::DoEffectRequest {
            write: self.write(campaign, "clear-debris"),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::ClearDebris(pb::ClearDebris {
                    volume: Some(pb::clear_debris::Volume::Box(pb::Box {
                        min: Some(min),
                        max: Some(max),
                    })),
                })),
            }),
        };
        let mut c = self.commands.clone();
        let r = self
            .rt
            .block_on(c.do_effect(req))
            .map_err(fail("DoEffect"))?
            .into_inner();
        Ok((r.result.map_or(0, |x| x.outcome), r.debris_removed))
    }

    /// Give a group an order (`SetOrder`).
    pub fn set_order(&mut self, campaign: u64, group: u64, order: pb::Order) -> Result<(), String> {
        let req = pb::SetOrderRequest {
            write: self.write(campaign, "order"),
            group_id: group,
            order: Some(order),
        };
        let mut c = self.commands.clone();
        self.rt
            .block_on(c.set_order(req))
            .map_err(fail("SetOrder"))?;
        Ok(())
    }

    /// What `ExportCampaign` streams, counted by line kind: lines and bytes
    /// of each `kind`, and for the journal the lines of each entry kind.
    /// The bytes are the export's JSON, not the store's rows.
    pub fn export_counts(&mut self, campaign: u64) -> Result<serde_json::Value, String> {
        let mut c = self.campaigns.clone();
        self.rt.block_on(async move {
            let mut s = c
                .export_campaign(pb::ExportCampaignRequest {
                    campaign_id: campaign,
                })
                .await
                .map_err(fail("ExportCampaign"))?
                .into_inner();
            let mut kinds: BTreeMap<String, (u64, u64)> = BTreeMap::new();
            let mut entries: BTreeMap<String, u64> = BTreeMap::new();
            while let Some(page) = s.message().await.map_err(fail("ExportCampaign"))? {
                for line in page.lines.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                    let v: serde_json::Value = serde_json::from_slice(line)
                        .map_err(|e| format!("ExportCampaign: a line is not JSON: {e}"))?;
                    let kind = v["kind"].as_str().unwrap_or("?").to_owned();
                    if kind == "journal" {
                        *entries
                            .entry(v["entry"].as_str().unwrap_or("?").to_owned())
                            .or_default() += 1;
                    }
                    let k = kinds.entry(kind).or_default();
                    k.0 += 1;
                    k.1 += line.len() as u64;
                }
            }
            let of = |k: &str| kinds.get(k).copied().unwrap_or_default();
            Ok(serde_json::json!({
                "journal_rows": of("journal").0,
                "journal_json_bytes": of("journal").1,
                "event_rows": of("event").0,
                "event_json_bytes": of("event").1,
                "entity_rows": of("entity").0,
                "inflight_rows": of("inflight").0,
                "journal_entries": entries,
            }))
        })
    }

    /// Keep a world watch and an event subscription open on `campaign`
    /// until the returned [`Streams`] is dropped, counting what they send.
    /// Each stream opens again 5 s after it ends: a sidecar restart ends
    /// both. The watch resumes from the last revision it saw, and after a
    /// refusal from the revision an area query gives, as a client that
    /// resynchronizes does.
    pub fn attach_streams(&self, campaign: u64) -> Streams {
        let counts = std::sync::Arc::new(StreamCounts::default());
        let filter = pb::WorldFilter {
            campaign_id: campaign,
            ..pb::WorldFilter::default()
        };
        let (mut w, n) = (self.world.clone(), std::sync::Arc::clone(&counts));
        let watch = self.rt.spawn(async move {
            use std::sync::atomic::Ordering::Relaxed;
            let mut since = 0u64;
            loop {
                let opened = w
                    .watch_world(pb::WatchWorldRequest {
                        filter: Some(filter.clone()),
                        since_revision: since,
                        ..pb::WatchWorldRequest::default()
                    })
                    .await;
                n.watch_opens.fetch_add(1, Relaxed);
                if let Ok(s) = opened {
                    let mut s = s.into_inner();
                    while let Ok(Some(m)) = s.message().await {
                        since = m.revision;
                        n.watch_messages.fetch_add(1, Relaxed);
                        n.watch_changes.fetch_add(m.changes.len() as u64, Relaxed);
                    }
                } else if let Ok(r) = w
                    .query_area(pb::QueryAreaRequest {
                        filter: Some(pb::WorldFilter {
                            radius: 1.0,
                            center: Some(pb::Position::default()),
                            ..filter.clone()
                        }),
                    })
                    .await
                {
                    since = r.into_inner().revision;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        let (mut e, n) = (self.events.clone(), std::sync::Arc::clone(&counts));
        let events = self.rt.spawn(async move {
            use std::sync::atomic::Ordering::Relaxed;
            let mut after = 0u64;
            loop {
                let opened = e
                    .subscribe_events(pb::SubscribeEventsRequest {
                        filter: Some(pb::EventFilter {
                            campaign_id: campaign,
                            ..pb::EventFilter::default()
                        }),
                        after_sequence: after,
                        live_only: false,
                    })
                    .await;
                n.event_opens.fetch_add(1, Relaxed);
                if let Ok(s) = opened {
                    let mut s = s.into_inner();
                    while let Ok(Some(m)) = s.message().await {
                        let Some(ev) = m.event else { continue };
                        after = after.max(ev.sequence);
                        n.event_sequence.store(after, Relaxed);
                        n.events.fetch_add(1, Relaxed);
                        if let Some(pb::event::Body::World(w)) = &ev.body {
                            // DCS's event IDs: 8 is S_EVENT_DEAD and 15 is
                            // S_EVENT_BIRTH (LIVE: the event catalog,
                            // docs/results/2026-11-13-event-catalogue.md).
                            match w.dcs_id {
                                8 => n.deaths.fetch_add(1, Relaxed),
                                15 => n.births.fetch_add(1, Relaxed),
                                _ => 0,
                            };
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        Streams {
            counts,
            tasks: vec![watch, events],
        }
    }

    /// The campaign's events after `after`, at most `limit`.
    pub fn events(
        &mut self,
        campaign: u64,
        after: u64,
        limit: u32,
    ) -> Result<Vec<pb::Event>, String> {
        let mut e = self.events.clone();
        Ok(self
            .rt
            .block_on(e.read_events(pb::ReadEventsRequest {
                filter: Some(pb::EventFilter {
                    campaign_id: campaign,
                    ..pb::EventFilter::default()
                }),
                after_sequence: after,
                limit,
            }))
            .map_err(fail("ReadEvents"))?
            .into_inner()
            .events)
    }
}

/// What the streams of [`Api::attach_streams`] have sent.
#[derive(Default)]
pub struct StreamCounts {
    pub watch_opens: std::sync::atomic::AtomicU64,
    pub watch_messages: std::sync::atomic::AtomicU64,
    pub watch_changes: std::sync::atomic::AtomicU64,
    pub event_opens: std::sync::atomic::AtomicU64,
    pub events: std::sync::atomic::AtomicU64,
    /// The newest event sequence number the subscription has seen.
    pub event_sequence: std::sync::atomic::AtomicU64,
    pub deaths: std::sync::atomic::AtomicU64,
    pub births: std::sync::atomic::AtomicU64,
}

/// A watch and an event subscription that stay attached; dropping it ends
/// both.
pub struct Streams {
    pub counts: std::sync::Arc<StreamCounts>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Streams {
    /// The counts as JSON, cumulative since the streams were attached.
    pub fn snapshot(&self) -> serde_json::Value {
        use std::sync::atomic::Ordering::Relaxed;
        let c = &self.counts;
        serde_json::json!({
            "watch_opens": c.watch_opens.load(Relaxed),
            "watch_messages": c.watch_messages.load(Relaxed),
            "watch_changes": c.watch_changes.load(Relaxed),
            "event_opens": c.event_opens.load(Relaxed),
            "events": c.events.load(Relaxed),
            "event_sequence": c.event_sequence.load(Relaxed),
            "deaths": c.deaths.load(Relaxed),
            "births": c.births.load(Relaxed),
        })
    }
}

impl Drop for Streams {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Drop for Api<'_> {
    fn drop(&mut self) {
        if let Some(r) = self.renew.take() {
            r.abort();
        }
    }
}

/// A position on `theater`.
pub fn at(theater: &str, north: f64, east: f64, alt: f64) -> pb::Position {
    pb::Position {
        theater: theater.into(),
        north,
        east,
        alt,
    }
}

/// A route order through `points` at `speed`.
pub fn route(theater: &str, points: &[(f64, f64, f64)], speed: f64) -> pb::Order {
    pb::Order {
        intent: Some(pb::order::Intent::Route(pb::RouteOrder {
            waypoints: points
                .iter()
                .map(|(n, e, a)| pb::Waypoint {
                    position: Some(at(theater, *n, *e, *a)),
                    speed,
                    on_road: false,
                    extra: None,
                })
                .collect(),
        })),
    }
}

/// A JSON value as a protobuf value.
pub fn pv(v: &serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    prost_types::Value {
        kind: Some(match v {
            serde_json::Value::Null => Kind::NullValue(0),
            serde_json::Value::Bool(b) => Kind::BoolValue(*b),
            serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) => Kind::StringValue(s.clone()),
            serde_json::Value::Array(a) => Kind::ListValue(prost_types::ListValue {
                values: a.iter().map(pv).collect(),
            }),
            serde_json::Value::Object(_) => Kind::StructValue(ps(v)),
        }),
    }
}

/// A JSON object as a protobuf struct; anything else is empty.
pub fn ps(v: &serde_json::Value) -> prost_types::Struct {
    prost_types::Struct {
        fields: v
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), pv(v))).collect())
            .unwrap_or_default(),
    }
}
