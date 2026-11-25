//! Fixtures are written fresh in the form of the files Eagle Dynamics
//! ships (docs/results/2026-11-24-payload-presets.md); the CLSIDs are made
//! up.

use super::*;
use proptest::prelude::*;

pub(crate) const FILE: &str = r#"local unitPayloads = {
	["name"] = "Test-Jet",
	["payloads"] = {
		[1] = {
			["displayName"] = "Shown name",
			["name"] = "Bombs*2, Fuel",
			["pylons"] = {
				[1] = {
					["CLSID"] = "{TEST-BOMB}",
					["num"] = 7,
					["settings"] = {
						["GUI_fuze_type"] = 1,
						["arm_delay"] = 0.5,
						["mode"] = "nose",
					},
				},
				[2] = {
					["CLSID"] = "{TEST-BOMB}",
					["num"] = 1,
				},
				[3] = {
					["CLSID"] = "{TEST-TANK}",
					["num"] = 4,
				},
			},
			["tasks"] = {
				[1] = 32,
			},
		},
		[2] = {
			["name"] = "Clean",
			["pylons"] = {
			},
			["tasks"] = {
			},
		},
		[3] = {
			["name"] = "Twice",
			["pylons"] = {
			},
		},
		[4] = {
			["name"] = "Twice",
			["pylons"] = {
			},
		},
	},
	["unitType"] = "Test-Jet",
}
return unitPayloads
"#;

fn s(v: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(v.into())),
    }
}

fn n(v: f64) -> Value {
    Value {
        kind: Some(Kind::NumberValue(v)),
    }
}

fn obj(pairs: &[(&str, Value)]) -> Value {
    Value {
        kind: Some(Kind::StructValue(Struct {
            fields: pairs
                .iter()
                .map(|(k, v)| ((*k).into(), v.clone()))
                .collect(),
        })),
    }
}

/// The pylons of `FILE`'s first preset, written by hand.
pub(crate) fn by_hand() -> Struct {
    let Some(Kind::StructValue(st)) = obj(&[
        (
            "7",
            obj(&[
                ("CLSID", s("{TEST-BOMB}")),
                (
                    "settings",
                    obj(&[
                        ("GUI_fuze_type", n(1.0)),
                        ("arm_delay", n(0.5)),
                        ("mode", s("nose")),
                    ]),
                ),
            ]),
        ),
        ("1", obj(&[("CLSID", s("{TEST-BOMB}"))])),
        ("4", obj(&[("CLSID", s("{TEST-TANK}"))])),
    ])
    .kind
    else {
        unreachable!()
    };
    st
}

fn file_of(src: &str) -> Result<File, Refusal> {
    read(src.as_bytes().to_vec())
}

fn one(pylon: &str) -> String {
    format!(
        "local p = {{ [\"name\"] = \"T\", [\"payloads\"] = {{ {{ [\"name\"] = \"x\", [\"pylons\"] = {{ {pylon} }} }} }} }}\nreturn p\n"
    )
}

#[test]
fn reads_a_preset_into_pylons_by_number() {
    let f = file_of(FILE).unwrap();
    assert_eq!(f.unit_type, "Test-Jet");
    let p = f.pylons("Test-Jet", "Bombs*2, Fuel").unwrap();
    assert_eq!(p.pylons, by_hand());
    assert_eq!(p.dropped, vec!["displayName", "tasks"]);
}

#[test]
fn an_empty_preset_is_a_clean_loadout() {
    let p = file_of(FILE).unwrap().pylons("Test-Jet", "Clean").unwrap();
    assert!(p.pylons.fields.is_empty());
}

#[test]
fn unit_type_comes_before_name_and_name_stands_in() {
    let both =
        "local p = { [\"name\"] = \"A\", [\"unitType\"] = \"B\", [\"payloads\"] = {} }\nreturn p";
    assert_eq!(file_of(both).unwrap().unit_type, "B");
    let name = "local p = { [\"name\"] = \"A\", [\"payloads\"] = {} }\nreturn p";
    assert_eq!(file_of(name).unwrap().unit_type, "A");
}

#[test]
fn refuses_a_preset_it_cannot_find_or_tell_apart() {
    let f = file_of(FILE).unwrap();
    let e = f.pylons("Test-Jet", "Shown name").unwrap_err();
    assert_eq!(
        e,
        Refusal {
            field: "name",
            text: "the file has no preset named \"Shown name\"".into()
        }
    );
    let e = f.pylons("Test-Jet", "Twice").unwrap_err();
    assert_eq!(e.field, "name");
    assert!(
        e.text.contains("more than one preset named \"Twice\""),
        "{e:?}"
    );
    assert_eq!(f.pylons("Test-Jet", "").unwrap_err().field, "name");
}

