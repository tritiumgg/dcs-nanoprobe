//! `sidecar <name> --part shell [--template FILE]`: the S19 live test. It
//! runs on the sidecar `--part hello` starts, with the two committed
//! templates as `[missions] dir`'s `Caucasus.miz` and `Afghanistan.miz`, and
//! drives the API from here. In order:
//!
//! 1. `reload`: a campaign on both theaters with a group on Caucasus; a
//!    `SetNextMission` with a start and a weather of its own and a `Reload`:
//!    the new mission's `env.mission` shows both;
//! 2. `theaters`: reloads to Afghanistan (a group put there), Caucasus and
//!    Afghanistan again: each time the other map's group is virtual and the
//!    loaded map's is live;
//! 3. `own_template`: with `--template FILE`, a second campaign with that
//!    mission as its own Caucasus template and dynamic spawn on one airport:
//!    the shell DCS loads keeps every member of the template but the runtime
//!    fields, sets the airport's `dynamicSpawn`, and its zones and trigger
//!    rules are in `env.mission`; the slot list the hook reads offers a
//!    template client unit and a slot at the dynamic-spawn airport (every
//!    slot field is recorded); without it the item is skipped;
//! 4. `finished`: a campaign made for it, with a group in its store,
//!    finished: it is listed with that group, and a change is refused with
//!    `CAMPAIGN_READ_ONLY`;
//! 5. `hung`: the agent's deliberate access violation crashes DCS; the
//!    status says hung within `heartbeat_stale_s` and a lifecycle event
//!    `SIM_STATE_HUNG` is published; the processes left in the container are
//!    recorded (E22); DCS is restarted;
//! 6. `killed`: the first campaign active, DCS killed in its container; the
//!    image starts it again, and the campaign's group is live again with no
//!    call made.
//!
//! Steps 5 and 6 end DCS for everyone: each checks that no player is
//! connected first.

use std::time::{Duration, Instant};

use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{self as pb, entity};
use nanoprobe_sidecar::shell;
use nanoprobe_transport::codec::Lv;
use serde_json::{Value, json};
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

use crate::install::Profile as _;
use crate::sampling::live::Run;

pub mod template;

/// The two committed templates.
const CAUCASUS: &[u8] = include_bytes!("../../../tests/fixtures/templates/caucasus.miz");
const AFGHANISTAN: &[u8] = include_bytes!("../../../tests/fixtures/templates/afghanistan.miz");

/// How long a mission load may take before the step fails. Not a design
/// limit (a load has no fixed duration); only when this test gives up.
const LOAD_LIMIT: Duration = Duration::from_secs(900);
/// `[health] heartbeat_stale_s` as the part's `sidecar.toml` sets it.
pub const HEARTBEAT_STALE_S: u64 = 10;
/// How long a hung verdict may take after the crash: the configured limit
/// plus this test's tolerance for the status poll and the sidecar's read.
const HUNG_LIMIT: Duration = Duration::from_secs(HEARTBEAT_STALE_S + 15);
/// The start the reload writes: 2031-07-04 15:30:00.
const START_S: i64 = 1_940_945_400;

/// The templates `[missions] dir` holds for the part.
pub fn templates() -> [(&'static str, &'static [u8]); 2] {
    [("Caucasus.miz", CAUCASUS), ("Afghanistan.miz", AFGHANISTAN)]
}

/// The folder `[missions] template_dirs` lists for the part, as the sidecar
/// in its container opens it: `own/` in the mission folder, which the run
/// makes and removes.
pub const TEMPLATE_DIR: &str = "/missions/own";

/// Put the `--template` mission where the sidecar may read it, and give its
/// path as the sidecar opens it (`SetCampaignTemplate` takes a path).
fn own_template_file(run: &Run, t: &[u8]) -> Result<String, String> {
    run.remote
        .write(&format!("{}/own/own.miz", crate::sidecar::MISSIONS_DIR), t)?;
    Ok(format!("{TEMPLATE_DIR}/own.miz"))
}

#[derive(Clone)]
struct Auth(MetadataValue<tonic::metadata::Ascii>);

impl Interceptor for Auth {
    fn call(&mut self, mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        req.metadata_mut().insert("authorization", self.0.clone());
        Ok(req)
    }
}

type Svc = InterceptedService<Channel, Auth>;

/// The API clients, with a runtime to call them from blocking code.
struct Api {
    rt: tokio::runtime::Runtime,
    session: SessionServiceClient<Svc>,
    campaigns: CampaignServiceClient<Svc>,
    commands: CommandsServiceClient<Svc>,
    world: WorldServiceClient<Svc>,
    mission: MissionServiceClient<Svc>,
    events: EventsServiceClient<Svc>,
    lease: Option<pb::Lease>,
    keys: u64,
}

fn status_text(what: &str) -> impl Fn(tonic::Status) -> String + '_ {
    move |s| format!("{what}: {:?} {}", s.code(), s.message())
}

