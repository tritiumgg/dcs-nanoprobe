//! `doctor` on fixture profiles: a good install, one missing each file, a
//! broken `autoexec.cfg`, and a profile inside a Wine prefix. Every fixture
//! file is made up here; none is copied from a DCS install.

use std::path::{Path, PathBuf};

use nanoprobe_cli::doctor::{self, Mark, Missions, RELEASE_LOADER, Report};
use nanoprobe_cli::profiles::{Env, Found, Where};
use nanoprobe_install::autoexec::AUTOEXEC;
use nanoprobe_install::{Local, Payload, install, manifest};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("nanoprobe-doctor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// One log record, as spec/log.md lays it out.
fn record(wall: &str, level: &str, target: &str, msg: &str, data: &str) -> String {
    format!(
        "{{\"wall\":\"{wall}\",\"level\":\"{level}\",\"target\":\"{target}\",\"msg\":\"{msg}\",\"data\":{{{data}}}}}\n"
    )
}

fn capabilities(wall: &str, target: &str, missing: Option<&str>) -> String {
    let absent = missing.map_or(String::new(), |name| format!(",\"{name}\":false"));
    record(
        wall,
        "info",
        target,
        "capabilities",
        &format!("\"DCS.getPause\":true{absent}"),
    )
}

fn hook_loaded(wall: &str, session: &str) -> String {
    record(
        wall,
        "info",
        "hook",
        "hook loaded",
        &format!("\"session\":\"{session}\""),
    )
}

/// The UTC date and time one minute from now, as dcs.log stamps them
/// (`YYYY-MM-DD HH:MM:SS`). Call it after the install: the doctor treats a log
/// that opened at or before the install as older than it.
fn after_install() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 60;
    let (days, rest) = (i64::try_from(secs / 86_400).unwrap(), secs % 86_400);
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// A DCS start of session `id` in dcs.log, running `build`, after the install.
fn dcs_log(root: &Path, id: &str, build: &str) {
    let at = after_install();
    write(
        &root.join("Logs/dcs.log"),
        &format!(
            "=== Log opened UTC {at}\n\
             {at}.000 INFO    EDCORE (Main): made-up line\n\
             {at}.000 INFO    NANOPROBE (Main): session {id} build {build}\n"
        ),
    );
}

/// A profile with the release installed, a DCS start that ran the hook, and
/// both capabilities records. Returns the active build.
fn good(root: &Path) -> String {
    write(&root.join("Config/options.lua"), "options = {}\n");
    write(
        &root.join("Config/autoexec.cfg"),
        "options.graphics.example = 1\n",
    );
    let payload = Payload {
        loader: RELEASE_LOADER.to_vec(),
        dll: b"MZ fake agent".to_vec(),
        files: vec![
            ("lua/hook.lua".into(), b"-- hook\n".to_vec()),
            ("lua/log.lua".into(), b"-- log\n".to_vec()),
            ("lua/shim.lua".into(), b"-- shim\n".to_vec()),
        ],
        dev: None,
    };
    let profile = Local::new(root).unwrap();
    let build = install(&profile, &payload).unwrap().deployed.build;
    dcs_log(root, "7a", &build);
    write(
        &root.join("nanoprobe/logs/hook.log"),
        &(hook_loaded("2026-11-15T10:00:02Z", "7a")
            + &capabilities("2026-11-15T10:00:02Z", "hook", None)),
    );
    write(
        &root.join("nanoprobe/logs/shim.log"),
        &capabilities("2026-11-15T10:00:30Z", "shim", None),
    );
    write(
        &root.join("nanoprobe/running.txt"),
        &format!("build={build}\n"),
    );
    build
}

fn check(root: &Path) -> Report {
    doctor::check(&Found {
        path: root.to_path_buf(),
        wine_prefix: None,
    })
}

fn assert_clean(report: &Report) {
    assert!(!report.failed(), "{report}");
}

#[test]
fn a_good_install_passes() {
    let scratch = Scratch::new("good");
    let root = scratch.0.join("DCS");
    let build = good(&root);
    let report = check(&root);
    assert_clean(&report);
    for text in [
        "Scripts/Hooks/nanoprobe-hook.lua is in place",
        &format!("active version {build}: 4 files match SHA256SUMS"),
        "nanoprobe/token is there",
        &format!("the hook started build {build}"),
        "the hook state has all 1 DCS names",
        "the server state has all 1 DCS names",
        "the nanoprobe block allows",
    ] {
        assert!(
            report.has(Mark::Ok, text),
            "no ok line with {text:?}:\n{report}"
        );
    }
}

#[test]
fn each_missing_file_fails() {
    let scratch = Scratch::new("missing");
    let root = scratch.0.join("DCS");
    let build = good(&root);
    let version = format!("nanoprobe/versions/{build}");
    let dll = format!("nanoprobe_agent_{build}.dll");
    let files = [
        (
            "Scripts/Hooks/nanoprobe-hook.lua".to_owned(),
            "no Scripts/Hooks/nanoprobe-hook.lua".to_owned(),
        ),
        (
            "nanoprobe/manifest.txt".to_owned(),
            "no active version in nanoprobe/manifest.txt".to_owned(),
        ),
        (
            "nanoprobe/token".to_owned(),
            "no nanoprobe/token".to_owned(),
        ),
        (
            format!("{version}/SHA256SUMS"),
            format!("no {version}/SHA256SUMS"),
        ),
        (format!("{version}/{dll}"), format!("{dll} is missing")),
        (
            format!("{version}/lua/hook.lua"),
            "lua/hook.lua is missing".to_owned(),
        ),
        (
            format!("{version}/lua/log.lua"),
            "lua/log.lua is missing".to_owned(),
        ),
        (
            format!("{version}/lua/shim.lua"),
            "lua/shim.lua is missing".to_owned(),
        ),
    ];
    for (file, text) in files {
        let _ = std::fs::remove_dir_all(&root);
        good(&root);
        std::fs::remove_file(root.join(&file)).unwrap();
        let report = check(&root);
        assert!(
            report.has(Mark::Fail, &text),
            "removing {file} gave no fail line with {text:?}:\n{report}"
        );
    }
    // A missing dcs.log, logs and agent.toml are notes: DCS has not run yet.
    let _ = std::fs::remove_dir_all(&root);
    good(&root);
    std::fs::remove_file(root.join("Logs/dcs.log")).unwrap();
    std::fs::remove_dir_all(root.join("nanoprobe/logs")).unwrap();
    let report = check(&root);
    assert_clean(&report);
    assert!(report.has(Mark::Note, "DCS has not run"), "{report}");
}

#[test]
fn a_changed_file_and_the_kill_switch_fail() {
    let scratch = Scratch::new("changed");
    let root = scratch.0.join("DCS");
    let build = good(&root);
    write(
        &root.join(format!("nanoprobe/versions/{build}/lua/shim.lua")),
        "-- edited\n",
    );
    let report = check(&root);
    assert!(report.has(Mark::Fail, "lua/shim.lua changed"), "{report}");

    let _ = std::fs::remove_dir_all(&root);
    good(&root);
    write(&root.join("nanoprobe/disable"), "");
    assert!(check(&root).has(Mark::Fail, "kill switch"));
}

#[test]
fn the_log_and_capabilities_report_failures() {
    let scratch = Scratch::new("log");
    let root = scratch.0.join("DCS");
    good(&root);
    let at = after_install();
    write(
        &root.join("Logs/dcs.log"),
        &format!(
            "=== Log opened UTC {at}\n\
             {at}.000 ERROR   NANOPROBE (Main): loader: no manifest at x\n"
        ),
    );
    let report = check(&root);
    assert!(report.has(Mark::Fail, "loader: no manifest"), "{report}");

    write(
        &root.join("Logs/dcs.log"),
        &format!("=== Log opened UTC {at}\n{at}.000 INFO    EDCORE (Main): x\n"),
    );
    assert!(check(&root).has(Mark::Fail, "did not run the loader"));

    dcs_log(&root, "7a", "0000000000000000");
    write(
        &root.join("nanoprobe/logs/shim.log"),
        &capabilities(
            "2026-11-15T10:00:30Z",
            "shim",
            Some("world.addEventHandler"),
        ),
    );
    assert!(
        check(&root).has(Mark::Fail, "lacks world.addEventHandler"),
        "{}",
        check(&root)
    );
}

#[test]
fn the_build_dcs_runs_is_compared_with_the_active_one() {
    let scratch = Scratch::new("older");
    let root = scratch.0.join("DCS");
    good(&root);
    dcs_log(&root, "7a", "0000000000000000");
    let report = check(&root);
    assert_clean(&report);
    assert!(report.has(Mark::Note, "restart DCS to load"), "{report}");
}

#[test]
fn a_broken_autoexec_fails() {
    let scratch = Scratch::new("autoexec");
    let root = scratch.0.join("DCS");
    let cfg = root.join(AUTOEXEC);

    good(&root);
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, text.replace("'server')", "'gui')")).unwrap();
    assert!(check(&root).has(Mark::Fail, "was edited or cut short"));

    let _ = std::fs::remove_dir_all(&root);
    good(&root);
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, text.replace("-- nanoprobe: end\n", "")).unwrap();
    assert!(check(&root).has(Mark::Fail, "was edited or cut short"));

    let _ = std::fs::remove_dir_all(&root);
    good(&root);
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("net.allow_dostring_in = { 'mission' }\n");
    std::fs::write(&cfg, text).unwrap();
    let report = check(&root);
    assert!(
        report.has(Mark::Fail, "net.allow_dostring_in is set to { mission }"),
        "{report}"
    );

    // Without a block, an assignment that drops `server` still fails.
    let _ = std::fs::remove_dir_all(&root);
    good(&root);
    std::fs::write(&cfg, "net.allow_dostring_in = { 'gui' }\n").unwrap();
    let report = check(&root);
    assert!(
        report.has(Mark::Fail, "is set to { gui } with no nanoprobe block"),
        "{report}"
    );

    // A value doctor cannot read is a note.
    let _ = std::fs::remove_dir_all(&root);
    good(&root);
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("net.allow_dostring_in = make_list()\n");
    std::fs::write(&cfg, text).unwrap();
    let report = check(&root);
    assert_clean(&report);
    assert!(
        report.has(Mark::Note, "form doctor cannot read"),
        "{report}"
    );

    // No block at all, as after a development deploy, is a note.
    let _ = std::fs::remove_dir_all(&root);
    good(&root);
    std::fs::write(&cfg, "options.graphics.example = 1\n").unwrap();
    let report = check(&root);
    assert_clean(&report);
    assert!(report.has(Mark::Note, "no nanoprobe block"), "{report}");
}