#[test]
fn refuses_another_type_naming_both_exactly() {
    let f = file_of(FILE).unwrap();
    let e = f.pylons("test-jet", "Clean").unwrap_err();
    assert_eq!(
        e,
        Refusal {
            field: "path",
            text:
                "the file's presets are for the type \"Test-Jet\"; the unit's type is \"test-jet\""
                    .into()
        }
    );
}

#[test]
fn refuses_a_file_with_code_whole_naming_the_line() {
    let src = "local tank = \"{TEST-TANK}\"\nlocal unitPayloads = {}\nreturn unitPayloads\n";
    let e = file_of(src).unwrap_err();
    assert_eq!(e.field, "path");
    assert!(e.text.contains("line 2: expected `return tank`"), "{e:?}");
    assert!(
        e.text.contains("save the preset from the Mission Editor"),
        "{e:?}"
    );
    let concat = FILE.replace("\"{TEST-TANK}\"", "\"{TEST\" .. \"-TANK}\"");
    let e = file_of(&concat).unwrap_err();
    assert!(e.text.contains("line 22:"), "{e:?}");
}

#[test]
fn refuses_files_that_are_not_presets() {
    for (src, want) in [
        ("unitPayloads = {}\n", "line 1"),
        ("local p = 1\nreturn p", "no `payloads` table"),
        (
            "local p = { [\"name\"] = \"A\" }\nreturn p",
            "no `payloads` table",
        ),
        (
            "local p = { [\"payloads\"] = {} }\nreturn p",
            "no string `unitType` or `name`",
        ),
        (
            "local p = { [\"unitType\"] = 3, [\"name\"] = \"A\", [\"payloads\"] = {} }\nreturn p",
            "no string `unitType` or `name`",
        ),
    ] {
        let e = file_of(src).unwrap_err();
        assert_eq!(e.field, "path", "{src}");
        assert!(e.text.contains(want), "{src}: {e:?}");
    }
    let mut big = b"local p = {".to_vec();
    big.resize(FILE_MAX + 1, b' ');
    assert!(read(big).unwrap_err().text.contains("over"));
}

/// `FILE`'s first preset with `extra` pylon entries put between its own,
/// in the place shipped files have them (`Tornado GR4.lua`,
/// `MosquitoFBMkVI.lua`).
fn with_empty(extra: &str) -> String {
    FILE.replacen(
        "\t\t\t\t[2] = {\n\t\t\t\t\t[\"CLSID\"] = \"{TEST-BOMB}\",",
        &format!("{extra}\t\t\t\t[2] = {{\n\t\t\t\t\t[\"CLSID\"] = \"{{TEST-BOMB}}\","),
        1,
    )
}

