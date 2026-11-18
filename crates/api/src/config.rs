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
    /// `[store] removed_keep_h`: how long a removed entity's rows stay
    /// before the purge may take them (spec/world.md, "Removal").
    pub removed_keep: Duration,
    /// `[store] dead_keep_h`: how long a dead group stays before it
    /// expires; zero never (spec/world.md, "Expiry").
    pub dead_keep: Duration,
    /// `[store] event_retention_days`; zero keeps every event. On disk and,
    /// with the store off, in memory.
    pub event_retention: Duration,
    /// `[store] server_log_days`: the server event log's retention, on disk
    /// and, with the store off, in memory.
    pub server_log_retention: Duration,
    /// `[store] journal_retention_days`; zero keeps every journal entry. On
    /// disk and, with the store off, in memory.
    pub journal_retention: Duration,
    /// `[store] events_max_mb` in bytes; zero is no size limit. The bytes
    /// of the event bodies kept per campaign, on disk and, with the store
    /// off, in memory. The server event log keeps its time limit only.
    pub events_max_bytes: u64,
    /// `[store] journal_max_mb` in bytes; zero is no size limit. The bytes
    /// of the journal entry bodies kept per campaign, on disk and, with the
    /// store off, in memory.
    pub journal_max_bytes: u64,
    /// The most events held in memory per campaign and in the server log.
    /// The default sets no count limit; a smaller number is for tests. The
    /// retentions (`event_retention_days`, `events_max_mb`,
    /// `server_log_days`) apply on disk and in memory.
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
    /// `[missions] template_dirs`: the folders a `SetCampaignTemplate` may
    /// name a file in. Empty refuses every path.
    pub template_dirs: Vec<PathBuf>,
}

/// The default of `[store] journal_retention_days` (spec/config.md): 30
/// days. Zero keeps the journal for the life of the campaign.
pub const JOURNAL_RETENTION_DEFAULT: Duration = Duration::from_secs(30 * 86_400);

/// A megabyte in `[store] journal_max_mb` and `events_max_mb`.
pub const MEGABYTE: u64 = 1_048_576;

/// The default of `[store] journal_max_mb` (spec/config.md): 1,024.
pub const JOURNAL_MAX_MB_DEFAULT: u64 = 1_024;

/// The default of `[store] events_max_mb` (spec/config.md): 1,024.
pub const EVENTS_MAX_MB_DEFAULT: u64 = 1_024;

/// The default of `[store] event_retention_days` (spec/config.md): 90 days.
pub const EVENT_RETENTION_DEFAULT: Duration = Duration::from_secs(90 * 86_400);

/// The default of `[store] removed_keep_h` (spec/config.md): one hour.
pub const REMOVED_KEEP_DEFAULT: Duration = Duration::from_secs(3_600);

/// The default of `[store] dead_keep_h` (spec/config.md): 24 hours.
pub const DEAD_KEEP_DEFAULT: Duration = Duration::from_secs(24 * 3_600);

/// The default of `[store] server_log_days` (spec/config.md): 30 days. It
/// also bounds the server log with the store off.
pub const SERVER_LOG_RETENTION_DEFAULT: Duration = Duration::from_secs(30 * 86_400);

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
}

impl StoreFiles {
    /// spec/config.md's defaults, in `dir`.
    pub fn new(dir: PathBuf) -> StoreFiles {
        StoreFiles {
            dir,
            checkpoint: Duration::from_secs(5),
            backup_interval: Duration::from_secs(24 * 3600),
            backup_keep: 7,
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
            removed_keep: REMOVED_KEEP_DEFAULT,
            dead_keep: DEAD_KEEP_DEFAULT,
            event_retention: EVENT_RETENTION_DEFAULT,
            server_log_retention: SERVER_LOG_RETENTION_DEFAULT,
            journal_retention: JOURNAL_RETENTION_DEFAULT,
            events_max_bytes: EVENTS_MAX_MB_DEFAULT * MEGABYTE,
            journal_max_bytes: JOURNAL_MAX_MB_DEFAULT * MEGABYTE,
            events_keep: usize::MAX,
            position_period_min: Duration::from_millis(100),
            materialize_per_frame: 1,
            destroy_retries: 3,
            on_lost: nanoprobe_sidecar::store::LostPolicy::Report,
            step: Duration::from_millis(2),
            store: None,
            template_dirs: Vec::new(),
        }
    }
}
