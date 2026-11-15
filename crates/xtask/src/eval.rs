//! The host side of the eval channel, as `spec/eval.md` lays it out: write a
//! request into `in/`, wait for the result in `out/`.

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::install::{EVAL, EVAL_HOOK, Profile};

/// How often the result folder is polled.
const POLL: Duration = Duration::from_millis(500);
/// The longest request ID.
const MAX_ID: usize = 64;

/// A request ID: `stem` reduced to `A-Z a-z 0-9 _ -` and cut, then 8 random
/// hex digits, at most 64 characters in all.
pub fn make_id(stem: &str) -> Result<String, String> {
    let mut random = [0u8; 4];
    getrandom::fill(&mut random).map_err(|e| format!("no random numbers: {e}"))?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut stem: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(MAX_ID - suffix.len() - 1)
        .collect();
    if stem.is_empty() {
        stem.push_str("eval");
    }
    Ok(format!("{stem}-{suffix}"))
}

/// Run `source` in `state` and return the result object the hook wrote. Waits
/// up to `timeout` for it.
pub fn run(
    profile: &dyn Profile,
    state: &str,
    stem: &str,
    source: &[u8],
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    run_polling(profile, state, stem, source, timeout, POLL)
}

/// [`run`] with a chosen polling interval.
fn run_polling(
    profile: &dyn Profile,
    state: &str,
    stem: &str,
    source: &[u8],
    timeout: Duration,
    poll: Duration,
) -> Result<serde_json::Value, String> {
    if state.is_empty() || !state.bytes().all(|b| b.is_ascii_lowercase()) {
        return Err(format!(
            "state {state:?} must be one or more lower-case ASCII letters"
        ));
    }
    if profile.read(EVAL_HOOK)?.is_none() {
        return Err(format!(
            "the eval hook is not installed: run `deploy <name> --dev`, then `restart` \
             ({EVAL_HOOK} is missing)"
        ));
    }
    let id = make_id(stem)?;
    let request = format!("{EVAL}/in/{id}.{state}.lua");
    let taken = format!("{EVAL}/taken/{id}.{state}.lua");
    let result = format!("{EVAL}/out/{id}.json");
    profile.write(&request, source)?;

    let start = Instant::now();
    loop {
        if let Some(bytes) = profile.read(&result)? {
            return finish(profile, &bytes, &result, &taken);
        }
        if start.elapsed() >= timeout {
            break;
        }
        sleep(poll.min(timeout.saturating_sub(start.elapsed())));
    }

    // One last look, in case the result landed during the final sleep.
    if let Some(bytes) = profile.read(&result)? {
        return finish(profile, &bytes, &result, &taken);
    }
    if profile.read(&request)?.is_some() {
        profile.remove_file(&request)?;
        return Err(format!(
            "{id}: the hook did not take the request in {}s; DCS is not running, the hook \
             is not loaded, or the kill switch is set",
            timeout.as_secs()
        ));
    }
    Err(format!(
        "{id}: the hook took the request and no result came in {}s; the snippet is still \
         running, or DCS stopped ({taken})",
        timeout.as_secs()
    ))
}

