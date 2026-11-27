//! `MissionService`: the server status, the next mission's settings and
//! the reload that writes a campaign's shell and loads it (plan 4.4).

use std::sync::Arc;

use nanoprobe_proto::v1alpha1::mission_service_server::MissionService;
use nanoprobe_proto::v1alpha1::{
    CommandResult, GetServerStatusRequest, GetServerStatusResponse, ListMissionFilesRequest,
    ListMissionFilesResponse, MissionFile, MissionFileKind, NextMission, Outcome, ReloadRequest,
    ReloadResponse, ServerStatus, SetMissionFileRequest, SetMissionFileResponse,
    SetNextMissionRequest, SetNextMissionResponse, SimState,
};
use tonic::{Request, Response, Status};

use crate::auth::{Access, require};
use crate::convert::{invalid, ms_of_timestamp};
use crate::hub::{Call, Inner, Need};
use crate::hub::{Hub, server_sim_state};
use crate::next::Next;

/// `ServerStatus.dcs_ids_low` is set when fewer IDs than this are left in the
/// pool of 2^30. 1% of the pool (about 10.7 million) is far more than a
/// campaign adds in a day, so the owner has time to plan a DCS restart.
pub const DCS_IDS_LOW: u64 = nanoprobe_sidecar::store::dcs_ids::SLOTS / 100;

/// The service.
pub struct Mission(pub Arc<Hub>);

#[tonic::async_trait]
impl MissionService for Mission {
    async fn get_server_status(
        &self,
        req: Request<GetServerStatusRequest>,
    ) -> Result<Response<GetServerStatusResponse>, Status> {
        require(&req, Access::Observer)?;
        let g = self.0.lock();
        let health = g.backend.health();
        let space = g.space(g.active);
        let age = g.backend.heartbeat_age().map(|d| prost_types::Duration {
            seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
        });
        let state: SimState = server_sim_state(health, g.link.lifecycle);
        let ids_left = g.dcs_ids_left();
        Ok(Response::new(GetServerStatusResponse {
            status: Some(ServerStatus {
                health: health as i32,
                heartbeat_age: age,
                sim_state: state as i32,
                theater: g.link.theater.clone(),
                now: Some(g.link.stamp()),
                campaign_id: g.active.unwrap_or(0),
                commands_queued: g.rec.as_ref().map_or(0, |r| {
                    u32::try_from(r.inflight().count()).unwrap_or(u32::MAX)
                }),
                dcs_version: g.link.dcs_version.clone(),
                agent_build: if g.link.agent_build == 0 {
                    String::new()
                } else {
                    format!("{:016x}", g.link.agent_build)
                },
                event_gaps: g.link.event_gaps,
                attached: g.link.attached,
                attach_error: g.backend.attach_error(),
                agent_interface: g.link.agent_interface,
                store_free_bytes: space.map(|(free, _)| free),
                store_space_low: space.is_some_and(|(free, held)| free < held),
                store_size: g.store_size(),
                last_backup: g.last_backup_ms.map(crate::hub::timestamp_ms),
                last_backup_failure: g
                    .backup_failure
                    .as_ref()
                    .map(|(at, _)| crate::hub::timestamp_ms(*at)),
                backup_error: g
                    .backup_failure
                    .as_ref()
                    .map(|(_, why)| why.clone())
                    .unwrap_or_default(),
                dcs_ids_left: ids_left,
                dcs_ids_low: ids_left < DCS_IDS_LOW,
                ..ServerStatus::default()
            }),
        }))
    }

