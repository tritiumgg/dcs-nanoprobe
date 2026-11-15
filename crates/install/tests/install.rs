//! Install then uninstall on fixture trees shaped like a Windows Saved Games
//! profile and a profile inside a Wine prefix. Every fixture file is made up
//! here; none is copied from a DCS install.

use std::path::{Path, PathBuf};

use nanoprobe_install::autoexec::{self, AUTOEXEC};
use nanoprobe_install::{Local, Payload, Profile as _, install, sha256, undeploy};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nanoprobe-install-it-{name}-{}",
            std::process::id()
        ));
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

/// Every path under `root` with its kind, its mode on Unix, and for a file
/// its SHA-256.
fn tree(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let mode = mode(&path);
            if path.is_dir() {
                out.push(format!("d {mode} {rel}"));
                walk(root, &path, out);
            } else {
                out.push(format!(
                    "f {mode} {} {rel}",
                    sha256(&std::fs::read(&path).unwrap())
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[cfg(unix)]
fn mode(path: &Path) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    format!(
        "{:o}",
        std::fs::metadata(path).unwrap().permissions().mode()
    )
}

#[cfg(not(unix))]
fn mode(path: &Path) -> String {
    let readonly = std::fs::metadata(path).unwrap().permissions().readonly();
    if readonly { "ro" } else { "rw" }.into()
}

/// Make a fixture file readable by its owner only, as a user may keep it.
fn private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn payload() -> Payload {
    Payload {
        loader: b"-- loader\n".to_vec(),
        dll: b"MZ fake agent".to_vec(),
        files: vec![
            ("lua/hook.lua".into(), b"-- hook\n".to_vec()),
            ("lua/log.lua".into(), b"-- log\n".to_vec()),
            ("lua/shim.lua".into(), b"-- shim\n".to_vec()),
        ],
        dev: None,
    }
}

/// The ways a profile's `autoexec.cfg` can start out.
#[derive(Clone, Copy, Debug)]
enum Autoexec {
    /// No `Config` folder.
    NoConfig,
    /// A `Config` folder without the file.
    NoFile,
    /// Other settings, ending in a line break.
    Other,
    /// Other settings, the last line without a line break, with CRLF lines.
    NoFinalBreak,
    /// An empty file.
    Empty,
}

const CASES: [Autoexec; 5] = [
    Autoexec::NoConfig,
    Autoexec::NoFile,
    Autoexec::Other,
    Autoexec::NoFinalBreak,
    Autoexec::Empty,
];

/// A profile: a log, an options file, and the case's `autoexec.cfg`.
fn profile(root: &Path, case: Autoexec, hooks: bool) {
    write(&root.join("Logs/dcs.log"), "made-up log line\n");
    if hooks {
        write(
            &root.join("Scripts/Hooks/other-hook.lua"),
            "-- another tool\n",
        );
    }
    let config = root.join("Config");
    match case {
        Autoexec::NoConfig => {}
        Autoexec::NoFile => std::fs::create_dir_all(&config).unwrap(),
        Autoexec::Other => write(
            &config.join("autoexec.cfg"),
            "options.graphics.example = 1\nnet.allow_unsafe_api = { 'gui' }\n",
        ),
        Autoexec::NoFinalBreak => write(
            &config.join("autoexec.cfg"),
            "options.example = true\r\nlog.example = 2",
        ),
        Autoexec::Empty => write(&config.join("autoexec.cfg"), ""),
    }
    if matches!(case, Autoexec::Other | Autoexec::NoFinalBreak) {
        private(&config.join("autoexec.cfg"));
    }
    if !matches!(case, Autoexec::NoConfig) {
        write(&config.join("options.lua"), "options = {}\n");
    }
}

fn round_trip(root: &Path) {
    for case in CASES {
        for hooks in [false, true] {
            let _ = std::fs::remove_dir_all(root);
            profile(root, case, hooks);
            let before = tree(root);
            let local = Local::new(root).unwrap();

            let first = install(&local, &payload()).unwrap();
            assert_eq!(first.merged, autoexec::Merged::Added, "{case:?}");
            let installed = tree(root);
            let again = install(&local, &payload()).unwrap();
            assert_eq!(again.merged, autoexec::Merged::Present, "{case:?}");
            assert_eq!(
                tree(root),
                installed,
                "{case:?}: a second install changed the tree"
            );

            let gates = autoexec::gates(&local).unwrap();
            assert!(gates.block && !gates.damaged, "{case:?}");
            assert!(gates.overrides().is_empty(), "{case:?}");

            assert!(undeploy(&local).unwrap());
            assert_eq!(tree(root), before, "{case:?} hooks={hooks}");
        }
    }
}

#[test]
fn install_then_uninstall_leaves_a_windows_profile_as_it_was() {
    let scratch = Scratch::new("windows");
    round_trip(&scratch.0.join("Users/pilot/Saved Games/DCS"));
}

#[test]
fn install_then_uninstall_leaves_a_wine_profile_as_it_was() {
    let scratch = Scratch::new("wine");
    round_trip(
        &scratch
            .0
            .join("prefix/drive_c/users/abc/Saved Games/DCS.dcs_serverrelease"),
    );
}

#[test]
fn uninstall_keeps_lines_added_around_the_block() {
    let scratch = Scratch::new("around");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::Other, false);
    let local = Local::new(&root).unwrap();
    install(&local, &payload()).unwrap();
    let mut text = local.read(AUTOEXEC).unwrap().unwrap();
    text.extend_from_slice(b"options.later = 3\n");
    local.write(AUTOEXEC, &text).unwrap();
    undeploy(&local).unwrap();
    assert_eq!(
        local.read(AUTOEXEC).unwrap().unwrap(),
        b"options.graphics.example = 1\nnet.allow_unsafe_api = { 'gui' }\noptions.later = 3\n"
    );
}

#[test]
fn a_file_install_created_stays_when_lines_were_added() {
    let scratch = Scratch::new("created");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::NoFile, false);
    let local = Local::new(&root).unwrap();
    install(&local, &payload()).unwrap();
    let mut text = b"options.mine = 1\n".to_vec();
    text.extend(local.read(AUTOEXEC).unwrap().unwrap());
    local.write(AUTOEXEC, &text).unwrap();
    undeploy(&local).unwrap();
    assert_eq!(
        local.read(AUTOEXEC).unwrap().unwrap(),
        b"options.mine = 1\n"
    );
}

#[test]
fn an_edited_block_is_refused_and_left() {
    let scratch = Scratch::new("edited");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::NoFile, false);
    let local = Local::new(&root).unwrap();
    install(&local, &payload()).unwrap();
    let text = String::from_utf8(local.read(AUTOEXEC).unwrap().unwrap()).unwrap();
    let edited = text.replace("'server'", "'gui'");
    local.write(AUTOEXEC, edited.as_bytes()).unwrap();

    let gates = autoexec::gates(&local).unwrap();
    assert!(gates.damaged && !gates.block);
    let error = install(&local, &payload()).unwrap_err();
    assert!(error.contains("edited"), "{error}");
    let error = undeploy(&local).unwrap_err();
    assert!(error.contains("by hand"), "{error}");
    assert_eq!(local.read(AUTOEXEC).unwrap().unwrap(), edited.as_bytes());
}

