//! The hello campaign: the smallest program that runs a campaign through the
//! nanoprobe API.
//!
//! It takes the controller lease, creates and activates a campaign, puts one
//! ground group in it with a route, and watches the world until the group is
//! live in DCS and its lead has reached the route's end, renewing its lease
//! at a third of the lease's time as it goes. Then it gives the lease up.

use std::time::Duration;

use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityState, EventFilter,
    GroupCategory, GroupSpec, HelloRequest, Idempotency, ListWorldRequest, Order, PutGroupRequest,
    ReleaseLeaseRequest, RenewLeaseRequest, RouteOrder, SubscribeEventsRequest, UnitSpec,
    WatchWorldRequest, Waypoint, WorldFilter, Write, entity, event, order, world_change,
};
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

/// What the run saw.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// The campaign it created.
    pub campaign: u64,
    /// The group it spawned.
    pub group: u64,
    /// The store revision when the group arrived.
    pub revision: u64,
    /// Lifecycle states the group went through, in order.
    pub states: Vec<EntityState>,
    /// Events received.
    pub events: usize,
}

/// Where the hello campaign runs: the theater, a start and an end.
const THEATER: &str = "Caucasus";
const START: (f64, f64) = (-290_000.0, 680_000.0);
const END: (f64, f64) = (-289_000.0, 680_000.0);
/// How close the lead must come to the end.
const ARRIVED_M: f64 = 50.0;

/// Adds the token, if any, to every call.
#[derive(Clone)]
pub struct Auth(Option<MetadataValue<tonic::metadata::Ascii>>);

impl Interceptor for Auth {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(v) = &self.0 {
            req.metadata_mut().insert("authorization", v.clone());
        }
        Ok(req)
    }
}

type Svc = InterceptedService<Channel, Auth>;

fn key(campaign: &str, what: &str) -> Option<Idempotency> {
    Some(Idempotency {
        key: format!("hello/{campaign}/{what}"),
    })
}

fn at(north: f64, east: f64) -> pb::Position {
    pb::Position {
        theater: THEATER.into(),
        north,
        east,
        alt: 0.0,
    }
}

/// Run the hello campaign against `endpoint`, such as
/// `http://127.0.0.1:47311`, with an optional token. Fails after `limit`.
pub async fn run(endpoint: &str, token: Option<&str>, limit: Duration) -> Result<Summary, String> {
    let channel = Endpoint::from_shared(endpoint.to_owned())
        .map_err(|e| format!("endpoint: {e}"))?
        .connect()
        .await
        .map_err(|e| format!("cannot connect to {endpoint}: {e}"))?;
    run_on(channel, token, limit).await
}

/// Run the hello campaign over a connected channel.
pub async fn run_on(
    channel: Channel,
    token: Option<&str>,
    limit: Duration,
) -> Result<Summary, String> {
    tokio::time::timeout(limit, run_inner(channel, token))
        .await
        .map_err(|_| format!("the group did not arrive within {limit:?}"))?
}

