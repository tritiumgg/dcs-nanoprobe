use super::lua::{self, Key};
use super::*;

const CAUCASUS: &[u8] = include_bytes!("../../../../tests/fixtures/templates/caucasus.miz");
const AFGHANISTAN: &[u8] = include_bytes!("../../../../tests/fixtures/templates/afghanistan.miz");

fn get<'a>(m: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
    member(m, name).unwrap()
}

/// The value at the root of a member, with `drop` keys removed from it.
fn without(src: &[u8], drop: &[&str]) -> Lv {
    let d = lua::parse(src).unwrap();
    let Lv::Table(pairs) = lua::value(src, &d.root).unwrap() else {
        panic!("not a table");
    };
    Lv::Table(
        pairs
            .into_iter()
            .filter(|(k, _)| !drop.iter().any(|d| *k == Lv::String(d.as_bytes().to_vec())))
            .collect(),
    )
}

fn at(src: &[u8], path: &[Key]) -> Lv {
    let d = lua::parse(src).unwrap();
    lua::value(src, d.root.at(path).unwrap()).unwrap()
}

fn weather() -> Lv {
    let s = |k: &str| Lv::String(k.as_bytes().to_vec());
    Lv::Table(vec![
        (s("atmosphere_type"), Lv::Number(0.0)),
        (
            s("wind"),
            Lv::Table(vec![(
                s("atGround"),
                Lv::Table(vec![
                    (s("speed"), Lv::Number(7.5)),
                    (s("dir"), Lv::Number(90.0)),
                ]),
            )]),
        ),
        (s("name"), s("np \"weather\"")),
        (
            s("clouds"),
            Lv::Table(vec![(s("density"), Lv::Number(0.0))]),
        ),
        (
            s("season"),
            Lv::Table(vec![(s("temperature"), Lv::Number(15.0))]),
        ),
    ])
}

#[test]
fn the_committed_templates_pass_the_check() {
    check(CAUCASUS, "Caucasus").unwrap();
    check(AFGHANISTAN, "Afghanistan").unwrap();
    assert!(
        check(CAUCASUS, "Afghanistan")
            .unwrap_err()
            .contains("theater")
    );
    assert_eq!(theatre(CAUCASUS).unwrap(), "Caucasus");
}

#[test]
fn no_change_keeps_every_member_byte_for_byte() {
    let out = write(CAUCASUS, &Runtime::default()).unwrap();
    let (a, b) = (members(CAUCASUS).unwrap(), members(&out).unwrap());
    assert_eq!(a, b);
}

