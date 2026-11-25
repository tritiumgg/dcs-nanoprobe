//! A test static group of heliports for the mission generator: one blue
//! (USA) static group of `FARP` units, with limited warehouse entries keyed
//! by the first unit only or by each unit.
//!
//! The Mission Editor writes a static group under its country's
//! `static.group` list, each unit with `category`, `shape_name`, `type`,
//! `unitId`, `name`, `x`, `y`, `heading` and a heliport's three `heliport_`
//! keys (ED-CODE, `MissionEditor/modules/me_mission.lua`, the unit table near
//! line 4465). It keys a heliport group's warehouse by its first unit,
//! `addWarehouse(group.units[1].unitId, ...)` (ED-CODE, the same file near
//! line 6202), in the `warehouses` section of the `warehouses` member.

/// Which units of the group get a `warehouses` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keys {
    /// Only the first unit, as the Mission Editor keys a heliport group.
    First,
    /// Every unit.
    Each,
}

/// The group to add.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Map place of the first unit: north and east, in meters.
    pub x: f64,
    pub y: f64,
    /// Number of `FARP` units, 1 to 8, each `SPACING` meters north of the last.
    pub units: u32,
    pub keys: Keys,
}

/// Meters between units, north. A `FARP` is four pads about 50 m apart.
pub const SPACING: f64 = 200.0;
/// Group name; unit `k` (from 1) is `np-hg-<k>`.
pub const NAME: &str = "np-hg";
/// The country: USA (2) under blue, as the Caucasus template lists it.
const COUNTRY: (u32, &str) = (2, "USA");

impl Group {
    /// Parse `X,Y,UNITS,first|each`.
    pub fn parse(text: &str) -> Result<Group, String> {
        let parts: Vec<&str> = text.split(',').collect();
        let [x, y, units, keys] = parts[..] else {
            return Err(format!(
                "--heliport-group {text:?} is not X,Y,UNITS,first|each"
            ));
        };
        let number = |v: &str| {
            v.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| format!("--heliport-group: {v:?} is not a coordinate"))
        };
        let units: u32 = units
            .parse()
            .ok()
            .filter(|n| (1..=8).contains(n))
            .ok_or_else(|| format!("--heliport-group: {units:?} units is not 1 to 8"))?;
        let keys = match keys {
            "first" => Keys::First,
            "each" => Keys::Each,
            other => return Err(format!("--heliport-group: {other:?} is not first or each")),
        };
        Ok(Group {
            x: number(x)?,
            y: number(y)?,
            units,
            keys,
        })
    }

    /// Jet fuel of unit `k`'s warehouse, in tons: 10 per unit number, so a read
    /// tells which entry an airbase took.
    pub fn jet_fuel(k: u32) -> u32 {
        10 * k
    }
}

