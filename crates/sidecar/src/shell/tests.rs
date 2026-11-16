use super::lua::{self, Key};
use super::*;

const CAUCASUS: &[u8] = include_bytes!("../../../../templates/caucasus.miz");
const AFGHANISTAN: &[u8] = include_bytes!("../../../../templates/afghanistan.miz");

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
    let flags = ["dynamicSpawn", "allowHotStart"];
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
            for f in flags {
                assert_eq!(at(wb, &[Key::s("airports"), Key::Num(id), Key::s(f)]), want);
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
    let p = save(dir.path(), 42, b"x").unwrap();
    assert_eq!(p, dir.path().join("shells").join("42.miz"));
    assert_eq!(std::fs::read(&p).unwrap(), b"x");
    save(dir.path(), 42, b"yz").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"yz");
    let names: Vec<_> = std::fs::read_dir(dir.path().join("shells"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("42.miz")]);
}
