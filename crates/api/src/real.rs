//! The server's backend over a real DCS: [`RealSim`] behind
//! [`Backend`](crate::hub::Backend).

use std::path::{Path, PathBuf};
use std::time::Duration;

use nanoprobe_proto::v1alpha1::Health;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sidecar::sim::real::{RealSim, Verdict};

use crate::hub::Backend;

/// The longest path the agent takes in a `MISSION_LOAD`, bytes.
pub const PATH_MAX: usize = 1024;

/// `[missions]` (spec/config.md): the folder that holds the shared
/// template of each theater and, under `shells/`, the shell of each
/// campaign, named once as the sidecar opens it and once as DCS opens it.
/// A theater's template is `<theater>.miz`, the name exactly as
/// `LIFECYCLE` carries it; a campaign's shells are `shells/<campaign ID>.a.miz`
/// and `.b.miz`.
#[derive(Clone, Debug, Default)]
pub struct Missions {
    /// `dir`; `None` when the key is empty.
    pub dir: Option<PathBuf>,
    /// `dcs_dir`.
    pub dcs_dir: String,
}

/// The longest shell name: `shells`, a separator, 20 digits and `.a.miz`.
const SHELL_MAX: usize = "shells".len() + 1 + 20 + ".a.miz".len();

impl Missions {
    /// Refuse a `dcs_dir` whose join with the longest shell name would pass
    /// the agent's path limit.
    pub fn check(&self) -> Result<(), String> {
        let longest = self.dcs_dir.len() + 1 + SHELL_MAX;
        if longest > PATH_MAX {
            return Err(format!(
                "[missions] dcs_dir: joined to the longest shell name it is {longest} bytes, over the agent's {PATH_MAX}"
            ));
        }
        Ok(())
    }

    /// `dir`, when both keys are set.
    pub fn dir(&self) -> Result<&Path, String> {
        match (&self.dir, self.dcs_dir.is_empty()) {
            (Some(dir), false) => Ok(dir),
            _ => Err("[missions] dir and dcs_dir are not both set".into()),
        }
    }

    /// The shared template of `theater`: the bytes of `<dir>/<theater>.miz`.
    pub fn template(&self, theater: &str) -> Result<Vec<u8>, String> {
        let none = |why: &str| format!("no template for theater {theater:?}: {why}");
        if !nanoprobe_sidecar::shell::is_theater_name(theater) {
            return Err(none("not a theater name"));
        }
        let dir = self.dir().map_err(|e| none(&e))?;
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
        std::fs::read(dir.join(&name)).map_err(|e| none(&e.to_string()))
    }

    /// Campaign `campaign`'s shell in `slot` as DCS opens it: `dcs_dir`
    /// joined to `shells/<campaign>.<slot>.miz` with `dcs_dir`'s separator.
    pub fn shell_dcs_path(&self, campaign: u64, slot: u8) -> String {
        let base = self.dcs_dir.trim_end_matches(['\\', '/']);
        let sep = if self.dcs_dir.contains('\\') {
            '\\'
        } else {
            '/'
        };
        format!(
            "{base}{sep}{}{sep}{}",
            nanoprobe_sidecar::shell::SHELLS,
            nanoprobe_sidecar::shell::shell_name(campaign, slot)
        )
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

    /// Never called: a real DCS loads the shell the hub writes, with the
    /// `MISSION_LOAD` the reconciler sends.
    fn load_mission(&mut self, _theater: &str) {}

    fn missions(&self) -> Option<&Missions> {
        Some(&self.1)
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