/// The largest `["unitId"]` or `["groupId"]` value in `mission`, or 0.
fn largest_id(mission: &str) -> u32 {
    mission
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix("[\"unitId\"] = ")
                .or_else(|| t.strip_prefix("[\"groupId\"] = "))
        })
        .filter_map(|rest| rest.trim_end_matches(',').trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

/// The changed `mission` and `warehouses` members, and the unit IDs.
pub type Added = (Vec<u8>, Vec<u8>, Vec<u32>);

/// Add the group to `mission` and its warehouse entries to `warehouses`.
/// The template's blue coalition must list no country, and its `warehouses`
/// section must be `{}`.
pub fn add(mission: &[u8], warehouses: &[u8], group: &Group) -> Result<Added, String> {
    let mission = std::str::from_utf8(mission).map_err(|_| "`mission` is not UTF-8".to_owned())?;
    let warehouses =
        std::str::from_utf8(warehouses).map_err(|_| "`warehouses` is not UTF-8".to_owned())?;
    let first = largest_id(mission) + 1;
    let group_id = first;
    let ids: Vec<u32> = (0..group.units).map(|k| first + 1 + k).collect();

    // The blue coalition's empty country list: the line after its name.
    let marker = "[\"name\"] = \"blue\",";
    let lines: Vec<&str> = mission.split_inclusive('\n').collect();
    let at: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| {
            l.trim() == marker
                && lines
                    .get(i + 1)
                    .is_some_and(|n| n.trim() == "[\"country\"] = {},")
        })
        .map(|(i, _)| i + 1)
        .collect();
    let [line] = at[..] else {
        return Err(format!(
            "`mission` has {} blue coalitions with an empty country list; one is needed",
            at.len()
        ));
    };
    let eol = if lines[line].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let indent = &lines[line][..lines[line].len() - lines[line].trim_start().len()];
    let mut units = String::new();
    for (k, id) in ids.iter().enumerate() {
        let n = k + 1;
        let x = group.x + SPACING * k as f64;
        units.push_str(&format!(
            "[{n}] = {{ [\"category\"] = \"Heliports\", [\"shape_name\"] = \"FARPS\", \
             [\"type\"] = \"FARP\", [\"unitId\"] = {id}, [\"rate\"] = 100, \
             [\"x\"] = {x}, [\"y\"] = {y}, [\"name\"] = \"{NAME}-{n}\", [\"heading\"] = 0, \
             [\"heliport_frequency\"] = \"127.5\", [\"heliport_modulation\"] = 0, \
             [\"heliport_callsign_id\"] = 1 }}, ",
            y = group.y
        ));
    }
    let (cid, cname) = COUNTRY;
    let country = format!(
        "{indent}[\"country\"] = {{ [1] = {{ [\"id\"] = {cid}, [\"name\"] = \"{cname}\", \
         [\"static\"] = {{ [\"group\"] = {{ [1] = {{ [\"heading\"] = 0, \
         [\"route\"] = {{ [\"points\"] = {{ [1] = {{ [\"alt\"] = 0, [\"type\"] = \"\", \
         [\"name\"] = \"\", [\"y\"] = {y}, [\"speed\"] = 0, [\"x\"] = {x}, \
         [\"formation_template\"] = \"\", [\"action\"] = \"\" }} }} }}, \
         [\"groupId\"] = {group_id}, [\"hidden\"] = false, [\"units\"] = {{ {units}}}, \
         [\"y\"] = {y}, [\"x\"] = {x}, [\"name\"] = \"{NAME}\", [\"dead\"] = false }} }} }} }} }},{eol}",
        x = group.x,
        y = group.y
    );
    let mut mission_out = String::with_capacity(mission.len() + country.len());
    for (i, l) in lines.iter().enumerate() {
        mission_out.push_str(if i == line { &country } else { l });
    }

    let keyed: Vec<(u32, u32)> = match group.keys {
        Keys::First => vec![(1, ids[0])],
        Keys::Each => ids
            .iter()
            .enumerate()
            .map(|(k, id)| (k as u32 + 1, *id))
            .collect(),
    };
    let mut entries = String::new();
    for (k, id) in &keyed {
        entries.push_str(&entry(*id, Group::jet_fuel(*k)));
    }
    let wlines: Vec<&str> = warehouses.split_inclusive('\n').collect();
    let at: Vec<usize> = wlines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == "[\"warehouses\"] = {},")
        .map(|(i, _)| i)
        .collect();
    let [wline] = at[..] else {
        return Err(format!(
            "`warehouses` has {} empty `warehouses` sections; one is needed",
            at.len()
        ));
    };
    let weol = if wlines[wline].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let windent = &wlines[wline][..wlines[wline].len() - wlines[wline].trim_start().len()];
    let section = format!("{windent}[\"warehouses\"] = {{ {entries}}},{weol}");
    let mut warehouses_out = String::with_capacity(warehouses.len() + section.len());
    for (i, l) in wlines.iter().enumerate() {
        warehouses_out.push_str(if i == wline { &section } else { l });
    }
    Ok((mission_out.into_bytes(), warehouses_out.into_bytes(), ids))
}