#[test]
fn finds_and_checks_a_profile_in_a_wine_prefix() {
    let scratch = Scratch::new("wine");
    let prefix = scratch.0.join("config/.wine");
    let root = prefix.join("drive_c/users/abc/Saved Games/DCS.dcs_serverrelease");
    good(&root);
    // A second profile without nanoprobe is skipped.
    std::fs::create_dir_all(prefix.join("drive_c/users/abc/Saved Games/DCS")).unwrap();
    let place = Where {
        wine_prefix: Some(prefix.clone()),
        ..Where::default()
    };
    let targets = doctor::targets(&place, &Env::default()).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].path, root);
    let report = doctor::check(&targets[0]);
    assert_clean(&report);
    assert!(
        report.has(
            Mark::Note,
            &format!("in the Wine prefix {}", prefix.display())
        ),
        "{report}"
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        doctor::run(
            &place,
            &Missions::default(),
            &Env::default(),
            &mut out,
            &mut err
        ),
        0
    );
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("note profile "), "{text}");
}

#[test]
fn no_installed_profile_is_exit_2() {
    let scratch = Scratch::new("none");
    let prefix = scratch.0.join("wine");
    std::fs::create_dir_all(prefix.join("drive_c/users/abc/Saved Games/DCS")).unwrap();
    let place = Where {
        wine_prefix: Some(prefix),
        ..Where::default()
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        doctor::run(
            &place,
            &Missions::default(),
            &Env::default(),
            &mut out,
            &mut err
        ),
        2
    );
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("not installed"), "{err}");
}

