//! `nanoprobe restore`: restore a campaign's backup offline, with the sidecar
//! stopped (spec/persistence.md, "Restore a backup").

use std::io::Write;
use std::path::PathBuf;

use nanoprobe_sidecar::campaigns::Campaigns;
use nanoprobe_sidecar::ids::CampaignId;

/// What to restore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// `[store] dir`.
    pub store: PathBuf,
    /// The campaign.
    pub campaign: u64,
    /// The backup's number.
    pub backup: u32,
}

/// Parse the arguments after `restore`.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let (mut store, mut campaign, mut backup) = (None, None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--store" => store = Some(PathBuf::from(value("--store")?)),
            "--campaign" => {
                campaign = Some(
                    value("--campaign")?
                        .parse::<u64>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or("--campaign needs a campaign ID")?,
                );
            }
            "--backup" => {
                backup = Some(
                    value("--backup")?
                        .parse::<u32>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or("--backup needs a backup number")?,
                );
            }
            f => return Err(format!("unknown option `{f}` for `restore`")),
        }
    }
    Ok(Options {
        store: store.ok_or("--store is required")?,
        campaign: campaign.ok_or("--campaign is required")?,
        backup: backup.ok_or("--backup is required")?,
    })
}

/// Restore, printing what was done. Exit 0 on success, 1 on a failure.
pub fn run(o: &Options, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if !o.store.is_dir() {
        let _ = writeln!(err, "nanoprobe: {} is not a store dir", o.store.display());
        return 1;
    }
    // Refused while a sidecar holds the store dir.
    let _lock = match nanoprobe_sidecar::server::lock(&o.store) {
        Ok(l) => l,
        Err(e) => {
            let _ = writeln!(err, "nanoprobe: stop the sidecar first: {e}");
            return 1;
        }
    };
    let campaigns = match Campaigns::new(&o.store, 1, 1) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "nanoprobe: {}: {e}", o.store.display());
            return 1;
        }
    };
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    match campaigns.restore(CampaignId(o.campaign), o.backup, at_ms) {
        Ok(r) => {
            let _ = writeln!(
                out,
                "restored backup {} of campaign {} (taken at {} ms, revision {})\n\
                 the store it replaced: {}\n\
                 next entity ID {}, store revision {}; the campaign is suspended unless the backup is finished\n\
                 events.sqlite is not rolled back: it keeps the events after the backup",
                r.backup.number,
                o.campaign,
                r.backup.at_ms,
                r.backup.revision,
                r.aside.display(),
                r.next_entity,
                r.revision,
            );
            0
        }
        Err(e) => {
            let _ = writeln!(err, "nanoprobe: campaign {}: {e}", o.campaign);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Options, String> {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_the_three_options() {
        assert_eq!(
            p(&["--store", "s", "--campaign=7", "--backup", "2"]),
            Ok(Options {
                store: "s".into(),
                campaign: 7,
                backup: 2
            })
        );
        assert!(p(&["--store", "s", "--campaign", "7"]).is_err());
        assert!(p(&["--store", "s", "--campaign", "0", "--backup", "1"]).is_err());
        assert!(p(&["--x"]).is_err());
    }

    #[test]
    fn refuses_a_held_store_dir_and_restores_a_free_one() {
        use nanoprobe_sidecar::campaigns::BackupReason;
        use nanoprobe_sidecar::store::{CampaignRecord, CampaignState};
        let d = tempfile::tempdir().expect("dir");
        let c = Campaigns::new(d.path(), 1, 2).expect("campaigns");
        let record = CampaignRecord {
            name: "c".into(),
            state: CampaignState::Suspended,
            theaters: vec!["Caucasus".into()],
            theater: "Caucasus".into(),
            clock_ms: None,
            created_ms: 1,
            component_schemas: Default::default(),
            outcome: None,
        };
        let mut s = c.create(CampaignId(3), record).expect("create");
        s.commit().expect("commit");
        c.backup(CampaignId(3), 1, s.revision(), BackupReason::Manual)
            .expect("backup");
        s.close().expect("close");
        drop(s);
        let o = Options {
            store: d.path().to_path_buf(),
            campaign: 3,
            backup: 1,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let _held = nanoprobe_sidecar::server::lock(d.path()).expect("lock");
            assert_eq!(run(&o, &mut out, &mut err), 1);
            assert!(String::from_utf8_lossy(&err).contains("stop the sidecar"));
        }
        assert_eq!(
            run(&o, &mut out, &mut err),
            0,
            "{}",
            String::from_utf8_lossy(&err)
        );
        assert!(String::from_utf8_lossy(&out).contains("restored backup 1 of campaign 3"));
    }
}
