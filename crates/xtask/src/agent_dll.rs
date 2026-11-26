//! The top-level `--dll <file>`: one agent DLL that every command installing
//! the agent deploys in place of the one it would build. With it, a measuring
//! command (`sampling`, `sidecar`, `persistence`, `soak` and the rest) runs a
//! staged release's DLL, and its command line names the file that ran.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The DLL given before the command, set once in `main`.
static GIVEN: OnceLock<PathBuf> = OnceLock::new();

/// Whether a deploy installed the given DLL.
static USED: AtomicBool = AtomicBool::new(false);

/// The commands that deploy the agent. Any other command refuses a leading
/// `--dll`, so a command line never names a DLL that did not run.
const DEPLOYING: &[&str] = &[
    "deploy",
    "roundtrip",
    "run",
    "catalog",
    "commands",
    "persistence",
    "sampling",
    "sidecar",
    "soak",
];

/// Split a leading `--dll <file>` off the arguments, checking the file and
/// that the command deploys the agent.
pub fn split<'a, 'b>(args: &'a [&'b str]) -> Result<(Option<PathBuf>, &'a [&'b str]), String> {
    match args {
        ["--dll", path, rest @ ..] => {
            let command = rest.first().copied().unwrap_or_default();
            if !DEPLOYING.contains(&command) {
                return Err(format!(
                    "--dll is for a command that deploys the agent ({}); {command:?} deploys none",
                    DEPLOYING.join(", ")
                ));
            }
            let path = PathBuf::from(path);
            check(&path)?;
            Ok((Some(path), rest))
        }
        ["--dll"] => Err("--dll needs a file".into()),
        _ => Ok((None, args)),
    }
}

/// Record the DLL given before the command.
pub fn set(path: PathBuf) {
    let _ = GIVEN.set(path);
}

/// The DLL given before the command, if any.
pub fn given() -> Option<PathBuf> {
    GIVEN.get().cloned()
}

/// Note that a deploy installed the given DLL.
pub fn mark_used() {
    USED.store(true, Ordering::Relaxed);
}

/// A DLL was given and no deploy installed it: the command ran on whatever
/// build was there, or deployed nothing.
pub fn unused() -> bool {
    GIVEN.get().is_some() && !USED.load(Ordering::Relaxed)
}

/// The DLL to deploy: the command's own `--dll`, or the top-level one, or
/// `None` to build it. `--faults` builds its own DLL, so it takes neither.
pub fn choose(
    command: Option<PathBuf>,
    top: Option<PathBuf>,
    faults: bool,
) -> Result<Option<PathBuf>, String> {
    let dll = match (command, top) {
        (Some(_), Some(_)) => {
            return Err("--dll is given twice: before the command and after it".into());
        }
        (one, None) | (None, one) => one,
    };
    if faults && dll.is_some() {
        return Err("--faults builds the DLL; it does not take --dll".into());
    }
    Ok(dll)
}

/// Read `path` and refuse anything but a 64-bit x86 Windows DLL.
pub fn check(path: &Path) -> Result<Vec<u8>, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if !is_x64_dll(&bytes) {
        return Err(format!(
            "{} is not a 64-bit x86 Windows DLL",
            path.display()
        ));
    }
    Ok(bytes)
}