#[test]
fn doctor_writes_nothing() {
    let scratch = Scratch::new("readonly");
    let root = scratch.0.join("DCS");
    good(&root);
    let before = nanoprobe_install::snapshot_local(&root).unwrap();
    let manifest_before = manifest(&Local::new(&root).unwrap()).unwrap();
    check(&root);
    assert_eq!(nanoprobe_install::snapshot_local(&root).unwrap(), before);
    assert_eq!(
        manifest(&Local::new(&root).unwrap()).unwrap(),
        manifest_before
    );
}

#[test]
fn only_the_newest_start_counts_in_the_platform_logs() {
    let scratch = Scratch::new("newest");
    let root = scratch.0.join("DCS");
    let build = good(&root);

    // A newer start whose hook could not reach the server state: the shim's
    // record from the earlier start no longer counts, and the error fails.
    dcs_log(&root, "8b", &build);
    let mut hook = std::fs::read_to_string(root.join("nanoprobe/logs/hook.log")).unwrap();
    hook.push_str(&hook_loaded("2026-11-15T12:00:00Z", "8b"));
    hook.push_str(&capabilities("2026-11-15T12:00:00Z", "hook", None));
    hook.push_str(&record(
        "2026-11-15T12:00:40Z",
        "error",
        "hook",
        "shim not started",
        "\"error\":\"refused\"",
    ));
    write(&root.join("nanoprobe/logs/hook.log"), &hook);
    let report = check(&root);
    assert!(report.has(Mark::Fail, "shim not started"), "{report}");
    assert!(
        report.has(Mark::Note, "the shim has not started"),
        "{report}"
    );
    assert!(
        !report.has(Mark::Ok, "the server state has all"),
        "{report}"
    );

    // An error from an earlier start does not count.
    let _ = std::fs::remove_dir_all(&root);
    let build = good(&root);
    let hook = record(
        "2026-11-15T09:00:00Z",
        "error",
        "hook",
        "agent not loaded",
        "",
    ) + &std::fs::read_to_string(root.join("nanoprobe/logs/hook.log")).unwrap();
    write(&root.join("nanoprobe/logs/hook.log"), &hook);
    assert_clean(&check(&root));

    // A start that left no hook record fails.
    dcs_log(&root, "9c", &build);
    assert!(check(&root).has(Mark::Fail, "no record of session 9c"));
}