#[test]
fn a_later_assignment_that_drops_a_name_is_an_override() {
    let scratch = Scratch::new("later");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::NoFile, false);
    let local = Local::new(&root).unwrap();
    install(&local, &payload()).unwrap();
    let mut text = local.read(AUTOEXEC).unwrap().unwrap();
    text.extend_from_slice(
        b"net.allow_unsafe_api = { 'userhooks', 'gui' }\nnet.allow_dostring_in = { 'mission' }\n",
    );
    local.write(AUTOEXEC, &text).unwrap();
    let gates = autoexec::gates(&local).unwrap();
    assert_eq!(gates.later.len(), 2);
    let overrides = gates.overrides();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0].1, "net.allow_dostring_in");
}

#[test]
fn a_damaged_block_stops_install_before_it_writes() {
    let scratch = Scratch::new("lone");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::NoFile, false);
    write(
        &root.join(AUTOEXEC),
        "options.mine = 1\n-- nanoprobe: begin. stray\n",
    );
    let before = tree(&root);
    let local = Local::new(&root).unwrap();
    let error = install(&local, &payload()).unwrap_err();
    assert!(error.contains("no `-- nanoprobe: end` line"), "{error}");
    assert_eq!(tree(&root), before);
}

#[test]
fn a_block_whose_line_endings_changed_uninstalls() {
    let scratch = Scratch::new("crlf");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::NoFinalBreak, false);
    let local = Local::new(&root).unwrap();
    install(&local, &payload()).unwrap();
    let text = String::from_utf8(local.read(AUTOEXEC).unwrap().unwrap()).unwrap();
    let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
    local.write(AUTOEXEC, crlf.as_bytes()).unwrap();
    assert!(autoexec::gates(&local).unwrap().block);
    assert_eq!(
        install(&local, &payload()).unwrap().merged,
        autoexec::Merged::Present
    );
    undeploy(&local).unwrap();
    assert_eq!(
        local.read(AUTOEXEC).unwrap().unwrap(),
        b"options.example = true\r\nlog.example = 2"
    );
}