#[test]
fn only_the_runtime_fields_change() {
    let rt = Runtime {
        start: Some(Start {
            year: 2031,
            month: 7,
            day: 4,
            seconds: 51_234,
        }),
        weather: Some(weather()),
        dynamic: vec![
            DynamicSpawn {
                airbase: 12,
                enabled: true,
                hot_start: true,
            },
            DynamicSpawn {
                airbase: 13,
                enabled: false,
                hot_start: false,
            },
        ],
        ..Runtime::default()
    };
    let out = write(CAUCASUS, &rt).unwrap();
    let (a, b) = (members(CAUCASUS).unwrap(), members(&out).unwrap());
    // Same members in the same order; only two of them change.
    assert_eq!(
        a.iter().map(|m| &m.0).collect::<Vec<_>>(),
        b.iter().map(|m| &m.0).collect::<Vec<_>>()
    );
    for ((n, x), (_, y)) in a.iter().zip(&b) {
        if n != "mission" && n != "warehouses" {
            assert_eq!(x, y, "{n}");
        }
    }
    let (ma, mb) = (get(&a, "mission"), get(&b, "mission"));
    let rest = ["date", "start_time", "weather"];
    assert_eq!(without(ma, &rest), without(mb, &rest));
    assert_eq!(at(mb, &[Key::s("start_time")]), Lv::Number(51_234.0));
    assert_eq!(
        at(mb, &[Key::s("date"), Key::s("Year")]),
        Lv::Number(2031.0)
    );
    assert_eq!(at(mb, &[Key::s("date"), Key::s("Month")]), Lv::Number(7.0));
    assert_eq!(at(mb, &[Key::s("date"), Key::s("Day")]), Lv::Number(4.0));
    assert_eq!(at(mb, &[Key::s("weather")]), weather());

    let (wa, wb) = (get(&a, "warehouses"), get(&b, "warehouses"));
    assert_eq!(without(wa, &["airports"]), without(wb, &["airports"]));
    let airports = |src: &[u8]| {
        let Lv::Table(p) = at(src, &[Key::s("airports")]) else {
            panic!()
        };
        p
    };
    let flags = ["dynamicSpawn", "allowHotStart", "unlimitedAircrafts"];
    for ((ka, va), (kb, vb)) in airports(wa).into_iter().zip(airports(wb)) {
        assert_eq!(ka, kb);
        let Lv::Number(id) = ka else { panic!() };
        let strip = |v: Lv| {
            let Lv::Table(p) = v else { panic!() };
            p.into_iter()
                .filter(|(k, _)| {
                    !flags
                        .iter()
                        .any(|f| *k == Lv::String(f.as_bytes().to_vec()))
                })
                .collect::<Vec<_>>()
        };
        if id == 12.0 || id == 13.0 {
            let want = Lv::Bool(id == 12.0);
            for f in &flags[..2] {
                assert_eq!(at(wb, &[Key::s("airports"), Key::Num(id), Key::s(f)]), want);
            }
            // Only the enabled airport's aircraft become limited; the
            // disabled one keeps the template's flag.
            let unlimited = [
                Key::s("airports"),
                Key::Num(id),
                Key::s("unlimitedAircrafts"),
            ];
            if id == 12.0 {
                assert_eq!(at(wb, &unlimited), Lv::Bool(false));
            } else {
                assert_eq!(at(wb, &unlimited), at(wa, &unlimited));
            }
            assert_eq!(strip(va), strip(vb));
        } else {
            assert_eq!(va, vb, "airport {id}");
        }
    }
}

#[test]
fn a_missing_flag_is_inserted() {
    let src = b"warehouses = \n{\n\t[\"airports\"] = \n\t{\n\t\t[7] = \n\t\t{\n\t\t\t[\"size\"] = 100,\n\t\t},\n\t},\n}\n";
    let out = edit_warehouses(
        src,
        &[DynamicSpawn {
            airbase: 7,
            enabled: true,
            hot_start: false,
        }],
    )
    .unwrap();
    let path = |k: &str| [Key::s("airports"), Key::Num(7.0), Key::s(k)];
    assert_eq!(at(&out, &path("dynamicSpawn")), Lv::Bool(true));
    assert_eq!(at(&out, &path("allowHotStart")), Lv::Bool(false));
    assert_eq!(at(&out, &path("unlimitedAircrafts")), Lv::Bool(false));
    assert_eq!(at(&out, &path("size")), Lv::Number(100.0));
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("\n\t\t\t[\"dynamicSpawn\"] = true,")
    );
}