#[test]
fn a_log_from_before_the_install_is_a_note() {
    let scratch = Scratch::new("before");
    let root = scratch.0.join("DCS");
    good(&root);
    let log = |year: u32| {
        format!(
            "=== Log opened UTC {year}-01-01 10:00:00\n{year}-01-01 10:00:01.000 INFO    EDCORE (Main): x\n"
        )
    };
    // A start that began before the install, in a log a running DCS still
    // writes to: a note.
    write(&root.join("Logs/dcs.log"), &log(2000));
    let report = check(&root);
    assert!(
        report.has(Mark::Note, "from before the install"),
        "{report}"
    );
    assert!(
        !report.has(Mark::Fail, "did not run the loader"),
        "{report}"
    );
    // A start that began after it and ran no loader: a failure.
    write(&root.join("Logs/dcs.log"), &log(2099));
    assert!(check(&root).has(Mark::Fail, "did not run the loader"));
}

#[test]
fn a_newest_start_without_the_hook_leaves_the_old_logs_alone() {
    let scratch = Scratch::new("nohook");
    let root = scratch.0.join("DCS");
    good(&root);
    // An earlier start's error in hook.log.
    let mut hook = std::fs::read_to_string(root.join("nanoprobe/logs/hook.log")).unwrap();
    hook.push_str(&record(
        "2026-11-15T10:00:05Z",
        "error",
        "hook",
        "agent not loaded",
        "",
    ));
    write(&root.join("nanoprobe/logs/hook.log"), &hook);
    // The newest start: the kill switch, so no session line.
    write(&root.join("nanoprobe/disable"), "");
    write(
        &root.join("Logs/dcs.log"),
        "=== Log opened UTC 2099-01-01 10:00:00\n\
         2099-01-01 10:00:01.000 INFO    NANOPROBE (Main): disabled by x/disable; nothing loaded\n",
    );
    let report = check(&root);
    assert!(!report.has(Mark::Fail, "agent not loaded"), "{report}");
    assert!(!report.has(Mark::Ok, "DCS names"), "{report}");
    assert!(report.has(Mark::Note, "ran no hook"), "{report}");
}

