//! The API server's settings: the `[api]` keys of spec/config.md and the
//! `[store]` and `[world]` keys it reads.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// The server's settings. [`Config::default`] gives spec/config.md's
/// defaults.
#[derive(Clone, Debug)]
pub struct Config {
    /// `[api] listen`.
    pub listen: SocketAddr,
    /// `[api] tokens_path`; empty for none.
    pub tokens_path: Option<PathBuf>,
    /// `[api] lease_ttl_s`.
    pub lease_ttl: Duration,
    /// `[api] stream_buffer`: messages a stream may fall behind.
    pub stream_buffer: usize,
    /// `[api] result_wait_s`.
    pub result_wait: Duration,
    /// `[store] idempotency_keep_h`.
    pub idempotency_keep: Duration,
    /// `[store] world_changes_keep`.
    pub world_changes_keep: usize,
    /// Events kept per campaign and in the server log. The default keeps
    /// every one, as `event_retention_days = 0` does (spec/config.md); a
    /// smaller number is for tests. `event_retention_days` applies to the
    /// history on disk.
    pub events_keep: usize,
    /// `[world] position_period_min_s`: the cap on position rates.
    pub position_period_min: Duration,
    /// `[world] materialize_per_frame`: spawns unanswered at a time outside
    /// the restore burst.
    pub materialize_per_frame: usize,
    /// `[world] destroy_retries`.
    pub destroy_retries: u32,
    /// `[world] on_lost`: the default policy for a lost group, of every
    /// campaign without its own.
    pub on_lost: nanoprobe_sidecar::store::LostPolicy,
    /// How often the driver reads the agent and steps the reconciler
    /// (`[agent] poll_ms` for a real agent; one fake frame for the fake).
    pub step: Duration,
    /// The `[store]` files; `None` keeps every store in memory, as
    /// `[store] enabled = false` does.
    pub store: Option<StoreFiles>,
}

/// The `[store]` keys of a store on disk (spec/config.md,
/// spec/persistence.md).
#[derive(Clone, Debug)]
pub struct StoreFiles {
    /// `[store] dir`: one folder per campaign.
    pub dir: PathBuf,
    /// `[store] checkpoint_s`.
    pub checkpoint: Duration,
    /// `[store] backup_interval_h`.
    pub backup_interval: Duration,
    /// `[store] backup_keep`.
    pub backup_keep: usize,
    /// `[store] event_retention_days`; zero keeps every event.
    pub event_retention: Duration,
    /// `[store] server_log_days`.
    pub server_log_retention: Duration,
}

impl StoreFiles {
    /// spec/config.md's defaults, in `dir`.
    pub fn new(dir: PathBuf) -> StoreFiles {
        StoreFiles {
            dir,
            checkpoint: Duration::from_secs(5),
            backup_interval: Duration::from_secs(24 * 3600),
            backup_keep: 7,
            event_retention: Duration::ZERO,
            server_log_retention: Duration::from_secs(30 * 24 * 3600),
        }
    }
}

impl Default for Config {
    fn default() -> Config {
        Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 47311)),
            tokens_path: None,
            lease_ttl: Duration::from_secs(30),
            stream_buffer: 1024,
            result_wait: Duration::from_secs(10),
            idempotency_keep: Duration::from_secs(24 * 3600),
            world_changes_keep: 100_000,
            events_keep: usize::MAX,
            position_period_min: Duration::from_millis(100),
            materialize_per_frame: 1,
            destroy_retries: 3,
            on_lost: nanoprobe_sidecar::store::LostPolicy::Report,
            step: Duration::from_millis(2),
            store: None,
        }
    }
}