impl Api {
    fn connect(endpoint: &str, token: &str) -> Result<Api, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let channel = rt
            .block_on(
                Endpoint::from_shared(endpoint.to_owned())
                    .map_err(|e| e.to_string())?
                    .connect(),
            )
            .map_err(|e| format!("connect {endpoint}: {e}"))?;
        let auth = Auth(
            format!("Bearer {token}")
                .parse()
                .map_err(|_| "the token is not header text")?,
        );
        let c = |ch: &Channel| InterceptedService::new(ch.clone(), auth.clone());
        let mut api = Api {
            session: SessionServiceClient::new(c(&channel)),
            campaigns: CampaignServiceClient::new(c(&channel)),
            commands: CommandsServiceClient::new(c(&channel)),
            world: WorldServiceClient::new(c(&channel)),
            mission: MissionServiceClient::new(c(&channel)),
            events: EventsServiceClient::new(c(&channel)),
            rt,
            lease: None,
            keys: 0,
        };
        let granted = api
            .rt
            .block_on(api.session.acquire_lease(pb::AcquireLeaseRequest {
                client_name: "xtask-shell".into(),
            }))
            .map_err(status_text("AcquireLease"))?
            .into_inner();
        api.lease = granted.lease;
        Ok(api)
    }

    fn renew(&mut self) -> Result<(), String> {
        let r = self
            .rt
            .block_on(self.session.renew_lease(pb::RenewLeaseRequest {
                lease: self.lease.clone(),
            }))
            .map_err(status_text("RenewLease"))?;
        drop(r);
        Ok(())
    }

    /// Renew the lease, or take a new one when the sidecar lost it: it exits
    /// when DCS restarts and the image starts it again with no leases.
    fn keep(&mut self) {
        if self.renew().is_ok() {
            return;
        }
        let granted = self
            .rt
            .block_on(self.session.acquire_lease(pb::AcquireLeaseRequest {
                client_name: "xtask-shell".into(),
            }));
        if let Ok(g) = granted {
            self.lease = g.into_inner().lease;
        }
    }

    fn key(&mut self, what: &str) -> Option<pb::Idempotency> {
        self.keys += 1;
        Some(pb::Idempotency {
            key: format!("xtask-shell/{}/{what}/{}", std::process::id(), self.keys),
        })
    }

    fn write(&mut self, what: &str, campaign: u64) -> Option<pb::Write> {
        Some(pb::Write {
            lease: self.lease.clone(),
            idempotency: self.key(what),
            campaign_id: campaign,
        })
    }

    fn create(&mut self, name: &str, theaters: &[&str], activate: bool) -> Result<u64, String> {
        let req = pb::CreateCampaignRequest {
            lease: self.lease.clone(),
            name: name.into(),
            theaters: theaters.iter().map(|t| (*t).to_owned()).collect(),
            first_theater: theaters[0].into(),
            clock: None,
            activate,
            idempotency: self.key("create"),
        };
        Ok(self
            .rt
            .block_on(self.campaigns.create_campaign(req))
            .map_err(status_text("CreateCampaign"))?
            .into_inner()
            .campaign
            .ok_or("no campaign")?
            .id)
    }

    fn group_request(
        &mut self,
        campaign: u64,
        label: &str,
        at: pb::Position,
    ) -> pb::PutGroupRequest {
        let unit = pb::UnitSpec {
            r#type: "M-113".into(),
            label: label.into(),
            position: Some(at),
            ..pb::UnitSpec::default()
        };
        pb::PutGroupRequest {
            write: self.write("group", campaign),
            group_id: 0,
            spec: Some(pb::GroupSpec {
                label: label.into(),
                country: 2,
                category: pb::GroupCategory::Ground as i32,
                units: vec![unit],
                ..pb::GroupSpec::default()
            }),
            order: None,
        }
    }

    fn group(&mut self, campaign: u64, label: &str, at: pb::Position) -> Result<u64, String> {
        let req = self.group_request(campaign, label, at);
        Ok(self
            .rt
            .block_on(self.commands.put_group(req))
            .map_err(status_text("PutGroup"))?
            .into_inner()
            .group_id)
    }

    fn state(&mut self, campaign: u64, group: u64) -> Result<i32, String> {
        let mut s = self
            .rt
            .block_on(self.world.list_world(pb::ListWorldRequest {
                filter: Some(pb::WorldFilter {
                    campaign_id: campaign,
                    ..pb::WorldFilter::default()
                }),
            }))
            .map_err(status_text("ListWorld"))?
            .into_inner();
        let mut found = None;
        while let Some(page) = self
            .rt
            .block_on(s.message())
            .map_err(status_text("ListWorld"))?
        {
            for e in page.entities {
                if let Some(entity::Kind::Group(g)) = e.kind
                    && g.id == group
                {
                    found = Some(g.state);
                }
            }
        }
        found.ok_or_else(|| format!("group {group} is not in campaign {campaign}'s world"))
    }

    fn status(&mut self) -> Result<pb::ServerStatus, String> {
        self.rt
            .block_on(
                self.mission
                    .get_server_status(pb::GetServerStatusRequest {}),
            )
            .map_err(status_text("GetServerStatus"))?
            .into_inner()
            .status
            .ok_or("no status".into())
    }

    fn next(&mut self, campaign: u64, next: pb::NextMission) -> Result<(), String> {
        let req = pb::SetNextMissionRequest {
            write: self.write("next", campaign),
            next: Some(next),
        };
        self.rt
            .block_on(self.mission.set_next_mission(req))
            .map_err(status_text("SetNextMission"))?;
        Ok(())
    }

    fn reload(&mut self) -> Result<(), String> {
        let req = pb::ReloadRequest {
            write: self.write("reload", 0),
        };
        self.rt
            .block_on(self.mission.reload(req))
            .map_err(status_text("Reload"))?;
        Ok(())
    }

    fn lifecycle_states(&mut self, server_log: bool, campaign: u64) -> Result<Vec<i32>, String> {
        let r = self
            .rt
            .block_on(self.events.read_events(pb::ReadEventsRequest {
                filter: Some(pb::EventFilter {
                    campaign_id: campaign,
                    server_log,
                    ..pb::EventFilter::default()
                }),
                after_sequence: 0,
                limit: 10_000,
            }))
            .map_err(status_text("ReadEvents"))?
            .into_inner();
        Ok(r.events
            .into_iter()
            .filter_map(|e| match e.body {
                Some(pb::event::Body::Lifecycle(l)) => Some(l.state),
                _ => None,
            })
            .collect())
    }
}

fn caucasus(north: f64, east: f64) -> pb::Position {
    pb::Position {
        theater: "Caucasus".into(),
        north,
        east,
        alt: 0.0,
    }
}