    async fn set_next_mission(
        &self,
        req: Request<SetNextMissionRequest>,
    ) -> Result<Response<SetNextMissionResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let w = r.write.clone().unwrap_or_default();
        let next = next_of(r.next.as_ref())?;
        let mut hashed = r.clone();
        if let Some(h) = hashed.write.as_mut() {
            h.lease = None;
        }
        let resp = self
            .0
            .write(
                Call {
                    method: "SetNextMission",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, id| {
                    g.set_next(id, &next)?;
                    Ok((SetNextMissionResponse {}, id))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn reload(
        &self,
        req: Request<ReloadRequest>,
    ) -> Result<Response<ReloadResponse>, Status> {
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let w = r.write.clone().unwrap_or_default();
        let mut hashed = r.clone();
        if let Some(h) = hashed.write.as_mut() {
            h.lease = None;
        }
        let cfg = self.0.cfg.clone();
        // The refusals come first; then the groups DCS holds are captured:
        // the load ends them.
        self.0
            .lock()
            .reload_check(w.idempotency.as_ref(), w.lease.as_ref(), w.campaign_id)?;
        self.0
            .capture_live(w.idempotency.as_ref(), w.lease.as_ref(), w.campaign_id)
            .await;
        let resp = self
            .0
            .write(
                Call {
                    method: "Reload",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Active,
                },
                &hashed,
                |g: &mut Inner, id| {
                    g.reload(&cfg, id)?;
                    Ok((
                        ReloadResponse {
                            result: Some(CommandResult {
                                outcome: Outcome::Pending as i32,
                                ..CommandResult::default()
                            }),
                        },
                        id,
                    ))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn set_mission_file(
        &self,
        req: Request<SetMissionFileRequest>,
    ) -> Result<Response<SetMissionFileResponse>, Status> {
        use nanoprobe_sidecar::shell::content::{is_safe_name, is_type_name};
        use nanoprobe_sidecar::shell::media::{self, Media};
        require(&req, Access::Controller)?;
        let r = req.into_inner();
        let w = r.write.clone().unwrap_or_default();
        let mut hashed = r.clone();
        if let Some(h) = hashed.write.as_mut() {
            h.lease = None;
        }
        let kind = match MissionFileKind::try_from(r.kind) {
            Ok(MissionFileKind::Mission) => 1u8,
            Ok(MissionFileKind::Kneeboard) => 2,
            Ok(MissionFileKind::KneeboardType) => 3,
            _ => return Err(invalid("kind", "MISSION, KNEEBOARD or KNEEBOARD_TYPE")),
        };
        if kind == 3 && !is_type_name(&r.aircraft_type) {
            return Err(invalid(
                "aircraft_type",
                "1 to 64 characters of letters, digits, `_`, `-` and `.`",
            ));
        }
        if kind != 3 && !r.aircraft_type.is_empty() {
            return Err(invalid("aircraft_type", "given only for KNEEBOARD_TYPE"));
        }
        if !is_safe_name(&r.name) {
            return Err(invalid(
                "name",
                "a file name ending in .png, .jpg, .jpeg, .ogg or .wav, with no `/`, `\\`, `..` or NUL",
            ));
        }
        let media = Media::of_name(&r.name).expect("a safe name has a type");
        if kind != 1 && !media.is_image() {
            return Err(invalid("name", "a kneeboard page is a PNG or JPEG image"));
        }
        // Read and checked here, without the lock, as a template is; the
        // refusal is given inside the call, after the lease and campaign.
        let body = if r.path.is_empty() {
            None
        } else {
            let dirs = self.0.cfg.template_dirs.clone();
            let path = r.path.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    let b = crate::template_file::read_for(
                        "SetMissionFile",
                        "a mission file",
                        media::FILE_MAX,
                        &dirs,
                        &path,
                    )?;
                    media::check(media, &b)?;
                    Ok(b)
                })
                .await
                .unwrap_or_else(|_| Err(format!("{:?} could not be read", r.path)))
                .map_err(|e: String| invalid("path", e)),
            )
        };
        let key = nanoprobe_sidecar::store::MissionFileKey {
            kind,
            aircraft_type: r.aircraft_type.clone(),
            name: r.name.clone(),
        };
        let resp = self
            .0
            .write(
                Call {
                    method: "SetMissionFile",
                    key: w.idempotency.as_ref(),
                    lease: w.lease.as_ref(),
                    campaign: w.campaign_id,
                    need: Need::Campaign,
                },
                &hashed,
                |g: &mut Inner, id| {
                    let body = body.transpose()?;
                    let file = body.as_ref().map(|b| shown_file(&key, b));
                    g.set_mission_file(id, &key, body)?;
                    Ok((SetMissionFileResponse { file }, id))
                },
            )
            .await?;
        Ok(Response::new(resp))
    }

    async fn list_mission_files(
        &self,
        req: Request<ListMissionFilesRequest>,
    ) -> Result<Response<ListMissionFilesResponse>, Status> {
        require(&req, Access::Observer)?;
        let id = req.into_inner().campaign_id;
        let mut g = self.0.lock();
        let id = g.resolve_read(id)?;
        let files = g
            .mission_file_rows(id)?
            .iter()
            .map(|(k, b)| shown_file(k, b))
            .collect();
        Ok(Response::new(ListMissionFilesResponse { files }))
    }
}

/// The next mission a request names. A weather table must be a table.
pub(crate) fn next_of(n: Option<&NextMission>) -> Result<Next, Status> {
    let Some(n) = n else {
        return Err(invalid("next", "the next mission is required"));
    };
    #[allow(deprecated)]
    let weather = n.weather.as_ref().map(crate::lua::struct_to_lua);
    if let Some(w) = &weather {
        nanoprobe_sidecar::store::check_lv(w).map_err(|e| invalid("next.weather", e))?;
        nanoprobe_sidecar::shell::lua::write(w, 0).map_err(|e| invalid("next.weather", e))?;
    }
    if let Some(w) = &weather {
        nanoprobe_sidecar::shell::weather::check_whole(w)
            .map_err(|e| invalid("next.weather", e))?;
        if n.conditions.is_some() {
            return Err(invalid(
                "next.conditions",
                "the whole weather table and typed weather cannot both be set",
            ));
        }
    }
    if let Some(t) = &n.start {
        crate::convert::mission_date(t, "next.start")?;
    }
    let conditions = n.conditions.as_ref().map(conditions_of).transpose()?;
    let briefing = briefing_of(n.briefing.as_ref())?;
    let images = images_of(n.briefing_images.as_ref())?;
    Ok(Next {
        theater: n.theater.clone(),
        start_ms: n.start.as_ref().map(ms_of_timestamp),
        conditions,
        briefing,
        images,
        weather,
        dynamic: n
            .dynamic_spawn
            .iter()
            .map(|d| nanoprobe_sidecar::shell::DynamicSpawn {
                airbase: d.airbase_id,
                enabled: d.enabled,
                hot_start: d.hot_start,
            })
            .collect(),
    })
}

/// Typed weather from the API, each field checked alone; the checks across
/// fields run on the merged table when the shell is written.
fn conditions_of(
    w: &nanoprobe_proto::v1alpha1::Weather,
) -> Result<nanoprobe_sidecar::shell::weather::WeatherSet, Status> {
    use nanoprobe_proto::v1alpha1 as pb;
    use nanoprobe_sidecar::shell::weather::{Fog, WeatherSet, Wind};
    let f = |name: &str| format!("next.conditions.{name}");
    let wind = |v: Option<&pb::Wind>| {
        v.map(|v| Wind {
            speed_mps: v.speed_mps,
            toward_deg: v.toward_deg,
        })
    };
    let mut set = WeatherSet {
        temperature_c: w.temperature_c,
        qnh_mmhg: w.qnh_mmhg,
        visibility_m: w.visibility_m,
        wind: [
            wind(w.wind_ground.as_ref()),
            wind(w.wind_2000m.as_ref()),
            wind(w.wind_8000m.as_ref()),
        ],
        ground_turbulence_mps: w.ground_turbulence_mps,
        ..WeatherSet::default()
    };
    if let Some(c) = &w.clouds {
        set.cloud_preset = match pb::CloudPreset::try_from(c.preset) {
            Ok(pb::CloudPreset::Unspecified) => None,
            Ok(pb::CloudPreset::None) => Some(None),
            Ok(_) => Some(Some(cloud_preset_id(c.preset))),
            Err(_) => {
                return Err(invalid(
                    &f("clouds.preset"),
                    format!("unknown value {}", c.preset),
                ));
            }
        };
        set.cloud_base_m = c.base_m;
        set.cloud_thickness_m = c.thickness_m;
        set.cloud_density = c.density;
        set.precipitation = match pb::Precipitation::try_from(c.precipitation) {
            Ok(pb::Precipitation::Unspecified) => None,
            Ok(p) => Some(p as u32 - 1),
            Err(_) => {
                return Err(invalid(
                    &f("clouds.precipitation"),
                    format!("unknown value {}", c.precipitation),
                ));
            }
        };
    }
    if let Some(fog) = &w.fog {
        let mode = pb::FogMode::try_from(fog.mode)
            .map_err(|_| invalid(&f("fog.mode"), format!("unknown value {}", fog.mode)))?;
        if mode != pb::FogMode::Manual && !fog.manual.is_empty() {
            return Err(invalid(
                &f("fog.manual"),
                "keys are given only with mode MANUAL",
            ));
        }
        set.fog = match mode {
            pb::FogMode::Unspecified => None,
            pb::FogMode::Off => Some(Fog::Off),
            pb::FogMode::Auto => Some(Fog::Auto),
            pb::FogMode::Manual => {
                let mut keys = Vec::new();
                for k in &fog.manual {
                    let Some(t) = k.time_of_day else {
                        return Err(invalid(
                            &f("fog.manual.time_of_day"),
                            "a time of day is required for each key",
                        ));
                    };
                    if t.nanos != 0 || !(0..=86_400).contains(&t.seconds) {
                        return Err(invalid(
                            &f("fog.manual.time_of_day"),
                            "a whole number of seconds, 0 to 24 hours",
                        ));
                    }
                    keys.push((t.seconds as u32, k.visibility_m, k.thickness_m));
                }
                Some(Fog::Manual(keys))
            }
        };
    }
    if let Some(d) = &w.dust {
        set.dust = Some((d.enabled, d.visibility_m));
    }
    if let Some(h) = &w.halo {
        set.halo = match pb::HaloPreset::try_from(h.preset) {
            Ok(pb::HaloPreset::Unspecified) => None,
            Ok(p) => Some(HALO_PRESETS[p as usize - 1].to_owned()),
            Err(_) => {
                return Err(invalid(
                    &f("halo.preset"),
                    format!("unknown value {}", h.preset),
                ));
            }
        };
        set.halo_crystals = match pb::HaloCrystals::try_from(h.crystals) {
            Ok(pb::HaloCrystals::Unspecified) => None,
            Ok(c) => Some(HALO_CRYSTALS[c as usize - 1].to_owned()),
            Err(_) => {
                return Err(invalid(
                    &f("halo.crystals"),
                    format!("unknown value {}", h.crystals),
                ));
            }
        };
    }
    set.check().map_err(|e| invalid("next.conditions", e))?;
    Ok(set)
}

use nanoprobe_sidecar::shell::weather::{HALO_CRYSTALS, HALO_PRESETS};

/// The DCS id of a cloud preset value from 2 (`Preset1`) up.
fn cloud_preset_id(v: i32) -> String {
    match v {
        2..=28 => format!("Preset{}", v - 1),
        29..=34 => format!("RainyPreset{}", v - 28),
        _ => "NEWRAINPRESET4".to_owned(),
    }
}

fn briefing_of(
    b: Option<&nanoprobe_proto::v1alpha1::Briefing>,
) -> Result<nanoprobe_sidecar::shell::content::Briefing, Status> {
    use nanoprobe_sidecar::shell::content::{Briefing, TEXT_MAX};
    let Some(b) = b else {
        return Ok(Briefing::default());
    };
    let text = |name: &str, v: &Option<String>| -> Result<Option<Vec<u8>>, Status> {
        match v {
            Some(t) if t.len() > TEXT_MAX => Err(invalid(
                &format!("next.briefing.{name}"),
                format!("at most {TEXT_MAX} bytes"),
            )),
            Some(t) if t.contains('\0') => Err(invalid(
                &format!("next.briefing.{name}"),
                "the text holds NUL",
            )),
            other => Ok(other.as_ref().map(|t| t.as_bytes().to_vec())),
        }
    };
    Ok(Briefing {
        sortie: text("sortie", &b.sortie)?,
        description: text("description", &b.description)?,
        blue_task: text("blue_task", &b.blue_task)?,
        red_task: text("red_task", &b.red_task)?,
        neutral_task: text("neutral_task", &b.neutral_task)?,
    })
}

fn images_of(
    i: Option<&nanoprobe_proto::v1alpha1::BriefingImages>,
) -> Result<nanoprobe_sidecar::shell::content::Images, Status> {
    use nanoprobe_sidecar::shell::content::{IMAGES_MAX, is_safe_name};
    use nanoprobe_sidecar::shell::media::Media;
    let Some(i) = i else {
        return Ok(Default::default());
    };
    let list = |name: &str, l: Option<&nanoprobe_proto::v1alpha1::ImageList>| {
        let Some(l) = l else {
            return Ok(None);
        };
        let field = format!("next.briefing_images.{name}");
        if l.files.len() > IMAGES_MAX {
            return Err(invalid(&field, format!("at most {IMAGES_MAX} images")));
        }
        for f in &l.files {
            if !is_safe_name(f) || !Media::of_name(f).is_some_and(Media::is_image) {
                return Err(invalid(
                    &field,
                    format!("{f:?} is not the name of a PNG or JPEG file"),
                ));
            }
        }
        Ok(Some(l.files.clone()))
    };
    Ok([
        list("blue", i.blue.as_ref())?,
        list("red", i.red.as_ref())?,
        list("neutral", i.neutral.as_ref())?,
        list("coalition_selection", i.coalition_selection.as_ref())?,
    ])
}

/// A mission file as the API shows it.
fn shown_file(k: &nanoprobe_sidecar::store::MissionFileKey, body: &[u8]) -> MissionFile {
    use sha2::Digest;
    MissionFile {
        name: k.name.clone(),
        kind: match k.kind {
            2 => MissionFileKind::Kneeboard,
            3 => MissionFileKind::KneeboardType,
            _ => MissionFileKind::Mission,
        } as i32,
        aircraft_type: k.aircraft_type.clone(),
        size_bytes: body.len() as u64,
        sha256: sha2::Sha256::digest(body).to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_proto::v1alpha1 as pb;

    fn field_of(e: &Status) -> String {
        nanoprobe_proto::errors::detail(e).expect("detail").field
    }

    fn next_with(conditions: pb::Weather) -> pb::NextMission {
        pb::NextMission {
            conditions: Some(conditions),
            ..pb::NextMission::default()
        }
    }

    fn clouds(preset: i32) -> pb::Weather {
        pb::Weather {
            clouds: Some(pb::Clouds {
                preset,
                ..pb::Clouds::default()
            }),
            ..pb::Weather::default()
        }
    }

    /// Every cloud preset value maps to the DCS id in the table of
    /// spec/persistence.md, "Weather".
    #[test]
    fn every_cloud_preset_maps_to_its_dcs_id() {
        let table: Vec<String> = (1..=27)
            .map(|n| format!("Preset{n}"))
            .chain((1..=6).map(|n| format!("RainyPreset{n}")))
            .chain(["NEWRAINPRESET4".to_owned()])
            .collect();
        assert_eq!(table.len(), 34);
        for (i, want) in table.iter().enumerate() {
            let v = i as i32 + 2;
            assert!(pb::CloudPreset::try_from(v).is_ok(), "{v}");
            let set = conditions_of(&clouds(v)).expect("typed");
            assert_eq!(set.cloud_preset, Some(Some(want.clone())), "value {v}");
        }
        assert!(pb::CloudPreset::try_from(36).is_err());
        let e = conditions_of(&clouds(36)).expect_err("unknown");
        assert_eq!(field_of(&e), "next.conditions.clouds.preset");
        let set = conditions_of(&clouds(pb::CloudPreset::None as i32)).expect("none");
        assert_eq!(set.cloud_preset, Some(None));
        let set = conditions_of(&clouds(0)).expect("unspecified");
        assert_eq!(set.cloud_preset, None);
    }

    /// Every halo preset and crystal value maps to the DCS name of the
    /// table of spec/persistence.md, "Weather".
    #[test]
    fn every_halo_value_maps_to_its_dcs_name() {
        let presets = [
            "off",
            "auto",
            "AtmoHighClouds",
            "CirrusOnly",
            "HighClouds",
            "VolumetricOnly",
        ];
        let crystals = [
            "AllKinds",
            "BasicHaloCircle",
            "BasicHaloWithSundogs",
            "BasicSundogsTangents",
            "SundogsArcs",
            "Tangents",
        ];
        for (i, want) in presets.iter().enumerate() {
            let w = pb::Weather {
                halo: Some(pb::Halo {
                    preset: i as i32 + 1,
                    crystals: 0,
                }),
                ..pb::Weather::default()
            };
            let set = conditions_of(&w).expect("preset");
            assert_eq!(set.halo.as_deref(), Some(*want), "preset {}", i + 1);
        }
        for (i, want) in crystals.iter().enumerate() {
            let w = pb::Weather {
                halo: Some(pb::Halo {
                    preset: pb::HaloPreset::Auto as i32,
                    crystals: i as i32 + 1,
                }),
                ..pb::Weather::default()
            };
            let set = conditions_of(&w).expect("crystals");
            assert_eq!(
                set.halo_crystals.as_deref(),
                Some(*want),
                "crystals {}",
                i + 1
            );
        }
        let w = pb::Weather {
            halo: Some(pb::Halo {
                preset: 7,
                crystals: 0,
            }),
            ..pb::Weather::default()
        };
        let e = conditions_of(&w).expect_err("unknown");
        assert_eq!(field_of(&e), "next.conditions.halo.preset");
    }

    #[test]
    fn next_of_refuses_with_the_field_path() {
        let text = |t: &str| pb::NextMission {
            briefing: Some(pb::Briefing {
                sortie: Some(t.into()),
                ..pb::Briefing::default()
            }),
            ..pb::NextMission::default()
        };
        let images = |f: &[&str]| pb::NextMission {
            briefing_images: Some(pb::BriefingImages {
                coalition_selection: Some(pb::ImageList {
                    files: f.iter().map(|s| (*s).to_owned()).collect(),
                }),
                ..pb::BriefingImages::default()
            }),
            ..pb::NextMission::default()
        };
        let refused = |n: pb::NextMission| field_of(&next_of(Some(&n)).expect_err("refused"));

        // The whole table and typed weather together.
        let table = |k: &str| prost_types::Value {
            kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                fields: [(k.to_owned(), prost_types::Value::default())].into(),
            })),
        };
        #[allow(deprecated)]
        let both = pb::NextMission {
            weather: Some(prost_types::Struct {
                fields: [
                    ("clouds".to_owned(), table("a")),
                    ("season".to_owned(), table("b")),
                ]
                .into(),
            }),
            conditions: Some(pb::Weather::default()),
            ..pb::NextMission::default()
        };
        assert_eq!(refused(both), "next.conditions");

        assert_eq!(refused(text("a\0b")), "next.briefing.sortie");
        assert_eq!(refused(text(&"x".repeat(65_537))), "next.briefing.sortie");
        let names: Vec<String> = (0..17).map(|i| format!("p{i}.png")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            refused(images(&names)),
            "next.briefing_images.coalition_selection"
        );
        assert_eq!(
            refused(images(&["sound.ogg"])),
            "next.briefing_images.coalition_selection"
        );
        // 16 images are within the bound.
        assert!(next_of(Some(&images(&names[..16]))).is_ok());
        assert_eq!(
            refused(next_with(pb::Weather {
                fog: Some(pb::Fog {
                    mode: pb::FogMode::Manual as i32,
                    manual: vec![pb::FogKey::default()],
                }),
                ..pb::Weather::default()
            })),
            "next.conditions.fog.manual.time_of_day"
        );
        // A time is whole seconds.
        assert_eq!(
            refused(next_with(pb::Weather {
                fog: Some(pb::Fog {
                    mode: pb::FogMode::Manual as i32,
                    manual: vec![pb::FogKey {
                        time_of_day: Some(prost_types::Duration {
                            seconds: 3600,
                            nanos: 500_000_000,
                        }),
                        ..pb::FogKey::default()
                    }],
                }),
                ..pb::Weather::default()
            })),
            "next.conditions.fog.manual.time_of_day"
        );
        assert_eq!(
            refused(next_with(pb::Weather {
                fog: Some(pb::Fog {
                    mode: pb::FogMode::Auto as i32,
                    manual: vec![pb::FogKey::default()],
                }),
                ..pb::Weather::default()
            })),
            "next.conditions.fog.manual"
        );
    }
}
