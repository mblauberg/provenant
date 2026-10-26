//! Quota arguments, composition through OS crate's safe filesystem, and render.
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
fn private(path: &Path) -> Result<Directory, EdgeError> {
    let dir = create_path(path)?;
    let meta = dir.metadata()?;
    if meta.mode & 0o077 != 0 || meta.uid != provenant_os::fs::effective_uid() {
        return Err(EdgeError::Permissions);
    }
    Ok(dir)
}
fn child(parent: &Directory, name: &OsStr) -> Result<Directory, EdgeError> {
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
    let root = match Directory::open_anchor(roots.state.as_path()) {
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
    write_intent(&directory, &intent, &mut records)
}

fn write_intent(
    dir: &Directory,
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
            if let Some(p) = pos {
                records.remove(p);
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
                    (!accounts.is_empty()
                        && !matches!(
                            status,
                            Ok(provenant_providers::AccountDetection::AuthRequired)
                        ))
                    .then_some(accounts)
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
fn execute(
    command: &Quota,
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    effects: &dyn QuotaEffects,
) -> Result<QuotaReport, EdgeError> {
    let all = Filter { adapter: None };
    let (filter, tick) = match command {
        Quota::Show(filter) => (filter, None),
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
    let policies = provenant_providers::quota_policies(registry);
    let configured = effects.accounts()?;
    let detected = detections(registry, &configured);
    let query = QuotaQuery {
        adapter: filter.adapter.clone(),
    };
    let Some(tick) = tick else {
        let mut records = read_cache(roots)?;
        add_configured(&mut records, &configured);
        let mut report =
            schedule::report(&policies, &records, &detected, &query, now, report_path)?;
        decorate(&mut report, &entries, &settings.display);
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
        let plan = schedule::plan_tick_with_settings(
            &policies, &records, &detected, &tick, now, &settings,
        )?;
        for drop in &plan.drops {
            write_intent(&dir, drop, &mut records)?;
        }
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
            write_intent(&dir, &intent, &mut records)?;
            current
        };
        // The durable claim owns this probe; never hold quota.lock across host I/O.
        let source = registry
            .iter()
            .filter_map(|r| r.quota_source)
            .find(|s| s.adapter_id() == account.adapter);
        let outcome = match source {
            Some(source) => {
                let input = QuotaProbeInput {
                    now_unix_seconds: at,
                    deadline_unix_seconds: plan.deadline_unix_seconds,
                    authority_ref: Some("configured-local-quota".into()),
                };
                match source.probe(account, &input) {
                    Ok(probe) => effects
                        .capture(&probe, current.as_ref())
                        .and_then(|capture| {
                            source
                                .decode(&probe, &capture)
                                .map_err(|_| ProbeFailure::Malformed)
                        }),
                    Err(_) => Err(ProbeFailure::Disabled),
                }
            }
            None => Err(ProbeFailure::Disabled),
        };
        let finished = clock
            .wall_now()
            .map_err(|_| EdgeError::Time)?
            .as_timestamp()
            .as_second();
        let _lock = lock()?;
        let mut records = read_records(&dir)?;
        if finished < at + schedule::TICK_BUDGET_SECONDS
            && let Some(reserved) = records.iter().find(|r| {
                r.account.adapter == account.adapter && r.account.account_key == account.account_key
            })
        {
            match schedule::complete(reserved, &token, finished, outcome, None) {
                Ok(complete) => write_intent(&dir, &complete, &mut records)?,
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
    let mut report = schedule::report(
        &policies,
        &records,
        &detected,
        &QuotaQuery { adapter: None },
        now,
        report_path,
    )?;
    decorate(&mut report, &entries, &settings.display);
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
    // Publish all accounts even when the caller requests a filtered projection.
    let mut shown = schedule::report(
        &policies,
        &records,
        &detected,
        &query,
        now,
        report.report_path,
    )?;
    decorate(&mut shown, &entries, &settings.display);
    Ok(shown)
}
pub fn run(
    command: Quota,
    globals: &crate::Globals,
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    effects: &dyn QuotaEffects,
    publish: impl FnOnce(
        &CommandResult<Option<provenant_quota::QuotaReport>>,
    ) -> io::Result<provenant_records::AbsolutePath>,
) -> u8 {
    let name = match command {
        Quota::Show(_) => "quota show",
        Quota::Display { .. } => "quota display",
        Quota::Tick { .. } => "quota tick",
    };
    let result = execute(&command, roots, registry, effects);
    let (envelope, human) = match result {
        Ok(report) => {
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
                human.push_str(&format!(
                    "{} [{}]: {status}, age {age}",
                    row.account.label, row.account.adapter
                ));
                if let Some(snapshot) = &row.snapshot {
                    if row.status == QuotaStatus::Stale {
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
                }
                if let Some(error) = row.error {
                    let reason = serde_json::to_value(error)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "unknown".into());
                    human.push_str(&format!("; refresh failed: {reason}"));
                    if error == ProbeFailure::Auth {
                        human.push_str(" (needs login in this identity)");
                    }
                }
                human.push('\n');
            }
            human.push_str(&format!("Report: {}\n", report.report_path));
            (CommandResult::new(name, Outcome::Ok, Some(report)), human)
        }
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
                code: "QUOTA".into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            (result, String::new())
        }
    };
    let mode = if globals.json {
        crate::RenderMode::Json
    } else {
        crate::RenderMode::Human {
            text: &human,
            read_only: matches!(command, Quota::Show(_)),
        }
    };
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
}