#[test]
fn refuses_what_it_cannot_write() {
    let one = |airbase| DynamicSpawn {
        airbase,
        enabled: true,
        hot_start: false,
    };
    let rt = |dynamic| Runtime {
        dynamic,
        ..Runtime::default()
    };
    assert!(
        write(CAUCASUS, &rt(vec![one(99_999)]))
            .unwrap_err()
            .contains("no airport")
    );
    assert!(
        write(CAUCASUS, &rt(vec![one(12), one(12)]))
            .unwrap_err()
            .contains("twice")
    );
    let bad_start = Runtime {
        start: Some(Start {
            year: 2000,
            month: 13,
            day: 1,
            seconds: 0,
        }),
        ..Runtime::default()
    };
    assert!(write(CAUCASUS, &bad_start).is_err());
    let bad_weather = Runtime {
        weather: Some(Lv::Number(1.0)),
        ..Runtime::default()
    };
    assert!(write(CAUCASUS, &bad_weather).is_err());
    assert!(
        write(b"not a zip", &Runtime::default())
            .unwrap_err()
            .contains(".miz")
    );
    // A template without `warehouses`.
    let mut w = ZipWriter::new(Cursor::new(Vec::new()));
    for (n, d) in members(CAUCASUS).unwrap() {
        if n != "warehouses" {
            w.start_file(n.as_str(), SimpleFileOptions::default())
                .unwrap();
            w.write_all(&d).unwrap();
        }
    }
    let partial = w.finish().unwrap().into_inner();
    assert!(
        check(&partial, "Caucasus")
            .unwrap_err()
            .contains("warehouses")
    );
}

#[test]
fn start_converts_both_ways() {
    for ms in [
        0i64,
        1_700_000_000_123 / 1000 * 1000,
        -86_400_000,
        951_782_400_000,
    ] {
        let s = Start::from_unix_ms(ms);
        assert_eq!(s.unix_ms(), ms.div_euclid(1000) * 1000, "{s:?}");
    }
    // 2000-02-29 is a leap day.
    let s = Start::from_unix_ms(951_782_400_000);
    assert_eq!((s.year, s.month, s.day, s.seconds), (2000, 2, 29, 0));
}

#[test]
fn save_writes_only_the_shell_inside_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let p = save(dir.path(), 42, b'a', b"x").unwrap();
    assert_eq!(p, dir.path().join("shells").join("42.a.miz"));
    assert_eq!(std::fs::read(&p).unwrap(), b"x");
    save(dir.path(), 42, b'a', b"yz").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"yz");
    std::fs::write(dir.path().join("shells").join(".42.b.miz.tmp"), b"cut").unwrap();
    clear_temps(dir.path());
    let names: Vec<_> = std::fs::read_dir(dir.path().join("shells"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("42.a.miz")]);
    assert_eq!(slot_of(42, "Z:\\m\\shells\\42.b.miz"), Some(b'b'));
    assert_eq!(slot_of(42, "Z:\\m\\shells\\142.b.miz"), None);
    assert_eq!(other_slot(Some(b'a')), b'b');
    assert_eq!(other_slot(None), b'a');
    assert!(airports(CAUCASUS).unwrap().contains(&12));
}

#[test]
fn the_start_reads_back_from_a_shell() {
    let tpl = start_of(CAUCASUS).unwrap();
    assert_eq!(tpl.seconds, 28_800);
    let want = Start {
        year: 2031,
        month: 7,
        day: 4,
        seconds: 51_234,
    };
    let out = write(
        CAUCASUS,
        &Runtime {
            start: Some(want),
            ..Runtime::default()
        },
    )
    .unwrap();
    assert_eq!(start_of(&out).unwrap(), want);
}

#[test]
fn placed_names_are_read_from_the_coalition_table() {
    // The committed templates place nothing: only the coalitions' own names.
    assert_eq!(placed_names(CAUCASUS).unwrap(), ["neutrals", "blue", "red"]);
    let src = b"mission = { [\"coalition\"] = { [\"blue\"] = { [\"country\"] = { [1] = { [\"name\"] = \"USA\", [\"vehicle\"] = { [\"group\"] = { [1] = { [\"name\"] = \"np!abc1\", [\"units\"] = { [1] = { [\"name\"] = \"u1\" } } } } } } } } } }";
    let d = lua::parse(src).unwrap();
    let mut out = Vec::new();
    collect_names(src, d.root.get(&Key::s("coalition")).unwrap(), &mut out);
    assert_eq!(out, ["USA", "np!abc1", "u1"]);
}

