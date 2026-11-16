//! The server's backend over a real DCS: [`RealSim`] behind
//! [`Backend`](crate::hub::Backend).

use std::path::PathBuf;
use std::time::Duration;

use nanoprobe_proto::v1alpha1::Health;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sidecar::sim::real::{RealSim, Verdict};

use crate::hub::Backend;

/// The longest theater name a mission file may carry, bytes.
pub const THEATER_MAX: usize = 64;
/// The longest path the agent takes in a `MISSION_LOAD`, bytes.
pub const PATH_MAX: usize = 1024;

/// `[missions]` (spec/config.md): the folder that holds a mission file per
/// theater, named once as the sidecar opens it and once as DCS opens it. A
/// theater's mission is `<theater>.miz`, the name exactly as `LIFECYCLE`
/// carries it.
#[derive(Clone, Debug, Default)]
pub struct Missions {
    /// `dir`; `None` when the key is empty.
    pub dir: Option<PathBuf>,
    /// `dcs_dir`.
    pub dcs_dir: String,
}

impl Missions {
    /// Refuse a `dcs_dir` whose join with the longest file name would pass
    /// the agent's path limit.
    pub fn check(&self) -> Result<(), String> {
        let longest = self.dcs_dir.len() + 1 + THEATER_MAX + ".miz".len();
        if longest > PATH_MAX {
            return Err(format!(
                "[missions] dcs_dir: joined to a {THEATER_MAX}-byte theater name it is {longest} bytes, over the agent's {PATH_MAX}"
            ));
        }
        Ok(())
    }

    /// The file for `theater`, as DCS opens it, when `<dir>/<theater>.miz`
    /// exists.
    pub fn file(&self, theater: &str) -> Result<String, String> {
        let none = |why: &str| format!("no mission for theater {theater:?}: {why}");
        let named = !theater.is_empty()
            && theater.len() <= THEATER_MAX
            && theater
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b));
        if !named {
            return Err(none("not a theater name"));
        }
        let (Some(dir), false) = (&self.dir, self.dcs_dir.is_empty()) else {
            return Err(none("[missions] dir and dcs_dir are not both set"));
        };
        let name = format!("{theater}.miz");
        // Exactly this name, also on a file system that ignores case.
        let found = std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .any(|e| e.file_name() == name.as_str() && e.path().is_file())
            })
            .unwrap_or(false);
        if !found {
            return Err(none(&format!("{} is missing", dir.join(&name).display())));
        }
        let base = self.dcs_dir.trim_end_matches(['\\', '/']);
        let sep = if self.dcs_dir.contains('\\') {
            '\\'
        } else {
            '/'
        };
        Ok(format!("{base}{sep}{name}"))
    }
}

/// A real DCS with the agent in it.
pub struct Real(pub RealSim, pub Missions);

impl Backend for Real {
    fn sim(&mut self) -> &mut dyn SimAdapter {
        &mut self.0
    }

    /// The agent runs on its own; a step only reads the status block for
    /// the verdict.
    fn frame(&mut self) {
        self.0.watch();
    }

    fn attach(&mut self, _sidecar_boot: u64) {
        // The link makes a sidecar boot of its own for each session loop.
        self.0.attach();
    }

    fn detach(&mut self) {
        self.0.detach();
    }

    /// Never called: [`Real::mission_file`] always names a file or refuses,
    /// and the reconciler sends the `MISSION_LOAD`.
    fn load_mission(&mut self, _theater: &str) {}

    fn mission_file(&self, theater: &str) -> Result<Option<String>, String> {
        self.1.file(theater).map(Some)
    }

    fn health(&self) -> Health {
        match self.0.verdict() {
            Verdict::Healthy => Health::Healthy,
            Verdict::Hung => Health::Hung,
            Verdict::Gone => Health::Gone,
            Verdict::Disabled => Health::Disabled,
        }
    }

    fn heartbeat_age(&self) -> Option<Duration> {
        self.0.heartbeat_age()
    }

    fn attach_error(&self) -> String {
        self.0.attach_error()
    }

    fn max_command(&self) -> usize {
        self.0.max_command().unwrap_or(1024 * 1024 / 8)
    }

    fn as_any(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut self.0)
    }

    fn achieved(&self) -> Option<[f64; 4]> {
        self.0.achieved()
    }
}