#[test]
fn an_empty_clsid_leaves_its_station_empty() {
    // Tornado-shaped: two empty stations, no settings. Mosquito-shaped: an
    // empty station with fuze settings.
    for (extra, empty) in [
        (
            "\t\t\t\t[8] = {\n\t\t\t\t\t[\"CLSID\"] = \"\",\n\t\t\t\t\t[\"num\"] = 8,\n\t\t\t\t},\n\
             \t\t\t\t[9] = {\n\t\t\t\t\t[\"CLSID\"] = \"\",\n\t\t\t\t\t[\"num\"] = 5,\n\t\t\t\t},\n",
            vec![8, 5],
        ),
        (
            "\t\t\t\t[8] = {\n\t\t\t\t\t[\"CLSID\"] = \"\",\n\t\t\t\t\t[\"num\"] = 2,\n\
             \t\t\t\t\t[\"settings\"] = {\n\t\t\t\t\t\t[\"GUI_fuze_type\"] = 1,\n\
             \t\t\t\t\t\t[\"arm_delay_ctrl\"] = 0.64,\n\t\t\t\t\t},\n\t\t\t\t},\n",
            vec![2],
        ),
    ] {
        let src = with_empty(extra);
        assert!(src.contains("[\"CLSID\"] = \"\""), "{src}");
        let p = file_of(&src)
            .unwrap()
            .pylons("Test-Jet", "Bombs*2, Fuel")
            .unwrap();
        // The preset without the empty entries, byte for byte as the API
        // carries it.
        assert_eq!(p.pylons, by_hand());
        assert_eq!(p.dropped, vec!["displayName", "tasks"]);
        let nums: Vec<String> = empty.iter().map(u32::to_string).collect();
        assert_eq!(
            p.note(),
            format!(
                " (keys not used: displayName, tasks) (pylons left empty, CLSID \"\": {})",
                nums.join(", ")
            )
        );
        assert_eq!(p.empty, empty);
    }
    let p = file_of(FILE)
        .unwrap()
        .pylons("Test-Jet", "Bombs*2, Fuel")
        .unwrap();
    assert!(p.empty.is_empty());
    // A preset of empty stations only is a clean loadout.
    let p = file_of(&one("{ [\"CLSID\"] = \"\", [\"num\"] = 3 }"))
        .unwrap()
        .pylons("T", "x")
        .unwrap();
    assert!(p.pylons.fields.is_empty());
    assert_eq!(p.empty, vec![3]);
    assert_eq!(p.note(), " (pylons left empty, CLSID \"\": 3)");
    // A key the entry does not use is named, as for any pylon.
    let p = file_of(&one("{ [\"CLSID\"] = \"\", [\"num\"] = 3, [\"odd\"] = 1 }"))
        .unwrap()
        .pylons("T", "x")
        .unwrap();
    assert_eq!(p.dropped, vec!["pylons[].odd"]);
    // Two empty entries for one pylon are two entries, as for any pylon.
    let e = file_of(&one(
        "{ [\"CLSID\"] = \"\", [\"num\"] = 3 }, { [\"CLSID\"] = \"\", [\"num\"] = 3 }",
    ))
    .unwrap()
    .pylons("T", "x")
    .unwrap_err();
    assert!(e.text.contains("two entries for pylon 3"), "{e:?}");
}

#[test]
fn bare_task_names_read_and_are_dropped() {
    // As `A6E.lua` has them, and positional as well.
    for tasks in ["{ [1] = CAS, [2] = GroundAttack }", "{ CAS, 31 }"] {
        let src = FILE.replacen(
            "[\"tasks\"] = {\n\t\t\t\t[1] = 32,\n\t\t\t}",
            &format!("[\"tasks\"] = {tasks}"),
            1,
        );
        assert!(src.contains(tasks), "{src}");
        let p = file_of(&src)
            .unwrap()
            .pylons("Test-Jet", "Bombs*2, Fuel")
            .unwrap();
        assert_eq!(p.pylons, by_hand());
        assert_eq!(p.dropped, vec!["displayName", "tasks"]);
    }
    // Anywhere else a bare name refuses the file.
    let src = FILE.replacen("[\"name\"] = \"Clean\"", "[\"name\"] = Clean", 1);
    assert!(file_of(&src).is_err());
}