#[test]
fn the_ends_of_i64_do_not_overflow_a_start() {
    let _ = Start::from_unix_ms(i64::MIN).unix_ms();
    let _ = Start::from_unix_ms(i64::MAX).unix_ms();
}

#[test]
fn an_enabled_airport_gets_limited_aircraft() {
    let src = b"warehouses = \n{\n\t[\"airports\"] = \n\t{\n\t\t[7] = \n\t\t{\n\t\t\t[\"unlimitedAircrafts\"] = true,\n\t\t\t[\"unlimitedMunitions\"] = true,\n\t\t},\n\t\t[8] = \n\t\t{\n\t\t\t[\"unlimitedAircrafts\"] = true,\n\t\t},\n\t},\n}\n";
    let flags = |airbase, enabled| DynamicSpawn {
        airbase,
        enabled,
        hot_start: false,
    };
    let out = edit_warehouses(src, &[flags(7, true), flags(8, false)]).unwrap();
    let path = |id: f64, k: &str| [Key::s("airports"), Key::Num(id), Key::s(k)];
    assert_eq!(at(&out, &path(7.0, "unlimitedAircrafts")), Lv::Bool(false));
    assert_eq!(at(&out, &path(7.0, "unlimitedMunitions")), Lv::Bool(true));
    assert_eq!(at(&out, &path(8.0, "unlimitedAircrafts")), Lv::Bool(true));
    assert_eq!(at(&out, &path(8.0, "dynamicSpawn")), Lv::Bool(false));
    // The enabled airport gets an empty aircraft table; the disabled one
    // gets none.
    assert_eq!(at(&out, &path(7.0, "aircrafts")), Lv::Table(vec![]));
    assert!(
        lua::parse(&out)
            .unwrap()
            .root
            .at(&path(8.0, "aircrafts"))
            .is_none()
    );
    // An existing aircraft table is kept.
    let kept = b"warehouses = \n{\n\t[\"airports\"] = \n\t{\n\t\t[7] = \n\t\t{\n\t\t\t[\"aircrafts\"] = \n\t\t\t{\n\t\t\t\t[\"helicopters\"] = 1,\n\t\t\t},\n\t\t},\n\t},\n}\n";
    let out = edit_warehouses(kept, &[flags(7, true)]).unwrap();
    assert_eq!(
        at(
            &out,
            &[
                Key::s("airports"),
                Key::Num(7.0),
                Key::s("aircrafts"),
                Key::s("helicopters")
            ]
        ),
        Lv::Number(1.0)
    );
}

/// A template from `CAUCASUS` with members added: a Russian dictionary that
/// holds the sortie key, a sound the triggers would play, the same sound in
/// the Russian folder, an image with a key and one without, and a kneeboard
/// page.
fn rich_template() -> Vec<u8> {
    let mut m = members(CAUCASUS).unwrap();
    let ru =
        b"dictionary = \n{\n\t[\"DictKey_sortie_5\"] = \"\\208\\159\",\n} -- end of dictionary\n";
    m.push(("l10n/RU/dictionary".into(), ru.to_vec()));
    m.push((
        "l10n/DEFAULT/alarm.ogg".into(),
        media::tests::ogg_file(1, 22_050),
    ));
    m.push((
        "l10n/RU/alarm.ogg".into(),
        media::tests::ogg_file(1, 22_050),
    ));
    m.push(("l10n/DEFAULT/map.png".into(), media::tests::png_file(2, 2)));
    m.push((
        "l10n/DEFAULT/loose.jpg".into(),
        media::tests::jpeg_file(2, 2),
    ));
    m.push((
        "KNEEBOARD/IMAGES/page.png".into(),
        media::tests::png_file(3, 3),
    ));
    for (n, d) in &mut m {
        if n == "l10n/DEFAULT/mapResource" {
            *d = b"mapResource = \n{\n\t[\"ResKey_Action_7\"] = \"alarm.ogg\",\n\t[\"ResKey_ImageBriefing_8\"] = \"map.png\",\n} -- end of mapResource\n".to_vec();
        }
    }
    zip_of(&m)
}