/// A limited warehouse entry as the Mission Editor's default one
/// (`createAirportEquipment` in `me_mission.lua`), with the `unlimited*` flags
/// false, no aircraft or weapons, dynamic spawn on, `jet_fuel` tons of jet
/// fuel and none of the other liquids.
fn entry(id: u32, jet_fuel: u32) -> String {
    format!(
        "[{id}] = {{ [\"unlimitedMunitions\"] = false, [\"unlimitedAircrafts\"] = false, \
         [\"unlimitedFuel\"] = false, [\"dynamicSpawn\"] = true, [\"allowHotStart\"] = false, \
         [\"dynamicCargo\"] = true, [\"coalition\"] = \"blue\", [\"suppliers\"] = {{}}, \
         [\"speed\"] = 16.666666, [\"periodicity\"] = 30, [\"size\"] = 100, \
         [\"OperatingLevel_Eqp\"] = 10, [\"OperatingLevel_Air\"] = 10, \
         [\"OperatingLevel_Fuel\"] = 10, \
         [\"aircrafts\"] = {{ [\"planes\"] = {{}}, [\"helicopters\"] = {{}} }}, \
         [\"weapons\"] = {{}}, [\"jet_fuel\"] = {{ [\"InitFuel\"] = {jet_fuel} }}, \
         [\"gasoline\"] = {{ [\"InitFuel\"] = 0 }}, \
         [\"methanol_mixture\"] = {{ [\"InitFuel\"] = 0 }}, \
         [\"diesel\"] = {{ [\"InitFuel\"] = 0 }} }}, "
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISSION: &str = "mission = \n{\n\t[\"coalition\"] = \n\t{\n\t\t[\"blue\"] = \n\t\t{\n\t\t\t[\"name\"] = \"blue\",\n\t\t\t[\"country\"] = {},\n\t\t},\n\t\t[\"red\"] = \n\t\t{\n\t\t\t[\"name\"] = \"red\",\n\t\t\t[\"country\"] = {},\n\t\t},\n\t},\n\t[\"groupId\"] = 7,\n}\n";
    const WAREHOUSES: &str =
        "warehouses = \n{\n\t[\"airports\"] = {},\n\t[\"warehouses\"] = {},\n}\n";

    fn group(units: u32, keys: Keys) -> Group {
        Group {
            x: -356_400.0,
            y: 619_200.5,
            units,
            keys,
        }
    }

    #[test]
    fn parses_and_refuses() {
        assert_eq!(
            Group::parse("-356400,619200.5,2,first").unwrap(),
            group(2, Keys::First)
        );
        for bad in [
            "1,2,2",
            "1,2,0,first",
            "1,2,9,each",
            "x,2,2,each",
            "1,2,2,all",
            "1,inf,2,each",
        ] {
            assert!(Group::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keys_the_first_unit_only() {
        let (m, w, ids) = add(
            MISSION.as_bytes(),
            WAREHOUSES.as_bytes(),
            &group(2, Keys::First),
        )
        .unwrap();
        let (m, w) = (String::from_utf8(m).unwrap(), String::from_utf8(w).unwrap());
        assert_eq!(ids, vec![9, 10]);
        assert!(m.contains("[\"groupId\"] = 8,"), "{m}");
        assert!(m.contains("[\"name\"] = \"np-hg-1\""), "{m}");
        assert!(
            m.contains("[\"x\"] = -356200, [\"y\"] = 619200.5, [\"name\"] = \"np-hg-2\""),
            "{m}"
        );
        // Red keeps its empty list.
        assert_eq!(m.matches("[\"country\"] = {},").count(), 1, "{m}");
        assert!(w.contains("[9] = {"), "{w}");
        assert!(!w.contains("[10] = {"), "{w}");
        assert!(w.contains("[\"InitFuel\"] = 10 }"), "{w}");
        assert_eq!(w.matches("= false,").count(), 4, "{w}");
    }

    #[test]
    fn keys_each_unit_with_its_own_fuel() {
        let (_, w, _) = add(
            MISSION.as_bytes(),
            WAREHOUSES.as_bytes(),
            &group(2, Keys::Each),
        )
        .unwrap();
        let w = String::from_utf8(w).unwrap();
        // Each entry runs from its key to the next one's.
        let (first, second) = w.split_once("[10] = {").expect("an entry for unit 10");
        let first = first.split_once("[9] = {").expect("an entry for unit 9").1;
        let fuel = |entry: &str| {
            let at = entry.find("[\"jet_fuel\"] = { [\"InitFuel\"] = ").unwrap();
            entry[at..].split('}').next().unwrap().to_owned()
        };
        assert!(fuel(first).ends_with("= 10 "), "{w}");
        assert!(fuel(second).ends_with("= 20 "), "{w}");
    }

    #[test]
    fn refuses_a_template_that_lists_countries_or_warehouses() {
        let listed = MISSION.replacen("[\"country\"] = {},", "[\"country\"] = { [1] = {} },", 1);
        assert!(
            add(
                listed.as_bytes(),
                WAREHOUSES.as_bytes(),
                &group(2, Keys::First)
            )
            .is_err()
        );
        let full = WAREHOUSES.replace("[\"warehouses\"] = {},", "[\"warehouses\"] = { [1] = {} },");
        assert!(add(MISSION.as_bytes(), full.as_bytes(), &group(2, Keys::First)).is_err());
    }

    /// The committed Caucasus template takes the group, and both members
    /// still parse as Lua tables with the group and entries where DCS reads
    /// them (checked by brace balance; the Lua check is the live load).
    #[test]
    fn adds_to_the_committed_caucasus_template() {
        let bytes =
            std::fs::read(crate::repository().join("tests/fixtures/templates/caucasus.miz"))
                .unwrap();
        let shell = crate::miz::generate(
            &bytes,
            &crate::miz::Changes {
                heliport_group: Some(group(2, Keys::Each)),
                ..Default::default()
            },
        )
        .unwrap();
        let members = crate::miz::read(&shell).unwrap();
        let text = |name: &str| {
            let data = &members.iter().find(|(n, _)| n == name).expect(name).1;
            let text = String::from_utf8(data.clone()).unwrap();
            assert_eq!(
                text.matches('{').count(),
                text.matches('}').count(),
                "{name}"
            );
            text
        };
        let mission = text("mission");
        assert!(mission.contains("[\"name\"] = \"np-hg-2\""), "{mission}");
        // The template holds no unit or group, so the group is 1, the units 2 and 3.
        assert!(mission.contains("[\"groupId\"] = 1,"), "{mission}");
        let warehouses = text("warehouses");
        assert!(
            warehouses.contains("[2] = { [\"unlimitedMunitions\"] = false")
                && warehouses.contains("[3] = { [\"unlimitedMunitions\"] = false"),
            "{warehouses}"
        );
    }
}