#[test]
fn refuses_pylons_it_cannot_carry() {
    for (pylon, want) in [
        ("{ [\"num\"] = 1 }", "no string `CLSID`"),
        (
            "{ [\"CLSID\"] = \" \", [\"num\"] = 1 }",
            "no string `CLSID`",
        ),
        (
            "{ [\"CLSID\"] = \"\\t\", [\"num\"] = 1 }",
            "no string `CLSID`",
        ),
        ("{ [\"CLSID\"] = 1, [\"num\"] = 1 }", "no string `CLSID`"),
        ("{ [\"CLSID\"] = \"\" }", "no `num`"),
        (
            "{ [\"CLSID\"] = \"\", [\"num\"] = 2 }, { [\"CLSID\"] = \"b\", [\"num\"] = 2 }",
            "two entries for pylon 2",
        ),
        (
            "{ [\"CLSID\"] = \"b\", [\"num\"] = 2 }, { [\"CLSID\"] = \"\", [\"num\"] = 2 }",
            "two entries for pylon 2",
        ),
        ("{ [\"CLSID\"] = \"a\" }", "no `num`"),
        ("{ [\"CLSID\"] = \"a\", [\"num\"] = 0 }", "no `num`"),
        ("{ [\"CLSID\"] = \"a\", [\"num\"] = 1.5 }", "no `num`"),
        ("{ [\"CLSID\"] = \"a\", [\"num\"] = \"1\" }", "no `num`"),
        ("{ [\"CLSID\"] = \"a\", [\"num\"] = 1001 }", "no `num`"),
        ("7", "is not a table"),
        (
            "{ [\"CLSID\"] = \"a\", [\"num\"] = 2 }, { [\"CLSID\"] = \"b\", [\"num\"] = 2 }",
            "two entries for pylon 2",
        ),
        (
            "{ [\"CLSID\"] = \"a\", [\"num\"] = 1, [\"settings\"] = 1 }",
            "not a table",
        ),
        (
            "{ [\"CLSID\"] = \"a\", [\"num\"] = 1, [\"settings\"] = { [1.5] = 1 } }",
            "cannot carry unchanged",
        ),
        (
            "{ [\"CLSID\"] = \"a\", [\"num\"] = 1, [\"settings\"] = { [\"k\"] = \"\\200\" } }",
            "cannot carry unchanged",
        ),
        (
            "{ [\"CLSID\"] = \"a\", [\"num\"] = 1, [\"settings\"] = { [\"k\"] = 1, [\"k\"] = 2 } }",
            "cannot carry unchanged",
        ),
        (
            "{ [\"CLSID\"] = \"a\", [\"num\"] = 1, [\"settings\"] = { [\"k\"] = { [\"j\"] = nil } } }",
            "holding `nil`",
        ),
        (
            "[1] = { [\"CLSID\"] = \"a\", [\"num\"] = 1 }, [1] = { [\"CLSID\"] = \"b\", [\"num\"] = 2 }",
            "`pylons` of the preset \"x\" has two entries under [1]",
        ),
    ] {
        let f = file_of(&one(pylon)).unwrap();
        let e = f.pylons("T", "x").unwrap_err();
        assert_eq!(e.field, "name", "{pylon}");
        assert!(e.text.contains(want), "{pylon}: {e:?}");
    }
    let deep = format!(
        "{{ [\"CLSID\"] = \"a\", [\"num\"] = 1, [\"settings\"] = {}1{} }}",
        "{".repeat(20),
        "}".repeat(20)
    );
    let e = file_of(&one(&deep)).unwrap().pylons("T", "x").unwrap_err();
    assert!(e.text.contains("cannot be stored"), "{e:?}");
}

#[test]
fn many_presets_are_found_by_name_without_a_scan_each() {
    // 20,000 presets in one file, each looked up: a scan per lookup would
    // read 400 million names. With the index the loop takes well under a
    // second in a debug build; the limit leaves room for a loaded machine.
    let n = 20_000;
    let body: String = (0..n)
        .map(|i| format!("{{ [\"name\"] = \"p{i}\", [\"pylons\"] = {{}} }},"))
        .collect();
    let src =
        format!("local p = {{ [\"name\"] = \"T\", [\"payloads\"] = {{ {body} }} }}\nreturn p");
    let f = file_of(&src).unwrap();
    let start = std::time::Instant::now();
    for i in 0..n {
        assert!(f.pylons("T", &format!("p{i}")).is_ok());
    }
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn refuses_two_presets_under_one_key() {
    let src = "local p = { [\"name\"] = \"T\", [\"payloads\"] = {\n\
               [1] = { [\"name\"] = \"A\", [\"pylons\"] = {} },\n\
               [1] = { [\"name\"] = \"B\", [\"pylons\"] = {} } } }\nreturn p";
    let e = file_of(src).unwrap_err();
    assert_eq!(e.field, "path");
    assert!(
        e.text.contains("`payloads` has two entries under [1]"),
        "{e:?}"
    );
}

#[test]
fn notes_unknown_pylon_keys() {
    let f = file_of(&one(
        "{ [\"CLSID\"] = \"a\", [\"num\"] = 1, [\"extra\"] = 2 }",
    ))
    .unwrap();
    assert_eq!(f.pylons("T", "x").unwrap().dropped, vec!["pylons[].extra"]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn arbitrary_bytes_never_panic(src in proptest::collection::vec(any::<u8>(), 0..400)) {
        if let Ok(f) = read(src) {
            let _ = f.pylons(&f.unit_type.clone(), "x");
        }
    }

    #[test]
    fn arbitrary_presets_never_panic(
        clsid in "[ -~]{0,12}",
        num in -3.0f64..1500.0,
        key in "[a-z0-9]{1,4}",
        val in any::<f64>(),
    ) {
        let pylon = format!(
            "{{ [\"CLSID\"] = {clsid:?}, [\"num\"] = {num}, [\"settings\"] = {{ [{key:?}] = {val:e} }} }}"
        );
        if let Ok(f) = read(one(&pylon).into_bytes()) {
            let _ = f.pylons("T", "x");
        }
    }
}