/// Parse a result that has appeared, and remove it and its taken request.
fn finish(
    profile: &dyn Profile,
    bytes: &[u8],
    result: &str,
    taken: &str,
) -> Result<serde_json::Value, String> {
    let value = serde_json::from_slice(bytes).map_err(|e| format!("{result} is not JSON: {e}"));
    profile.remove_file(result)?;
    profile.remove_file(taken)?;
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::Local;

    /// A scratch folder's path, removed when the test ends, whether it passes
    /// or fails.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for Scratch {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    fn scratch(name: &str) -> (Scratch, Local) {
        let dir =
            std::env::temp_dir().join(format!("nanoprobe-eval-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = Scratch(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        let local = Local::new(&dir).unwrap();
        (guard, local)
    }

    #[test]
    fn ids_have_the_right_shape() {
        let id = make_id("probe name!").unwrap();
        let (stem, suffix) = id.rsplit_once('-').unwrap();
        assert_eq!(stem, "probe_name_");
        assert_eq!(suffix.len(), 8);
        assert!(suffix.bytes().all(|b| b.is_ascii_hexdigit()));

        let long = make_id(&"a".repeat(200)).unwrap();
        assert_eq!(long.len(), 64);
        assert!(make_id("").unwrap().starts_with("eval-"));
        assert_ne!(make_id("x").unwrap(), make_id("x").unwrap());
    }

    #[test]
    fn refuses_a_bad_state_and_a_missing_hook() {
        let (_dir, local) = scratch("refuse");
        let timeout = Duration::from_millis(50);
        assert!(run(&local, "GUI", "x", b"", timeout).is_err());
        assert!(run(&local, "", "x", b"", timeout).is_err());
        let error = run(&local, "gui", "x", b"", timeout).unwrap_err();
        assert!(error.contains("deploy <name> --dev"), "{error}");
    }

    #[test]
    fn returns_the_result_a_hook_writes() {
        let (dir, local) = scratch("hook");
        local.write(EVAL_HOOK, b"-- hook\n").unwrap();
        let player_dir = dir.to_path_buf();
        let player = std::thread::spawn(move || {
            let local = Local::new(&player_dir).unwrap();
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                for name in local.list(&format!("{EVAL}/in")).unwrap() {
                    if name.ends_with(".partial") {
                        continue;
                    }
                    let bytes = local.read(&format!("{EVAL}/in/{name}")).unwrap().unwrap();
                    assert_eq!(bytes, b"return 1\n");
                    local
                        .write(&format!("{EVAL}/taken/{name}"), &bytes)
                        .unwrap();
                    local.remove_file(&format!("{EVAL}/in/{name}")).unwrap();
                    let id = name.strip_suffix(".gui.lua").unwrap();
                    let json = format!(r#"{{"id":"{id}","state":"gui","ok":true}}"#);
                    local
                        .write(&format!("{EVAL}/out/{id}.json"), json.as_bytes())
                        .unwrap();
                    return;
                }
                sleep(Duration::from_millis(5));
            }
            panic!("no request arrived");
        });
        let value = run_polling(
            &local,
            "gui",
            "probe",
            b"return 1\n",
            Duration::from_secs(10),
            Duration::from_millis(5),
        )
        .unwrap();
        player.join().unwrap();
        assert_eq!(value["ok"], true);
        assert!(value["id"].as_str().unwrap().starts_with("probe-"));
        assert!(local.list(&format!("{EVAL}/out")).unwrap().is_empty());
        assert!(local.list(&format!("{EVAL}/taken")).unwrap().is_empty());
        assert!(local.list(&format!("{EVAL}/in")).unwrap().is_empty());
    }

    #[test]
    fn a_request_nobody_takes_is_removed() {
        let (_dir, local) = scratch("untaken");
        local.write(EVAL_HOOK, b"-- hook\n").unwrap();
        let error = run_polling(
            &local,
            "server",
            "x",
            b"return 1\n",
            Duration::from_millis(50),
            Duration::from_millis(5),
        )
        .unwrap_err();
        assert!(error.contains("did not take"), "{error}");
        assert!(local.list(&format!("{EVAL}/in")).unwrap().is_empty());
    }

    #[test]
    fn a_taken_request_with_no_result_stays() {
        let (dir, local) = scratch("taken");
        local.write(EVAL_HOOK, b"-- hook\n").unwrap();
        let player_dir = dir.to_path_buf();
        let player = std::thread::spawn(move || {
            let local = Local::new(&player_dir).unwrap();
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                for name in local.list(&format!("{EVAL}/in")).unwrap() {
                    if name.ends_with(".partial") {
                        continue;
                    }
                    let bytes = local.read(&format!("{EVAL}/in/{name}")).unwrap().unwrap();
                    local
                        .write(&format!("{EVAL}/taken/{name}"), &bytes)
                        .unwrap();
                    local.remove_file(&format!("{EVAL}/in/{name}")).unwrap();
                    return;
                }
                sleep(Duration::from_millis(5));
            }
        });
        let error = run_polling(
            &local,
            "gui",
            "x",
            b"while true do end\n",
            Duration::from_millis(400),
            Duration::from_millis(5),
        )
        .unwrap_err();
        player.join().unwrap();
        assert!(error.contains("took the request"), "{error}");
        assert_eq!(local.list(&format!("{EVAL}/taken")).unwrap().len(), 1);
    }
}