/// The administrator's per-map template, here the repository's test fixture.
const CAUCASUS: &[u8] = include_bytes!("../../../tests/fixtures/templates/caucasus.miz");

fn templates(root: &Path, missions: &Missions) -> Report {
    let mut report = Report::default();
    doctor::templates(root, missions, &mut report);
    report
}

#[test]
fn the_per_map_templates_are_checked() {
    let scratch = Scratch::new("templates");
    let root = scratch.0.join("DCS");
    let profile = Missions::default();

    // No mission folder in the profile: the sidecar keeps it elsewhere.
    let report = templates(&root, &profile);
    assert_clean(&report);
    assert!(report.has(Mark::Note, "--missions"), "{report}");
    // Unless a theater is named: then the folder must be there.
    let named = Missions {
        theaters: vec!["Caucasus".into()],
        ..Missions::default()
    };
    let report = templates(&root, &named);
    assert!(
        report.has(Mark::Fail, "cannot read the mission folder"),
        "{report}"
    );
    assert!(!report.has(Mark::Note, "--missions"), "{report}");

    // A file where the mission folder should be fails without any flag.
    std::fs::create_dir_all(root.join("nanoprobe")).unwrap();
    std::fs::write(root.join("nanoprobe/missions"), b"").unwrap();
    let report = templates(&root, &profile);
    assert!(
        report.has(Mark::Fail, "cannot read the mission folder"),
        "{report}"
    );
    std::fs::remove_file(root.join("nanoprobe/missions")).unwrap();

    // A mission folder without a template.
    let dir = root.join("nanoprobe/missions");
    std::fs::create_dir_all(dir.join("shells")).unwrap();
    std::fs::write(dir.join("shells/1.a.miz"), CAUCASUS).unwrap();
    let report = templates(&root, &profile);
    assert!(report.has(Mark::Fail, "no per-map template in"), "{report}");

    // One template; a theater named on the command line that has none.
    std::fs::write(dir.join("Caucasus.miz"), CAUCASUS).unwrap();
    let report = templates(&root, &profile);
    assert_clean(&report);
    assert!(report.has(Mark::Ok, "per-map templates in"), "{report}");
    assert!(report.has(Mark::Ok, ": Caucasus"), "{report}");
    let syria = Missions {
        theaters: vec!["Caucasus".into(), "Syria".into()],
        ..Missions::default()
    };
    let report = templates(&root, &syria);
    assert!(
        report.has(Mark::Fail, "no per-map template for Syria"),
        "{report}"
    );
    assert!(!report.has(Mark::Fail, "for Caucasus"), "{report}");

    // A file named for another theater than its `theatre` member, one
    // that is not a mission, and one whose name is no theater's.
    std::fs::write(dir.join("Syria.miz"), CAUCASUS).unwrap();
    std::fs::write(dir.join("Nevada.miz"), b"not a zip").unwrap();
    std::fs::write(dir.join("my mission.miz"), CAUCASUS).unwrap();
    let report = templates(&root, &syria);
    assert!(
        report.has(
            Mark::Fail,
            "is a mission of \"Caucasus\": name it Caucasus.miz"
        ),
        "{report}"
    );
    assert!(report.has(Mark::Fail, "Nevada.miz"), "{report}");
    assert!(report.has(Mark::Note, "my mission.miz"), "{report}");
    assert!(
        !report.has(Mark::Fail, "no per-map template for Syria"),
        "{report}"
    );

    // --missions names the folder; one that does not exist fails.
    let elsewhere = Missions {
        dir: Some(scratch.0.join("elsewhere")),
        ..Missions::default()
    };
    let report = templates(&root, &elsewhere);
    assert!(
        report.has(Mark::Fail, "cannot read the mission folder"),
        "{report}"
    );
    std::fs::create_dir_all(scratch.0.join("elsewhere")).unwrap();
    std::fs::write(scratch.0.join("elsewhere/Caucasus.miz"), CAUCASUS).unwrap();
    assert_clean(&templates(&root, &elsewhere));
}