#[test]
fn reads_a_list_over_several_lines_and_other_forms() {
    let scratch = Scratch::new("forms");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::NoFile, false);
    let local = Local::new(&root).unwrap();
    install(&local, &payload()).unwrap();
    let mut text = local.read(AUTOEXEC).unwrap().unwrap();
    text.extend_from_slice(
        b"net.allow_dostring_in = { -- mine\n  'server', -- this one\n  'gui',\n}\nnet['allow_unsafe_api'] = {}\n",
    );
    local.write(AUTOEXEC, &text).unwrap();
    let gates = autoexec::gates(&local).unwrap();
    assert_eq!(gates.later.len(), 2, "{:?}", gates.later);
    assert_eq!(
        gates.later[0].2,
        Some(vec!["server".to_owned(), "gui".to_owned()])
    );
    assert!(gates.overrides().is_empty());
    assert_eq!(gates.unreadable().len(), 1);
    assert_eq!(gates.unreadable()[0].1, "net.allow_unsafe_api");
}

/// The block's Lua under the stock Lua 5.1 that mise provides, against a
/// stub `net` table. Skipped where no `lua` is on the path, as on Windows.
#[test]
fn the_block_adds_each_name_once_and_keeps_the_others() {
    let script = format!(
        r#"
local function run(unsafe, dostring)
  net = {{ allow_unsafe_api = unsafe, allow_dostring_in = dostring }}
  local chunk = assert(loadstring({body:?}))
  chunk()
  chunk()
  return table.concat(net.allow_unsafe_api, ','), table.concat(net.allow_dostring_in, ',')
end
local cases = {{
  {{ nil, nil, 'userhooks', 'server' }},
  {{ {{ 'gui' }}, {{ 'mission' }}, 'gui,userhooks', 'mission,server' }},
  {{ {{ 'userhooks' }}, {{ 'server', 'gui' }}, 'userhooks', 'server,gui' }},
  {{ 'text', 7, 'userhooks', 'server' }},
}}
for i, c in ipairs(cases) do
  local a, b = run(c[1], c[2])
  assert(a == c[3] and b == c[4], 'case ' .. i .. ': ' .. a .. ' / ' .. b)
end
print('ok')
"#,
        body = autoexec::body()
    );
    let output = match std::process::Command::new("lua")
        .arg("-e")
        .arg(&script)
        .output()
    {
        Ok(output) => output,
        Err(_) => {
            eprintln!("skipped: no lua on the path");
            return;
        }
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
}

#[test]
fn a_file_ending_in_a_lone_cr_keeps_it() {
    let scratch = Scratch::new("lonecr");
    let root = scratch.0.join("DCS");
    profile(&root, Autoexec::NoFile, false);
    write(&root.join(AUTOEXEC), "x = 1\r");
    let local = Local::new(&root).unwrap();
    install(&local, &payload()).unwrap();
    assert!(autoexec::gates(&local).unwrap().block);
    undeploy(&local).unwrap();
    assert_eq!(local.read(AUTOEXEC).unwrap().unwrap(), b"x = 1\r");
}