async fn run_inner(channel: Channel, token: Option<&str>) -> Result<Summary, String> {
    let auth = Auth(
        token
            .map(|t| format!("Bearer {t}").parse())
            .transpose()
            .map_err(|_| "the token is not valid header text".to_owned())?,
    );
    let mut session = SessionServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut campaigns = CampaignServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut commands = CommandsServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut world: WorldServiceClient<Svc> =
        WorldServiceClient::with_interceptor(channel.clone(), auth.clone());
    let mut events = EventsServiceClient::with_interceptor(channel, auth);
    let fail = |what: &str| {
        let what = what.to_owned();
        move |s: Status| format!("{what}: {} {}", s.code(), s.message())
    };

    let hello = session
        .hello(HelloRequest {
            client_name: "hello-rust".into(),
            schema_version: nanoprobe_proto::SCHEMA_VERSION.into(),
        })
        .await
        .map_err(fail("Hello"))?
        .into_inner();
    println!(
        "server {} schema {} role {:?}",
        hello.server_version,
        hello.schema_version,
        pb::Role::try_from(hello.role).unwrap_or(pb::Role::Unspecified)
    );

    let granted = session
        .acquire_lease(AcquireLeaseRequest {
            client_name: "hello-rust".into(),
        })
        .await
        .map_err(fail("AcquireLease"))?
        .into_inner();
    let lease = granted.lease;
    let ttl = granted
        .ttl
        .and_then(|d| {
            Some(Duration::new(
                u64::try_from(d.seconds).ok()?,
                u32::try_from(d.nanos).ok()?,
            ))
        })
        .ok_or("AcquireLease returned no lease time")?;
    let mut renew = tokio::time::interval((ttl / 3).max(Duration::from_millis(10)));
    renew.tick().await;
    println!("lease epoch {}", lease.as_ref().map_or(0, |l| l.epoch));

    // A name per run, so a second run on the same server makes its own.
    let name = format!(
        "hello-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    );
    let campaign = campaigns
        .create_campaign(CreateCampaignRequest {
            lease: lease.clone(),
            name: name.clone(),
            theaters: vec![THEATER.into()],
            first_theater: THEATER.into(),
            clock: None,
            activate: true,
            idempotency: key(&name, "create"),
        })
        .await
        .map_err(fail("CreateCampaign"))?
        .into_inner()
        .campaign
        .ok_or("CreateCampaign returned no campaign")?;
    println!("campaign {} `{}`", campaign.id, campaign.name);

    // Events from the start of the campaign's log.
    let mut stream = events
        .subscribe_events(SubscribeEventsRequest {
            filter: Some(EventFilter {
                campaign_id: campaign.id,
                ..EventFilter::default()
            }),
            after_sequence: 0,
            live_only: false,
        })
        .await
        .map_err(fail("SubscribeEvents"))?
        .into_inner();

    let unit = |label: &str, east: f64| UnitSpec {
        r#type: "M-113".into(),
        label: label.into(),
        position: Some(at(START.0, START.1 + east)),
        ..UnitSpec::default()
    };
    let put = commands
        .put_group(PutGroupRequest {
            write: Some(Write {
                lease: lease.clone(),
                idempotency: key(&name, "convoy"),
                campaign_id: campaign.id,
            }),
            group_id: 0,
            spec: Some(GroupSpec {
                label: "convoy".into(),
                country: 2,
                category: GroupCategory::Ground as i32,
                units: vec![unit("lead", 0.0), unit("second", 20.0)],
                ..GroupSpec::default()
            }),
            order: Some(Order {
                intent: Some(order::Intent::Route(RouteOrder {
                    waypoints: vec![Waypoint {
                        position: Some(at(END.0, END.1)),
                        speed: 10.0,
                        on_road: false,
                        extra: None,
                    }],
                })),
            }),
        })
        .await
        .map_err(fail("PutGroup"))?
        .into_inner();
    println!(
        "group {} units {:?} at revision {}",
        put.group_id, put.unit_ids, put.revision
    );
    let lead = *put.unit_ids.first().ok_or("PutGroup returned no units")?;

    // List, then watch from the listed revision.
    let filter = WorldFilter {
        campaign_id: campaign.id,
        ..WorldFilter::default()
    };
    let mut listed = world
        .list_world(ListWorldRequest {
            filter: Some(filter.clone()),
        })
        .await
        .map_err(fail("ListWorld"))?
        .into_inner();
    let mut revision = 0;
    while let Some(page) = listed.message().await.map_err(fail("ListWorld"))? {
        revision = page.revision;
    }
    let mut watch = world
        .watch_world(WatchWorldRequest {
            filter: Some(filter),
            since_revision: revision,
            rate: pb::RateClass::Fast as i32,
            min_period: None,
        })
        .await
        .map_err(fail("WatchWorld"))?
        .into_inner();

    let mut summary = Summary {
        campaign: campaign.id,
        group: put.group_id,
        ..Summary::default()
    };
    let mut live = false;
    let mut arrived = false;
    while !(live && arrived) {
        tokio::select! {
            _ = renew.tick() => {
                session
                    .renew_lease(RenewLeaseRequest { lease: lease.clone() })
                    .await
                    .map_err(fail("RenewLease"))?;
            }
            m = watch.message() => {
                let m = m.map_err(fail("WatchWorld"))?.ok_or("the watch ended")?;
                summary.revision = m.revision;
                for c in m.changes {
                    match c.change {
                        Some(world_change::Change::Upsert(pb::Entity { kind: Some(entity::Kind::Group(g)) }))
                            if g.id == put.group_id =>
                        {
                            let s = EntityState::try_from(g.state).unwrap_or(EntityState::Unspecified);
                            if summary.states.last() != Some(&s) {
                                println!("group {} is {:?} at revision {}", g.id, s, m.revision);
                                summary.states.push(s);
                            }
                            live = s == EntityState::Live;
                        }
                        Some(world_change::Change::Positions(b)) => {
                            for (i, u) in b.unit_id.iter().enumerate() {
                                if *u == lead {
                                    let (dn, de) = (b.north[i] - END.0, b.east[i] - END.1);
                                    if (dn * dn + de * de).sqrt() <= ARRIVED_M && !arrived {
                                        println!("lead {lead} reached the end of its route");
                                        arrived = true;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            e = stream.message() => {
                let e = e.map_err(fail("SubscribeEvents"))?.ok_or("the event stream ended")?;
                if let Some(ev) = e.event {
                    summary.events += 1;
                    match ev.body {
                        Some(event::Body::Lifecycle(l)) => println!(
                            "event {}: sim {:?} on `{}`",
                            ev.sequence,
                            pb::SimState::try_from(l.state).unwrap_or(pb::SimState::Unspecified),
                            l.theater
                        ),
                        Some(event::Body::World(w)) => println!("event {}: DCS event {}", ev.sequence, w.dcs_id),
                        Some(event::Body::Gap(g)) => println!("event {}: {} events lost", ev.sequence, g.lost),
                        _ => println!("event {}", ev.sequence),
                    }
                }
            }
        }
    }

    session
        .release_lease(ReleaseLeaseRequest { lease })
        .await
        .map_err(fail("ReleaseLease"))?;
    println!("done: lease released");
    Ok(summary)
}