/// `doctor --profile <root>` with `missions`: the exit code and what it
/// printed.
fn run(root: &Path, missions: &Missions) -> (i32, String) {
    let place = Where {
        profile: Some(root.to_path_buf()),
        ..Where::default()
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = doctor::run(&place, missions, &Env::default(), &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

/// A `sidecar.toml` beside the mission folder and the template folder it
/// names, with the token path `token`.
fn sidecar_toml(dir: &Path, token: &Path, template_dirs: &str) -> PathBuf {
    let path = dir.join("sidecar.toml");
    write(
        &path,
        &format!(
            "[agent]\nmap_path = \"nanoprobe.map\"\ntoken_path = '{}'\n\
             [missions]\ndir = \"missions\"\ndcs_dir = 'C:\\missions'\ntemplate_dirs = {template_dirs}\n",
            token.display()
        ),
    );
    path
}

#[test]
fn the_sidecar_config_is_checked() {
    let scratch = Scratch::new("sidecar-config");
    let root = scratch.0.join("DCS");
    good(&root);
    let side = scratch.0.join("sidecar");
    std::fs::create_dir_all(side.join("missions")).unwrap();
    std::fs::write(side.join("missions/Caucasus.miz"), CAUCASUS).unwrap();
    std::fs::create_dir_all(side.join("templates")).unwrap();

    // The file names the profile's token, a mission folder with a template
    // and a template folder: every line is ok, and doctor read the mission
    // folder the file names.
    let file = sidecar_toml(&side, &root.join("nanoprobe/token"), "[\"templates\"]");
    let with_file = Missions {
        sidecar_config: Some(file.clone()),
        ..Missions::default()
    };
    let (code, out) = run(&root, &with_file);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("is a file the sidecar accepts"), "{out}");
    assert!(out.contains("per-map templates in"), "{out}");
    assert!(out.contains(": Caucasus"), "{out}");
    assert!(out.contains("templates is a folder"), "{out}");
    let token = std::fs::read_to_string(root.join("nanoprobe/token")).unwrap();
    assert!(!out.contains(token.trim()), "doctor printed the token");

    // A listed folder that does not exist allows nothing.
    sidecar_toml(
        &side,
        &root.join("nanoprobe/token"),
        "[\"templates\", \"absent\"]",
    );
    let (code, out) = run(&root, &with_file);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("the sidecar allows no file in it"), "{out}");

    // An empty list is the default: a note, not a failure.
    sidecar_toml(&side, &root.join("nanoprobe/token"), "[]");
    let (code, out) = run(&root, &with_file);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("note [missions] template_dirs is empty"),
        "{out}"
    );

    // --template-dir and --missions stand in for the file's paths, as for a
    // sidecar in a container: the file's own folders are not read.
    sidecar_toml(&side, &root.join("nanoprobe/token"), "[\"absent\"]");
    let seen_here = Missions {
        sidecar_config: Some(file.clone()),
        dir: Some(side.join("missions")),
        template_dirs: vec![side.join("templates")],
        ..Missions::default()
    };
    let (code, out) = run(&root, &seen_here);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("absent"), "{out}");

    // One of dir and dcs_dir alone: the sidecar starts and refuses every
    // activation.
    let text = std::fs::read_to_string(&file).unwrap();
    write(
        &file,
        &text.replace("dcs_dir = 'C:\\missions'", "dcs_dir = ''"),
    );
    let (code, out) = run(&root, &with_file);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("fail [missions] dir and dcs_dir are not both set"),
        "{out}"
    );

    // Another token than the profile's: the handshake would be refused.
    let other = side.join("other-token");
    std::fs::write(&other, "cd".repeat(32) + "\n").unwrap();
    sidecar_toml(&side, &other, "[]");
    let (code, out) = run(&root, &with_file);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("names another token"), "{out}");
    assert!(!out.contains(&"cd".repeat(32)), "doctor printed a token");

    // A file the sidecar refuses: its own message. The mission folder is
    // then the profile's, and a --template-dir is still checked.
    let profile_missions = root.join("nanoprobe/missions");
    std::fs::create_dir_all(&profile_missions).unwrap();
    std::fs::write(profile_missions.join("Caucasus.miz"), CAUCASUS).unwrap();
    let refused = Missions {
        sidecar_config: Some(file.clone()),
        template_dirs: vec![side.join("templates")],
        ..Missions::default()
    };
    for text in [
        "[agent]\nmap_path = \"m\"\ntoken_path = \"none\"\n".to_owned(),
        "[nonsense]\n".to_owned(),
        // A listener off loopback needs a tokens file.
        format!(
            "[agent]\nmap_path = \"m\"\ntoken_path = '{}'\n[api]\nlisten = \"0.0.0.0:47311\"\n",
            root.join("nanoprobe/token").display()
        ),
        // A tokens file the sidecar cannot read.
        format!(
            "[agent]\nmap_path = \"m\"\ntoken_path = '{}'\n[api]\ntokens_path = \"no-tokens.toml\"\n",
            root.join("nanoprobe/token").display()
        ),
    ] {
        write(&file, &text);
        let (code, out) = run(&root, &refused);
        assert_eq!(code, 1, "{text}\n{out}");
        assert!(
            out.contains("fail the sidecar refuses to start"),
            "{text}\n{out}"
        );
        assert!(
            !out.contains("is a file the sidecar accepts"),
            "{text}\n{out}"
        );
        assert!(
            out.contains(&format!(
                "per-map templates in {}",
                profile_missions.display()
            )),
            "{text}\n{out}"
        );
        assert!(out.contains("templates is a folder"), "{text}\n{out}");
    }
    std::fs::remove_dir_all(&profile_missions).unwrap();

    // A sidecar serves one DCS: with two installed profiles in a prefix the
    // file has no one profile to be checked against.
    let prefix = scratch.0.join("wine");
    for name in ["DCS", "DCS.dcs_serverrelease"] {
        good(&prefix.join("drive_c/users/abc/Saved Games").join(name));
    }
    sidecar_toml(&side, &root.join("nanoprobe/token"), "[]");
    let place = Where {
        wine_prefix: Some(prefix),
        ..Where::default()
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = doctor::run(&place, &with_file, &Env::default(), &mut out, &mut err);
    assert_eq!(code, 2, "{}", String::from_utf8_lossy(&out));
    assert!(out.is_empty());
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.contains("name the one the sidecar serves with --profile"),
        "{err}"
    );
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        doctor::run(
            &place,
            &Missions::default(),
            &Env::default(),
            &mut out,
            &mut err
        ),
        0,
        "{}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn the_template_folders_are_checked() {
    let scratch = Scratch::new("template-dirs");
    let dir = scratch.0.join("templates");
    std::fs::create_dir_all(&dir).unwrap();
    let file = scratch.0.join("file");
    std::fs::write(&file, "x").unwrap();
    let check = |dirs: &[&Path]| {
        let mut report = Report::default();
        let dirs: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
        doctor::template_dirs(&dirs, &mut report);
        report
    };

    let report = check(&[&dir]);
    assert_clean(&report);
    assert!(report.has(Mark::Ok, "is a folder"), "{report}");
    assert!(
        check(&[&file]).has(Mark::Fail, "is not a folder"),
        "{}",
        check(&[&file])
    );
    let absent = scratch.0.join("absent");
    assert!(check(&[&absent]).has(Mark::Fail, "allows no file in it"));
    assert!(check(&[]).lines.is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |m| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(m)).unwrap();
        mode(0o775);
        let report = check(&[&dir]);
        assert_clean(&report);
        assert!(
            report.has(Mark::Note, "the group of the template folder"),
            "{report}"
        );
        mode(0o777);
        assert!(check(&[&dir]).has(Mark::Fail, "every user of this machine can write"));
        // A link to a folder is judged by the folder it names.
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(check(&[&link]).has(Mark::Fail, "every user of this machine can write"));
        mode(0o755);
        assert_clean(&check(&[&link]));
    }
}