/// The DOS stub's `MZ`, the `PE` signature, machine x86-64 (0x8664), and the
/// DLL flag (0x2000) among the file header's characteristics.
fn is_x64_dll(bytes: &[u8]) -> bool {
    let u16_at = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(..2) != Some(b"MZ") {
        return false;
    }
    let Some(nt) = u32_at(0x3c).map(|n| n as usize) else {
        return false;
    };
    u32_at(nt) == Some(0x0000_4550)
        && u16_at(nt + 4) == Some(0x8664)
        && u16_at(nt + 22).is_some_and(|c| c & 0x2000 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest header `check` accepts: `MZ`, the `PE` signature at
    /// 0x40, machine x86-64, and `characteristics` as given.
    fn header(characteristics: u16) -> Vec<u8> {
        let mut b = vec![0u8; 0x80];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        b[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
        b[0x56..0x58].copy_from_slice(&characteristics.to_le_bytes());
        b
    }

    struct Scratch(pub PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("nanoprobe-xtask-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_leading_dll_is_split_off_and_the_command_follows() {
        let s = Scratch::new("split");
        let dll = s.file("agent.dll", &header(0x2022));
        let path = dll.to_str().unwrap();
        let args = ["--dll", path, "sampling", "live", "t"];
        let (given, rest) = split(&args).unwrap();
        assert_eq!(given, Some(dll.clone()));
        assert_eq!(rest, ["sampling", "live", "t"]);
    }

    #[test]
    fn a_command_that_deploys_no_agent_refuses_a_leading_dll() {
        let s = Scratch::new("refuse");
        let dll = s.file("agent.dll", &header(0x2022));
        let path = dll.to_str().unwrap();
        for command in ["chaos", "faults", "link", "target", "undeploy"] {
            let args = ["--dll", path, command, "t"];
            let err = split(&args).unwrap_err();
            assert!(err.contains("deploys none"), "{command}: {err}");
        }
        assert!(split(&["--dll", path]).is_err());
        for command in DEPLOYING {
            assert!(split(&["--dll", path, command, "t"]).is_ok(), "{command}");
        }
    }

    #[test]
    fn the_built_dll_takes_the_flags_of_mise_run_windows() {
        let repo = crate::repository();
        let args = crate::dll_build_args(&repo, false).unwrap();
        let base = [
            "--release",
            "-p",
            "nanoprobe-agent",
            "--target",
            crate::DLL_TRIPLE,
        ];
        assert!(args.windows(base.len()).any(|w| w == base), "{args:?}");
        assert!(!args.iter().any(|a| a == "faults"));
        let faults = crate::dll_build_args(&repo, true).unwrap();
        assert!(faults.windows(2).any(|w| w == ["--features", "faults"]));
        if cfg!(windows) {
            assert!(!args.iter().any(|a| a == "--config"), "{args:?}");
        } else {
            let out = std::process::Command::new("sh")
                .args(["tools/remap.sh", crate::DLL_TRIPLE])
                .current_dir(&repo)
                .output()
                .unwrap();
            let value = String::from_utf8(out.stdout).unwrap();
            assert!(value.contains("--remap-path-prefix="), "{value}");
            assert_eq!(args[..2], ["xwin", "build"]);
            assert_eq!(args[args.len() - 2..], ["--config", value.trim_end()]);
            assert_eq!(faults[faults.len() - 2..], ["--config", value.trim_end()]);
        }
    }

    #[test]
    fn without_a_leading_dll_the_arguments_are_unchanged() {
        let args = ["deploy", "t", "--dll", "x.dll"];
        let (given, rest) = split(&args).unwrap();
        assert_eq!(given, None);
        assert_eq!(rest, args);
        assert_eq!(choose(None, None, false).unwrap(), None);
        assert_eq!(choose(None, None, true).unwrap(), None);
    }

    #[test]
    fn a_missing_file_is_refused() {
        let s = Scratch::new("missing");
        let path = s.0.join("absent.dll");
        let args = ["--dll", path.to_str().unwrap(), "deploy", "t"];
        assert!(split(&args).unwrap_err().contains("could not read"));
        assert!(split(&["--dll"]).is_err());
    }

    #[test]
    fn a_file_that_is_not_an_x64_dll_is_refused() {
        let s = Scratch::new("notdll");
        let mut arm = header(0x2022);
        arm[0x44..0x46].copy_from_slice(&0xaa64u16.to_le_bytes());
        let mut bad_offset = header(0x2022);
        bad_offset[0x3c..0x40].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        for (name, bytes) in [
            ("text.dll", b"not a DLL at all".to_vec()),
            ("empty.dll", Vec::new()),
            ("program.exe", header(0x0022)),
            ("arm64.dll", arm),
            ("cut.dll", header(0x2022)[..0x50].to_vec()),
            ("offset.dll", bad_offset),
        ] {
            let path = s.file(name, &bytes);
            let err = check(&path).unwrap_err();
            assert!(
                err.contains("not a 64-bit x86 Windows DLL"),
                "{name}: {err}"
            );
        }
        assert!(check(&s.file("ok.dll", &header(0x2022))).is_ok());
    }

    #[test]
    fn the_given_file_is_deployed_unchanged() {
        use nanoprobe_install::{Local, Profile as _, VERSIONS, deploy};

        let s = Scratch::new("deploy");
        let mut bytes = header(0x2022);
        bytes.extend_from_slice(b"the staged agent's code");
        let dll = s.file("nanoprobe_agent.dll", &bytes);
        let root = s.0.join("profile");
        std::fs::create_dir_all(&root).unwrap();
        let local = Local::new(&root).unwrap();

        // As a measuring command deploys: with the development files.
        let payload = crate::payload(Some(dll), true, false).unwrap();
        let done = deploy(&local, &payload).unwrap();
        assert_eq!(done.build, payload.build());
        let installed = local
            .read(&format!(
                "{VERSIONS}/{0}/nanoprobe_agent_{0}.dll",
                done.build
            ))
            .unwrap();
        assert_eq!(installed, Some(bytes));
    }

    #[test]
    fn one_dll_is_taken_from_either_place_and_never_both() {
        let a = PathBuf::from("a.dll");
        let b = PathBuf::from("b.dll");
        assert_eq!(
            choose(Some(a.clone()), None, false).unwrap(),
            Some(a.clone())
        );
        assert_eq!(
            choose(None, Some(b.clone()), false).unwrap(),
            Some(b.clone())
        );
        assert!(choose(Some(a.clone()), Some(b.clone()), false).is_err());
        assert!(
            choose(None, Some(b), true)
                .unwrap_err()
                .contains("--faults")
        );
        assert!(choose(Some(a), None, true).is_err());
    }
}
