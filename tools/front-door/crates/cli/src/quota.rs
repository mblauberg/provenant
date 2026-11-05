//! Quota arguments, composition through OS crate's safe filesystem, and render.
#[path = "quota_notice.rs"]
mod notice;
use crate::metadata::CatalogueArg;
use clap::{Args, Subcommand, ValueEnum};
use provenant_core::{CancelToken, Clock, Roots, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, FsError, LockMode, Publication, PublishMode},
};
use provenant_quota::{
    schedule::{self, Detection, QuotaQuery, QuotaTick},
    *,
};
use provenant_records::{CommandError, CommandResult, Outcome};
use std::{
    ffi::OsStr,
    io::{self, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Args)]
pub struct Filter {
    /// Restrict to this adapter
    #[arg(long)]
    pub adapter: Option<String>,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Trigger {
    Timer,
    Panel,
    Manual,
    Wake,
    Power,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DisplayHeadline {
    Auto,
    #[value(name = "5h")]
    FiveHour,
    Weekly,
}
impl From<DisplayHeadline> for Headline {
    fn from(value: DisplayHeadline) -> Self {
        match value {
            DisplayHeadline::Auto => Self::Auto,
            DisplayHeadline::FiveHour => Self::FiveHour,
            DisplayHeadline::Weekly => Self::Weekly,
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Power {
    Ac,
    Battery,
    Low,
}
#[derive(Debug, Subcommand)]
pub enum Quota {
    /// Show recorded provider quota
    Show(Filter),
    /// Show recorded use by account and project, with chair sessions and lane tokens
    Usage {
        #[command(flatten)]
        adapter: Filter,
        /// Days to include, as a rolling window back from now
        #[arg(long, default_value_t = 7, catalog_u32_range = 1..=365)]
        days: u32,
    },
    /// List recorded quota readings with resets, plan changes and the plan price in force
    ///
    /// Readings are kept by `quota tick` and refreshes, one per distinct
    /// reading, oldest first. `--since` and `--until` take seconds ago, a date
    /// or an RFC3339 timestamp. With `--limit` the newest readings are kept.
    History {
        /// Account label or key
        #[arg(long)]
        account: Option<String>,
        /// Earliest reading
        #[arg(long, value_parser = crate::grammar::since)]
        since: Option<jiff::Timestamp>,
        /// Latest reading
        #[arg(long, value_parser = crate::grammar::since)]
        until: Option<jiff::Timestamp>,
        /// Most readings to return
        #[arg(long, default_value_t = 1000, catalog_u32_range = 1..=100_000)]
        limit: u32,
    },
    /// Set menu bar display choices, then show quota
    ///
    /// With no options nothing changes. `--icon` takes `most_constrained` or an
    /// account entry id or unique label, and stores the entry id.
    Display {
        /// Window the menu bar leads with
        #[arg(long, value_enum)]
        headline: Option<DisplayHeadline>,
        /// Account the menu bar icon follows, or `most_constrained`
        #[arg(long)]
        icon: Option<String>,
    },
    /// Refresh quota and record a pacing tick
    Tick {
        #[command(flatten)]
        adapter: Filter,
        /// What caused this tick
        #[arg(long, value_enum, default_value = "manual")]
        trigger: Trigger,
        /// Current power state
        #[arg(long, value_enum, default_value = "ac")]
        power: Power,
    },
}
impl From<Trigger> for schedule::Trigger {
    fn from(value: Trigger) -> Self {
        match value {
            Trigger::Timer => Self::Timer,
            Trigger::Panel => Self::Panel,
            Trigger::Manual => Self::Manual,
            Trigger::Wake => Self::Wake,
            Trigger::Power => Self::Power,
        }
    }
}
impl From<Power> for schedule::Power {
    fn from(value: Power) -> Self {
        match value {
            Power::Ac => Self::Ac,
            Power::Battery => Self::Battery,
            Power::Low => Self::Low,
        }
    }
}
#[derive(Debug, Error)]
pub(crate) enum EdgeError {
    #[error(transparent)]
    Usage(#[from] provenant_telemetry::usage::UsageError),
    #[error(transparent)]
    Telemetry(#[from] provenant_telemetry::TelemetryError),
    #[error(transparent)]
    Run(#[from] provenant_run::RunError),
    #[error(transparent)]
    Quota(#[from] QuotaError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error("cannot encode quota record")]
    Encode(#[from] serde_json::Error),
    #[error("cannot read quota record at {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("quota state requires private owned directories")]
    Permissions,
    #[error("quota time is out of range")]
    Time,
    #[error("invalid quota adapter")]
    Adapter,
    #[error("quota report exceeds 1 MiB; filter with --adapter")]
    TooLarge,
    #[error(
        "quota history report exceeds 16 MiB; filter with --account, --since or --until, or lower --limit"
    )]
    HistoryTooLarge,
    #[error("{0}")]
    Display(String),
    #[error("quota report path is not UTF-8")]
    Path,
}
fn durable(publication: Publication) -> Result<(), EdgeError> {
    Ok(publication.require_durable()?)
}
fn create_path(path: &Path) -> Result<Directory, EdgeError> {
    let dir = match Directory::open_anchor(path) {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => {
            let parent = Directory::open_anchor(path.parent().ok_or(EdgeError::Permissions)?)?;
            let name = path.file_name().ok_or(EdgeError::Permissions)?;
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(e) if e.is_exists() => {}
                Err(e) => return Err(e.into()),
            }
            parent.open_verified_directory(name)?
        }
        Err(error) => return Err(error.into()),
    };
    Ok(dir)
}
pub(crate) fn private(path: &Path) -> Result<Directory, EdgeError> {
    let dir = create_path(path)?;
    let meta = dir.metadata()?;
    if meta.mode & 0o077 != 0 || meta.uid != provenant_os::fs::effective_uid() {
        return Err(EdgeError::Permissions);
    }
    Ok(dir)
}
pub(crate) fn child(parent: &Directory, name: &OsStr) -> Result<Directory, EdgeError> {
    match parent.mkdir(name, 0o700) {
        Ok(()) => parent.sync()?,
        Err(e) if e.is_exists() => {}
        Err(e) => return Err(e.into()),
    }
    let dir = parent.open_verified_directory(name)?;
    let meta = dir.metadata()?;
    if meta.mode & 0o077 != 0 || meta.uid != provenant_os::fs::effective_uid() {
        return Err(EdgeError::Permissions);
    }
    Ok(dir)
}
fn check_private(dir: &Directory) -> Result<(), EdgeError> {
    let meta = dir.metadata()?;
    if meta.mode & 0o777 != 0o700 || meta.uid != provenant_os::fs::effective_uid() {
        return Err(EdgeError::Permissions);
    }
    Ok(())
}
fn read_records(dir: &Directory) -> Result<Vec<AccountRecord>, EdgeError> {
    check_private(dir)?;
    let mut records = Vec::new();
    for entry in dir.entries()? {
        let entry = entry?;
        if entry == "report.json"
            || entry == "quota.lock"
            || entry == crate::quota_history::DIRECTORY
            || entry.to_str().is_some_and(|s| s.starts_with('.'))
        {
            continue;
        }
        let Some(key) = entry.to_str().filter(|s| component(s)) else {
            return Err(QuotaError::Invalid("account directory").into());
        };
        let account_dir = dir.open_verified_directory(&entry)?;
        check_private(&account_dir)?;
        for file in account_dir.entries()? {
            let file = file?;
            if file.to_str().is_some_and(|s| s.starts_with('.')) {
                continue;
            }
            let name = file
                .to_str()
                .and_then(|s| s.strip_suffix(".json"))
                .filter(|s| component(s))
                .ok_or(QuotaError::Invalid("snapshot path"))?;
            let mut bytes = Vec::new();
            let reader = account_dir.open_read(&file)?;
            let meta = reader.metadata()?;
            if meta.mode & 0o777 != 0o600 || meta.uid != provenant_os::fs::effective_uid() {
                return Err(EdgeError::Permissions);
            }
            reader
                .take(MAX_RECORD_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|source| EdgeError::Read {
                    path: PathBuf::from(key).join(&file),
                    source,
                })?;
            let record = AccountRecord::decode(&bytes)?;
            if record.account.adapter != name || record.account.account_key != key {
                return Err(QuotaError::Invalid("snapshot path identity").into());
            }
            records.push(record);
            if records.len() > MAX_ACCOUNTS {
                return Err(QuotaError::Invalid("account bound").into());
            }
        }
    }
    records.sort_by(|a, b| {
        (&a.account.adapter, &a.account.account_key)
            .cmp(&(&b.account.adapter, &b.account.account_key))
    });
    Ok(records)
}

/// Read validated quota state without creating directories or refreshing providers.
pub(crate) fn read_cache(roots: &Roots) -> Result<Vec<AccountRecord>, EdgeError> {
    read_cache_at(roots.state.as_path())
}

pub(crate) fn read_cache_at(state: &Path) -> Result<Vec<AccountRecord>, EdgeError> {
    let root = match Directory::open_anchor(state) {
        Ok(root) => root,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let directory = match root.open_verified_directory(OsStr::new("quota")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    check_private(&root)?;
    read_records(&directory)
}

/// Persist only typed quota scope from a provider event; its message stays in
/// the redacted attempt reason and is never copied into quota state. Without a
/// pool the refusal holds only the route's model.
pub(crate) fn persist_provider_refusal(
    state_root: &Path,
    route: &provenant_records::run::RouteRecord,
    pool: Option<&str>,
    now_unix_seconds: i64,
    resets_unix_seconds: Option<i64>,
) -> Result<(), EdgeError> {
    let adapter = route
        .provider
        .as_deref()
        .ok_or(QuotaError::Invalid("provider refusal route"))?;
    let model = route
        .model
        .as_deref()
        .ok_or(QuotaError::Invalid("provider refusal route"))?;
    let account = AccountScope {
        adapter: adapter.to_owned(),
        account_key: route
            .account_key
            .clone()
            .ok_or(QuotaError::Invalid("provider refusal account"))?,
        label: route
            .account_label
            .clone()
            .ok_or(QuotaError::Invalid("provider refusal account label"))?,
    };
    account.validate()?;

    let root = private(state_root)?;
    let directory = child(&root, OsStr::new("quota"))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 1).map_err(|_| EdgeError::Time)?;
    let _lock = directory.lock(
        OsStr::new("quota.lock"),
        LockMode::Exclusive,
        budget,
        &cancel,
        &clock,
    )?;
    let mut records = read_records(&directory)?;
    let current = records
        .iter()
        .find(|record| {
            record.account.adapter == account.adapter
                && record.account.account_key == account.account_key
        })
        .cloned();
    let account = current.as_ref().map_or(&account, |record| &record.account);
    let intent = provenant_quota::record_refusal(
        current.as_ref(),
        account,
        model,
        pool,
        now_unix_seconds,
        resets_unix_seconds,
    )?;
    write_intent(&directory, state_root, &intent, &mut records)
}

pub(crate) fn persist_capacity_cooldown(
    state_root: &Path,
    route: &provenant_records::run::RouteRecord,
    now_unix_seconds: i64,
    duration_seconds: u64,
) -> Result<(), EdgeError> {
    if duration_seconds == 0 {
        return Ok(());
    }
    let adapter = route
        .provider
        .as_deref()
        .ok_or(QuotaError::Invalid("capacity cooldown route"))?;
    let model = route
        .model
        .as_deref()
        .ok_or(QuotaError::Invalid("capacity cooldown route"))?;
    let account = AccountScope {
        adapter: adapter.to_owned(),
        account_key: route
            .account_key
            .clone()
            .ok_or(QuotaError::Invalid("capacity cooldown account"))?,
        label: route
            .account_label
            .clone()
            .ok_or(QuotaError::Invalid("capacity cooldown account label"))?,
    };
    account.validate()?;

    let root = private(state_root)?;
    let directory = child(&root, OsStr::new("quota"))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 1).map_err(|_| EdgeError::Time)?;
    let _lock = directory.lock(
        OsStr::new("quota.lock"),
        LockMode::Exclusive,
        budget,
        &cancel,
        &clock,
    )?;
    let mut records = read_records(&directory)?;
    let current = records
        .iter()
        .find(|record| {
            record.account.adapter == account.adapter
                && record.account.account_key == account.account_key
        })
        .cloned();
    let account = current.as_ref().map_or(&account, |record| &record.account);
    let Some(intent) = provenant_quota::record_capacity_cooldown(
        current.as_ref(),
        account,
        model,
        now_unix_seconds,
        duration_seconds,
    )?
    else {
        return Ok(());
    };
    write_intent(&directory, state_root, &intent, &mut records)
}

/// Record successful account-scoped provider evidence, without inventing quota.
pub(crate) fn persist_provider_success(
    state_root: &Path,
    route: &provenant_records::run::RouteRecord,
    now: i64,
) -> Result<(), EdgeError> {
    let (Some(adapter), Some(key)) = (route.provider.as_deref(), route.account_key.as_deref())
    else {
        return Ok(());
    };
    let root = match Directory::open_anchor(state_root) {
        Ok(root) => root,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let directory = match root.open_verified_directory(OsStr::new("quota")) {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    check_private(&root)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 1).map_err(|_| EdgeError::Time)?;
    let _lock = directory.lock(
        OsStr::new("quota.lock"),
        LockMode::Exclusive,
        budget,
        &cancel,
        &clock,
    )?;
    let mut records = read_records(&directory)?;
    let current = records
        .iter()
        .find(|r| r.account.adapter == adapter && r.account.account_key == key)
        .cloned();
    let mut next = match &current {
        Some(record) => record.clone(),
        None => {
            let Some(label) = route.account_label.as_ref() else {
                return Ok(());
            };
            let account = AccountScope {
                adapter: adapter.into(),
                account_key: key.into(),
                label: label.clone(),
            };
            account.validate()?;
            AccountRecord::empty(account)
        }
    };
    // A lane launched before a mismatch is not evidence of the current
    // signed-in identity, and must not supersede the verifying probe.
    if next.last_error == Some(ProbeFailure::AccountMismatch) {
        return Ok(());
    }
    next.auth_verified_unix_seconds = Some(now);
    next.auth_retry_pending = false;
    // A probe started before this lane may not overwrite newer evidence.
    next.claim = None;
    if next.last_error.is_some_and(ProbeFailure::needs_login) {
        next.last_error = None;
        next.failures = 0;
        next.backoff_until_unix_seconds = None;
        next.last_attempt_unix_seconds = None;
    }
    if next
        .snapshot
        .as_ref()
        .is_some_and(|s| s.status == QuotaStatus::AuthRequired)
    {
        next.snapshot = None;
    }
    next.revision = next
        .revision
        .checked_add(1)
        .ok_or(QuotaError::NumberOutOfRange)?;
    let intent = UpdateIntent {
        account: next.account.clone(),
        expected: current,
        replacement: Some(next),
        readiness_probe_result: None,
        login_probe_expiry: None,
    };
    write_intent(&directory, state_root, &intent, &mut records)?;

    // Publish the changed row so an already-open menu receives the event.
    let mut bytes = Vec::new();
    let reader = match directory.open_read(OsStr::new("report.json")) {
        Ok(reader) => reader,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    reader
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| EdgeError::Read {
            path: PathBuf::from("report.json"),
            source,
        })?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(EdgeError::TooLarge);
    }
    let mut report: QuotaReport = serde_json::from_slice(&bytes)?;
    if report.generated_unix_seconds > now {
        return Ok(());
    }
    if let Some(row) = report
        .accounts
        .iter_mut()
        .find(|r| r.account.adapter == adapter && r.account.account_key == key)
        && row.status == QuotaStatus::AuthRequired
    {
        row.status = if row.snapshot.is_some() {
            QuotaStatus::Stale
        } else {
            QuotaStatus::Unknown
        };
        if row
            .snapshot
            .as_ref()
            .is_some_and(|s| s.status == QuotaStatus::AuthRequired)
        {
            row.snapshot = None;
            row.status = QuotaStatus::Unknown;
            row.age_seconds = None;
        }
        row.error = None;
        row.refreshing = false;
        row.next_due_unix_seconds = Some(now);
        report
            .refreshing_ids
            .retain(|id| id != &format!("{key}/{adapter}"));
        report.next_due_unix_seconds = report
            .accounts
            .iter()
            .filter_map(|r| r.next_due_unix_seconds)
            .min();
        let elapsed =
            u64::try_from(now.saturating_sub(report.generated_unix_seconds)).unwrap_or(u64::MAX);
        for row in &mut report.accounts {
            if let Some(age) = &mut row.age_seconds {
                *age = age.saturating_add(elapsed);
            }
        }
        report.generated_unix_seconds = now;
        let bytes = serde_json::to_vec(&report)?;
        durable(provenant_os::fs::Directory::atomic_write_with_permissions(
            &directory,
            OsStr::new("report.json"),
            ".report",
            &bytes,
            0o600,
            PublishMode::Replace,
        )?)?;
        signal_quota(state_root);
    }
    Ok(())
}

/// Publish the quota generation after a committed quota write. The state root
/// is passed by each caller, so no signal depends on the directory's name.
fn signal_quota(state: &Path) {
    provenant_os::changes::hint(state, &[provenant_core::changes::Scope::Quota]);
}

fn open_readiness_account(state: &Path, key: &str) -> Result<Option<Directory>, EdgeError> {
    let root = match Directory::open_anchor(state) {
        Ok(root) => root,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut dir = root;
    for name in [READINESS_DIRECTORY, key] {
        dir = match dir.open_verified_directory(OsStr::new(name)) {
            Ok(next) => next,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        check_private(&dir)?;
    }
    Ok(Some(dir))
}

fn read_readiness_file(
    account_dir: &Directory,
    adapter: &str,
    key: &str,
) -> Result<Option<ReadinessFile>, EdgeError> {
    let name = format!("{adapter}.json");
    let reader = match account_dir.open_read(OsStr::new(&name)) {
        Ok(reader) => reader,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = reader.metadata()?;
    if meta.mode & 0o777 != 0o600 || meta.uid != provenant_os::fs::effective_uid() {
        return Err(EdgeError::Permissions);
    }
    let mut bytes = Vec::new();
    reader
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| EdgeError::Read {
            path: PathBuf::from(key).join(&name),
            source,
        })?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(QuotaError::Invalid("readiness bound").into());
    }
    Ok(Some(ReadinessFile::decode(&bytes, adapter, key)?))
}

/// The readiness of one route, read without locks, directory creation or
/// provider I/O. A missing, stale, mismatched or damaged record is unknown;
/// nothing here reads as ready unless fresh evidence says so. Routing and the
/// route and host commands read through this.
pub(crate) fn read_readiness(state: &Path, scope: &ReadinessScope, now: i64) -> ReadinessRead {
    read_readiness_after_probe(state, scope, now, None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginEvidence {
    Verified(i64),
    Unverified(i64),
}

impl LoginEvidence {
    pub(crate) fn observed_at(self) -> i64 {
        match self {
            Self::Verified(at) | Self::Unverified(at) => at,
        }
    }
}

pub(crate) fn read_readiness_after_probe(
    state: &Path,
    scope: &ReadinessScope,
    now: i64,
    reprobe: Option<LoginEvidence>,
) -> ReadinessRead {
    if !component(&scope.adapter) || !component(&scope.account_key) {
        return ReadinessRead::unknown(ReadinessReason::InvalidEvidence);
    }
    let read = open_readiness_account(state, &scope.account_key).and_then(|dir| {
        dir.map_or(Ok(None), |dir| {
            read_readiness_file(&dir, &scope.adapter, &scope.account_key)
        })
    });
    match read {
        Ok(Some(mut file)) => {
            if let Some(reprobe) = reprobe {
                let observed_at = reprobe.observed_at();
                file.records.retain(|record| {
                    !(record.scope.host == scope.host
                        && record.scope.adapter == scope.adapter
                        && record.scope.account_key == scope.account_key
                        && record.observed_unix_seconds <= observed_at
                        && (matches!(
                            record.reason,
                            Some(ReadinessReason::SignedOut | ReadinessReason::LoginExpired)
                        ) || (matches!(reprobe, LoginEvidence::Verified(_))
                            && record.reason == Some(ReadinessReason::AccountMismatch))))
                });
                if file.login.as_ref().is_some_and(|login| {
                    login.scope.host == scope.host
                        && login.scope.adapter == scope.adapter
                        && login.scope.account_key == scope.account_key
                        && login.observed_unix_seconds <= observed_at
                }) {
                    file.login = None;
                }
            }
            file.get(scope, now)
        }
        Ok(None) => ReadinessRead::absent(),
        Err(_) => ReadinessRead::unknown(ReadinessReason::InvalidEvidence),
    }
}

/// Publish this host's readiness for the account just written, under the
/// quota lock the caller holds. Quota is the only writer: provider refusals
/// in the account record become unavailable observations, and clear when the
/// record stops holding them. A dropped account loses its file.
fn sync_readiness(state: &Path, intent: &UpdateIntent, now: i64) -> Result<(), EdgeError> {
    let (adapter, key) = (&intent.account.adapter, &intent.account.account_key);
    let name = format!("{adapter}.json");
    let existing = open_readiness_account(state, key)?;
    let current = match &existing {
        Some(dir) => match read_readiness_file(dir, adapter, key) {
            Ok(file) => file,
            // A damaged file is rebuilt from the account record.
            Err(EdgeError::Quota(_)) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    let mut file = current.clone().unwrap_or_default();
    if let Some(record) = &intent.replacement {
        // Default policies date the clearing reading; a custom probe interval
        // only moves when a fresh reading stops counting as fresh.
        let policies = provenant_providers::quota_policies(provenant_providers::REGISTRY);
        let admission = Admission {
            policies: &policies,
            now_unix_seconds: now,
            records: std::slice::from_ref(record),
            pools: &[],
        };
        if let Some(reason) = intent.readiness_probe_result {
            let interval = policies
                .iter()
                .find(|policy| policy.adapter == adapter)
                .map_or(300, |policy| policy.timer_seconds);
            file.sync_probe_result(record, LOCAL_HOST, now, interval, reason)?;
        }
        if let Some((expiry, interval)) = intent.login_probe_expiry {
            file.login = Some(LoginObservation {
                scope: ReadinessScope {
                    host: LOCAL_HOST.into(),
                    adapter: adapter.clone(),
                    account_key: key.clone(),
                    route: ACCOUNT_ROUTE.into(),
                },
                observed_unix_seconds: now,
                valid_until_unix_seconds: now
                    .saturating_add(i64::try_from(interval.max(1)).unwrap_or(i64::MAX)),
                expires_unix_seconds: expiry,
            });
        }
        file.sync_refusals(record, LOCAL_HOST, &admission)?;
    } else {
        file.records.clear();
        file.login = None;
    }
    if file.records.is_empty() && file.login.is_none() {
        if let Some(dir) = existing {
            match dir.unlink(OsStr::new(&name)) {
                Ok(()) => dir.sync()?,
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
            drop(dir);
            let root = Directory::open_anchor(state)?;
            let readiness = root.open_verified_directory(OsStr::new(READINESS_DIRECTORY))?;
            let _ = readiness.rmdir(OsStr::new(key));
            let _ = readiness.sync();
        }
        return Ok(());
    }
    if current.as_ref() == Some(&file) {
        return Ok(());
    }
    let bytes = serde_json::to_vec(&file)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(QuotaError::Invalid("readiness bound").into());
    }
    let root = private(state)?;
    let readiness = child(&root, OsStr::new(READINESS_DIRECTORY))?;
    let account_dir = child(&readiness, OsStr::new(key))?;
    durable(provenant_os::fs::Directory::atomic_write_with_permissions(
        &account_dir,
        OsStr::new(&name),
        ".readiness",
        &bytes,
        0o600,
        PublishMode::Replace,
    )?)
}

fn write_intent(
    dir: &Directory,
    state: &Path,
    intent: &UpdateIntent,
    records: &mut Vec<AccountRecord>,
) -> Result<(), EdgeError> {
    let pos = records.iter().position(|r| {
        r.account.adapter == intent.account.adapter
            && r.account.account_key == intent.account.account_key
    });
    intent.check(pos.and_then(|p| records.get(p)))?;
    let account_dir = child(dir, OsStr::new(&intent.account.account_key))?;
    let name = format!("{}.json", intent.account.adapter);
    match &intent.replacement {
        Some(record) => {
            let bytes = serde_json::to_vec(record)?;
            if bytes.len() as u64 > MAX_RECORD_BYTES {
                return Err(QuotaError::Invalid("record bound").into());
            }
            durable(provenant_os::fs::Directory::atomic_write_with_permissions(
                &account_dir,
                OsStr::new(&name),
                ".quota",
                &bytes,
                0o600,
                PublishMode::Replace,
            )?)?;
            if let Some(p) = pos {
                records[p] = record.clone();
            } else {
                records.push(record.clone());
            }
        }
        None => {
            account_dir.unlink(OsStr::new(&name))?;
            account_dir.sync()?;
            drop(account_dir);
            let _ = dir.rmdir(OsStr::new(&intent.account.account_key));
            let _ = dir.sync();
            if let Some(p) = pos {
                records.remove(p);
            }
        }
    }
    // The account commit remains authoritative if derived publication fails.
    if let Err(error) = sync_readiness(state, intent, SystemClock::new().unix_seconds()) {
        use std::io::Write;
        let _ = writeln!(
            io::stderr().lock(),
            "Warning: could not sync quota readiness: {error}"
        );
    }
    signal_quota(state);
    Ok(())
}

fn clean_orphan_dirs(dir: &Directory) -> Result<(), EdgeError> {
    for entry in dir.entries()? {
        let entry = entry?;
        if entry == "report.json"
            || entry == "quota.lock"
            || entry == crate::quota_history::DIRECTORY
            || entry.to_str().is_some_and(|s| s.starts_with('.'))
        {
            continue;
        }
        if let Ok(account_dir) = dir.open_verified_directory(&entry) {
            let is_empty = account_dir.entries()?.next().is_none();
            drop(account_dir);
            if is_empty {
                let _ = dir.rmdir(&entry);
                let _ = dir.sync();
            }
        }
    }
    Ok(())
}
fn detections(
    registry: &[provenant_providers::Registration],
    configured: &[AccountScope],
) -> Vec<Detection> {
    registry
        .iter()
        .filter(|r| {
            provenant_records::AdapterSpec::get(r.adapter.id())
                .is_some_and(|spec| spec.quota_probe != provenant_records::QuotaProbeKind::None)
        })
        .map(|r| {
            let status = r.adapter.stream(None).and_then(|s| r.adapter.account(&s));
            Detection {
                adapter: r.adapter.id().into(),
                accounts: {
                    let accounts: Vec<_> = configured
                        .iter()
                        .filter(|a| a.adapter == r.adapter.id())
                        .cloned()
                        .collect();
                    (!accounts.is_empty()).then_some(accounts)
                },
                status: match status {
                    Ok(provenant_providers::AccountDetection::AuthRequired) => {
                        QuotaStatus::AuthRequired
                    }
                    _ if configured.iter().any(|a| a.adapter == r.adapter.id()) => {
                        QuotaStatus::Available
                    }
                    // An adapter with no registered account is never probed; say so
                    // rather than leaving it silently unknown.
                    _ => QuotaStatus::Unsupported,
                },
            }
        })
        .collect()
}
fn add_configured(records: &mut Vec<AccountRecord>, configured: &[AccountScope]) {
    for account in configured {
        if !records.iter().any(|r| {
            r.account.adapter == account.adapter && r.account.account_key == account.account_key
        }) {
            records.push(AccountRecord::empty(account.clone()));
        }
    }
}

/// Observe metadata only: neither credentials nor profile contents are opened.
fn credential_metadata(
    registration: &provenant_providers::accounts::Registration,
    home: Option<&Path>,
) -> Option<Vec<CredentialMetadata>> {
    let directory = &registration.directory;
    let spec = provenant_records::AdapterSpec::get(&registration.adapter);
    let mut paths = vec![directory.clone()];
    if let Some(spec) = spec {
        paths.extend(
            spec.credential_files
                .iter()
                .map(|name| directory.join(name)),
        );
        if spec.credential_profile_sibling {
            paths.push(directory.with_extension("json"));
        }
        if spec.credential_host_keychain
            && cfg!(target_os = "macos")
            && let Some(home) = home
        {
            paths.push(home.join("Library/Keychains/login.keychain-db"));
        }
    }
    paths
        .into_iter()
        .map(|path| {
            let stat = match std::fs::metadata(&path) {
                Ok(metadata) => {
                    let metadata = provenant_os::fs::Metadata::from_std(metadata);
                    Some(CredentialStat {
                        device: metadata.device,
                        inode: metadata.inode,
                        mode: metadata.mode,
                        uid: metadata.uid,
                        size: metadata.size,
                        modified_ns: metadata.modified_ns,
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                // An inaccessible path is not a credential change.
                Err(_) => return None,
            };
            Some(CredentialMetadata { path, stat })
        })
        .collect()
}

fn refresh_auth_metadata(
    dir: &Directory,
    state: &Path,
    records: &mut Vec<AccountRecord>,
    entries: &provenant_providers::accounts::Settings,
    detected: &[Detection],
    tick: &QuotaTick,
    home: Option<&Path>,
) -> Result<(), EdgeError> {
    if !matches!(
        tick.trigger,
        schedule::Trigger::Panel | schedule::Trigger::Wake | schedule::Trigger::Manual
    ) {
        return Ok(());
    }
    for registration in entries
        .entries
        .values()
        .filter(|entry| tick.adapter.as_ref().is_none_or(|id| id == &entry.adapter))
    {
        let Some(metadata) = credential_metadata(registration, home) else {
            continue;
        };
        let account = AccountScope {
            adapter: registration.adapter.clone(),
            account_key: registration.key.clone(),
            label: registration.label.clone(),
        };
        let current = records
            .iter()
            .find(|record| {
                record.account.adapter == account.adapter
                    && record.account.account_key == account.account_key
            })
            .cloned();
        let mut next = current
            .clone()
            .unwrap_or_else(|| AccountRecord::empty(account));
        if next.credential_metadata == metadata {
            continue;
        }
        let ambient_auth = detected.iter().any(|detection| {
            detection.adapter == next.account.adapter
                && detection.status == QuotaStatus::AuthRequired
                && detection.accounts.as_ref().is_some_and(|accounts| {
                    accounts.len() == 1 && accounts[0].account_key == next.account.account_key
                })
        });
        if next.last_error.is_some_and(ProbeFailure::needs_login)
            || next
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.status == QuotaStatus::AuthRequired)
            || ambient_auth
        {
            // Consumed by reserve. A probe already in flight keeps its claim;
            // completion preserves this pending event for the next tick.
            next.auth_retry_pending = true;
        }
        next.credential_metadata = metadata;
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or(QuotaError::NumberOutOfRange)?;
        write_intent(
            dir,
            state,
            &UpdateIntent {
                account: next.account.clone(),
                expected: current,
                replacement: Some(next),
                readiness_probe_result: None,
                login_probe_expiry: None,
            },
            records,
        )?;
    }
    Ok(())
}
/// Repair text for an agy account whose isolated keychain is missing. Without a registered entry the
/// expected path is unknown, so the text names the registry check instead of the path.
fn keychain_repair(
    entry: Option<&provenant_providers::accounts::Registration>,
    label: &str,
) -> String {
    let expected = entry.map_or_else(
        || "the account's isolated keychain (login.keychain-db under its registered directory; no registered agy account matches this row, check `provenant accounts list`)".to_string(),
        |entry| entry.directory.join("Library/Keychains/login.keychain-db").display().to_string(),
    );
    let label = entry.map_or(label, |entry| entry.label.as_str());
    format!(
        "expected {expected}; re-register with `provenant accounts register --adapter agy --directory ~ {label}` if this is the login-keychain account, or create the isolated keychain per docs/runbooks/providers.md"
    )
}
pub(crate) fn login_repair(entry: Option<&provenant_providers::accounts::Registration>) -> String {
    let Some(entry) = entry else {
        return "account-specific login remedy unavailable; check `provenant accounts list`".into();
    };
    let Some(directory) = entry.directory.to_str().filter(|path| {
        entry.directory.is_absolute() && !path.chars().any(char::is_control) && !path.contains('`')
    }) else {
        return "account-specific login remedy unavailable; check the registered directory".into();
    };
    let quoted = format!("'{}'", directory.replace('\'', "'\\''"));
    let Some(spec) = provenant_records::AdapterSpec::get(&entry.adapter) else {
        return "account-specific login remedy unavailable; consult the provider's sign-in instructions".into();
    };
    if let Some(template) = spec.login_template {
        format!(
            "run `{}` interactively",
            template.replace("{directory}", &quoted)
        )
    } else if !spec.login_hint.is_empty() {
        format!(
            "consult the provider's sign-in instructions (`{}`)",
            spec.login_hint
        )
    } else {
        "account-specific login remedy unavailable; consult the provider's sign-in instructions"
            .into()
    }
}

/// Assemble account-scoped login facts from one configuration and cache snapshot.
pub(crate) fn account_login_diagnostic<'a>(
    entries: impl IntoIterator<Item = &'a provenant_providers::accounts::Registration>,
    records: &[AccountRecord],
    adapter: &str,
    key: Option<&str>,
    reason: &provenant_records::RouteExclusionReason,
) -> String {
    let entry = entries
        .into_iter()
        .find(|entry| entry.adapter == adapter && Some(entry.key.as_str()) == key);
    let last = records
        .iter()
        .find(|record| {
            record.account.adapter == adapter && Some(record.account.account_key.as_str()) == key
        })
        .and_then(|record| record.snapshot.as_ref())
        .map(|snapshot| snapshot.observed_unix_seconds);
    login_diagnostic(entry, last, reason)
}

/// Diagnostic facts are account-scoped and contain no provider output.
pub(crate) fn login_diagnostic(
    entry: Option<&provenant_providers::accounts::Registration>,
    last_success: Option<i64>,
    reason: &provenant_records::RouteExclusionReason,
) -> String {
    let account = entry.map_or_else(
        || "account unavailable".to_owned(),
        |entry| {
            format!(
                "{} [{}]",
                entry.label.escape_debug(),
                entry.adapter.escape_debug()
            )
        },
    );
    let cause = match reason {
        provenant_records::RouteExclusionReason::LoginExpired => "login expired",
        provenant_records::RouteExclusionReason::AccountMismatch => {
            "signed-in account differs from registration"
        }
        _ => "login required",
    };
    let observed = last_success
        .and_then(|at| jiff::Timestamp::from_second(at).ok())
        .map_or_else(|| "unknown".to_owned(), |at| at.to_string());
    format!(
        "{account}: {cause}; last successful observation {observed}; {}",
        login_repair(entry)
    )
}
/// Attaches menu bar choices from config; hidden and order are applied by the app.
fn decorate(
    report: &mut QuotaReport,
    accounts: &provenant_providers::accounts::Settings,
    display: &Display,
) {
    report.display = display.clone();
    report.display_icon_warning =
        if display.icon != MOST_CONSTRAINED && !accounts.entries.contains_key(&display.icon) {
            report.display.icon = MOST_CONSTRAINED.into();
            Some(format!(
                "display.icon names unregistered account '{}'; using most_constrained",
                display.icon
            ))
        } else {
            None
        };
    for row in &mut report.accounts {
        let entry = accounts.entries.values().find(|entry| {
            entry.adapter == row.account.adapter && entry.key == row.account.account_key
        });
        row.repair = match row.error {
            Some(ProbeFailure::IsolatedKeychainUnavailable) => {
                Some(keychain_repair(entry, &row.account.label))
            }
            Some(
                ProbeFailure::Auth | ProbeFailure::LoginExpired | ProbeFailure::AccountMismatch,
            ) => Some(login_repair(entry)),
            None if row.status == QuotaStatus::AuthRequired => Some(login_repair(entry)),
            _ => None,
        };
        row.display = accounts
            .entries
            .iter()
            .find(|(_, e)| e.adapter == row.account.adapter && e.key == row.account.account_key)
            .map(|(id, e)| RowDisplay {
                id: id.clone(),
                name: e.display_name.clone(),
                hidden: e.hidden,
                order: e.order,
                headline: e.headline,
            });
    }
}

/// Project the same account-scoped expiry evidence for human and JSON output.
fn decorate_login(
    report: &mut QuotaReport,
    state: &Path,
    accounts: &provenant_providers::accounts::Settings,
    warning_seconds: u64,
    now: i64,
) {
    for row in &mut report.accounts {
        row.login_expiry = Some(login_expiry_at(state, &row.account, warning_seconds, now));
        if row.repair.is_none()
            && row.error != Some(provenant_quota::ProbeFailure::Network)
            && row.login_expiry.as_ref().is_some_and(|view| {
                matches!(
                    view.state,
                    LoginExpiryState::Expiring | LoginExpiryState::Expired
                )
            })
        {
            let entry = accounts.entries.values().find(|entry| {
                entry.adapter == row.account.adapter && entry.key == row.account.account_key
            });
            row.repair = Some(login_repair(entry));
        }
    }
}
fn login_expiry_at(
    state: &Path,
    account: &AccountScope,
    warning_seconds: u64,
    now: i64,
) -> LoginExpiryView {
    let observation = (component(&account.adapter) && component(&account.account_key))
        .then(|| open_readiness_account(state, &account.account_key))
        .transpose()
        .ok()
        .flatten()
        .flatten()
        .and_then(|dir| {
            read_readiness_file(&dir, &account.adapter, &account.account_key)
                .ok()
                .flatten()
        })
        .and_then(|file| file.login)
        .filter(|login| login.scope.host == LOCAL_HOST);
    let status = observation
        .as_ref()
        .map_or(LoginExpiryState::Unknown, |item| {
            item.state(now, warning_seconds)
        });
    LoginExpiryView {
        state: status,
        expires_unix_seconds: observation.as_ref().and_then(|item| {
            (status != LoginExpiryState::Unknown)
                .then_some(item.expires_unix_seconds)
                .flatten()
        }),
        observed_unix_seconds: observation.map(|item| item.observed_unix_seconds),
    }
}

/// Read account login facts for doctor without probing providers or credentials.
pub fn recorded_login_rows(roots: &Roots) -> Result<Vec<AccountRow>, String> {
    recorded_login_rows_inner(roots).map_err(|error| error.to_string())
}
fn recorded_login_rows_inner(roots: &Roots) -> Result<Vec<AccountRow>, EdgeError> {
    let domains = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?.domains;
    let accounts = domains.accounts.with_derived_accounts();
    let mut configured: Vec<_> = accounts
        .entries
        .values()
        .map(|entry| AccountScope {
            adapter: entry.adapter.clone(),
            account_key: entry.key.clone(),
            label: entry.label.clone(),
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    configured
        .retain(|account| seen.insert((account.adapter.clone(), account.account_key.clone())));
    let mut records = read_cache(roots)?;
    records.retain(|record| {
        configured.iter().any(|account| {
            account.adapter == record.account.adapter
                && account.account_key == record.account.account_key
        })
    });
    add_configured(&mut records, &configured);
    let now = SystemClock::new().unix_seconds();
    let policies =
        provenant_providers::scheduling_policies(&domains.quota, provenant_providers::REGISTRY);
    let mut report = schedule::report(
        &policies,
        &records,
        &[],
        &QuotaQuery { adapter: None },
        now,
        String::new(),
    )?;
    decorate(&mut report, &accounts, &domains.quota.display);
    decorate_login(
        &mut report,
        roots.state.as_path(),
        &accounts,
        domains.quota.login_expiry_warning_seconds,
        now,
    );
    Ok(report.accounts)
}
fn execute(
    command: &Quota,
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    effects: &dyn QuotaEffects,
) -> Result<QuotaReport, EdgeError> {
    let all = Filter { adapter: None };
    let (filter, tick) = match command {
        Quota::Show(filter)
        | Quota::Usage {
            adapter: filter, ..
        } => (filter, None),
        // Dispatched by `run` before execute; reads the cache like `show`.
        Quota::History { .. } => (&all, None),
        Quota::Display { headline, icon } => {
            crate::accounts::set_quota_display(roots, headline.map(Into::into), icon.as_deref())
                .map_err(EdgeError::Display)?;
            (&all, None)
        }
        Quota::Tick {
            adapter,
            trigger,
            power,
        } => (
            adapter,
            Some(QuotaTick {
                adapter: adapter.adapter.clone(),
                trigger: (*trigger).into(),
                power: (*power).into(),
            }),
        ),
    };
    if filter
        .adapter
        .as_ref()
        .is_some_and(|id| !registry.iter().any(|r| r.adapter.id() == id))
    {
        return Err(EdgeError::Adapter);
    }
    let domains = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?.domains;
    let (settings, entries) = (domains.quota, domains.accounts);
    let clock = SystemClock::new();
    let now = clock
        .wall_now()
        .map_err(|_| EdgeError::Time)?
        .as_timestamp()
        .as_second();
    let path = roots.state.as_path().join("quota/report.json");
    let report_path = path.to_str().ok_or(EdgeError::Path)?.to_owned();
    let policies = provenant_providers::scheduling_policies(&settings, registry);
    let configured = effects.accounts()?;
    let detected = detections(registry, &configured);
    let query = QuotaQuery {
        adapter: filter.adapter.clone(),
    };
    let Some(tick) = tick else {
        let mut records = read_cache(roots)?;
        add_configured(&mut records, &configured);
        settings.apply_calibration(&mut records, now);
        let mut report =
            schedule::report(&policies, &records, &detected, &query, now, report_path)?;
        decorate(&mut report, &entries, &settings.display);
        decorate_login(
            &mut report,
            roots.state.as_path(),
            &entries,
            settings.login_expiry_warning_seconds,
            now,
        );
        return Ok(report);
    };
    let root = private(roots.state.as_path())?;
    let dir = child(&root, OsStr::new("quota"))?;
    let cancel = CancelToken::new();
    let lock = || -> Result<_, EdgeError> {
        let budget = WaitBudget::cli(clock.mono_now(), 1).map_err(|_| EdgeError::Time)?;
        Ok(dir.lock(
            OsStr::new("quota.lock"),
            LockMode::Exclusive,
            budget,
            &cancel,
            &clock,
        )?)
    };
    let plan = {
        let _lock = lock()?;
        let mut records = read_records(&dir)?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        refresh_auth_metadata(
            &dir,
            roots.state.as_path(),
            &mut records,
            &entries,
            &detected,
            &tick,
            home.as_deref(),
        )?;
        let plan = schedule::plan_tick_with_settings(
            &policies, &records, &detected, &tick, now, &settings,
        )?;
        for drop in &plan.drops {
            write_intent(&dir, roots.state.as_path(), drop, &mut records)?;
        }
        clean_orphan_dirs(&dir)?;
        plan
    };
    for account in plan.batches.iter().flatten() {
        let at = clock
            .wall_now()
            .map_err(|_| EdgeError::Time)?
            .as_timestamp()
            .as_second();
        if at >= plan.deadline_unix_seconds - 1 {
            break;
        }
        let token = format!("quota-{}-{at}", std::process::id());
        let current = {
            let _lock = lock()?;
            let mut records = read_records(&dir)?;
            let current = records
                .iter()
                .find(|r| {
                    r.account.adapter == account.adapter
                        && r.account.account_key == account.account_key
                })
                .cloned();
            let intent = match schedule::reserve(
                &policies,
                current.as_ref(),
                account,
                at,
                &token,
                tick.trigger,
                tick.power,
            ) {
                Ok(intent) => intent,
                Err(QuotaError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            };
            write_intent(&dir, roots.state.as_path(), &intent, &mut records)?;
            current
        };
        // The durable claim owns this probe; never hold quota.lock across host I/O.
        let source = registry
            .iter()
            .filter_map(|r| r.quota_source)
            .find(|s| s.adapter_id() == account.adapter);
        let mut login_expiry = None;
        let outcome = match source {
            Some(source) => {
                let probe_deadline = schedule::probe_deadline(plan.deadline_unix_seconds, at)?;
                let input = QuotaProbeInput {
                    now_unix_seconds: at,
                    deadline_unix_seconds: probe_deadline,
                    authority_ref: Some("configured-local-quota".into()),
                };
                source
                    .probe(account, &input)
                    .map_err(|_| ProbeFailure::Disabled)
                    .and_then(|probe| {
                        let capture = effects.capture(&probe, current.as_ref());
                        login_expiry = effects.login_expiry_unix_seconds();
                        let capture = capture?;
                        source
                            .decode(&probe, &capture)
                            .map_err(|_| ProbeFailure::Malformed)
                    })
            }
            None => Err(ProbeFailure::Disabled),
        };
        let readiness_reason = outcome
            .as_ref()
            .err()
            .map(|failure| effects.readiness_reason(*failure));
        let finished = clock
            .wall_now()
            .map_err(|_| EdgeError::Time)?
            .as_timestamp()
            .as_second();
        let _lock = lock()?;
        let mut records = read_records(&dir)?;
        if finished <= schedule::probe_deadline(plan.deadline_unix_seconds, at)?
            && let Some(reserved) = records.iter().find(|r| {
                r.account.adapter == account.adapter && r.account.account_key == account.account_key
            })
        {
            match schedule::complete(reserved, &token, finished, outcome, None) {
                Ok(mut complete) => {
                    // A later failure cannot disprove a previously verified
                    // mismatch: publish the same sticky reason as the account
                    // record until a successful probe establishes recovery.
                    let reason = if readiness_reason.is_some()
                        && complete.replacement.as_ref().is_some_and(|record| {
                            record.last_error == Some(ProbeFailure::AccountMismatch)
                        }) {
                        Some(ReadinessReason::AccountMismatch)
                    } else {
                        readiness_reason
                    };
                    complete.readiness_probe_result = Some(reason);
                    let interval = policies
                        .iter()
                        .find(|policy| policy.adapter == account.adapter)
                        .map_or(300, |policy| policy.timer_seconds);
                    complete.login_probe_expiry = Some((login_expiry, interval));
                    write_intent(&dir, roots.state.as_path(), &complete, &mut records)?;
                    // Best effort: a history fault never fails the refresh.
                    if let Some(snapshot) = complete
                        .replacement
                        .as_ref()
                        .and_then(|record| record.snapshot.as_ref())
                    {
                        let _ = crate::quota_history::record(
                            &dir,
                            snapshot,
                            settings.history_max_bytes,
                        );
                    }
                }
                Err(QuotaError::Conflict) => {} // A newer claim or observation owns the record.
                Err(error) => return Err(error.into()),
            }
        }
    }
    let _lock = lock()?;
    let mut records = read_records(&dir)?;
    add_configured(&mut records, &configured);
    let now = clock
        .wall_now()
        .map_err(|_| EdgeError::Time)?
        .as_timestamp()
        .as_second();
    settings.apply_calibration(&mut records, now);
    let mut report = schedule::report(
        &policies,
        &records,
        &detected,
        &QuotaQuery { adapter: None },
        now,
        report_path,
    )?;
    decorate(&mut report, &entries, &settings.display);
    decorate_login(
        &mut report,
        roots.state.as_path(),
        &entries,
        settings.login_expiry_warning_seconds,
        now,
    );
    let bytes = serde_json::to_vec(&report)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(EdgeError::TooLarge);
    }
    durable(provenant_os::fs::Directory::atomic_write_with_permissions(
        &dir,
        OsStr::new("report.json"),
        ".report",
        &bytes,
        0o600,
        PublishMode::Replace,
    )?)?;
    signal_quota(roots.state.as_path());
    // Publish all accounts even when the caller requests a filtered projection.
    let mut shown = schedule::report(
        &policies,
        &records,
        &detected,
        &query,
        now,
        report.report_path.clone(),
    )?;
    decorate(&mut shown, &entries, &settings.display);
    decorate_login(
        &mut shown,
        roots.state.as_path(),
        &entries,
        settings.login_expiry_warning_seconds,
        now,
    );
    // Fabric delivery must not hold the quota lock. A deterministic operation
    // ID makes retry safe if the process stops after delivery.
    drop(_lock);
    if !crate::fabric::in_lane()
        && let Err(error) = notice::deliver(
            roots,
            &report,
            settings.login_expiry_warning_seconds,
            &clock,
        )
    {
        use std::io::Write;
        let _ = writeln!(
            io::stderr().lock(),
            "Warning: could not deliver login expiry notice: {error}"
        );
    }
    Ok(shown)
}
/// Chair sessions the SessionStart hook recorded in `[since, until)`, with the
/// tokens read from their transcripts. The hook stores the resolved account
/// key; lane facts carry labels, so a registered account's key becomes its
/// label here. A session on an unregistered config directory keeps the
/// reserved `unattributed` account.
pub(crate) fn chair_sessions_with_usage(
    roots: &Roots,
    home: Option<&Path>,
    since: jiff::Timestamp,
    until: jiff::Timestamp,
) -> Result<Vec<provenant_telemetry::usage::ChairSession>, EdgeError> {
    use std::path::Component;
    let accounts = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .accounts;
    let mut chairs = provenant_telemetry::usage::chair_sessions(roots, since, until)?;
    provenant_telemetry::attach_chair_usage(&mut chairs, |chair| {
        accounts
            .entries
            .values()
            .find(|entry| entry.adapter == chair.adapter && entry.key == chair.account)
            .map(|entry| entry.directory.clone())
            .or_else(|| {
                // The hook records only a plain relative path under home.
                let relative = Path::new(chair.config.as_deref()?);
                relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                    .then(|| home.map(|home| home.join(relative)))
                    .flatten()
            })
    });
    for chair in &mut chairs {
        if let Some(entry) = accounts
            .entries
            .values()
            .find(|entry| entry.adapter == chair.adapter && entry.key == chair.account)
        {
            chair.account = entry.label.clone();
        }
    }
    Ok(chairs)
}

fn usage_report(
    roots: &Roots,
    filter: &Filter,
    days: u32,
) -> Result<(serde_json::Value, String), EdgeError> {
    use provenant_run::{AttemptFilter, AttemptReader, ExecutionContext, Request, RunStore};
    use provenant_telemetry::{MAX_ATTEMPTS, Window};
    let clock = SystemClock::new();
    let until = clock.wall_now().map_err(|_| EdgeError::Time)?;
    let since = provenant_core::WallTime::from_micros(
        until.as_timestamp().as_microsecond() - i64::from(days) * 86_400_000_000,
    )
    .map_err(|_| EdgeError::Time)?;
    let window = Window {
        since: Some(since),
        until: Some(until),
        ..Window::default()
    };
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let projects = provenant_config::registered_projects(roots)?;
    let mut facts = Vec::new();
    for project in &projects {
        let context = ExecutionContext {
            state_root: roots.state.clone(),
            project_id: project.project_id(),
            // Read-only facts do not use actor authority; prompts are never read.
            actor_id: provenant_core::ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        if let Some(store) = RunStore::open_read_only(&context, &call)? {
            for fact in store.attempts(AttemptFilter::default(), &call)? {
                let fact = fact?;
                if fact.finished_at < since || fact.finished_at >= until {
                    continue;
                }
                if facts.len() as u64 >= MAX_ATTEMPTS {
                    return Err(provenant_telemetry::TelemetryError::TooLarge(
                        "100,000 selected attempts",
                    )
                    .into());
                }
                facts.push(fact);
            }
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let chairs = chair_sessions_with_usage(
        roots,
        home.as_deref(),
        since.as_timestamp(),
        until.as_timestamp(),
    )?;
    let mut rows = provenant_telemetry::quota_usage(facts, chairs, &window)?;
    if let Some(adapter) = &filter.adapter {
        rows.retain(|row| row.adapter.as_ref() == Some(adapter));
    }
    let note = "Chair tokens are read from each recorded chair session's own transcript when this report runs, so they cover sessions the SessionStart hook recorded in this window, sub-agents included. A session on a config directory that is not a registered account is reported under the account `unattributed`; launch the chair with CLAUDE_CONFIG_DIR set to a registered account's directory to attribute it. Sessions whose transcript cannot be read are counted as unread, not as zero. Chair project attribution requires usage.project_attribution=true; otherwise the project is unknown. Usage logs are bounded and rotated. Lane tokens cover terminal attempts in registered projects only, not transient workspaces, and are not provider quota percentages.";
    let mut human = format!("Recorded use: {since} to {until}\n{note}\n");
    for row in &rows {
        let project = projects
            .iter()
            .find(|p| Some(p.project_id()) == row.project)
            .map_or_else(
                || {
                    row.project
                        .map_or_else(|| "unknown project".into(), |p| p.to_string())
                },
                |p| p.common_dir().display().to_string(),
            );
        human.push_str(&format!(
            "{} [{}] project={} ({})\n  chair: {} sessions ({} unread)\n  lanes: {} attempts\n",
            row.account
                .as_deref()
                .unwrap_or("unknown account")
                .escape_debug(),
            row.adapter
                .as_deref()
                .unwrap_or("unknown adapter")
                .escape_debug(),
            project.escape_debug(),
            row.project
                .map_or_else(|| "unattributed".into(), |p| p.to_string()),
            row.chair_sessions,
            row.chair_sessions_unread,
            row.lane_attempts
        ));
        for (source, attempts, usage, cache) in [
            ("lanes", row.lane_attempts, &row.lane_usage, &row.lane_cache),
            (
                "chair",
                row.chair_sessions.saturating_sub(row.chair_sessions_unread),
                &row.chair_usage,
                &row.chair_cache,
            ),
        ] {
            if attempts == 0 {
                continue;
            }
            for (label, total) in [
                ("uncached input", &usage.uncached_input),
                ("cache read", &usage.cache_read),
                ("cache write", &usage.cache_write),
                (
                    "output excluding reasoning",
                    &usage.output_excluding_reasoning,
                ),
                ("reasoning", &usage.reasoning),
            ] {
                human.push_str(&format!(
                    "    {source} {label}: {} tokens; {} records missing this category\n",
                    total.known, total.missing
                ));
            }
            human.push_str(&format!(
                "    {source} cache-read share: {}\n",
                cache_share(cache)
            ));
        }
    }
    if rows.is_empty() {
        human.push_str("No recorded use in this window.\n");
    }
    let data = serde_json::json!({
        "since": since, "until": until, "rows": rows, "chair_usage_note": note,
        "projects": projects.iter().map(|p| serde_json::json!({"project":p.project_id(), "common_dir":p.common_dir()})).collect::<Vec<_>>(),
    });
    if serde_json::to_vec(&data)?.len() as u64 > MAX_RECORD_BYTES {
        return Err(EdgeError::TooLarge);
    }
    Ok((data, human))
}

/// Schema of `quota history --json`.
const HISTORY_SCHEMA_VERSION: u64 = 1;

fn history_report(
    roots: &Roots,
    query: &crate::quota_history::Query,
    limit: u32,
) -> Result<(serde_json::Value, String), EdgeError> {
    let clock = SystemClock::new();
    let now = clock
        .wall_now()
        .map_err(|_| EdgeError::Time)?
        .as_timestamp()
        .as_second();
    let accounts = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .accounts;
    let mut records = crate::quota_history::read(roots.state.as_path(), query)?;
    let total = records.len();
    let limit = limit as usize;
    if total > limit {
        records.drain(..total - limit);
    }
    // One price lookup per record serves both the JSON and the table.
    let prices: Vec<Option<PlanPrice>> = records
        .iter()
        .map(|record| {
            accounts
                .entries
                .values()
                .find(|e| e.adapter == record.provider && e.key == record.account_key)
                .and_then(|e| price_in_force(&e.plans, record.observed_unix_seconds))
                .cloned()
        })
        .collect();
    let readings: Vec<serde_json::Value> = records
        .iter()
        .zip(&prices)
        .map(|(record, price)| {
            let mut value = serde_json::to_value(record)?;
            value["plan_price"] = serde_json::to_value(price)?;
            Ok(value)
        })
        .collect::<Result<_, serde_json::Error>>()?;
    let data = serde_json::json!({
        "schema_version": HISTORY_SCHEMA_VERSION,
        "generated_unix_seconds": now,
        "count": readings.len(),
        "matched": total,
        "truncated": total > readings.len(),
        "readings": readings,
    });
    if serde_json::to_vec(&data)?.len() as u64 > 16 * MAX_RECORD_BYTES {
        return Err(EdgeError::HistoryTooLarge);
    }
    Ok((data, history_human(&records, &prices)))
}

fn history_human(records: &[HistoryRecord], prices: &[Option<PlanPrice>]) -> String {
    if records.is_empty() {
        return "Quota history: no readings\n".into();
    }
    let mut human = format!("Quota history: {} readings\n", records.len());
    for (record, price) in records.iter().zip(prices) {
        let when = jiff::Timestamp::from_second(record.observed_unix_seconds).map_or_else(
            |_| "?".into(),
            |t| t.strftime("%Y-%m-%d %H:%M:%SZ").to_string(),
        );
        human.push_str(&format!(
            "{when}  {} [{}]  {}",
            record.label.escape_debug(),
            record.provider,
            record.plan.as_deref().unwrap_or("-").escape_debug()
        ));
        for window in &record.windows {
            human.push_str(&format!(
                "  {} {}.{:02}%",
                window.pool.escape_debug(),
                window.used_basis_points / 100,
                window.used_basis_points % 100
            ));
        }
        if let Some(balance) = &record.credit_balance {
            human.push_str(&format!("  credit {}", balance.escape_debug()));
        }
        if let Some(plan) = price {
            human.push_str(&format!(
                "  {} {} {}/mo",
                plan.name.escape_debug(),
                plan.price,
                plan.currency
            ));
        }
        for marker in &record.markers {
            match marker.kind {
                MarkerKind::Reset => human.push_str(&format!(
                    "  RESET {}",
                    marker.pool.as_deref().unwrap_or("").escape_debug()
                )),
                MarkerKind::PlanChange => human.push_str(&format!(
                    "  PLAN CHANGE {} -> {}",
                    marker.from.as_deref().unwrap_or("-").escape_debug(),
                    marker.to.as_deref().unwrap_or("-").escape_debug()
                )),
            }
        }
        human.push('\n');
    }
    human
}

/// `93.4% of 1200 input tokens`, or why there is no share.
pub(crate) fn cache_share(cache: &provenant_telemetry::CacheSummary) -> String {
    match cache.read_share_basis_points {
        Some(share) => format!(
            "{}.{}% of {} input tokens ({} records reported, {} did not)",
            share / 100,
            share % 100 / 10,
            cache.input_tokens,
            cache.reporting,
            cache.missing
        ),
        None => format!(
            "not reported ({} records reported, {} did not)",
            cache.reporting, cache.missing
        ),
    }
}

fn quota_human(report: &QuotaReport) -> String {
    quota_human_at(report, jiff::tz::TimeZone::system())
}

fn quota_human_at(report: &QuotaReport, time_zone: jiff::tz::TimeZone) -> String {
    let mut human = format!(
        "Quota: {}/{} current readings\n",
        report.coverage.known, report.coverage.total
    );
    if let Some(warning) = &report.display_icon_warning {
        human.push_str(&format!("Warning: {warning}\n"));
    }
    for row in &report.accounts {
        let status = serde_json::to_value(row.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into())
            .to_uppercase();
        let age = row
            .age_seconds
            .map_or_else(|| "unknown".into(), |age| format!("{age}s"));
        let reset = row
            .resets_at
            .as_deref()
            .and_then(|at| reset_label(at, report.generated_unix_seconds, &time_zone));
        human.push_str(&format!(
            "{} [{}]: {status}{}, age {age}",
            row.account.label,
            row.account.adapter,
            reset.map_or_else(String::new, |label| format!(" {label}"))
        ));
        if let Some(login) = &row.login_expiry {
            match (
                login.state,
                login
                    .expires_unix_seconds
                    .and_then(|at| jiff::Timestamp::from_second(at).ok()),
            ) {
                (LoginExpiryState::Fresh, Some(expiry)) => {
                    human.push_str(&format!("; login expires {expiry}"))
                }
                (LoginExpiryState::Expiring, Some(expiry)) => {
                    human.push_str(&format!("; login expiring {expiry}"))
                }
                (LoginExpiryState::Expired, Some(expiry)) => {
                    human.push_str(&format!("; login expired {expiry}"))
                }
                _ => human.push_str("; login expiry unknown"),
            }
        }
        for refusal in displayed_refusals(row) {
            let name = refusal.model.escape_debug();
            let pool = refusal
                .pool
                .as_deref()
                .map(|pool| format!(" in pool {}", pool.escape_debug()))
                .unwrap_or_default();
            let reset = refusal
                .resets_at
                .as_deref()
                .and_then(|at| reset_label(at, report.generated_unix_seconds, &time_zone));
            human.push_str(&format!(
                "; recorded refusal for {name}{pool}{}",
                reset.map_or_else(String::new, |label| format!(" {label}"))
            ));
        }
        if let Some(snapshot) = &row.snapshot {
            if let Some(details) = &snapshot.details {
                if let Some(source) = &details.source {
                    human.push_str(&format!("; source: {}", source.escape_debug()));
                }
                if let Some(coverage) = &details.coverage {
                    human.push_str(&format!("; coverage: {}", coverage.escape_debug()));
                    if coverage == "partial" {
                        human.push_str(" (lower bound, this host only)");
                    }
                }
                if let Some(expiry) = details.calibration_expires_unix_seconds {
                    human.push_str(&format!("; dashboard calibration until {expiry}"));
                }
            }
            if row.error.is_some()
                || matches!(row.status, QuotaStatus::Stale | QuotaStatus::AuthRequired)
            {
                human.push_str(" (last known)");
            }
            for window in &snapshot.windows {
                human.push_str(&format!(
                    "; {} {}.{:02}% used",
                    window.pool.escape_debug(),
                    window.used_basis_points / 100,
                    window.used_basis_points % 100
                ));
            }
            if let Some(note) = shared_pool_exhaustions(snapshot) {
                human.push_str(&format!("; {note}"));
            }
        }
        if let Some(error) = row.error {
            let reason = serde_json::to_value(error)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".into());
            human.push_str(&format!("; refresh failed: {reason}"));
            if matches!(error, ProbeFailure::Auth | ProbeFailure::LoginExpired) {
                human.push_str(" (needs login in this identity)");
                if let Some(repair) = &row.repair {
                    human.push_str(&format!("; {repair}"));
                } else {
                    human.push_str("; account-specific login remedy unavailable");
                }
            } else if error == ProbeFailure::AccountMismatch {
                human.push_str(" (the signed-in account differs from the registered one)");
                if let Some(repair) = &row.repair {
                    human.push_str(&format!("; {repair}"));
                } else {
                    human.push_str("; account-specific login remedy unavailable");
                }
            } else if error == ProbeFailure::IsolatedKeychainUnavailable
                && let Some(repair) = &row.repair
            {
                human.push_str(&format!("; isolated keychain unavailable: {repair}"));
            }
        }
        if row.error.is_none() && row.status == QuotaStatus::AuthRequired {
            human.push_str("; ");
            human.push_str(
                row.repair
                    .as_deref()
                    .unwrap_or("account-specific login remedy unavailable"),
            );
        }
        if !(row.error.is_none() && row.status == QuotaStatus::AuthRequired)
            && !matches!(
                row.error,
                Some(
                    ProbeFailure::Auth
                        | ProbeFailure::LoginExpired
                        | ProbeFailure::AccountMismatch
                        | ProbeFailure::IsolatedKeychainUnavailable
                )
            )
            && row.login_expiry.as_ref().is_some_and(|view| {
                matches!(
                    view.state,
                    LoginExpiryState::Expiring | LoginExpiryState::Expired
                )
            })
            && let Some(repair) = &row.repair
        {
            human.push_str(&format!("; {repair}"));
        }
        for cooldown in &row.capacity_cooldowns {
            human.push_str(&format!(
                "; capacity cooldown {}: {}s remaining",
                cooldown.model.escape_debug(),
                cooldown.remaining_seconds
            ));
        }
        human.push('\n');
    }
    human.push_str(&format!("Report: {}\n", report.report_path));
    human
}

/// A refusal reset in the host's local time, with a weekday when it is not today.
fn reset_label(resets_at: &str, now: i64, time_zone: &jiff::tz::TimeZone) -> Option<String> {
    let reset = resets_at
        .parse::<jiff::Timestamp>()
        .ok()?
        .to_zoned(time_zone.clone());
    let today = jiff::Timestamp::from_second(now)
        .ok()?
        .to_zoned(time_zone.clone())
        .date();
    let format = if reset.date() == today {
        "%H:%M"
    } else {
        "%a %H:%M"
    };
    Some(format!("until {}", reset.strftime(format)))
}

/// Avoid repeating the account-level free-pool expiry already printed beside its status.
fn displayed_refusals(row: &provenant_quota::AccountRow) -> Vec<&provenant_quota::RefusalRow> {
    let free_pool = row
        .resets_at
        .as_ref()
        .and_then(|_| provenant_records::AdapterSpec::get(&row.account.adapter))
        .and_then(|spec| spec.free_pool);
    row.refusals
        .iter()
        .filter(|refusal| free_pool.is_none_or(|pool| refusal.pool.as_deref() != Some(pool)))
        .collect()
}
/// Per-pool peak use in first-seen order, grouping each window by the pool
/// prefix before its `.` separator.
fn pool_usage(snapshot: &provenant_quota::QuotaSnapshot) -> Vec<(&str, u64)> {
    let mut pools: Vec<(&str, u64)> = Vec::new();
    for window in &snapshot.windows {
        let pool = window
            .pool
            .split_once('.')
            .map_or(window.pool.as_str(), |(p, _)| p);
        match pools.iter_mut().find(|(name, _)| *name == pool) {
            Some((_, used)) => *used = (*used).max(window.used_basis_points),
            None => pools.push((pool, window.used_basis_points)),
        }
    }
    pools
}

/// Per-pool state for an account with more than one pool, for example
/// `gemini exhausted, claude 40%`. A pool is exhausted when any of its windows is.
fn pool_states(snapshot: &provenant_quota::QuotaSnapshot) -> Option<String> {
    let pools = pool_usage(snapshot);
    let partial = snapshot
        .details
        .as_ref()
        .is_some_and(|d| d.coverage.as_deref() == Some("partial"));
    (pools.len() > 1 || (partial && !pools.is_empty())).then(|| {
        pools
            .iter()
            .map(|(pool, used)| {
                if *used >= 10_000 {
                    format!("{} exhausted", pool.escape_debug())
                } else {
                    format!(
                        "{} {}{}%",
                        pool.escape_debug(),
                        if partial { ">=" } else { "" },
                        used / 100
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    })
}
/// The exhausted pools of a grouped adapter, for example
/// `claude pool exhausted (shared)`. Each pool of a grouped adapter is shared
/// by its own model family, so a spent pool refuses only those models and the
/// account itself stays available; the note names the pool instead of letting
/// the account read as wholly exhausted.
fn shared_pool_exhaustions(snapshot: &provenant_quota::QuotaSnapshot) -> Option<String> {
    let grouped = provenant_records::AdapterSpec::get(&snapshot.account.adapter)
        .is_some_and(|spec| spec.grouped_quota_pools);
    let pools = pool_usage(snapshot);
    if !grouped || pools.len() <= 1 {
        return None;
    }
    let exhausted: Vec<String> = pools
        .iter()
        .filter(|(_, used)| *used >= 10_000)
        .map(|(pool, _)| format!("{} pool exhausted (shared)", pool.escape_debug()))
        .collect();
    (!exhausted.is_empty()).then(|| exhausted.join(", "))
}
/// Counts by state, then the accounts that need attention, then the command
/// that shows the rest.
fn quota_summary(report: &QuotaReport) -> crate::Summary {
    quota_summary_at(report, jiff::tz::TimeZone::system())
}

fn quota_summary_at(report: &QuotaReport, time_zone: jiff::tz::TimeZone) -> crate::Summary {
    let state = |row: &provenant_quota::AccountRow| {
        serde_json::to_value(row.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into())
    };
    let mut counts: Vec<(String, usize)> = Vec::new();
    for row in &report.accounts {
        let name = state(row);
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, n)) => *n += 1,
            None => counts.push((name, 1)),
        }
    }
    let tally = crate::Summary::tally(
        &counts
            .iter()
            .map(|(n, c)| (n.as_str(), *c))
            .collect::<Vec<_>>(),
    );
    let mut rows: Vec<String> = report
        .display_icon_warning
        .iter()
        .map(|warning| format!("Warning: {warning}"))
        .collect();
    rows.extend(
        report
            .accounts
            .iter()
            .filter(|row| {
                !matches!(row.status, QuotaStatus::Available | QuotaStatus::CreditOnly)
                    || row
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.details.as_ref())
                        .is_some_and(|details| details.coverage.as_deref() == Some("partial"))
                    || !row.refusals.is_empty()
                    || row
                        .snapshot
                        .as_ref()
                        .and_then(shared_pool_exhaustions)
                        .is_some()
            })
            .map(|row| {
                let pools = row
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        shared_pool_exhaustions(snapshot).or_else(|| pool_states(snapshot))
                    })
                    .map_or_else(String::new, |pools| format!(" ({pools})"));
                let reset = row
                    .resets_at
                    .as_deref()
                    .and_then(|at| reset_label(at, report.generated_unix_seconds, &time_zone));
                let refusals = displayed_refusals(row)
                    .into_iter()
                    .map(|refusal| {
                        let name = refusal.model.escape_debug();
                        let pool = refusal
                            .pool
                            .as_deref()
                            .map(|pool| format!(" in pool {}", pool.escape_debug()))
                            .unwrap_or_default();
                        let reset = refusal.resets_at.as_deref().and_then(|at| {
                            reset_label(at, report.generated_unix_seconds, &time_zone)
                        });
                        format!(
                            "recorded refusal for {name}{pool}{}",
                            reset.map_or_else(String::new, |label| format!(" {label}"))
                        )
                    })
                    .collect::<Vec<_>>();
                let refusals = if refusals.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", refusals.join(", "))
                };
                format!(
                    "{} [{}]: {}{}{}{}",
                    row.account.label,
                    row.account.adapter,
                    state(row),
                    pools,
                    reset.map_or_else(String::new, |label| format!(" {label}")),
                    refusals
                )
            }),
    );
    let next = report
        .accounts
        .iter()
        .any(|row| !matches!(row.status, QuotaStatus::Available | QuotaStatus::CreditOnly))
        .then(|| "provenant quota tick".to_owned());
    crate::Summary::new(
        format!(
            "quota {}/{} current{}{}",
            report.coverage.known,
            report.coverage.total,
            if tally.is_empty() { "" } else { ": " },
            tally
        ),
        rows,
        next,
    )
}
pub fn run(
    command: Quota,
    globals: &crate::Globals,
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    effects: &dyn QuotaEffects,
    publish: impl FnOnce(
        &CommandResult<Option<serde_json::Value>>,
    ) -> io::Result<provenant_records::AbsolutePath>,
) -> u8 {
    let name = match command {
        Quota::Show(_) => "quota show",
        Quota::Usage { .. } => "quota usage",
        Quota::History { .. } => "quota history",
        Quota::Display { .. } => "quota display",
        Quota::Tick { .. } => "quota tick",
    };
    let mut summary = None;
    let result = match &command {
        Quota::Usage { adapter, days } => usage_report(roots, adapter, *days),
        Quota::History {
            account,
            since,
            until,
            limit,
        } => history_report(
            roots,
            &crate::quota_history::Query {
                account: account.clone(),
                since: since.map(|t| t.as_second()),
                until: until.map(|t| t.as_second()),
            },
            *limit,
        ),
        _ => execute(&command, roots, registry, effects).and_then(|report| {
            let human = quota_human(&report);
            if matches!(command, Quota::Show(_)) {
                summary = Some(quota_summary(&report));
            }
            Ok((serde_json::to_value(report)?, human))
        }),
    };
    let (envelope, human) = match result {
        Ok((report, human)) => (CommandResult::new(name, Outcome::Ok, Some(report)), human),
        Err(error) => {
            let status = match &error {
                EdgeError::Config(provenant_config::ConfigError::Read { .. }) => Outcome::Error,
                EdgeError::Adapter
                | EdgeError::Display(_)
                | EdgeError::Config(_)
                | EdgeError::Quota(
                    QuotaError::Invalid(_) | QuotaError::Record(_) | QuotaError::NumberOutOfRange,
                ) => Outcome::Invalid,
                EdgeError::Quota(QuotaError::Refused(_) | QuotaError::Disabled) => Outcome::Refused,
                EdgeError::Fs(FsError::Cancelled { .. }) => Outcome::Cancelled,
                _ => Outcome::Error,
            };
            let mut result = CommandResult::new(name, status, None);
            result.error = Some(CommandError {
                diagnostic: None,
                code: "QUOTA".into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            (result, String::new())
        }
    };
    let mode = globals.render_mode(
        &human,
        matches!(
            command,
            Quota::Show(_) | Quota::Usage { .. } | Quota::History { .. }
        ),
        summary.as_ref(),
    );
    let outcome = crate::render_or_publish(
        &envelope,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        publish,
    );
    outcome.map_or(3, |outcome| crate::exit_status(&outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_doctor_rows_use_the_same_login_observation()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let root = private(roots.state.as_path())?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version = 1\n[domains.accounts.entries.work]\nadapter = 'claude'\nlabel = 'work'\nkey = 'key'\ndirectory = '/fixture/account'\n",
        )?;
        let dir = child(&root, OsStr::new("quota"))?;
        let now = SystemClock::new().unix_seconds();
        let account = AccountScope {
            adapter: "claude".into(),
            account_key: "key".into(),
            label: "work".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.revision = 1;
        record.last_error = Some(ProbeFailure::LoginExpired);
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: account.clone(),
            revision: 1,
            status: QuotaStatus::Available,
            observed_unix_seconds: now - 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![],
            details: None,
        });
        let intent = UpdateIntent {
            account,
            expected: None,
            replacement: Some(record),
            readiness_probe_result: Some(Some(ReadinessReason::LoginExpired)),
            login_probe_expiry: Some((Some(now - 1), 300)),
        };
        write_intent(&dir, roots.state.as_path(), &intent, &mut Vec::new())?;
        let rows = recorded_login_rows(&roots).map_err(std::io::Error::other)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account.label, "work");
        assert_eq!(rows[0].error, Some(ProbeFailure::LoginExpired));
        assert_eq!(
            rows[0].login_expiry.as_ref().unwrap().state,
            LoginExpiryState::Expired
        );
        let readiness = read_readiness(
            roots.state.as_path(),
            &ReadinessScope {
                host: LOCAL_HOST.into(),
                adapter: "claude".into(),
                account_key: "key".into(),
                route: "claude/model".into(),
            },
            SystemClock::new().unix_seconds(),
        );
        assert_eq!(readiness.reason, Some(ReadinessReason::LoginExpired));
        assert!(matches!(
            readiness.project(),
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::LoginExpired,
                ..
            }
        ));
        assert_eq!(
            rows[0].snapshot.as_ref().unwrap().observed_unix_seconds,
            now - 100
        );
        assert!(
            rows[0]
                .repair
                .as_ref()
                .unwrap()
                .contains("CLAUDE_CONFIG_DIR='/fixture/account'")
        );
        Ok(())
    }

    #[test]
    fn login_repair_uses_registered_account_context_or_says_unavailable() {
        let mut entry: provenant_providers::accounts::Registration = serde_json::from_value(
            serde_json::json!({"adapter":"claude","label":"work","key":"key","directory":"/tmp/account's files"}),
        )
        .unwrap();
        assert_eq!(
            login_repair(Some(&entry)),
            "run `CLAUDE_CONFIG_DIR='/tmp/account'\\''s files' claude auth login` interactively"
        );
        let diagnostic = login_diagnostic(
            Some(&entry),
            Some(100),
            &provenant_records::RouteExclusionReason::LoginExpired,
        );
        assert!(
            diagnostic.contains("work [claude]: login expired"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("1970-01-01T00:01:40Z"), "{diagnostic}");
        let mismatch = login_diagnostic(
            Some(&entry),
            Some(100),
            &provenant_records::RouteExclusionReason::AccountMismatch,
        );
        assert!(mismatch.contains("signed-in account differs from registration"));
        assert!(mismatch.contains("CLAUDE_CONFIG_DIR="));
        assert!(
            diagnostic.contains("CLAUDE_CONFIG_DIR='/tmp/account'\\''s files'"),
            "{diagnostic}"
        );
        let missing = login_diagnostic(
            None,
            None,
            &provenant_records::RouteExclusionReason::LoginExpired,
        );
        assert!(
            missing.contains("last successful observation unknown"),
            "{missing}"
        );
        assert!(missing.contains("remedy unavailable"), "{missing}");
        entry.directory = "/tmp/line\nbreak".into();
        assert!(login_repair(Some(&entry)).contains("unavailable"));
        entry.directory = "/tmp/`command`".into();
        assert!(login_repair(Some(&entry)).contains("unavailable"));
        entry.directory = "/tmp/account".into();
        entry.adapter = "unknown".into();
        assert!(login_repair(Some(&entry)).contains("unavailable"));
        assert!(login_repair(None).contains("unavailable"));
    }

    #[test]
    fn account_mismatch_names_the_repair_and_sign_out_keeps_login_text() {
        let human = |error| {
            let report = QuotaReport {
                schema_version: 1,
                generated_unix_seconds: 0,
                accounts: vec![AccountRow {
                    account: AccountScope {
                        adapter: "agy".into(),
                        account_key: "agy-google-abc".into(),
                        label: "work".into(),
                    },
                    login_expiry: None,
                    snapshot: None,
                    status: QuotaStatus::AuthRequired,
                    resets_at: None,
                    refusals: vec![],
                    free_pool_refusal: false,
                    age_seconds: None,
                    next_due_unix_seconds: None,
                    refreshing: false,
                    error: Some(error),
                    repair: None,
                    capacity_cooldowns: vec![],
                    display: None,
                }],
                sources: vec![],
                coverage: Coverage { total: 1, known: 0 },
                refreshing_ids: vec![],
                next_due_unix_seconds: None,
                report_path: String::new(),
                display: Default::default(),
                display_icon_warning: None,
            };
            quota_human(&report)
        };
        let mismatch = human(ProbeFailure::AccountMismatch);
        assert!(mismatch.contains("account_mismatch"), "{mismatch}");
        assert!(
            mismatch.contains("signed-in account differs from the registered one"),
            "{mismatch}"
        );
        assert!(
            mismatch.contains("account-specific login remedy unavailable"),
            "{mismatch}"
        );
        let signed_out = human(ProbeFailure::Auth);
        assert!(signed_out.contains("needs login"), "{signed_out}");
        assert!(!signed_out.contains("differs"), "{signed_out}");
    }

    #[test]
    fn decorate_gives_generic_keychain_repair_without_a_matching_registration() {
        let row = |label: &str| AccountRow {
            account: AccountScope {
                adapter: "agy".into(),
                account_key: "agy-google-abc".into(),
                label: label.into(),
            },
            login_expiry: None,
            snapshot: None,
            status: QuotaStatus::KeychainUnavailable,
            resets_at: None,
            refusals: vec![],
            free_pool_refusal: false,
            age_seconds: None,
            next_due_unix_seconds: None,
            refreshing: false,
            error: Some(ProbeFailure::IsolatedKeychainUnavailable),
            repair: None,
            capacity_cooldowns: vec![],
            display: None,
        };
        let mut report = QuotaReport {
            schema_version: 1,
            generated_unix_seconds: 0,
            accounts: vec![row("legacy")],
            sources: vec![],
            coverage: Coverage { total: 1, known: 0 },
            refreshing_ids: vec![],
            next_due_unix_seconds: None,
            report_path: String::new(),
            display: Default::default(),
            display_icon_warning: None,
        };
        decorate(
            &mut report,
            &provenant_providers::accounts::Settings::default(),
            &Display::default(),
        );
        let repair = report.accounts[0].repair.as_deref().expect("repair text");
        assert!(repair.contains("isolated keychain"), "{repair}");
        assert!(repair.contains("--directory ~ legacy"), "{repair}");
        assert!(repair.contains("docs/runbooks/providers.md"), "{repair}");
    }

    #[test]
    fn isolated_keychain_status_names_expected_path_and_repair() {
        let report = QuotaReport {
            schema_version: 1,
            generated_unix_seconds: 0,
            accounts: vec![AccountRow {
                account: AccountScope {
                    adapter: "agy".into(),
                    account_key: "agy-google-abc".into(),
                    label: "legacy".into(),
                },
                login_expiry: None,
                snapshot: None,
                status: QuotaStatus::KeychainUnavailable,
                resets_at: None,
                refusals: vec![],
                free_pool_refusal: false,
                age_seconds: None,
                next_due_unix_seconds: None,
                refreshing: false,
                error: Some(ProbeFailure::IsolatedKeychainUnavailable),
                repair: Some("expected /tmp/.gemini/Library/Keychains/login.keychain-db; re-register with `provenant accounts register --adapter agy --directory ~ legacy`".into()),
                capacity_cooldowns: vec![],
                display: None,
            }],
            sources: vec![],
            coverage: Coverage { total: 1, known: 0 },
            refreshing_ids: vec![],
            next_due_unix_seconds: None,
            report_path: String::new(),
            display: Default::default(),
            display_icon_warning: None,
        };
        let human = quota_human(&report);
        assert!(human.contains("KEYCHAIN_UNAVAILABLE"), "{human}");
        assert!(
            human.contains("/tmp/.gemini/Library/Keychains/login.keychain-db"),
            "{human}"
        );
        assert!(human.contains("--directory ~ legacy"), "{human}");
    }

    #[test]
    fn summary_only_flags_accounts_needing_attention() {
        let row = |status| AccountRow {
            account: AccountScope {
                adapter: "test".into(),
                account_key: "key".into(),
                label: format!("{status:?}"),
            },
            login_expiry: None,
            snapshot: None,
            status,
            resets_at: None,
            refusals: vec![],
            free_pool_refusal: false,
            age_seconds: None,
            next_due_unix_seconds: None,
            refreshing: false,
            error: None,
            repair: None,
            capacity_cooldowns: vec![],
            display: None,
        };
        let mut report = QuotaReport {
            schema_version: 1,
            generated_unix_seconds: 0,
            accounts: vec![row(QuotaStatus::Available), row(QuotaStatus::CreditOnly)],
            sources: vec![],
            coverage: Coverage { total: 2, known: 2 },
            refreshing_ids: vec![],
            next_due_unix_seconds: None,
            report_path: String::new(),
            display: Default::default(),
            display_icon_warning: Some("warning".into()),
        };
        let summary = quota_summary(&report);
        assert_eq!(summary.lines.len(), 2);
        assert_eq!(summary.next, None);
        report.accounts.push(row(QuotaStatus::Stale));
        let summary = quota_summary(&report);
        assert!(summary.lines.iter().any(|line| line.contains("stale")));
        assert_eq!(summary.next.as_deref(), Some("provenant quota tick"));
    }

    #[test]
    fn quota_reset_output_uses_local_time_and_does_not_invent_missing_expiries() {
        let zone = jiff::tz::TimeZone::get("Australia/Brisbane").unwrap();
        let second = |value: &str| value.parse::<jiff::Timestamp>().unwrap().as_second();
        let row = |label: &str, resets_at: Option<&str>| AccountRow {
            account: AccountScope {
                adapter: "opencode".into(),
                account_key: label.into(),
                label: label.into(),
            },
            login_expiry: None,
            snapshot: None,
            status: QuotaStatus::Exhausted,
            resets_at: resets_at.map(str::to_owned),
            refusals: vec![],
            free_pool_refusal: label == "unknown" || resets_at.is_some(),
            age_seconds: Some(0),
            next_due_unix_seconds: None,
            refreshing: false,
            error: None,
            repair: None,
            capacity_cooldowns: vec![],
            display: None,
        };
        let report = |now, resets_at| QuotaReport {
            schema_version: 1,
            generated_unix_seconds: second(now),
            accounts: vec![row("zen", resets_at), row("unknown", None)],
            sources: vec![],
            coverage: Coverage { total: 2, known: 2 },
            refreshing_ids: vec![],
            next_due_unix_seconds: None,
            report_path: String::new(),
            display: Default::default(),
            display_icon_warning: None,
        };

        let today = report("2026-11-02T00:00:00Z", Some("2026-11-02T00:11:00Z"));
        let human = quota_human_at(&today, zone.clone());
        assert!(
            human.contains("zen [opencode]: EXHAUSTED until 10:11"),
            "{human}"
        );
        assert!(
            human.contains("unknown [opencode]: EXHAUSTED, age"),
            "{human}"
        );
        assert!(
            !human.contains("unknown [opencode]: EXHAUSTED until"),
            "{human}"
        );
        let compact = quota_summary_at(&today, zone.clone());
        assert!(
            compact
                .lines
                .iter()
                .any(|line| line == "zen [opencode]: exhausted until 10:11")
        );
        assert!(
            compact
                .lines
                .iter()
                .any(|line| line == "unknown [opencode]: exhausted")
        );
        assert_eq!(
            serde_json::to_value(&today).unwrap()["accounts"][0]["resets_at"],
            "2026-11-02T00:11:00Z"
        );

        let mut failed = today.clone();
        let row = &mut failed.accounts[0];
        row.error = Some(ProbeFailure::Auth);
        row.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: row.account.clone(),
            revision: 0,
            status: QuotaStatus::Exhausted,
            observed_unix_seconds: second("2026-11-01T23:00:00Z"),
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![],
            details: None,
        });
        let failed_human = quota_human_at(&failed, zone.clone());
        assert!(failed_human.contains("zen [opencode]: EXHAUSTED"));
        assert!(failed_human.contains("(last known)"));
        assert!(failed_human.contains("refresh failed: auth"));

        let mut scoped = today.clone();
        scoped.accounts[1].status = QuotaStatus::Available;
        scoped.accounts[1].free_pool_refusal = false;
        scoped.accounts[1].refusals = vec![provenant_quota::RefusalRow {
            model: "opencode/beam-pro".into(),
            pool: Some("opencode-go".into()),
            resets_at: Some("2026-11-02T00:11:00Z".into()),
        }];
        let human = quota_human_at(&scoped, zone.clone());
        assert!(
            human
                .contains("recorded refusal for opencode/beam-pro in pool opencode-go until 10:11"),
            "{human}"
        );
        let compact = quota_summary_at(&scoped, zone.clone());
        assert!(compact.lines.iter().any(|line| {
            line == "unknown [opencode]: available (recorded refusal for opencode/beam-pro in pool opencode-go until 10:11)"
        }));
        assert_eq!(
            serde_json::to_value(&scoped).unwrap()["accounts"][1]["refusals"][0]["resets_at"],
            "2026-11-02T00:11:00Z"
        );

        let tomorrow = report("2026-11-02T10:00:00Z", Some("2026-11-03T00:11:00Z"));
        assert!(
            quota_human_at(&tomorrow, zone).contains("zen [opencode]: EXHAUSTED until Tue 10:11")
        );
    }

    #[test]
    fn history_size_error_names_its_limit_and_filters() {
        let message = EdgeError::HistoryTooLarge.to_string();
        assert!(message.contains("16 MiB"));
        assert!(message.contains("--limit"));
        assert!(!message.contains("--adapter"));
    }

    #[test]
    fn summary_reports_per_pool_state_when_only_one_pool_is_spent()
    -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = |windows: serde_json::Value| -> Result<_, serde_json::Error> {
            serde_json::from_value::<provenant_quota::QuotaSnapshot>(serde_json::json!({
                "schema_version": 1,
                "account": {"adapter": "agy", "account_key": "k", "label": "agy"},
                "revision": 1, "status": "available", "observed_unix_seconds": 1,
                "source_ref": "t", "authority_ref": null, "windows": windows,
            }))
        };
        let window = |pool: &str, used: u64| serde_json::json!({"pool": pool, "used_basis_points": used, "resets_unix_seconds": null});
        let mixed = snapshot(serde_json::json!([
            window("gemini.weekly", 10_000),
            window("gemini.5h", 500),
            window("claude.weekly", 4_000),
        ]))?;
        assert_eq!(
            pool_states(&mixed).as_deref(),
            Some("gemini exhausted, claude 40%")
        );
        let single = snapshot(serde_json::json!([window("weekly", 10_000)]))?;
        assert_eq!(pool_states(&single), None);
        Ok(())
    }

    #[test]
    fn account_line_names_the_exhausted_shared_pool_without_exhausting_the_account()
    -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = |adapter: &str,
                        status: &str,
                        windows: serde_json::Value|
         -> Result<_, serde_json::Error> {
            serde_json::from_value::<provenant_quota::QuotaSnapshot>(serde_json::json!({
                "schema_version": 1,
                "account": {"adapter": adapter, "account_key": "k", "label": "agy"},
                "revision": 1, "status": status, "observed_unix_seconds": 1,
                "source_ref": "t", "authority_ref": null, "windows": windows,
            }))
        };
        let window = |pool: &str, used: u64| serde_json::json!({"pool": pool, "used_basis_points": used, "resets_unix_seconds": null});
        // The reported registration: the gemini windows have headroom while
        // the shared Claude pool is spent.
        let mixed = snapshot(
            "agy",
            "available",
            serde_json::json!([
                window("gemini.weekly", 2_256),
                window("gemini.5h", 1_701),
                window("claude.weekly", 10_000),
            ]),
        )?;
        assert_eq!(
            shared_pool_exhaustions(&mixed).as_deref(),
            Some("claude pool exhausted (shared)")
        );
        let spent = snapshot(
            "agy",
            "exhausted",
            serde_json::json!([
                window("gemini.weekly", 10_000),
                window("claude.weekly", 10_000),
            ]),
        )?;
        assert_eq!(
            shared_pool_exhaustions(&spent).as_deref(),
            Some("gemini pool exhausted (shared), claude pool exhausted (shared)")
        );
        // A single pool is the account status's own business; ungrouped
        // adapters have no shared pools to name.
        let single = snapshot(
            "agy",
            "exhausted",
            serde_json::json!([window("claude.weekly", 10_000)]),
        )?;
        assert_eq!(shared_pool_exhaustions(&single), None);
        let ungrouped = snapshot(
            "claude",
            "exhausted",
            serde_json::json!([window("weekly", 10_000)]),
        )?;
        assert_eq!(shared_pool_exhaustions(&ungrouped), None);
        let row = |snapshot, status| AccountRow {
            account: AccountScope {
                adapter: "agy".into(),
                account_key: "k".into(),
                label: "agy".into(),
            },
            login_expiry: None,
            snapshot,
            status,
            resets_at: None,
            refusals: vec![],
            free_pool_refusal: false,
            age_seconds: Some(7),
            next_due_unix_seconds: None,
            refreshing: false,
            error: None,
            repair: None,
            capacity_cooldowns: vec![],
            display: None,
        };
        let report = QuotaReport {
            schema_version: 1,
            generated_unix_seconds: 0,
            accounts: vec![row(Some(mixed), QuotaStatus::Available)],
            sources: vec![],
            coverage: Coverage { total: 1, known: 1 },
            refreshing_ids: vec![],
            next_due_unix_seconds: None,
            report_path: String::new(),
            display: Default::default(),
            display_icon_warning: None,
        };
        let human = quota_human(&report);
        assert!(human.contains("agy [agy]: AVAILABLE"), "{human}");
        assert!(
            human.contains("; claude pool exhausted (shared)"),
            "{human}"
        );
        assert!(!human.contains("EXHAUSTED"), "{human}");
        let summary = quota_summary(&report);
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line == "agy [agy]: available (claude pool exhausted (shared))"),
            "{summary:?}"
        );
        let report = QuotaReport {
            accounts: vec![row(Some(spent), QuotaStatus::Exhausted)],
            ..report
        };
        let human = quota_human(&report);
        assert!(
            human.contains("; gemini pool exhausted (shared), claude pool exhausted (shared)"),
            "{human}"
        );
        let summary = quota_summary(&report);
        assert!(summary.lines.iter().any(|line| line == "agy [agy]: exhausted (gemini pool exhausted (shared), claude pool exhausted (shared))"), "{summary:?}");
        Ok(())
    }

    #[test]
    fn quota_record_commit_bumps_generation() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let root = private(temp.path())?;
        let dir = child(&root, OsStr::new("quota"))?;
        let account = AccountScope {
            adapter: "codex".into(),
            account_key: "work".into(),
            label: "work".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.revision = 1;
        write_intent(
            &dir,
            temp.path(),
            &UpdateIntent {
                account,
                expected: None,
                replacement: Some(record),
                readiness_probe_result: None,
                login_probe_expiry: None,
            },
            &mut Vec::new(),
        )?;
        assert_eq!(provenant_os::changes::read(temp.path())?.quota, Some(1));
        Ok(())
    }

    #[test]
    fn credential_metadata_registry_preserves_watched_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let directory = temp.path().join("account");
        let cases: &[(&str, &[&str])] = &[
            ("claude", &[".credentials.json", "settings.json"]),
            ("codex", &["auth.json", "config.toml"]),
            (
                "cursor",
                &["state.vscdb", "state.vscdb-wal", "cli-config.json"],
            ),
            (
                "agy",
                &[
                    "state.vscdb",
                    "state.vscdb-wal",
                    "Library/Keychains/login.keychain-db",
                ],
            ),
            (
                "opencode",
                &[
                    "auth.json",
                    "opencode.db",
                    "opencode.db-wal",
                    "opencode.json",
                ],
            ),
            ("unknown", &[]),
        ];
        for (adapter, files) in cases {
            let registration = serde_json::from_value(serde_json::json!({
                "adapter": adapter, "label": "fixture", "key": "fixture", "directory": directory,
            }))?;
            let mut expected = vec![directory.clone()];
            expected.extend(files.iter().map(|file| directory.join(file)));
            if *adapter == "claude" {
                expected.push(directory.with_extension("json"));
                if cfg!(target_os = "macos") {
                    expected.push(temp.path().join("Library/Keychains/login.keychain-db"));
                }
            }
            let observed = credential_metadata(&registration, Some(temp.path())).unwrap();
            assert_eq!(
                observed
                    .into_iter()
                    .map(|metadata| metadata.path)
                    .collect::<Vec<_>>(),
                expected,
                "{adapter}"
            );
        }
        Ok(())
    }

    #[test]
    fn agy_credential_metadata_watches_the_isolated_keychain()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let directory = temp.path().join("agy-second");
        let registration = serde_json::from_value(serde_json::json!({
            "adapter": "agy", "label": "second", "key": "agy-fixture", "directory": directory,
        }))?;
        let keychain = directory.join("Library/Keychains/login.keychain-db");
        let watched = credential_metadata(&registration, None).unwrap();
        assert!(
            watched
                .iter()
                .any(|m| m.path == keychain && m.stat.is_none())
        );
        std::fs::create_dir_all(keychain.parent().unwrap())?;
        std::fs::write(&keychain, b"first")?;
        let created = credential_metadata(&registration, None).unwrap();
        assert_ne!(watched, created);
        std::fs::write(&keychain, b"second login")?;
        assert_ne!(created, credential_metadata(&registration, None).unwrap());
        Ok(())
    }

    #[test]
    fn credential_change_on_panel_or_wake_retries_once_without_reading_contents()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let root = private(temp.path())?;
        let dir = child(&root, OsStr::new("quota"))?;
        let profile = temp.path().join("codex-profile");
        std::fs::create_dir(&profile)?;
        let registration = serde_json::from_value(serde_json::json!({
            "adapter": "codex", "label": "work", "key": "work", "directory": profile,
        }))?;
        let mut entries = provenant_providers::accounts::Settings {
            entries: [("codex-work".into(), registration)].into(),
            ..Default::default()
        };
        let account = AccountScope {
            adapter: "codex".into(),
            account_key: "work".into(),
            label: "work".into(),
        };
        let policies = [QuotaPolicy {
            adapter: "codex",
            timer_seconds: 300,
            panel_seconds: 120,
            probe_seconds: 10,
            low_power: true,
            solo: false,
        }];
        let mut record = AccountRecord::empty(account.clone());
        record.revision = 1;
        record.last_attempt_unix_seconds = Some(100);
        record.last_error = Some(ProbeFailure::Auth);
        record.failures = 1;
        record.backoff_until_unix_seconds = Some(1000);
        record.credential_metadata =
            credential_metadata(&entries.entries["codex-work"], None).unwrap();
        let mut records = Vec::new();
        write_intent(
            &dir,
            temp.path(),
            &UpdateIntent {
                account: account.clone(),
                expected: None,
                replacement: Some(record),
                readiness_probe_result: None,
                login_probe_expiry: None,
            },
            &mut records,
        )?;
        let detected = [Detection {
            adapter: "codex".into(),
            accounts: Some(vec![account.clone()]),
            status: QuotaStatus::Available,
        }];
        let tick = QuotaTick {
            adapter: None,
            trigger: schedule::Trigger::Panel,
            power: schedule::Power::Ac,
        };
        refresh_auth_metadata(
            &dir,
            temp.path(),
            &mut records,
            &entries,
            &detected,
            &tick,
            None,
        )?;
        assert!(
            schedule::plan_tick(&policies, &records, &detected, &tick, 110)?
                .batches
                .is_empty()
        );

        // This is deliberately invalid credential data, unreadable by a normal
        // user. Stat observes creation without parsing or opening the file.
        let credential = profile.join("auth.json");
        std::fs::write(&credential, b"fake credential contents are never parsed")?;
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o000))?;
        refresh_auth_metadata(
            &dir,
            temp.path(),
            &mut records,
            &entries,
            &detected,
            &tick,
            None,
        )?;
        assert_eq!(
            schedule::plan_tick(&policies, &records, &detected, &tick, 110)?.batches,
            vec![vec![account.clone()]]
        );
        assert!(read_records(&dir)?[0].auth_retry_pending);
        let reserved = schedule::reserve(
            &policies,
            Some(&records[0]),
            &account,
            110,
            "fake",
            tick.trigger,
            tick.power,
        )?
        .replacement
        .unwrap();
        write_intent(
            &dir,
            temp.path(),
            &UpdateIntent {
                account: account.clone(),
                expected: Some(records[0].clone()),
                replacement: Some(reserved.clone()),
                readiness_probe_result: None,
                login_probe_expiry: None,
            },
            &mut records,
        )?;
        let failed = schedule::complete(&reserved, "fake", 112, Err(ProbeFailure::Auth), None)?
            .replacement
            .unwrap();
        write_intent(
            &dir,
            temp.path(),
            &UpdateIntent {
                account: account.clone(),
                expected: Some(records[0].clone()),
                replacement: Some(failed),
                readiness_probe_result: None,
                login_probe_expiry: None,
            },
            &mut records,
        )?;
        refresh_auth_metadata(
            &dir,
            temp.path(),
            &mut records,
            &entries,
            &detected,
            &tick,
            None,
        )?;
        assert!(
            schedule::plan_tick(&policies, &records, &detected, &tick, 113)?
                .batches
                .is_empty()
        );

        // Profile selection changes also bypass auth backoff on wake.
        let second_profile = temp.path().join("second-profile");
        std::fs::create_dir(&second_profile)?;
        entries.entries.get_mut("codex-work").unwrap().directory = second_profile;
        let wake = QuotaTick {
            trigger: schedule::Trigger::Wake,
            ..tick
        };
        refresh_auth_metadata(
            &dir,
            temp.path(),
            &mut records,
            &entries,
            &detected,
            &wake,
            None,
        )?;
        assert_eq!(
            schedule::plan_tick(&policies, &records, &detected, &wake, 114)?.batches,
            vec![vec![account]]
        );
        Ok(())
    }

    #[test]
    fn successful_lane_clears_only_its_auth_and_supersedes_a_probe()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let root = private(temp.path())?;
        let dir = child(&root, OsStr::new("quota"))?;
        let mut records = Vec::new();
        for key in ["first", "second", "mismatch"] {
            let account = AccountScope {
                adapter: "claude".into(),
                account_key: key.into(),
                label: key.into(),
            };
            let mut record = AccountRecord::empty(account.clone());
            record.revision = 1;
            record.last_attempt_unix_seconds = Some(100);
            record.last_error = Some(if key == "mismatch" {
                ProbeFailure::AccountMismatch
            } else {
                ProbeFailure::Auth
            });
            record.failures = 1;
            record.claim = Some(RefreshClaim {
                token: "old".into(),
                deadline_unix_seconds: 155,
            });
            write_intent(
                &dir,
                temp.path(),
                &UpdateIntent {
                    account,
                    expected: None,
                    replacement: Some(record),
                    readiness_probe_result: None,
                    login_probe_expiry: None,
                },
                &mut records,
            )?;
        }
        let report = schedule::report(
            &[],
            &records,
            &[],
            &QuotaQuery { adapter: None },
            110,
            temp.path()
                .join("quota/report.json")
                .to_string_lossy()
                .into_owned(),
        )?;
        let bytes = serde_json::to_vec(&report)?;
        durable(provenant_os::fs::Directory::atomic_write_with_permissions(
            &dir,
            OsStr::new("report.json"),
            ".report",
            &bytes,
            0o600,
            PublishMode::Replace,
        )?)?;
        let route = provenant_records::run::RouteRecord {
            extra: Default::default(),
            provider: Some("claude".into()),
            model: None,
            effort: None,
            account_label: Some("first".into()),
            account_key: Some("first".into()),
            source: None,
            pool_selection: None,
        };
        persist_provider_success(temp.path(), &route, 120)?;
        let mut mismatch_route = route.clone();
        mismatch_route.account_key = Some("mismatch".into());
        mismatch_route.account_label = Some("mismatch".into());
        persist_provider_success(temp.path(), &mismatch_route, 120)?;
        let found = read_records(&dir)?;
        let first = found
            .iter()
            .find(|r| r.account.account_key == "first")
            .unwrap();
        let second = found
            .iter()
            .find(|r| r.account.account_key == "second")
            .unwrap();
        assert_eq!(first.auth_verified_unix_seconds, Some(120));
        assert_eq!(first.last_error, None);
        assert!(first.claim.is_none());
        assert!(schedule::complete(first, "old", 121, Err(ProbeFailure::Auth), None).is_err());
        assert_eq!(second.last_error, Some(ProbeFailure::Auth));
        let mismatch = found
            .iter()
            .find(|r| r.account.account_key == "mismatch")
            .unwrap();
        assert_eq!(mismatch.last_error, Some(ProbeFailure::AccountMismatch));
        assert_eq!(mismatch.auth_verified_unix_seconds, None);
        assert!(mismatch.claim.is_some());
        let report: QuotaReport =
            serde_json::from_slice(&std::fs::read(temp.path().join("quota/report.json"))?)?;
        assert_eq!(
            report
                .accounts
                .iter()
                .find(|r| r.account.account_key == "first")
                .unwrap()
                .status,
            QuotaStatus::Unknown
        );
        assert_eq!(
            report
                .accounts
                .iter()
                .find(|r| r.account.account_key == "second")
                .unwrap()
                .status,
            QuotaStatus::AuthRequired
        );
        Ok(())
    }

    #[test]
    fn healthy_account_probe_supersedes_only_older_auth_readiness()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path();
        let directory = state.join("readiness/key");
        std::fs::create_dir_all(&directory)?;
        for path in [
            state,
            state.join("readiness").as_path(),
            directory.as_path(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let scope = ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: "codex".into(),
            account_key: "key".into(),
            route: "codex/model".into(),
        };
        let mut file = ReadinessFile::default();
        let mut account = AccountRecord::empty(provenant_quota::AccountScope {
            adapter: "codex".into(),
            account_key: "key".into(),
            label: "work".into(),
        });
        account.backoff_until_unix_seconds = Some(500);
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            100,
            300,
            Some(ReadinessReason::SignedOut),
        )?;
        let path = directory.join("codex.json");
        std::fs::write(&path, serde_json::to_vec(&file)?)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        assert_eq!(
            read_readiness(state, &scope, 150).reason,
            Some(ReadinessReason::SignedOut)
        );
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Verified(120)))
                .reason,
            Some(ReadinessReason::NotProbed)
        );
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Verified(90)))
                .reason,
            Some(ReadinessReason::SignedOut)
        );
        Ok(())
    }

    #[test]
    fn unverified_login_preserves_mismatch_refusal_and_newer_auth()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path();
        let dir = state.join("readiness/go");
        std::fs::create_dir_all(&dir)?;
        for path in [state, state.join("readiness").as_path(), dir.as_path()] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let scope = ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: "opencode".into(),
            account_key: "go".into(),
            route: "opencode/model".into(),
        };
        let account = AccountRecord::empty(provenant_quota::AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        });
        let path = dir.join("opencode.json");
        let write = |file: &ReadinessFile| -> Result<(), Box<dyn std::error::Error>> {
            std::fs::write(&path, serde_json::to_vec(file)?)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(())
        };
        let mut file = ReadinessFile::default();
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            100,
            300,
            Some(ReadinessReason::SignedOut),
        )?;
        write(&file)?;
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Unverified(120)))
                .reason,
            Some(ReadinessReason::NotProbed)
        );
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Unverified(90)))
                .reason,
            Some(ReadinessReason::SignedOut)
        );
        file = ReadinessFile::default();
        file.login = Some(provenant_quota::LoginObservation {
            scope: ReadinessScope {
                route: provenant_quota::ACCOUNT_ROUTE.into(),
                ..scope.clone()
            },
            observed_unix_seconds: 100,
            valid_until_unix_seconds: 400,
            expires_unix_seconds: Some(120),
        });
        write(&file)?;
        assert_eq!(
            read_readiness(state, &scope, 150).reason,
            Some(ReadinessReason::LoginExpired)
        );
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Unverified(120)))
                .reason,
            Some(ReadinessReason::NotProbed)
        );
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Unverified(90)))
                .reason,
            Some(ReadinessReason::LoginExpired)
        );
        file.login = None;
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            100,
            300,
            Some(ReadinessReason::AccountMismatch),
        )?;
        write(&file)?;
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Unverified(120)))
                .reason,
            Some(ReadinessReason::AccountMismatch)
        );
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            100,
            300,
            Some(ReadinessReason::ProviderRefusal),
        )?;
        write(&file)?;
        assert_eq!(
            read_readiness_after_probe(state, &scope, 150, Some(LoginEvidence::Unverified(120)))
                .reason,
            Some(ReadinessReason::ProviderRefusal)
        );
        Ok(())
    }

    #[test]
    fn display_icon_resolves_against_registered_accounts() -> Result<(), Box<dyn std::error::Error>>
    {
        let accounts = serde_json::from_value(serde_json::json!({
            "entries": {"codex-work": {
                "adapter": "codex", "label": "work", "key": "opaque", "directory": "/work"
            }}
        }))?;
        // No rows are shown, as when an adapter filter excludes the icon account.
        let mut report = schedule::report(
            &[],
            &[],
            &[],
            &QuotaQuery { adapter: None },
            0,
            "/report.json".into(),
        )?;
        for (icon, expected) in [
            ("codex-removed", MOST_CONSTRAINED),
            ("codex-work", "codex-work"),
            (MOST_CONSTRAINED, MOST_CONSTRAINED),
        ] {
            let display = Display {
                headline: Headline::Weekly,
                icon: icon.into(),
            };
            decorate(&mut report, &accounts, &display);
            assert_eq!(report.display.icon, expected);
            assert_eq!(report.display.headline, Headline::Weekly);
            assert_eq!(display.icon, icon);
            let json = serde_json::to_value(&report)?;
            if icon == "codex-removed" {
                assert_eq!(
                    json["display_icon_warning"],
                    "display.icon names unregistered account 'codex-removed'; using most_constrained"
                );
            } else {
                assert!(json.get("display_icon_warning").is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn refusal_publishes_scoped_readiness_outside_quota_and_fresh_evidence_clears_it()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let state = temp.path();
        let now = SystemClock::new().unix_seconds();
        let scope = |adapter: &str, key: &str, route: &str| ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: adapter.into(),
            account_key: key.into(),
            route: route.into(),
        };
        let pool = scope("claude", "key", "pool");
        // No file reads as unknown, never ready.
        assert_eq!(read_readiness(state, &pool, now), ReadinessRead::absent());

        let refuse = |pool: Option<&str>, model: &str| {
            persist_provider_refusal(
                state,
                &provenant_records::run::RouteRecord {
                    provider: Some("claude".into()),
                    model: Some(model.into()),
                    account_key: Some("key".into()),
                    account_label: Some("work".into()),
                    ..Default::default()
                },
                pool,
                now - 100,
                Some(now + 600),
            )
        };
        refuse(Some("pool"), "claude/one")?;
        refuse(None, "claude/two")?;
        let held = read_readiness(state, &pool, now);
        assert_eq!(held.state, ReadinessState::Unavailable);
        assert_eq!(held.reason, Some(ReadinessReason::ProviderRefusal));
        assert_eq!(held.reset_unix_seconds, Some(now + 600));
        // The pool refusal covers its pool, the model refusal only its model,
        // and neither covers another pool or another account.
        let state_of = |s: &ReadinessScope| read_readiness(state, s, now).state;
        assert_eq!(
            state_of(&scope("claude", "key", "claude/two")),
            ReadinessState::Unavailable
        );
        assert_eq!(
            state_of(&scope("claude", "key", "claude/one")),
            ReadinessState::Unknown
        );
        assert_eq!(
            state_of(&scope("claude", "key", "other")),
            ReadinessState::Unknown
        );
        assert_eq!(
            state_of(&scope("claude", "second", "pool")),
            ReadinessState::Unknown
        );
        // Readiness is its own tree: quota/ keeps only the account snapshot.
        let account_dir = state.join("quota").join("key");
        assert_eq!(std::fs::read_dir(&account_dir)?.count(), 1);
        assert!(state.join("readiness/key/claude.json").is_file());
        assert_eq!(read_cache_at(state)?.len(), 1);
        // A reset time passing does not turn the refusal into ready.
        assert_eq!(
            read_readiness(state, &pool, now + 600).state,
            ReadinessState::Unknown
        );

        // A fresh reading with headroom in the pool clears that refusal only.
        let mut records = read_cache_at(state)?;
        let current = records[0].clone();
        let mut next = current.clone();
        next.revision += 1;
        next.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: next.account.clone(),
            revision: 0,
            status: QuotaStatus::Available,
            observed_unix_seconds: now - 10,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![QuotaWindow {
                pool: "pool".into(),
                window_seconds: None,
                used_basis_points: 1000,
                resets_unix_seconds: None,
            }],
        });
        let dir = child(&private(state)?, OsStr::new("quota"))?;
        let account = current.account.clone();
        write_intent(
            &dir,
            state,
            &UpdateIntent {
                account,
                expected: Some(current),
                replacement: Some(next),
                readiness_probe_result: None,
                login_probe_expiry: None,
            },
            &mut records,
        )?;
        assert_eq!(state_of(&pool), ReadinessState::Unknown);
        assert_eq!(
            state_of(&scope("claude", "key", "claude/two")),
            ReadinessState::Unavailable
        );
        // Forgetting the account removes its readiness file.
        let drop = UpdateIntent {
            account: records[0].account.clone(),
            expected: Some(records[0].clone()),
            replacement: None,
            readiness_probe_result: None,
            login_probe_expiry: None,
        };
        write_intent(&dir, state, &drop, &mut records)?;
        assert!(!state.join("readiness/key").exists());
        Ok(())
    }

    #[test]
    fn execute_publishes_probe_outcomes_as_readiness() -> Result<(), Box<dyn std::error::Error>> {
        struct Effects {
            failure: Option<ProbeFailure>,
            malformed: bool,
            expiry: Option<i64>,
        }
        impl QuotaEffects for Effects {
            fn accounts(&self) -> Result<Vec<AccountScope>, QuotaError> {
                Ok(vec![AccountScope {
                    adapter: "codex".into(),
                    account_key: "key".into(),
                    label: "fixture".into(),
                }])
            }
            fn capture(
                &self,
                _: &QuotaProbePlan,
                _: Option<&AccountRecord>,
            ) -> Result<ProbeCapture, ProbeFailure> {
                if let Some(failure) = self.failure {
                    return Err(failure);
                }
                Ok(ProbeCapture {
                    stdout: if self.malformed {
                        b"{".to_vec()
                    } else {
                        include_bytes!("../../providers/src/quota/fixtures/codex-app-server.json")
                            .to_vec()
                    },
                    stderr: vec![],
                    exit_code: Some(0),
                    overflowed: false,
                })
            }
            fn login_expiry_unix_seconds(&self) -> Option<i64> {
                self.expiry
            }
        }
        for (failure, malformed, expected) in [
            (None, false, None),
            (None, true, Some(ReadinessReason::CommandFailure)),
            (
                Some(ProbeFailure::MissingCli),
                false,
                Some(ReadinessReason::MissingExecutable),
            ),
            (
                Some(ProbeFailure::Auth),
                false,
                Some(ReadinessReason::SignedOut),
            ),
            (
                Some(ProbeFailure::AccountMismatch),
                false,
                Some(ReadinessReason::AccountMismatch),
            ),
            (
                Some(ProbeFailure::Network),
                false,
                Some(ReadinessReason::CommandFailure),
            ),
        ] {
            let temp = tempfile::tempdir()?;
            let mut inputs = provenant_core::RootInputs::new(
                temp.path().join("home"),
                temp.path().join("checkout"),
            );
            inputs.state_dir = Some(temp.path().join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let command = Quota::Tick {
                adapter: Filter {
                    adapter: Some("codex".into()),
                },
                trigger: Trigger::Manual,
                power: Power::Ac,
            };
            let expiry = SystemClock::new().unix_seconds() + 60;
            let expiry = (failure.is_none() && !malformed).then_some(expiry);
            let report = execute(
                &command,
                &roots,
                provenant_providers::REGISTRY,
                &Effects {
                    failure,
                    malformed,
                    expiry,
                },
            )?;
            let login = report.accounts[0].login_expiry.as_ref().unwrap();
            assert_eq!(login.expires_unix_seconds, expiry);
            assert_eq!(
                login.state,
                if expiry.is_some() {
                    LoginExpiryState::Expiring
                } else {
                    LoginExpiryState::Unknown
                }
            );
            let human = quota_human(&report);
            assert!(
                human.contains(if expiry.is_some() {
                    "login expiring"
                } else {
                    "login expiry unknown"
                }),
                "{human}"
            );
            if expiry.is_some() {
                assert!(
                    human.contains("account-specific login remedy unavailable"),
                    "{human}"
                );
            }
            let json = serde_json::to_value(&report)?;
            assert_eq!(
                json["accounts"][0]["login_expiry"]["state"],
                if expiry.is_some() {
                    "expiring"
                } else {
                    "unknown"
                }
            );
            let read = read_readiness(
                roots.state.as_path(),
                &ReadinessScope {
                    host: LOCAL_HOST.into(),
                    adapter: "codex".into(),
                    account_key: "key".into(),
                    route: "codex/model".into(),
                },
                SystemClock::new().unix_seconds(),
            );
            assert_eq!(
                read.reason,
                Some(expected.unwrap_or(ReadinessReason::NotProbed)),
                "{failure:?}/{malformed}"
            );
            assert_eq!(
                read.state,
                if expected.is_some() {
                    ReadinessState::Unavailable
                } else {
                    ReadinessState::Unknown
                }
            );
        }
        Ok(())
    }

    #[test]
    fn readiness_edge_rejects_damaged_wrong_mode_and_oversized_files()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let root = private(temp.path())?;
        let readiness = child(&root, OsStr::new(READINESS_DIRECTORY))?;
        let _account = child(&readiness, OsStr::new("key"))?;
        let path = temp.path().join("readiness/key/claude.json");
        let scope = ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: "claude".into(),
            account_key: "key".into(),
            route: "claude/model".into(),
        };
        let file = ReadinessFile {
            schema_version: 1,
            login: None,
            records: vec![provenant_quota::RouteReadiness {
                schema_version: 1,
                scope: scope.clone(),
                state: ReadinessState::Ready,
                reason: None,
                observed_unix_seconds: 100,
                expires_unix_seconds: 200,
                reset_unix_seconds: None,
                evidence_ref: Some("fixture".into()),
            }],
        };
        let valid = serde_json::to_vec(&file)?;
        for (bytes, mode, expected) in [
            (valid.clone(), 0o600, ReadinessState::Ready),
            (b"{".to_vec(), 0o600, ReadinessState::Unknown),
            (valid, 0o644, ReadinessState::Unknown),
            (
                vec![b' '; MAX_RECORD_BYTES as usize + 1],
                0o600,
                ReadinessState::Unknown,
            ),
        ] {
            std::fs::write(&path, bytes)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
            let read = read_readiness(temp.path(), &scope, 150);
            assert_eq!(read.state, expected);
            if expected == ReadinessState::Unknown {
                assert_eq!(read.reason, Some(ReadinessReason::InvalidEvidence));
            }
        }
        Ok(())
    }

    #[test]
    fn probe_failure_readiness_is_account_wide_and_a_fresh_success_clears_it()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let state = temp.path();
        let dir = child(&private(state)?, OsStr::new("quota"))?;
        let account = AccountScope {
            adapter: "agy".into(),
            account_key: "work".into(),
            label: "work".into(),
        };
        let mut records = Vec::new();
        let mut initial = AccountRecord::empty(account.clone());
        initial.revision = 1;
        let now = SystemClock::new().unix_seconds();
        initial.refusals = (0..provenant_quota::MAX_READINESS_RECORDS)
            .map(|i| provenant_quota::Refusal {
                extra: Default::default(),
                account: account.clone(),
                model: format!("model-{i}"),
                pool: Some(format!("pool-{i}")),
                observed_unix_seconds: now - 10,
                resets_unix_seconds: Some(now + 600),
            })
            .collect();
        write_intent(
            &dir,
            state,
            &UpdateIntent {
                account: account.clone(),
                expected: None,
                replacement: Some(initial),
                readiness_probe_result: None,
                login_probe_expiry: None,
            },
            &mut records,
        )?;

        let current = records[0].clone();
        let mut failed = current.clone();
        failed.revision += 1;
        failed.last_error = Some(ProbeFailure::Network);
        let failure = UpdateIntent {
            account: account.clone(),
            expected: Some(current),
            replacement: Some(failed),
            readiness_probe_result: Some(Some(ReadinessReason::ProbeTimeout)),
            login_probe_expiry: None,
        };
        write_intent(&dir, state, &failure, &mut records)?;
        let route = ReadinessScope {
            host: LOCAL_HOST.into(),
            adapter: "agy".into(),
            account_key: "work".into(),
            route: "agy/model".into(),
        };
        let held = read_readiness(state, &route, SystemClock::new().unix_seconds());
        assert_eq!(held.state, ReadinessState::Unavailable);
        assert_eq!(held.reason, Some(ReadinessReason::ProbeTimeout));

        let current = records[0].clone();
        let mut succeeded = current.clone();
        succeeded.revision += 1;
        succeeded.last_error = None;
        let success = UpdateIntent {
            account,
            expected: Some(current),
            replacement: Some(succeeded),
            readiness_probe_result: Some(None),
            login_probe_expiry: None,
        };
        write_intent(&dir, state, &success, &mut records)?;
        assert_eq!(
            read_readiness(state, &route, SystemClock::new().unix_seconds()).state,
            ReadinessState::Unknown
        );
        let pool = ReadinessScope {
            route: "pool-0".into(),
            ..route.clone()
        };
        assert_eq!(
            read_readiness(state, &pool, SystemClock::new().unix_seconds()).reason,
            Some(ReadinessReason::ProviderRefusal)
        );
        let bytes = std::fs::read(state.join("quota/work/agy.json"))?;
        assert_eq!(AccountRecord::decode(&bytes)?, records[0]);
        Ok(())
    }

    #[test]
    fn drop_removes_empty_orphan_quota_directory() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let dir = private(temp.path())?;
        let account = AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        };
        let mut records = Vec::new();
        let mut initial = AccountRecord::empty(account.clone());
        initial.revision = 1;
        let add = UpdateIntent {
            account: account.clone(),
            expected: None,
            replacement: Some(initial),
            readiness_probe_result: None,
            login_probe_expiry: None,
        };
        write_intent(&dir, temp.path(), &add, &mut records)?;
        // The state root is explicit, so a directory not named quota still signals.
        assert_eq!(provenant_os::changes::read(temp.path())?.quota, Some(1));
        assert_eq!(records.len(), 1);
        assert!(dir.open_verified_directory(OsStr::new("go")).is_ok());

        let drop_intent = UpdateIntent {
            account: account.clone(),
            expected: Some(records[0].clone()),
            replacement: None,
            readiness_probe_result: None,
            login_probe_expiry: None,
        };
        write_intent(&dir, temp.path(), &drop_intent, &mut records)?;
        assert_eq!(records.len(), 0);
        assert!(dir.open_verified_directory(OsStr::new("go")).is_err());

        // Pre-existing empty orphan directory is cleaned up by clean_orphan_dirs.
        let orphan = child(&dir, OsStr::new("anese"))?;
        drop(orphan);
        assert!(dir.open_verified_directory(OsStr::new("anese")).is_ok());
        clean_orphan_dirs(&dir)?;
        assert!(dir.open_verified_directory(OsStr::new("anese")).is_err());
        Ok(())
    }

    #[test]
    fn quota_human_and_json_display_partial_ledger_with_calibration()
    -> Result<(), Box<dyn std::error::Error>> {
        let account = AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: account.clone(),
            revision: 0,
            status: QuotaStatus::Available,
            observed_unix_seconds: 100,
            source_ref: "quota.provider.opencode".into(),
            authority_ref: None,
            windows: vec![QuotaWindow {
                pool: "five_hour".into(),
                used_basis_points: 401,
                resets_unix_seconds: None,
                window_seconds: Some(18_000),
            }],
            details: None,
        });
        let mut records = vec![record];
        let mut settings = Settings::default();
        settings.calibration.insert(
            "go".into(),
            Calibration {
                five_hour: Some(43),
                seven_day: Some(17),
                thirty_day: Some(59),
                expires: provenant_core::WallTime::parse_rfc3339("2026-11-04T00:00:00Z").unwrap(),
            },
        );
        settings.apply_calibration(&mut records, 100);
        let report = schedule::report(
            &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            &records,
            &[Detection {
                adapter: "opencode".into(),
                accounts: Some(vec![account]),
                status: QuotaStatus::Available,
            }],
            &QuotaQuery { adapter: None },
            100,
            "/report.json".into(),
        )?;
        let human = quota_human(&report);
        let snapshot = report.accounts[0].snapshot.as_ref().unwrap();
        assert!(pool_states(snapshot).unwrap().contains("five_hour >=43%"));
        assert!(
            quota_summary(&report)
                .lines
                .iter()
                .any(|line| line.contains("five_hour >=43%"))
        );
        assert!(
            human.contains("source: local-ledger; coverage: partial (lower bound, this host only)"),
            "{human}"
        );
        assert!(human.contains("43.00% used"), "{human}");
        let json = serde_json::to_value(&report)?;
        assert_eq!(
            json["accounts"][0]["snapshot"]["details"]["coverage"],
            "partial"
        );
        assert_eq!(
            json["accounts"][0]["snapshot"]["windows"][0]["used_basis_points"],
            4300
        );
        Ok(())
    }

    #[test]
    fn quota_human_output_prints_three_of_three_current_readings() {
        let accounts =
            [("claude", "michael"), ("claude", "pro"), ("codex", "work")].map(|(adapter, key)| {
                AccountScope {
                    adapter: adapter.into(),
                    account_key: key.into(),
                    label: key.into(),
                }
            });
        let records: Vec<_> = accounts
            .iter()
            .map(|a| {
                let mut r = AccountRecord::empty(a.clone());
                r.revision = 1;
                r.snapshot = Some(QuotaSnapshot {
                    schema_version: 1,
                    account: a.clone(),
                    revision: 1,
                    status: QuotaStatus::Available,
                    observed_unix_seconds: 100,
                    source_ref: "fixture".into(),
                    authority_ref: None,
                    windows: vec![],
                    details: None,
                });
                r
            })
            .collect();
        // Include an unregistered cached account and an unsupported source adapter.
        let mut all_records = records;
        all_records.push(AccountRecord::empty(AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        }));
        let detections = vec![
            Detection {
                adapter: "claude".into(),
                accounts: Some(vec![accounts[0].clone(), accounts[1].clone()]),
                status: QuotaStatus::Available,
            },
            Detection {
                adapter: "codex".into(),
                accounts: Some(vec![accounts[2].clone()]),
                status: QuotaStatus::Available,
            },
            Detection {
                adapter: "cursor".into(),
                accounts: None,
                status: QuotaStatus::Unsupported,
            },
            Detection {
                adapter: "opencode".into(),
                accounts: None,
                status: QuotaStatus::Unsupported,
            },
        ];
        let policies = [
            QuotaPolicy {
                adapter: "claude",
                timer_seconds: 300,
                panel_seconds: 120,
                probe_seconds: 14,
                low_power: true,
                solo: false,
            },
            QuotaPolicy {
                adapter: "codex",
                timer_seconds: 300,
                panel_seconds: 120,
                probe_seconds: 14,
                low_power: true,
                solo: false,
            },
        ];
        let rep = schedule::report(
            &policies,
            &all_records,
            &detections,
            &QuotaQuery { adapter: None },
            100,
            "/report.json".into(),
        )
        .unwrap();
        assert_eq!(rep.coverage.known, 3);
        assert_eq!(rep.coverage.total, 3);
        let headline = format!(
            "Quota: {}/{} current readings\n",
            rep.coverage.known, rep.coverage.total
        );
        assert_eq!(headline, "Quota: 3/3 current readings\n");
    }

    struct AuthRequiredAdapter;
    impl provenant_providers::Adapter for AuthRequiredAdapter {
        fn id(&self) -> &'static str {
            "claude"
        }
        fn account(
            &self,
            _: &provenant_providers::StreamState,
        ) -> Result<provenant_providers::AccountDetection, provenant_providers::ProviderError>
        {
            Ok(provenant_providers::AccountDetection::AuthRequired)
        }
        fn prepare(
            &self,
            _: provenant_providers::PrepareInput<'_>,
        ) -> Result<provenant_providers::LaunchSpec, provenant_providers::ProviderError> {
            unimplemented!()
        }
        fn stream(
            &self,
            continuation: Option<&provenant_providers::Continuation>,
        ) -> Result<provenant_providers::StreamState, provenant_providers::ProviderError> {
            provenant_providers::claude::REGISTRATION
                .adapter
                .stream(continuation)
        }
        fn observe(
            &self,
            _: &mut provenant_providers::StreamState,
            _: &[u8],
        ) -> Result<Vec<provenant_providers::ProviderEvent>, provenant_providers::ProviderError>
        {
            unimplemented!()
        }
        fn finish(
            &self,
            _: provenant_providers::StreamState,
            _: provenant_providers::ExitObservation,
        ) -> Result<provenant_providers::ProviderOutcome, provenant_providers::ProviderError>
        {
            unimplemented!()
        }
        fn continuation(
            &self,
            _: &provenant_providers::ProviderOutcome,
            _: &provenant_records::ResolvedRoute,
            _: &str,
        ) -> Result<provenant_providers::Continuation, provenant_providers::ProviderError> {
            unimplemented!()
        }
        fn validate_provenance(
            &self,
            _: &provenant_providers::RouteProvenance<'_>,
        ) -> Result<provenant_providers::VerifiedLineage, provenant_providers::ProviderError>
        {
            unimplemented!()
        }
    }

    static AUTH_REQUIRED_ADAPTER: AuthRequiredAdapter = AuthRequiredAdapter;

    #[test]
    fn auth_required_adapter_with_registered_account_shows_stale_cached_reading() {
        let policy = QuotaPolicy {
            adapter: "claude",
            timer_seconds: 300,
            panel_seconds: 120,
            probe_seconds: 14,
            low_power: true,
            solo: false,
        };
        let registration = provenant_providers::Registration {
            adapter: &AUTH_REQUIRED_ADAPTER,
            quota_source: None,
            stdout_decoder: None,
            stderr_decoder: None,
            display_name: "Claude",
            quota: policy,
        };
        let account = AccountScope {
            adapter: "claude".into(),
            account_key: "michael".into(),
            label: "michael".into(),
        };
        let configured = vec![account.clone()];
        let detected = detections(&[registration], &configured);
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].status, QuotaStatus::AuthRequired);
        assert_eq!(detected[0].accounts, Some(configured.clone()));
    }
}