/// The example `sidecar.toml` of each release archive is one the sidecar's
/// loader accepts once its placeholders are replaced.
#[test]
fn the_release_s_example_sidecar_configs_are_accepted() {
    use nanoprobe_api::sidecar_toml::SidecarToml;

    let scratch = Scratch::new("examples");
    let token = scratch.0.join("token");
    std::fs::write(&token, "ab".repeat(32) + "\n").unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |name: &str| std::fs::read_to_string(repo.join(name)).unwrap();
    let token_text = token.to_str().unwrap();
    // Outside its comments, no placeholder is left.
    let filled = |text: &str| {
        text.lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .all(|line| !line.contains('<'))
    };

    // The Windows archive's: a sidecar beside DCS, every path in the profile.
    let windows = read("tools/release/sidecar.windows.toml")
        .replace(r"<DCS profile>\nanoprobe\token", token_text)
        .replace(
            "<temp folder of the account that runs DCS>",
            scratch.0.to_str().unwrap(),
        )
        .replace("<DCS profile>", r"C:\Users\me\Saved Games\DCS");
    assert!(filled(&windows), "a placeholder is left:\n{windows}");
    let t = SidecarToml::parse(&windows, &scratch.0).expect("the Windows example");
    assert_eq!(t.api.listen, "127.0.0.1:47311".parse().unwrap());
    assert_eq!(
        t.missions.dcs_dir,
        r"C:\Users\me\Saved Games\DCS\nanoprobe\missions"
    );
    assert!(t.api.template_dirs.is_empty());
    // With Windows line ends, as the archive carries it.
    SidecarToml::parse(&windows.replace('\n', "\r\n"), &scratch.0).expect("CR LF");

    // The Linux archive's, in deploy/: a sidecar in a container.
    let linux = read("deploy/sidecar.toml")
        // The whole quoted value, as a literal string: a Windows host's
        // path has backslashes, which a basic string would read as escapes.
        .replace("\"/etc/nanoprobe/agent-token\"", &format!("'{token_text}'"))
        .replace("<Windows path of the DCS profile>", r"Z:\profile");
    assert!(
        linux.contains(token_text),
        "the token path was not replaced"
    );
    assert!(filled(&linux), "a placeholder is left:\n{linux}");
    let t = SidecarToml::parse(&linux, &scratch.0).expect("the Linux example");
    // On Windows `/missions` has no drive and resolves under the base.
    assert!(t.missions.dir.is_some_and(|dir| dir.ends_with("missions")));
}