/// Poll `check` while the simulation is held running, until it gives
/// `Some` or `limit` passes.
fn wait<T>(
    run: &Run,
    api: &mut Api,
    limit: Duration,
    what: &str,
    mut check: impl FnMut(&mut Api) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(v) = check(api)? {
            return Ok(v);
        }
        if Instant::now() > deadline {
            return Err(format!("{what}: not within {limit:?}"));
        }
        api.keep();
        // Unpauses and lets the mission run a moment; a load is not held.
        // With DCS down the hold fails at once: wait instead.
        if run.hold(3.0).is_err() {
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

/// The longest a person test holds the shell.
const PERSON_HOLD: Duration = Duration::from_secs(45 * 60);

/// With `NP_SHELL_PERSON=<stop file>`: activate a campaign with `template` as
/// its own template and dynamic spawn on its first airport, as step 3 does,
/// set that airport blue and stock it, and spawn and stock a FARP beside it
/// (`owned_and_stocked`), then keep the lease while a person, or a scripted
/// client, looks at the slot screen. A dead static's ruin is cleared
/// (`ruin_check`) when the ruin file (the stop file with extension `ruin`)
/// exists, else at the hold's end. Ends when the stop file exists on this
/// machine, or after `PERSON_HOLD`.
/// The dynamic-spawn reads and the hook's slot list are recorded at the start
/// and at the end.
fn person(
    run: &Run,
    api: &mut Api,
    t: &[u8],
    generation: u32,
    stop: &str,
    results: &mut Value,
) -> Result<(), String> {
    println!("== person: own template with dynamic spawn, held");
    let theater = shell::theatre(t)?;
    let c = api.create("s19-person", &[theater.as_str()], false)?;
    let airport = shell::airports(t)?.into_iter().next().ok_or("no airport")?;
    let req = pb::SetCampaignTemplateRequest {
        write: api.write("template", c),
        theater: theater.clone(),
        path: own_template_file(run, t)?,
    };
    api.rt
        .block_on(api.campaigns.set_campaign_template(req))
        .map_err(status_text("SetCampaignTemplate"))?;
    api.next(
        c,
        pb::NextMission {
            dynamic_spawn: vec![pb::DynamicSpawn {
                airbase_id: airport,
                enabled: true,
                hot_start: false,
            }],
            ..pb::NextMission::default()
        },
    )?;
    let req = pb::ActivateCampaignRequest {
        lease: api.lease.clone(),
        campaign_id: c,
        idempotency: api.key("activate"),
    };
    api.rt
        .block_on(api.campaigns.activate_campaign(req))
        .map_err(status_text("ActivateCampaign"))?;
    loaded(run, api, &theater, generation)?;
    let farp = owned_and_stocked(run, api, c, &theater, airport)?;
    let read = format!(
        "local out = {{ airport = {airport}, slots = {{}} }}\nlocal ok, s = pcall(DCS.getDynamicSpawnSettings, {airport}, true)\nout.settings = ok and type(s) == 'table' and (tostring(s.dynamicSpawnAvailable) .. ',' .. tostring(s.allowHotSpawn)) or tostring(s)\nfor _, side in ipairs({{ 'red', 'blue' }}) do for _, s in ipairs(DCS.getAvailableSlots(side) or {{}}) do local e = {{ side = side }}\nfor k, v in pairs(s) do e[tostring(k)] = tostring(v) end\nout.slots[#out.slots + 1] = e end end\nout.players = #(net.get_player_list() or {{}})\nout.coalitions = {{}}\nfor _, v in pairs(DCS.getAirdromesCoalition() or {{}}) do if v.airdromeId == {airport} then out.coalitions[#out.coalitions + 1] = tostring(v.coalition) end end\nout.farps = {{}}\nfor id, v in pairs((DCS.getFarpsAndCarriersMissionData() or {{}}).farps or {{}}) do local f = {{ id = tostring(id), coalition = tostring(v.coalition) }}\nlocal okf, fs = pcall(DCS.getDynamicSpawnSettings, id, false)\nf.settings = okf and type(fs) == 'table' and tostring(fs.dynamicSpawnAvailable) or tostring(fs)\nout.farps[#out.farps + 1] = f end\nreturn out\n"
    );
    let stock_read = format!(
        "local out = {{}}\nlocal function inv(base) if not base then return 'none' end local i = base:getWarehouse():getInventory() local a = {{}} for k, v in pairs(i.aircraft or {{}}) do a[#a + 1] = k .. '=' .. v end return {{ coalition = base:getCoalition(), aircraft = table.concat(a, ' '), jet = (i.liquids or {{}})[0] }} end\nfor _, b in ipairs(world.getAirbases()) do if b:getID() == {airport} then out.airport = inv(b) end end\nout.farp = inv(Airbase.getByName({farp:?}))\nreturn out\n"
    );
    let stocks = run.eval("server", "s19-person-stock", &stock_read, 60)?;
    println!("   stock: {stocks}");
    let dead = dead_static(run, &farp).unwrap_or_else(|e| {
        println!("   dead statics: {e}");
        Vec::new()
    });
    let start = run
        .eval("hook", "s19-person-start", &read, 60)
        .unwrap_or_else(|e| json!({ "error": e }));
    println!("   held: {start}");
    // A stop file left from an earlier hold would end this one at once.
    let stop = std::path::Path::new(stop);
    if stop.exists() {
        std::fs::remove_file(stop).map_err(|e| format!("{}: {e}", stop.display()))?;
    }
    // The ruin check runs when the person is at the FARP and touches the
    // ruin file, so the client sees the ruin go; else at the hold's end.
    let go = stop.with_extension("ruin");
    let _ = std::fs::remove_file(&go);
    println!(
        "   touch {} to run the ruin check, {} to end the hold",
        go.display(),
        stop.display()
    );
    let held = Instant::now();
    let mut ruin = Value::Null;
    while held.elapsed() < PERSON_HOLD && !stop.exists() {
        std::thread::sleep(Duration::from_secs(15));
        api.keep();
        if ruin.is_null() && go.exists() {
            let _ = std::fs::remove_file(&go);
            ruin =
                ruin_check(run, api, c, &theater, &farp).unwrap_or_else(|e| json!({ "error": e }));
        }
    }
    if ruin.is_null() {
        ruin = ruin_check(run, api, c, &theater, &farp).unwrap_or_else(|e| json!({ "error": e }));
    }
    // An error in a step after the hold is recorded, so the hold's record
    // is written.
    let dead = dead_static_clear(run, api, c, &theater, dead);
    // The box clear's cost runs last: it removes every dead static nobody
    // manages, the ones above included.
    let box_clear =
        box_clear(run, api, c, &theater, &farp).unwrap_or_else(|e| json!({ "error": e }));
    let ended = if stop.exists() {
        "stop_file"
    } else {
        "timeout"
    };
    let _ = std::fs::remove_file(stop);
    let end = run
        .eval("hook", "s19-person-end", &read, 60)
        .unwrap_or_else(|e| json!({ "error": e }));
    // No verdict: what the person saw is recorded by hand.
    results["person"] = json!({
        "verdict": "recorded by hand",
        "ended": ended,
        "campaign": c,
        "airport": airport,
        "held_s": held.elapsed().as_secs(),
        "start": start,
        "end": end,
        "farp": farp,
        "stock": stocks,
        "ruin": ruin,
        "dead_statics": dead,
        "box_clear": box_clear,
    });
    println!("== HELD ({ended}); the verdict is the person's, recorded by hand");
    Ok(())
}

/// The statics `dead_static` adds with `dead = true`: name, type, and the
/// distance west of the FARP in meters.
/// The types: a structure with a destroyed shape, a vehicle, a ship (on
/// land here), an aircraft, a helicopter, and a structure that may have no
/// destroyed shape (UNVERIFIED for the windsock). 60 m apart, so each
/// clear's 30 m sphere holds one.
const DEAD_STATICS: [(&str, &str, f64); 6] = [
    ("person-fixes dead depot", "FARP Fuel Depot", 300.0),
    ("person-fixes dead brdm", "BRDM-2", 360.0),
    ("person-fixes dead boat", "speedboat", 420.0),
    ("person-fixes dead f15", "F-15C", 480.0),
    ("person-fixes dead huey", "UH-1H", 540.0),
    ("person-fixes dead sock", "Windsock", 600.0),
];

/// A read of a static by name at `x`, `z`: found by name, `isExist`,
/// `getLife`, and how often `world.searchObjects` lists it in a 30 m sphere.
fn static_read(name: &str, x: f64, z: f64) -> String {
    format!(
        "local out = {{}}\nlocal f = StaticObject.getByName({name:?})\nout.found = f ~= nil\nif f then local oke, e = pcall(f.isExist, f) if oke then out.exists = e else out.exists = 'raised' end local okl, l = pcall(f.getLife, f) out.life = okl and l or 'raised' end\nout.listed = 0\npcall(world.searchObjects, Object.Category.STATIC, {{ id = world.VolumeType.SPHERE, params = {{ point = {{ x = {x}, y = land.getHeight({{ x = {x}, y = {z} }}), z = {z} }}, radius = 30 }} }}, function(o) local okn, n = pcall(o.getName, o) if okn and n == {name:?} then out.listed = out.listed + 1 end return true end)\nreturn out\n"
    )
}

/// Statics added by script west of the FARP with `dead = true`, for the
/// owner's question whether a static added dead shows its destroyed model on
/// a client and reads as dead, for a structure and for a vehicle type.
/// Records what the server state reads of each; a step that fails is
/// recorded in that static's value. What a client shows is recorded by hand.
/// Returns each static's name, point and record.
fn dead_static(run: &Run, farp: &str) -> Result<Vec<(String, f64, f64, Value)>, String> {
    let at = run.eval(
        "server",
        "s19-person-dead-at",
        &format!(
            "local p = Airbase.getByName({farp:?}):getPoint() return {{ x = p.x, z = p.z }}\n"
        ),
        60,
    )?;
    let (fx, fz) = (
        at["x"].as_f64().ok_or("no FARP point")?,
        at["z"].as_f64().ok_or("no FARP point")?,
    );
    let mut out = Vec::new();
    for (name, kind, west) in DEAD_STATICS {
        let (x, z) = (fx, fz - west);
        let added = run.eval(
            "server",
            "s19-person-dead-add",
            &format!(
                "local ok, s = pcall(coalition.addStaticObject, country.id.USA, {{ name = {name:?}, type = {kind:?}, x = {x}, y = {z}, heading = 0, dead = true, hidden = false }})\nreturn {{ ok = ok and s ~= nil, error = not ok and tostring(s) or nil }}\n"
            ),
            60,
        );
        let read = run.eval(
            "server",
            "s19-person-dead-read",
            &static_read(name, x, z),
            60,
        );
        let v = json!({
            "type": kind,
            "west_m": west,
            "added": added.unwrap_or_else(|e| json!({ "error": e })),
            "read": read.unwrap_or_else(|e| json!({ "error": e })),
        });
        println!("   dead {kind} ({name}, {west} m west of the FARP): {v}");
        out.push((name.to_string(), x, z, v));
    }
    Ok(out)
}

/// After the hold: a `ClearDebris` 30 m sphere at the ground over each
/// static `dead_static` added, the read after it, then a destroy by name of
/// every static in `DEAD_STATICS`, so the mission is as before. A step that
/// fails is recorded in that static's value, and the rest still run.
fn dead_static_clear(
    run: &Run,
    api: &mut Api,
    campaign: u64,
    theater: &str,
    added: Vec<(String, f64, f64, Value)>,
) -> Value {
    let mut out = Vec::new();
    for (name, x, z, mut v) in added {
        let cleared = (|| -> Result<Value, String> {
            let req = pb::DoEffectRequest {
                write: api.write("clear-dead", campaign),
                effect: Some(pb::Effect {
                    effect: Some(pb::effect::Effect::ClearDebris(pb::ClearDebris {
                        volume: Some(pb::clear_debris::Volume::Sphere(pb::Sphere {
                            center: Some(pb::Position {
                                theater: theater.to_string(),
                                north: x,
                                east: z,
                                alt: ground(run, x, z)?,
                            }),
                            radius: 30.0,
                        })),
                    })),
                }),
            };
            let r = api
                .rt
                .block_on(api.commands.do_effect(req))
                .map_err(status_text("DoEffect"))?
                .into_inner();
            let after = run.eval(
                "server",
                "s19-person-dead-after",
                &static_read(&name, x, z),
                60,
            )?;
            Ok(json!({
                "outcome": r.result.as_ref().map(|c| c.outcome),
                "removed": r.debris_removed,
                "after": after,
            }))
        })();
        v["clear"] = cleared.unwrap_or_else(|e| json!({ "error": e }));
        out.push(v);
    }
    let names: Vec<&str> = DEAD_STATICS.iter().map(|d| d.0).collect();
    let removed = run
        .eval(
            "server",
            "s19-person-dead-remove",
            &format!(
                "local left = {{}}\nfor _, n in ipairs({{ {} }}) do local s = StaticObject.getByName(n) if s then pcall(s.destroy, s) end if StaticObject.getByName(n) then left[#left + 1] = n end end\nreturn left\n",
                names.iter().map(|n| format!("{n:?}")).collect::<Vec<_>>().join(", ")
            ),
            60,
        )
        .unwrap_or_else(|e| json!({ "error": e }));
    json!({ "statics": out, "left_after_destroy": removed })
}

/// The aircraft the hold stocks: `NP_SHELL_AIRCRAFT`, a comma-separated
/// list of DCS type names, else the AH-64D of the S19 template.
fn hold_aircraft() -> Vec<String> {
    std::env::var("NP_SHELL_AIRCRAFT")
        .unwrap_or_else(|_| "AH-64D_BLK_II".into())
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// After the hold's mission loads: the flagged airport is set to blue with
/// `SetAirbaseOwner` and stocked with `SetWarehouseStock` (4 of each hold
/// aircraft, 50,000 kg of jet fuel), and a FARP is spawned 2 km north of it
/// with `PutStatic` and the four dynamic-spawn keys, then stocked the same
/// way under its DCS name (spec/persistence.md, "Mission shells";
/// spec/commands.md, "Layout: STATIC_SPAWN"). Returns the FARP's DCS name.
fn owned_and_stocked(
    run: &Run,
    api: &mut Api,
    campaign: u64,
    theater: &str,
    airport: u64,
) -> Result<String, String> {
    let at = run.eval(
        "server",
        "s19-person-airport",
        &format!(
            "for _, b in ipairs(world.getAirbases()) do if b:getID() == {airport} then local p = b:getPoint() return {{ name = b:getName(), x = p.x, z = p.z }} end end\nreturn 'none'\n"
        ),
        60,
    )?;
    let name = at["name"]
        .as_str()
        .ok_or_else(|| format!("airport {airport} not found: {at}"))?
        .to_string();
    let (x, z) = (
        at["x"].as_f64().unwrap_or(0.0),
        at["z"].as_f64().unwrap_or(0.0),
    );
    let aircraft: std::collections::BTreeMap<String, f64> =
        hold_aircraft().into_iter().map(|t| (t, 4.0)).collect();
    let stock = |api: &mut Api, airbase: &str| -> Result<(), String> {
        let req = pb::SetWarehouseStockRequest {
            write: api.write("stock", campaign),
            theater: theater.to_string(),
            airbase: airbase.to_string(),
            aircraft: aircraft.clone(),
            liquids: [(0u32, 50_000.0)].into_iter().collect(),
            ..pb::SetWarehouseStockRequest::default()
        };
        api.rt
            .block_on(api.commands.set_warehouse_stock(req))
            .map_err(status_text("SetWarehouseStock"))?;
        Ok(())
    };
    let req = pb::SetAirbaseOwnerRequest {
        write: api.write("owner", campaign),
        theater: theater.to_string(),
        airbase: name.clone(),
        owner: pb::Coalition::Blue as i32,
    };
    api.rt
        .block_on(api.commands.set_airbase_owner(req))
        .map_err(status_text("SetAirbaseOwner"))?;
    stock(api, &name)?;
    let flag = |b: bool| prost_types::Value {
        kind: Some(prost_types::value::Kind::BoolValue(b)),
    };
    let text = |t: &str| prost_types::Value {
        kind: Some(prost_types::value::Kind::StringValue(t.into())),
    };
    let extra = prost_types::Struct {
        fields: [
            ("category", text("Heliports")),
            ("dynamicSpawn", flag(true)),
            ("allowHotStart", flag(true)),
            ("dynamicCargo", flag(true)),
            ("hidden", flag(false)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    };
    let req = pb::PutStaticRequest {
        write: api.write("farp", campaign),
        static_id: 0,
        spec: Some(pb::StaticSpec {
            label: "person-fixes FARP".into(),
            country: 2,
            r#type: "FARP".into(),
            position: Some(pb::Position {
                theater: theater.to_string(),
                north: x + 2000.0,
                east: z,
                ..pb::Position::default()
            }),
            heading: 0.0,
            dead: false,
            extra: Some(extra),
        }),
    };
    api.rt
        .block_on(api.commands.put_static(req))
        .map_err(status_text("PutStatic"))?;
    let farp = wait(
        run,
        api,
        Duration::from_secs(60),
        "the FARP's airbase",
        |_| {
            let v = run.eval(
                "server",
                "s19-person-farp",
                // `coalition.getStaticObjects` does not list a FARP (LIVE);
                // `world.getAirbases` lists its airbase under its name.
                "for _, b in ipairs(world.getAirbases()) do local n = b:getName() if n:sub(1, 3) == 'np#' then return n end end\nreturn ''\n",
                60,
            )?;
            Ok(v.as_str().filter(|n| !n.is_empty()).map(str::to_string))
        },
    )?;
    stock(api, &farp)?;
    println!("   {name} set blue and stocked; FARP {farp} spawned and stocked");
    Ok(farp)
}

/// The cost of one 250 km `ClearDebris` box, as a whole-map clear sends,
/// centered on the FARP: the static search runs over every static in it.
fn box_clear(
    run: &Run,
    api: &mut Api,
    campaign: u64,
    theater: &str,
    farp: &str,
) -> Result<Value, String> {
    let at = run.eval(
        "server",
        "s19-person-box-at",
        &format!(
            "local p = Airbase.getByName({farp:?}):getPoint() return {{ x = p.x, z = p.z }}\n"
        ),
        60,
    )?;
    let (x, z) = (
        at["x"].as_f64().ok_or("no FARP point")?,
        at["z"].as_f64().ok_or("no FARP point")?,
    );
    let req = pb::DoEffectRequest {
        write: api.write("clear-box", campaign),
        effect: Some(pb::Effect {
            effect: Some(pb::effect::Effect::ClearDebris(pb::ClearDebris {
                volume: Some(pb::clear_debris::Volume::Box(pb::Box {
                    min: Some(pb::Position {
                        theater: theater.to_string(),
                        north: x - 125_000.0,
                        east: z - 125_000.0,
                        alt: -10_000.0,
                    }),
                    max: Some(pb::Position {
                        theater: theater.to_string(),
                        north: x + 125_000.0,
                        east: z + 125_000.0,
                        alt: 100_000.0,
                    }),
                })),
            })),
        }),
    };
    let b = api
        .rt
        .block_on(api.commands.do_effect(req))
        .map_err(status_text("DoEffect box"))?
        .into_inner();
    Ok(json!({
        "outcome": b.result.as_ref().map(|c| c.outcome),
        "cost_us": b.result.as_ref().map(|c| c.cost_us),
        "removed": b.debris_removed,
    }))
}

/// The ground's height in meters above sea level at `x`, `z`: a sphere's
/// `alt` is above sea level, so one centered at 0 sits under high ground.
fn ground(run: &Run, x: f64, z: f64) -> Result<f64, String> {
    run.eval(
        "server",
        "s19-person-ground",
        &format!("return land.getHeight({{ x = {x}, y = {z} }})\n"),
        60,
    )?
    .as_f64()
    .ok_or_else(|| "no ground height".to_string())
}

/// A dead static's ruin through `DoEffect` `ClearDebris` (spec/commands.md,
/// "Layout: CLEAR_DEBRIS"): a `FARP Fuel Depot` added by script 300 m east of
/// the FARP, so unmanaged, killed with a scripted explosion of power 1,000, then a
/// 30 m sphere cleared on it 10 s later. Records the clear's count and what
/// the server state reads of the static before and after, by name and by
/// `world.searchObjects`, at once and 5 s later.
fn ruin_check(
    run: &Run,
    api: &mut Api,
    campaign: u64,
    theater: &str,
    farp: &str,
) -> Result<Value, String> {
    let at = run.eval(
        "server",
        "s19-person-ruin-at",
        &format!(
            "local p = Airbase.getByName({farp:?}):getPoint() return {{ x = p.x, z = p.z }}\n"
        ),
        60,
    )?;
    let (x, z) = (
        at["x"].as_f64().ok_or("no FARP point")?,
        at["z"].as_f64().ok_or("no FARP point")? + 300.0,
    );
    // A scripted static, unmanaged: the clear's handling of a dead static the
    // platform did not spawn.
    let name = "person-fixes ruin".to_string();
    let added = run.eval(
        "server",
        "s19-person-ruin-add",
        &format!(
            "local s = coalition.addStaticObject(country.id.USA, {{ name = {name:?}, type = 'FARP Fuel Depot', x = {x}, y = {z}, heading = 0 }})\nreturn s ~= nil and s:getName() or ''\n"
        ),
        60,
    )?;
    if added.as_str() != Some(name.as_str()) {
        return Err(format!("the ruin static was not added: {added}"));
    }
    let read = format!(
        "local out = {{}}\nlocal s = StaticObject.getByName({name:?})\nout.found = s ~= nil\nif s then local ok, e = pcall(s.isExist, s) if ok then out.exists = e else out.exists = 'raised' end local okl, l = pcall(s.getLife, s) out.life = okl and l or 'raised' end\nout.listed = 0\nworld.searchObjects(Object.Category.STATIC, {{ id = world.VolumeType.SPHERE, params = {{ point = {{ x = {x}, y = land.getHeight({{ x = {x}, y = {z} }}), z = {z} }}, radius = 30 }} }}, function(o) local ok, n = pcall(o.getName, o) if ok and n == {name:?} then out.listed = out.listed + 1 end return true end)\nreturn out\n"
    );
    run.eval(
        "server",
        "s19-person-ruin-kill",
        &format!("trigger.action.explosion({{ x = {x}, y = land.getHeight({{ x = {x}, y = {z} }}), z = {z} }}, 1000) return 'ok'\n"),
        60,
    )?;
    let _ = run.hold(10.0);
    let killed = run.eval("server", "s19-person-ruin-killed", &read, 60)?;
    if killed["life"].as_f64().is_some_and(|l| l > 0.0) || killed["exists"] == json!(true) {
        return Err(format!(
            "the explosion left the ruin static alive: {killed}"
        ));
    }
    let players = run.eval(
        "hook",
        "s19-person-ruin-players",
        "return #(net.get_player_list() or {})\n",
        60,
    )?;
    let req = pb::DoEffectRequest {
        write: api.write("clear", campaign),
        effect: Some(pb::Effect {
            effect: Some(pb::effect::Effect::ClearDebris(pb::ClearDebris {
                volume: Some(pb::clear_debris::Volume::Sphere(pb::Sphere {
                    center: Some(pb::Position {
                        theater: theater.to_string(),
                        north: x,
                        east: z,
                        alt: ground(run, x, z)?,
                    }),
                    radius: 30.0,
                })),
            })),
        }),
    };
    let r = api
        .rt
        .block_on(api.commands.do_effect(req))
        .map_err(status_text("DoEffect"))?
        .into_inner();
    let after = run.eval("server", "s19-person-ruin-after", &read, 60)?;
    let _ = run.hold(5.0);
    let later = run.eval("server", "s19-person-ruin-later", &read, 60)?;
    let v = json!({
        "static": name,
        "players": players,
        "killed": killed,
        "outcome": r.result.as_ref().map(|c| c.outcome),
        "message": r.result.as_ref().map(|c| c.message.clone()),
        "cost_us": r.result.as_ref().map(|c| c.cost_us),
        "debris_removed": r.debris_removed,
        "after": after,
        "after_5s": later,
    });
    // The server-side verdict: the clear applied, found the dead static
    // before it, counted at least one removal, and the static is gone from
    // the search 5 s later. What a person's client showed is recorded by hand.
    let applied = r.result.as_ref().map(|c| c.outcome) == Some(pb::Outcome::Applied as i32);
    let fails = !applied
        || killed["listed"].as_u64() == Some(0)
        || r.debris_removed.unwrap_or(0) < 1
        || later["listed"].as_u64().unwrap_or(0) > 0;
    let mut v = v;
    v["ruin_verdict"] = json!(if fails { "fails" } else { "passes" });
    println!("   ruin ({}): {v}", v["ruin_verdict"]);
    Ok(v)
}

/// Wait for the mission on `theater` loaded after `generation`.
fn loaded(run: &Run, api: &mut Api, theater: &str, after_gen: u32) -> Result<u32, String> {
    wait(
        run,
        api,
        LOAD_LIMIT,
        &format!("a mission on {theater}"),
        |api| {
            let s = api.status()?;
            let generation = s.now.as_ref().map_or(0, |n| n.mission_generation);
            Ok((s.theater == theater
                && generation > after_gen
                && s.sim_state != pb::SimState::Loading as i32)
                .then_some(generation))
        },
    )
}

fn live(run: &Run, api: &mut Api, campaign: u64, group: u64) -> Result<(), String> {
    wait(
        run,
        api,
        LOAD_LIMIT,
        &format!("group {group} live"),
        |api| Ok((api.state(campaign, group)? == pb::EntityState::Live as i32).then_some(())),
    )
}

fn state_name(s: i32) -> String {
    pb::EntityState::try_from(s).map_or_else(|_| s.to_string(), |s| s.as_str_name().into())
}

/// A Lua value as a JSON value, for a weather table built from the template.
fn weather_struct(v: &Lv) -> Option<prost_types::Struct> {
    nanoprobe_api::lua::lua_to_struct(v)
}

/// Run the part's steps; each records its result under its name.
#[allow(clippy::too_many_arguments)]
pub fn drive(
    run: &Run,
    endpoint: &str,
    token: &str,
    template: Option<&[u8]>,
    players: impl Fn() -> Result<usize, String>,
    restart: impl Fn() -> Result<(), String>,
    kill: impl Fn() -> Result<String, String>,
    results: &mut Value,
) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let mut generation = api.status()?.now.map_or(0, |n| n.mission_generation);

    // A person test: only the own-template shell, held for a client.
    if let Ok(stop) = std::env::var("NP_SHELL_PERSON") {
        let t = template.ok_or("NP_SHELL_PERSON needs --template")?;
        return person(run, &mut api, t, generation, &stop, results);
    }

    // 1. Weather and time across a commanded reload.
    println!("== reload: start and weather");
    let c1 = api.create("s19-two-maps", &["Caucasus", "Afghanistan"], true)?;
    generation = loaded(run, &mut api, "Caucasus", generation)?;
    let (x, z) = crate::sidecar::HELLO_POINTS[0];
    let a = api.group(c1, "on-caucasus", caucasus(x, z))?;
    live(run, &mut api, c1, a)?;
    let tpl = shell::members(CAUCASUS)?;
    let mission = &tpl.iter().find(|(n, _)| n == "mission").ok_or("mission")?.1;
    let doc = shell::lua::parse(mission)?;
    let mut weather = shell::lua::value(
        mission,
        doc.root
            .get(&shell::lua::Key::s("weather"))
            .ok_or("weather")?,
    )
    .ok_or("weather value")?;
    // Set the ground wind and the temperature to values of the test's own.
    let (wind, temp) = (7.0, 31.0);
    set_path(
        &mut weather,
        &["wind", "atGround", "speed"],
        Lv::Number(wind),
    );
    set_path(&mut weather, &["season", "temperature"], Lv::Number(temp));
    api.next(
        c1,
        pb::NextMission {
            theater: String::new(),
            start: Some(prost_types::Timestamp {
                seconds: START_S,
                nanos: 0,
            }),
            weather: weather_struct(&weather),
            dynamic_spawn: Vec::new(),
        },
    )?;
    api.reload()?;
    generation = loaded(run, &mut api, "Caucasus", generation)?;
    live(run, &mut api, c1, a)?;
    let seen = run.eval(
        "server",
        "s19-mission",
        "local m = env.mission\nreturn { day = m.date.Day, month = m.date.Month, year = m.date.Year, start = m.start_time, wind = m.weather.wind.atGround.speed, temp = m.weather.season.temperature, abs = timer.getAbsTime() }\n",
        60,
    )?;
    // An eval can outlast the lease's 30 s; renew before the next call.
    api.renew()?;
    let want = shell::Start::from_unix_ms(START_S * 1000);
    let pass = seen["day"] == json!(want.day)
        && seen["month"] == json!(want.month)
        && seen["year"] == json!(want.year)
        && seen["start"].as_f64() == Some(f64::from(want.seconds))
        && seen["wind"].as_f64() == Some(wind)
        && seen["temp"].as_f64() == Some(temp);
    let clock = api
        .rt
        .block_on(
            api.campaigns
                .get_campaign(pb::GetCampaignRequest { campaign_id: c1 }),
        )
        .map_err(status_text("GetCampaign"))?
        .into_inner()
        .campaign
        .and_then(|c| c.clock)
        .map(|t| t.seconds);
    // The campaign clock took the requested start.
    let pass = pass && clock.is_some_and(|c| (START_S..START_S + 3 * 3600).contains(&c));
    results["reload"] =
        json!({ "pass": pass, "seen": seen, "clock_s": clock, "want_start_s": START_S });
    println!("   {} {seen}", if pass { "ok  " } else { "FAIL" });

    // 2. Two theaters, there and back.
    println!("== theaters");
    let mut rounds = Vec::new();
    let mut b = 0;
    for (i, theater) in ["Afghanistan", "Caucasus", "Afghanistan"]
        .iter()
        .enumerate()
    {
        api.next(
            c1,
            pb::NextMission {
                theater: (*theater).into(),
                ..pb::NextMission::default()
            },
        )?;
        api.reload()?;
        generation = loaded(run, &mut api, theater, generation)?;
        if i == 0 {
            results["e7_airbase_ids"]["Afghanistan"] = airbase_ids(run, AFGHANISTAN)?;
            api.renew()?;
            // A point on land near the first airbase of the map.
            let p = run.eval(
                "server",
                "s19-afghan-point",
                "local ab = world.getAirbases()\nfor _, a in ipairs(ab) do local p = a:getPoint()\nfor _, d in ipairs({ 300, 600, 900, -300, -600 }) do local q = { x = p.x + d, y = p.z + d }\nlocal t = land.getSurfaceType(q)\nif t == 1 or t == 4 then return { q.x, q.y } end end end\nreturn nil\n",
                60,
            )?;
            api.renew()?;
            let (n, e) = (
                p[0].as_f64().ok_or(format!("no land point: {p}"))?,
                p[1].as_f64().ok_or("no land point")?,
            );
            b = api.group(
                c1,
                "on-afghanistan",
                pb::Position {
                    theater: "Afghanistan".into(),
                    north: n,
                    east: e,
                    alt: 0.0,
                },
            )?;
        }
        let (here, there) = if *theater == "Caucasus" {
            (a, b)
        } else {
            (b, a)
        };
        live(run, &mut api, c1, here)?;
        let other = api.state(c1, there)?;
        rounds.push(json!({
            "theater": theater,
            "generation": generation,
            "loaded_live": true,
            "other": state_name(other),
            "pass": other == pb::EntityState::Virtual as i32,
        }));
    }
    let pass = rounds.iter().all(|r| r["pass"] == json!(true));
    results["theaters"] = json!({ "pass": pass, "rounds": rounds });
    println!(
        "   {} {}",
        if pass { "ok  " } else { "FAIL" },
        results["theaters"]
    );

    // 3. A campaign's own editor-built template.
    let mut own = false;
    if let Some(t) = template {
        println!("== own template");
        let theater = shell::theatre(t)?;
        let c2 = api.create("s19-own-template", &[theater.as_str()], false)?;
        let airport = shell::airports(t)?.into_iter().next().ok_or("no airport")?;
        let req = pb::SetCampaignTemplateRequest {
            write: api.write("template", c2),
            theater: theater.clone(),
            path: own_template_file(run, t)?,
        };
        api.rt
            .block_on(api.campaigns.set_campaign_template(req))
            .map_err(status_text("SetCampaignTemplate"))?;
        api.next(
            c2,
            pb::NextMission {
                dynamic_spawn: vec![pb::DynamicSpawn {
                    airbase_id: airport,
                    enabled: true,
                    hot_start: false,
                }],
                ..pb::NextMission::default()
            },
        )?;
        let req = pb::ActivateCampaignRequest {
            lease: api.lease.clone(),
            campaign_id: c2,
            idempotency: api.key("activate"),
        };
        api.rt
            .block_on(api.campaigns.activate_campaign(req))
            .map_err(status_text("ActivateCampaign"))?;
        generation = loaded(run, &mut api, &theater, generation)?;
        let _ = run.hold(5.0);
        let seen = run.eval(
            "server",
            "s19-own",
            &format!(
                "local m = env.mission\nlocal zones = {{}}\nfor _, z in ipairs(m.triggers and m.triggers.zones or {{}}) do zones[#zones + 1] = z.name end\nlocal clients = {{}}\nfor side, c in pairs(m.coalition) do for _, country in ipairs(c.country or {{}}) do for _, cat in ipairs({{ 'plane', 'helicopter', 'vehicle', 'ship' }}) do for _, g in ipairs(country[cat] and country[cat].group or {{}}) do for _, u in ipairs(g.units or {{}}) do if u.skill == 'Client' or u.skill == 'Player' then clients[#clients + 1] = {{ name = u.name, id = tostring(u.unitId), group = g.name }} end end end end end end\nlocal rules = m.trigrules and #m.trigrules or 0\nlocal z1 = zones[1] and trigger.misc.getZone(zones[1]) ~= nil\nreturn {{ zones = zones, zone_found = z1, clients = clients, rules = rules, airport = {airport} }}\n"
            ),
            60,
        )?;
        let slots = run.eval(
            "hook",
            "s19-slots",
            "local out = {}\nfor _, side in ipairs({ 'red', 'blue' }) do for _, s in ipairs(DCS.getAvailableSlots(side) or {}) do local e = { side = side }\nfor k, v in pairs(s) do e[tostring(k)] = tostring(v) end\nout[#out + 1] = e end end\nreturn out\n",
            60,
        )?;
        api.renew()?;
        // The shell DCS loaded, compared with the template member by member.
        let shell_file = run
            .remote
            .read(&format!("nanoprobe/missions/shells/{c2}.a.miz"))?
            .ok_or("the shell is not in the mission folder")?;
        let (tm, sm) = (shell::members(t)?, shell::members(&shell_file)?);
        let same_members = tm.iter().map(|m| &m.0).collect::<Vec<_>>()
            == sm.iter().map(|m| &m.0).collect::<Vec<_>>();
        let unchanged: Vec<String> = tm
            .iter()
            .zip(&sm)
            .filter(|((n, x), (_, y))| n != "mission" && n != "warehouses" && x == y)
            .map(|(m, _)| m.0.clone())
            .collect();
        let others = tm.len().saturating_sub(2);
        let mission_rest = rest_equal(&tm, &sm, "mission", &["date", "start_time", "weather"])?;
        let warehouses_rest = rest_equal(&tm, &sm, "warehouses", &["airports"])?;
        // The template's own trigger rules, all of them in the mission.
        let tpl_rules = {
            let src = &tm.iter().find(|(n, _)| n == "mission").ok_or("mission")?.1;
            let d = shell::lua::parse(src)?;
            d.root
                .get(&shell::lua::Key::s("trigrules"))
                .map_or(0, |n| n.fields.len())
        };
        // The airport's flag in the shell's warehouses.
        let spawn_flag = {
            let src = &sm
                .iter()
                .find(|(n, _)| n == "warehouses")
                .ok_or("warehouses")?
                .1;
            let d = shell::lua::parse(src)?;
            d.root
                .at(&[
                    shell::lua::Key::s("airports"),
                    shell::lua::Key::Num(airport as f64),
                    shell::lua::Key::s("dynamicSpawn"),
                ])
                .and_then(|n| shell::lua::scalar_value(src, n))
                == Some(Lv::Bool(true))
        };
        let client_ids: Vec<String> = seen["clients"]
            .as_array()
            .map(|c| {
                c.iter()
                    .filter_map(|c| c["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let slot_list = slots.as_array().cloned().unwrap_or_default();
        // A fixed slot: one of the template's client units.
        let fixed = slot_list.iter().any(|s| {
            s["unitId"]
                .as_str()
                .is_some_and(|id| client_ids.iter().any(|c| c == id))
        });
        // A dynamic slot: one that names the airport set for dynamic spawn,
        // or says it is dynamic (field names UNVERIFIED: every field is
        // recorded).
        let airport_s = airport.to_string();
        // Every field arrives as text: a dynamic field counts when it is
        // "true" or a non-zero number.
        let flagged = |v: &serde_json::Value| match v {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
            serde_json::Value::String(t) => t == "true" || t.parse::<f64>().is_ok_and(|n| n != 0.0),
            _ => false,
        };
        let dynamic_rule = slot_list.iter().find_map(|s| {
            // A fixed client unit parked there is not a dynamic slot.
            let fixed_unit = s["unitId"]
                .as_str()
                .is_some_and(|id| client_ids.iter().any(|c| c == id));
            if fixed_unit {
                return None;
            }
            s.as_object()?.iter().find_map(|(k, v)| {
                let k = k.to_lowercase();
                if k.contains("dynamic") && flagged(v) {
                    Some(format!("field {k} is {v}"))
                } else if k.contains("airdrome") && v.as_str() == Some(airport_s.as_str()) {
                    Some(format!("field {k} names airport {airport_s}"))
                } else {
                    None
                }
            })
        });
        let dynamic = dynamic_rule.is_some();
        let pass = same_members
            && unchanged.len() == others
            && mission_rest
            && warehouses_rest
            && spawn_flag
            && seen["zone_found"] == json!(true)
            && tpl_rules > 0
            && seen["rules"].as_u64() == Some(tpl_rules as u64)
            && fixed;
        // LIVE (2.9.31.29434, Wine, 2026-11-16): `DCS.getAvailableSlots`
        // listed the template's Client seats and no dynamic-spawn slot while
        // the shell carried the airport's flag. Whether a client is offered
        // one is a person test; the listing is recorded, not passed on.
        results["own_template"] = json!({
            "pass": pass,
            "same_members": same_members,
            "unchanged_members": unchanged,
            "mission_rest_equal": mission_rest,
            "warehouses_rest_equal": warehouses_rest,
            "shell_dynamic_spawn": spawn_flag,
            "template_rules": tpl_rules,
            "fixed_slot": fixed,
            "dynamic_slot_listed": dynamic,
            "dynamic_slot_rule": dynamic_rule,
            "seen": seen,
            "slots": slots,
        });
        println!(
            "   {} {}",
            if pass { "ok  " } else { "FAIL" },
            results["own_template"]["seen"]
        );
        own = true;
    }

    // 4. A finished campaign is readable and refuses changes: one made
    // for it, with a group in its store.
    println!("== finished");
    let finish = api.create("s19-finished", &["Caucasus"], false)?;
    let kept = api.group(finish, "kept", caucasus(x, z))?;
    let req = pb::FinishCampaignRequest {
        lease: api.lease.clone(),
        campaign_id: finish,
        outcome: None,
        idempotency: api.key("finish"),
    };
    api.rt
        .block_on(api.campaigns.finish_campaign(req))
        .map_err(status_text("FinishCampaign"))?;
    let got = api
        .rt
        .block_on(api.campaigns.get_campaign(pb::GetCampaignRequest {
            campaign_id: finish,
        }))
        .map_err(status_text("GetCampaign"))?
        .into_inner()
        .campaign
        .map_or(0, |c| c.state);
    let listed = api.state(finish, kept);
    let req = api.group_request(finish, "late", caucasus(x, z));
    let refused = api.rt.block_on(api.commands.put_group(req));
    let reason = refused
        .as_ref()
        .err()
        .and_then(nanoprobe_proto::errors::detail)
        .map(|d| d.reason);
    let read_only = reason == Some(pb::ErrorReason::CampaignReadOnly as i32);
    let pass = got == pb::CampaignState::Finished as i32 && listed.is_ok() && read_only;
    results["finished"] = json!({
        "pass": pass,
        "state": got,
        "listed": listed.as_ref().map(|s| state_name(*s)).map_err(Clone::clone),
        "refused_reason": reason,
    });
    println!(
        "   {} {}",
        if pass { "ok  " } else { "FAIL" },
        results["finished"]
    );

    // 5 and 6 need the first campaign active and an empty server.
    if own {
        let req = pb::ActivateCampaignRequest {
            lease: api.lease.clone(),
            campaign_id: c1,
            idempotency: api.key("activate-c1"),
        };
        api.rt
            .block_on(api.campaigns.activate_campaign(req))
            .map_err(status_text("ActivateCampaign"))?;
        loaded(run, &mut api, "Afghanistan", generation)?;
    }
    live(run, &mut api, c1, b)?;

    println!("== hung: a deliberate access violation");
    let n = players()?;
    if n > 0 {
        results["hung"] = json!({ "skipped": format!("{n} players connected") });
        return Ok(());
    }
    // No result comes back: DCS crashes. The clock starts before the fault
    // is sent; the eval's own return time is recorded apart.
    let crashed = Instant::now();
    let _ = run.eval(
        "server",
        "s19-fault",
        "local E = ...\nlocal agent = E.lib('agent').open()\nagent.crumb(91, 'S19 hung verdict')\nagent.fault()\nreturn 'not reached'\n",
        2,
    );
    let eval_returned = crashed.elapsed();
    let deadline = crashed + HUNG_LIMIT + Duration::from_secs(35);
    let mut verdicts = Vec::new();
    let mut hung_after = None;
    while Instant::now() < deadline {
        let s = api.status()?;
        verdicts.push(json!({
            "t_s": crashed.elapsed().as_secs_f64(),
            "health": s.health,
            "sim_state": s.sim_state,
            "age_s": s.heartbeat_age.map(|d| d.seconds),
        }));
        if s.health == pb::Health::Hung as i32 && s.sim_state == pb::SimState::Hung as i32 {
            hung_after = Some(crashed.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let processes = run
        .sh("ps -eo pid,etimes,args 2>/dev/null | grep -i -E 'dcs|crash|wine' | grep -v grep || true", &[])
        .map(|o| String::from_utf8_lossy(&o).into_owned())
        .unwrap_or_default();
    let events = api.lifecycle_states(false, c1).unwrap_or_default();
    let event = events.contains(&(pb::SimState::Hung as i32));
    let pass = hung_after.is_some_and(|d| d <= HUNG_LIMIT) && event;
    results["hung"] = json!({
        "pass": pass,
        "hung_after_s": hung_after.map(|d| d.as_secs_f64()),
        "fault_eval_returned_s": eval_returned.as_secs_f64(),
        "limit_s": HUNG_LIMIT.as_secs(),
        "heartbeat_stale_s": HEARTBEAT_STALE_S,
        "lifecycle_event": event,
        "verdicts": verdicts,
        "processes": processes,
    });
    println!(
        "   {} hung after {hung_after:?}",
        if pass { "ok  " } else { "FAIL" }
    );
    println!("== restart DCS");
    restart()?;
    // DCS starts with the server's own mission; the campaign takes it.
    wait(
        run,
        &mut api,
        LOAD_LIMIT,
        "DCS back after the restart",
        |api| {
            // The sidecar restarts with DCS: no answer is not back yet.
            let Ok(s) = api.status() else {
                return Ok(None);
            };
            Ok((s.health == pb::Health::Healthy as i32
                && s.sim_state == pb::SimState::Running as i32)
                .then_some(()))
        },
    )?;
    live(run, &mut api, c1, b).ok();

    // 6. Killed, started again by the image, restored with no call made.
    println!("== killed");
    let n = players()?;
    if n > 0 {
        results["killed"] = json!({ "skipped": format!("{n} players connected") });
        return Ok(());
    }
    let before = api.status()?;
    let killed = kill()?;
    // The new DCS process is told by its health leaving Healthy and coming
    // back Healthy and Running, or by its frame count falling below the
    // one before the kill. `mission_generation` restarts in each process,
    // so it is only recorded.
    let mut left_healthy = false;
    let back = wait(
        run,
        &mut api,
        LOAD_LIMIT,
        "DCS back after the kill",
        |api| {
            let Ok(s) = api.status() else {
                // The sidecar restarts after the kill: DCS left Healthy.
                left_healthy = true;
                return Ok(None);
            };
            let healthy = s.health == pb::Health::Healthy as i32;
            if !healthy {
                left_healthy = true;
            }
            let frame_fell = s.now.as_ref().map(|n| n.frame) < before.now.as_ref().map(|n| n.frame);
            Ok((healthy
                && s.sim_state == pb::SimState::Running as i32
                && (left_healthy || frame_fell))
                .then_some(s))
        },
    )?;
    let restored = wait(run, &mut api, LOAD_LIMIT, "the group live again", |api| {
        let here = if back.theater == "Caucasus" { a } else { b };
        Ok((api.state(c1, here)? == pb::EntityState::Live as i32).then_some(here))
    });
    let pass = restored.is_ok();
    results["killed"] = json!({
        "pass": pass,
        "kill": killed,
        "theater_after": back.theater,
        "generation_before": before.now.as_ref().map(|n| n.mission_generation),
        "generation_after": back.now.as_ref().map(|n| n.mission_generation),
        "restored_group": restored.as_ref().ok(),
        "error": restored.err(),
    });
    println!(
        "   {} {}",
        if pass { "ok  " } else { "FAIL" },
        results["killed"]
    );
    // Leave no campaign active.
    let req = pb::SuspendCampaignRequest {
        lease: api.lease.clone(),
        campaign_id: c1,
        idempotency: api.key("suspend"),
    };
    let _ = api.rt.block_on(api.campaigns.suspend_campaign(req));
    Ok(())
}

/// E7: whether every airdrome's `getID()` on the loaded map is a key of
/// the template's `warehouses` airports (the ruling's condition for
/// `Airbase.id`).
fn airbase_ids(run: &Run, template: &[u8]) -> Result<Value, String> {
    let ids = run.eval(
        "server",
        "s19-airbase-ids",
        "local out = {}\nfor _, a in ipairs(world.getAirbases()) do local d = a:getDesc()\nif d and d.category == Airbase.Category.AIRDROME then out[#out + 1] = { name = a:getName(), id = tonumber(a:getID()) } end end\nreturn out\n",
        60,
    )?;
    let keys = shell::airports(template)?;
    let missing: Vec<&Value> = ids
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|v| v["id"].as_u64().is_none_or(|id| !keys.contains(&id)))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "airdromes": ids.as_array().map_or(0, Vec::len),
        "warehouse_airports": keys.len(),
        "not_in_warehouses": missing,
        "equal": ids.as_array().is_some_and(|a| !a.is_empty()) && missing.is_empty(),
    }))
}

/// Whether member `name` of two `.miz` files is equal apart from the root
/// keys in `drop`.
fn rest_equal(
    a: &[(String, Vec<u8>)],
    b: &[(String, Vec<u8>)],
    name: &str,
    drop: &[&str],
) -> Result<bool, String> {
    let rest = |m: &[(String, Vec<u8>)]| -> Result<Lv, String> {
        let src = &m.iter().find(|(n, _)| n == name).ok_or(name)?.1;
        let d = shell::lua::parse(src)?;
        let Lv::Table(p) = shell::lua::value(src, &d.root).ok_or("value")? else {
            return Err("not a table".into());
        };
        Ok(Lv::Table(
            p.into_iter()
                .filter(|(k, _)| !drop.iter().any(|d| *k == Lv::String(d.as_bytes().to_vec())))
                .collect(),
        ))
    };
    Ok(rest(a)? == rest(b)?)
}

/// Set the value at `path` in a table, where every step exists.
fn set_path(v: &mut Lv, path: &[&str], to: Lv) {
    let Some((first, rest)) = path.split_first() else {
        *v = to;
        return;
    };
    if let Lv::Table(p) = v
        && let Some((_, child)) = p
            .iter_mut()
            .find(|(k, _)| *k == Lv::String(first.as_bytes().to_vec()))
    {
        set_path(child, rest, to);
    }
}