fn zip_of(m: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut w = ZipWriter::new(Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (n, d) in m {
        w.start_file(n.as_str(), o).unwrap();
        w.write_all(d).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn file(kind: content::FileKind, name: &str, body: Vec<u8>) -> content::File {
    content::File {
        kind,
        name: name.into(),
        body,
    }
}

/// The runtime of the golden shell: typed weather, every briefing text,
/// images for two coalitions, and files of each kind.
fn rich_runtime() -> Runtime {
    use content::FileKind;
    Runtime {
        conditions: Some(weather::WeatherSet {
            temperature_c: Some(-4.0),
            qnh_mmhg: Some(748.0),
            wind: [
                Some(weather::Wind {
                    speed_mps: 6.0,
                    toward_deg: 240,
                }),
                None,
                None,
            ],
            cloud_preset: Some(None),
            cloud_base_m: Some(900.0),
            cloud_thickness_m: Some(800.0),
            cloud_density: Some(9),
            precipitation: Some(3),
            fog: Some(weather::Fog::Manual(vec![
                (0, 2000.0, 150.0),
                (7200, 9000.0, 50.0),
            ])),
            ..weather::WeatherSet::default()
        }),
        briefing: content::Briefing {
            sortie: Some("Operation \"Ice\"".into()),
            description: Some("Line 1\nLine 2: Привет".into()),
            blue_task: Some("Hold the pass.".into()),
            red_task: Some(Vec::new()),
            neutral_task: None,
        },
        images: [
            Some(vec![
                "map.png".into(),
                "intel.png".into(),
                "loose.jpg".into(),
            ]),
            Some(Vec::new()),
            None,
            Some(vec!["intel.png".into()]),
        ],
        files: vec![
            file(
                FileKind::Mission,
                "alarm.ogg",
                media::tests::ogg_file(2, 44_100),
            ),
            file(FileKind::Mission, "intel.png", media::tests::png_file(4, 4)),
            file(FileKind::Mission, "call.wav", media::tests::wav_file(1, 16)),
            file(
                FileKind::Kneeboard,
                "page.png",
                media::tests::png_file(5, 5),
            ),
            file(
                FileKind::KneeboardType("UH-1H".into()),
                "freqs.jpg",
                media::tests::jpeg_file(6, 6),
            ),
        ],
        ..Runtime::default()
    }
}

fn dict(src: &[u8], key: &str) -> Option<Lv> {
    let d = lua::parse(src).unwrap();
    d.root.get(&Key::s(key)).and_then(|n| lua::value(src, n))
}

fn text(v: &str) -> Lv {
    Lv::String(v.as_bytes().to_vec())
}

#[test]
fn briefing_text_goes_into_every_dictionary_under_the_mission_keys() {
    let out = write(&rich_template(), &rich_runtime()).unwrap();
    let m = members(&out).unwrap();
    let mission = get(&m, "mission");
    // The template's keys are kept; the sortie text is in both locales.
    assert_eq!(at(mission, &[Key::s("sortie")]), text("DictKey_sortie_5"));
    for loc in ["DEFAULT", "RU"] {
        let d = get(&m, &format!("l10n/{loc}/dictionary"));
        assert_eq!(
            dict(d, "DictKey_sortie_5"),
            Some(text("Operation \"Ice\"")),
            "{loc}"
        );
        assert_eq!(
            dict(d, "DictKey_descriptionText_1"),
            Some(text("Line 1\nLine 2: Привет")),
            "{loc}"
        );
        assert_eq!(
            dict(d, "DictKey_descriptionRedTask_2"),
            Some(text("")),
            "{loc}"
        );
    }
    // The neutral task is kept.
    assert_eq!(
        dict(
            get(&m, "l10n/DEFAULT/dictionary"),
            "DictKey_descriptionNeutralsTask_4"
        ),
        Some(text(""))
    );
}

#[test]
fn a_plain_string_field_gets_a_new_key_above_max_dict_id() {
    let mut m = members(CAUCASUS).unwrap();
    for (n, d) in &mut m {
        if n == "mission" {
            let s = String::from_utf8(d.clone()).unwrap();
            *d = s.replace("\"DictKey_sortie_5\"", "\"plain\"").into_bytes();
        }
    }
    let rt = Runtime {
        briefing: content::Briefing {
            sortie: Some("New".into()),
            ..content::Briefing::default()
        },
        ..Runtime::default()
    };
    let out = write(&zip_of(&m), &rt).unwrap();
    let m = members(&out).unwrap();
    let mission = get(&m, "mission");
    assert_eq!(at(mission, &[Key::s("sortie")]), text("DictKey_sortie_6"));
    assert_eq!(at(mission, &[Key::s("maxDictId")]), Lv::Number(6.0));
    assert_eq!(
        dict(get(&m, "l10n/DEFAULT/dictionary"), "DictKey_sortie_6"),
        Some(text("New"))
    );
}

#[test]
fn files_replace_the_templates_by_name_or_are_added_with_new_keys() {
    let out = write(&rich_template(), &rich_runtime()).unwrap();
    let m = members(&out).unwrap();
    let new_ogg = media::tests::ogg_file(2, 44_100);
    assert_eq!(get(&m, "l10n/DEFAULT/alarm.ogg"), &new_ogg[..]);
    assert_eq!(get(&m, "l10n/RU/alarm.ogg"), &new_ogg[..]);
    assert_eq!(
        get(&m, "KNEEBOARD/IMAGES/page.png"),
        &media::tests::png_file(5, 5)[..]
    );
    assert_eq!(
        get(&m, "KNEEBOARD/UH-1H/IMAGES/freqs.jpg"),
        &media::tests::jpeg_file(6, 6)[..]
    );
    let map = get(&m, "l10n/DEFAULT/mapResource");
    // The template's keys stay; the new files and the unkeyed image get keys
    // above maxDictId (5), which is raised.
    assert_eq!(dict(map, "ResKey_Action_7"), Some(text("alarm.ogg")));
    assert_eq!(dict(map, "ResKey_np_6"), Some(text("intel.png")));
    assert_eq!(dict(map, "ResKey_np_7"), Some(text("call.wav")));
    assert_eq!(dict(map, "ResKey_np_8"), Some(text("loose.jpg")));
    let mission = get(&m, "mission");
    assert_eq!(at(mission, &[Key::s("maxDictId")]), Lv::Number(8.0));
    assert_eq!(
        at(mission, &[Key::s("pictureFileNameB")]),
        Lv::Table(vec![
            (Lv::Number(1.0), text("ResKey_ImageBriefing_8")),
            (Lv::Number(2.0), text("ResKey_np_6")),
            (Lv::Number(3.0), text("ResKey_np_8")),
        ])
    );
    assert_eq!(
        at(mission, &[Key::s("pictureFileNameR")]),
        Lv::Table(Vec::new())
    );
    assert_eq!(
        at(mission, &[Key::s("pictureFileNameN")]),
        Lv::Table(Vec::new())
    );
    // The selection screen's image shares the key of the same file.
    assert_eq!(
        at(mission, &[Key::s("pictureFileNameServer")]),
        Lv::Table(vec![(Lv::Number(1.0), text("ResKey_np_6"))])
    );
}

#[test]
fn a_shell_reads_back_through_the_writer() {
    // The .miz round trip: a written shell is a template the writer takes,
    // and writing it again with the same runtime changes nothing.
    let out = write(&rich_template(), &rich_runtime()).unwrap();
    check(&out, "Caucasus").unwrap();
    let again = write(&out, &rich_runtime()).unwrap();
    let (a, b) = (members(&out).unwrap(), members(&again).unwrap());
    let names = |m: &[(String, Vec<u8>)]| m.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
    assert_eq!(names(&a), names(&b));
    for ((n, x), (_, y)) in a.iter().zip(&b) {
        if n == "mission" {
            let rest = ["weather"];
            assert_eq!(without(x, &rest), without(y, &rest));
            assert_eq!(at(x, &[Key::s("weather")]), at(y, &[Key::s("weather")]));
        } else {
            assert_eq!(x, y, "{n}");
        }
    }
}

#[test]
fn content_refusals_name_the_problem() {
    use content::FileKind;
    let cases: Vec<(Runtime, &str)> = vec![
        (
            Runtime {
                images: [None, None, None, Some(vec!["nowhere.png".into()])],
                ..Runtime::default()
            },
            "neither a campaign file nor in the template",
        ),
        (
            Runtime {
                images: [Some(vec!["alarm.ogg".into()]), None, None, None],
                ..Runtime::default()
            },
            "not an image",
        ),
        (
            Runtime {
                files: vec![file(
                    FileKind::Mission,
                    "ALARM.ogg",
                    media::tests::ogg_file(1, 8000),
                )],
                ..Runtime::default()
            },
            "differs only in case",
        ),
        (
            Runtime {
                files: vec![file(
                    FileKind::Kneeboard,
                    "x.ogg",
                    media::tests::ogg_file(1, 8000),
                )],
                ..Runtime::default()
            },
            "a kneeboard page is an image",
        ),
        (
            Runtime {
                files: vec![file(
                    FileKind::Mission,
                    "Odd name.ogg",
                    media::tests::ogg_file(1, 8000),
                )],
                ..Runtime::default()
            },
            "not a file name",
        ),
        (
            Runtime {
                weather: Some(weather()),
                conditions: Some(weather::WeatherSet {
                    qnh_mmhg: Some(760.0),
                    ..weather::WeatherSet::default()
                }),
                ..Runtime::default()
            },
            "both set",
        ),
        (
            Runtime {
                weather: Some(Lv::Table(vec![(text("qnh"), Lv::Number(1.0))])),
                ..Runtime::default()
            },
            "no clouds table",
        ),
        (
            Runtime {
                conditions: Some(weather::WeatherSet {
                    cloud_base_m: Some(100.0),
                    ..weather::WeatherSet::default()
                }),
                ..Runtime::default()
            },
            "cloud base",
        ),
    ];
    for (rt, want) in cases {
        let e = write(&rich_template(), &rt).expect_err(want);
        assert!(e.contains(want), "{want}: {e}");
    }
}

/// The golden shell (spec/shell.golden): each member [`rich_runtime`]
/// changes or adds, Lua members as text and other files by size and
/// SHA-256 prefix. Set `NANOPROBE_BLESS=1` to write it after an intended
/// change.
#[test]
fn the_rich_shell_matches_the_golden_file() {
    const GOLDEN: &str = include_str!("../../../../spec/shell.golden");
    let template = members(&rich_template()).unwrap();
    let out = members(&write(&rich_template(), &rich_runtime()).unwrap()).unwrap();
    let mut lines = String::new();
    for (name, data) in &out {
        if template.iter().any(|(n, d)| n == name && d == data) {
            continue;
        }
        lines.push_str(&format!("== {name}\n"));
        if media::Media::of_name(name).is_some() {
            use sha2::Digest;
            let h = sha2::Sha256::digest(data);
            let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
            lines.push_str(&format!("{} bytes sha256 {hex}\n", data.len()));
        } else {
            lines.push_str(&String::from_utf8_lossy(data));
            if !lines.ends_with('\n') {
                lines.push('\n');
            }
        }
    }
    if std::env::var_os("NANOPROBE_BLESS").is_some() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/shell.golden");
        std::fs::write(path, &lines).expect("write the golden file");
        return;
    }
    assert_eq!(
        GOLDEN, lines,
        "the shell changed; review and bless spec/shell.golden"
    );
}
