//! Host ingestion and portable journal subscriptions. Source stores remain authoritative.
#![allow(clippy::disallowed_methods)] // Process edge owns deadlines and the host forwarder.
use provenant_core::{CancelToken, Clock, ProjectId, Roots, WallTime};
use provenant_fabric_store::{Request, Store};
use provenant_os::{
    changes::watch::Watcher,
    clock::SystemClock,
    fs::{Directory, LockMode, PublishMode},
};
use provenant_records::{
    RecordHeader,
    lane_contract::{
        Coverage, ObservationCursor, ObservationEvent, ObservationKind, ObservationLineage,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(crate) fn warn(message: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
}
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const LIMIT: u64 = 4 * 1024 * 1024;
const REPLAY_PAGE: u64 = 100;
const LABEL_BYTES: usize = 128;
const MAX_PR_ASSOCIATIONS: usize = 128;
const GAP: Duration = Duration::from_secs(60);

fn directory(roots: &Roots, project: ProjectId) -> Result<Directory> {
    let mut dir = Directory::open_anchor(roots.state.as_path())?;
    for name in ["event-delivery".to_owned(), project.to_string()] {
        match dir.mkdir(name.as_ref(), 0o700) {
            Ok(()) => {}
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        dir = dir.open_verified_directory(name.as_ref())?;
    }
    Ok(dir)
}

fn save(directory: &Directory, name: &str, value: &impl Serialize) -> Result<()> {
    directory
        .atomic_write(
            name.as_ref(),
            ".event-delivery",
            &serde_json::to_vec(value)?,
            PublishMode::Replace,
        )?
        .require_durable()?;
    Ok(())
}
fn read<T: serde::de::DeserializeOwned + Default>(directory: &Directory, name: &str) -> Result<T> {
    match directory.read_bounded(name.as_ref(), LIMIT) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.is_not_found() => Ok(T::default()),
        Err(error) => Err(error.into()),
    }
}
fn key(value: &impl Serialize) -> Result<String> {
    Ok(provenant_records::CanonicalV1::file_digest(&serde_json::to_vec(value)?).to_string())
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Subscription {
    repository: String,
    number: u64,
    head: Option<String>,
}

pub(crate) struct Journal {
    roots: Roots,
    project: ProjectId,
    store: Store,
    watcher: Watcher,
    cursor: i64,
}
impl Journal {
    pub(crate) fn open(state: &Path, project: &Path) -> Result<Self> {
        let home = crate::roots::home()?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.to_owned());
        inputs.state_dir = Some(state.to_owned());
        inputs.workspaces.push(project.to_owned());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = provenant_cli::fabric::Context::open(roots.clone(), &home, project)?;
        Self::for_project(roots, context.identity.project)
    }
    pub(crate) fn for_project(roots: Roots, project: ProjectId) -> Result<Self> {
        let watcher = Watcher::subscribe(&[roots
            .state
            .as_path()
            .join(provenant_core::changes::FILE_NAME)])?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = Store::open(&roots.state, &request)?;
        let cursor = store.observation_tip(project, &request)?;
        Ok(Self {
            roots,
            project,
            store,
            watcher,
            cursor,
        })
    }
    pub(crate) fn resume_graphs(roots: Roots, project: ProjectId, cursor: i64) -> Result<Self> {
        let mut journal = Self::for_project(roots, project)?;
        journal.cursor = cursor;
        Ok(journal)
    }
    pub(crate) fn tip(roots: &Roots, project: ProjectId) -> Result<i64> {
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        Ok(Store::open(&roots.state, &request)?.observation_tip(project, &request)?)
    }
    pub(crate) fn register_pr(
        &self,
        repository: &str,
        number: u64,
        head: Option<&str>,
    ) -> Result<()> {
        if !valid_repository(repository) || number == 0 {
            return Err("invalid forge subscription identity".into());
        }
        let subscription = Subscription {
            repository: repository.into(),
            number,
            head: head.map(str::to_owned),
        };
        let name = format!("pr-{}.json", key(&subscription)?);
        let dir = directory(&self.roots, self.project)?;
        save(&dir, &name, &subscription)
    }
    /// An unchanged conditional host read is still a healthy source. Its
    /// local receipt prevents every quiet waiter starting its own forge poll.
    pub(crate) fn source_current(
        &self,
        repository: &str,
        number: u64,
        head: Option<&str>,
    ) -> Result<bool> {
        self.source_current_at(
            repository,
            number,
            head,
            SystemClock.wall_now()?.as_micros(),
        )
    }
    fn source_current_at(
        &self,
        repository: &str,
        number: u64,
        head: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        let subscription = Subscription {
            repository: repository.into(),
            number,
            head: head.map(str::to_owned),
        };
        let dir = directory(&self.roots, self.project)?;
        let progress: PollProgress =
            read(&dir, &format!("progress-pr-{}.json", key(&subscription)?))?;
        let age = now.saturating_sub(progress.last_success_micros);
        // Allow one missed host tick before declaring a source gap. Poller
        // cadence is bounded by the installed scheduler, not a chair timer.
        let freshness = i64::from(crate::tick_agent::INTERVAL) * 2_000_000;
        Ok(progress.failures == 0
            && !progress.closed
            && progress.last_success_micros > 0
            && (0..=freshness).contains(&age))
    }
    pub(crate) fn drain_attempts(
        &mut self,
    ) -> Result<std::collections::BTreeSet<provenant_core::AttemptId>> {
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let mut attempts = std::collections::BTreeSet::new();
        loop {
            let page = self
                .store
                .observations(self.project, self.cursor, REPLAY_PAGE, &request)?;
            let full = page.items.len() == REPLAY_PAGE as usize;
            for event in page.items {
                self.cursor = event.cursor.seq;
                if let Some(attempt) = event.lineage.attempt {
                    attempts.insert(attempt);
                }
            }
            if !page.truncated && !full {
                return Ok(attempts);
            }
        }
    }
    /// Consume checkpoint/lifecycle markers once. Reconcile reads are scoped
    /// to affected graphs; a timeout heals a missing or coalesced host hint.
    pub(crate) fn drain_graphs(&mut self) -> Result<std::collections::BTreeSet<String>> {
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let mut graphs = std::collections::BTreeSet::new();
        loop {
            let page = self
                .store
                .observations(self.project, self.cursor, REPLAY_PAGE, &request)?;
            let full = page.items.len() == REPLAY_PAGE as usize;
            for event in page.items {
                self.cursor = event.cursor.seq;
                if let Some(graph) = event.detail["graph"].as_str() {
                    graphs.insert(graph.into());
                }
            }
            if !page.truncated && !full {
                return Ok(graphs);
            }
        }
    }
    /// Replays all intervening records before waiting. Hints never advance a cursor.
    /// None marks a gap/expiry: the caller reconciles its authoritative source.
    pub(crate) fn next_pr(
        &mut self,
        repository: &str,
        number: u64,
        maximum: Duration,
        cancel: &CancelToken,
    ) -> Result<Option<Value>> {
        let deadline = Instant::now() + maximum;
        loop {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            let clock = SystemClock;
            let request = Request {
                clock: &clock,
                cancel,
            };
            let page = self
                .store
                .observations(self.project, self.cursor, REPLAY_PAGE, &request)?;
            let full = page.items.len() == REPLAY_PAGE as usize;
            for event in page.items {
                self.cursor = event.cursor.seq;
                let detail = &event.detail;
                let webhook = detail["source"] == "webhook";
                let mentions_pr = detail["number"].as_u64() == Some(number)
                    || detail["pull_requests"].as_array().is_some_and(|pulls| {
                        pulls.iter().any(|pull| pull.as_u64() == Some(number))
                    });
                if detail["repository"].as_str() == Some(repository)
                    && mentions_pr
                    && (webhook || detail["status"].is_object())
                {
                    // Webhooks are change hints, not authoritative state. Keep
                    // the local marker to the identifiers needed for a targeted
                    // read; the source store remains authoritative.
                    return if webhook {
                        Ok(Some(json!({"repository":repository,"number":number})))
                    } else {
                        Ok(Some(event.detail))
                    };
                }
            }
            if page.truncated || full {
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || cancel.is_cancelled() {
                return Ok(None);
            }
            // Cancellation checkpoints do not trigger database rereads.
            loop {
                if cancel.is_cancelled() {
                    return Ok(None);
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(None);
                }
                match self.watcher.wait(left.min(Duration::from_millis(250)))? {
                    provenant_os::changes::watch::WaitOutcome::Changed => break,
                    provenant_os::changes::watch::WaitOutcome::Woken => break,
                    provenant_os::changes::watch::WaitOutcome::TimedOut => {}
                }
            }
        }
    }
}

fn valid_repository(repository: &str) -> bool {
    let parts: Vec<_> = repository.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}
fn append(
    roots: &Roots,
    project: ProjectId,
    id: String,
    kind: ObservationKind,
    detail: Value,
) -> Result<()> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = Store::open(&roots.state, &request)?;
    let event = ObservationEvent {
        header: RecordHeader::V1,
        id,
        cursor: ObservationCursor { project, seq: 0 },
        lineage: ObservationLineage {
            attempt: None,
            parent_attempt: None,
            work: None,
            extra: Default::default(),
        },
        kind,
        coverage: Coverage::Partial,
        detail,
        extra: Default::default(),
    };
    // Stable source identity requires a stable envelope on retry, including time.
    store.append_observation(&event, WallTime::from_micros(0)?, &request)?;
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
struct PollProgress {
    next_micros: i64,
    failures: u32,
    last: Option<String>,
    generation: u64,
    #[serde(default)]
    closed: bool,
    #[serde(default)]
    last_success_micros: i64,
}
impl PollProgress {
    fn schedule(&mut self, now: i64, success: bool, interval: Duration) {
        self.failures = if success {
            0
        } else {
            self.failures.saturating_add(1)
        };
        let delay = interval
            .saturating_mul(2_u32.saturating_pow(self.failures.min(8)))
            .min(Duration::from_secs(900));
        self.next_micros = now.saturating_add(i64::try_from(delay.as_micros()).unwrap_or(i64::MAX));
    }
}

/// One invocation belongs to the existing host tick. A project lock shares all
/// subscriptions across concurrent manual/scheduled ticks, never across chairs.
pub(crate) fn tick(resolved: &crate::roots::Resolved) -> Result<()> {
    if provenant_cli::fabric::in_lane() {
        return Err("forge ingestion requires host custody".into());
    }
    let mut errors = Vec::new();
    for binding in provenant_config::registered_projects(&resolved.roots)? {
        let result = (|| -> Result<()> {
            let context = provenant_cli::fabric::Context::open_for_project(
                resolved.roots.clone(),
                &resolved.home,
                &binding,
            )?;
            let project = binding.project_id();
            let dir = directory(&resolved.roots, project)?;
            let _guard = match dir.try_lock("poll.lock".as_ref(), LockMode::Exclusive) {
                Ok(guard) => guard,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            continue_after_index(
                project,
                || index_graphs(&resolved.roots, project),
                || index_notes(&resolved.roots, project),
                || {
                    let mut inputs = provenant_core::RootInputs::new(
                        resolved.home.clone(),
                        context.project_root.clone(),
                    );
                    inputs.state_dir = Some(resolved.roots.state.as_path().to_owned());
                    inputs.workspaces.push(context.project_root.clone());
                    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
                    let (settings, origin) = provenant_cli::wait::Settings::resolve(&roots)?;
                    let now = SystemClock.wall_now()?.as_micros();
                    // Monitor-only consumers also receive forge events. Repository
                    // discovery is cached by the host, never repeated by each chair.
                    let mut repository: String = read(&dir, "repository.json")?;
                    if repository.is_empty() {
                        repository = match crate::pr::status::wait_repository(
                            roots.state.as_path(),
                            &context.project_root,
                            &provenant_config::ports::Request::within(Duration::from_secs(
                                settings.pr_read_timeout_seconds,
                            )),
                        ) {
                            Ok(repository) => repository,
                            Err(error) => {
                                warn(&format!(
                                    "forge discovery unavailable for {project}: {error}; local journal sources continue; {}",
                                    origin.describe("domains.wait.forge_source")
                                ));
                                return Ok(());
                            }
                        };
                        if !valid_repository(&repository) {
                            return Err("invalid discovered repository".into());
                        }
                        save(&dir, "repository.json", &repository)?;
                    }
                    let mut feed: PollProgress = read(&dir, "feed-progress.json")?;
                    if now >= feed.next_micros {
                        let request = provenant_config::ports::Request::within(
                            Duration::from_secs(settings.pr_read_timeout_seconds),
                        );
                        let result = (|| -> Result<()> {
                            let selected = source(
                                &dir,
                                &repository,
                                &settings.forge_source,
                                &origin.describe("domains.wait.forge_source"),
                                &context,
                                &request,
                                now,
                            )?;
                            if selected == "webhook"
                                && let Err(error) = start_forwarder(
                                    &roots,
                                    project,
                                    &context.project_root,
                                    &repository,
                                )
                            {
                                warn(&format!(
                                    "forwarder unavailable: {error}; conditional reconciliation continues"
                                ));
                            }
                            poll_repository(
                                &roots,
                                project,
                                &context.project_root,
                                &repository,
                                &dir,
                                &request,
                            )
                        })();
                        feed.schedule(
                            now,
                            result.is_ok(),
                            Duration::from_millis(settings.pr_slow_interval_ms),
                        );
                        save(&dir, "feed-progress.json", &feed)?;
                        if let Err(error) = result {
                            errors.push(format!("{repository}: {error}"));
                        }
                    }
                    for name in dir.entries()? {
                        let name = name?;
                        let Some(name) = name
                            .to_str()
                            .filter(|name| name.starts_with("pr-") && name.ends_with(".json"))
                        else {
                            continue;
                        };
                        let subscription: Subscription = read(&dir, name)?;
                        if !valid_repository(&subscription.repository)
                            || subscription.number == 0
                            || !subscription.repository.eq_ignore_ascii_case(&repository)
                        {
                            errors.push(format!("invalid forge subscription: {name}"));
                            continue;
                        }
                        let progress_name = format!("progress-{name}");
                        let mut progress: PollProgress = read(&dir, &progress_name)?;
                        if progress.closed || now < progress.next_micros {
                            continue;
                        }
                        let request = provenant_config::ports::Request::within(
                            Duration::from_secs(settings.pr_read_timeout_seconds),
                        );
                        let mut selected = source(
                            &dir,
                            &subscription.repository,
                            &settings.forge_source,
                            &origin.describe("domains.wait.forge_source"),
                            &context,
                            &request,
                            now,
                        )?;
                        if selected == "webhook"
                            && let Err(error) = start_forwarder(
                                &roots,
                                project,
                                &context.project_root,
                                &subscription.repository,
                            )
                        {
                            warn(&format!(
                                "forwarder unavailable: {error}; using conditional poller"
                            ));
                            selected = "poll".into();
                        }
                        // Conditional reconciliation also heals forwarder disconnects and
                        // forge history gaps. Webhook mode uses the slower gap cadence.
                        let interval = if selected == "webhook" {
                            GAP
                        } else {
                            Duration::from_millis(settings.pr_interval_ms)
                        };
                        let result = poll_one(
                            &roots,
                            project,
                            &context.project_root,
                            &subscription,
                            &request,
                            &mut progress,
                        );
                        progress.schedule(now, result.is_ok(), interval);
                        save(&dir, &progress_name, &progress)?;
                        if let Err(error) = result {
                            errors.push(format!(
                                "{} #{}: {error}",
                                subscription.repository, subscription.number
                            ));
                        }
                    }
                    Ok(())
                },
            )
        })();
        if let Err(error) = result {
            errors.push(format!("{}: {error}", binding.project_id()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; ").into())
    }
}
fn poll_repository(
    roots: &Roots,
    project: ProjectId,
    path: &Path,
    repository: &str,
    dir: &Directory,
    request: &provenant_config::ports::Request,
) -> Result<()> {
    let mut observer =
        crate::pr::observer::Observer::open(roots.state.as_path(), path, repository, request)?;
    let pulls = observer.resource("pulls", Duration::ZERO, request)?;
    for pull in pulls.as_array().ok_or("invalid open pull request feed")? {
        let Some(number) = pull["number"].as_u64() else {
            continue;
        };
        let subscription = Subscription {
            repository: repository.into(),
            number,
            head: None,
        };
        save(
            dir,
            &format!("pr-{}.json", key(&subscription)?),
            &subscription,
        )?;
        let progress_name = format!("progress-pr-{}.json", key(&subscription)?);
        let mut progress: PollProgress = read(dir, &progress_name)?;
        if progress.closed {
            progress.closed = false;
            progress.next_micros = 0;
            save(dir, &progress_name, &progress)?;
        }

        let detail = json!({"repository":repository,"number":number,"head":pull["head"]["sha"],"state":pull["state"],"updated_at":pull["updated_at"],"source":"host_poller"});
        append(
            roots,
            project,
            format!("forge-pull:{}", key(&detail)?),
            ObservationKind::PullRequest,
            detail,
        )?;
        if let Some(head) = pull["head"]["sha"].as_str() {
            let suites = observer.resource(
                &format!("commits/{head}/check-suites"),
                Duration::ZERO,
                request,
            )?;
            for suite in suites["check_suites"].as_array().into_iter().flatten() {
                let (kind, mut detail) =
                    normalise_webhook(repository, json!({"check_suite":suite,"number":number}))
                        .ok_or("invalid check suite observation")?;
                detail["head"] = json!(head);
                detail["source"] = json!("host_poller");
                append(
                    roots,
                    project,
                    format!("forge-suite:{}", key(&detail)?),
                    kind,
                    detail,
                )?;
            }
        }
    }
    let feed = observer.resource("events", Duration::ZERO, request)?;
    for event in feed.as_array().ok_or("invalid repository event feed")? {
        let payload = &event["payload"];
        if let Some((kind, mut detail)) = normalise_webhook(repository, payload.clone()) {
            detail["source"] = json!("host_poller");
            detail["source_id"] = event["id"].clone();
            detail["source_at"] = event["created_at"].clone();
            append(
                roots,
                project,
                format!("forge-feed:{}", key(&detail)?),
                kind,
                detail,
            )?;
        }
    }
    let workflows = observer.resource("actions/runs", Duration::ZERO, request)?;
    for workflow in workflows["workflow_runs"].as_array().into_iter().flatten() {
        let (kind, mut detail) = normalise_webhook(repository, json!({"workflow_run":workflow}))
            .ok_or("invalid workflow observation")?;
        detail["source"] = json!("host_poller");
        append(
            roots,
            project,
            format!("forge-workflow:{}", key(&detail)?),
            kind,
            detail,
        )?;
    }
    Ok(())
}

/// Indexing is replayable enrichment. A broken local journal must not stop the
/// host from discovering repositories or polling their authoritative source.
fn continue_after_index<T>(
    project: ProjectId,
    graphs: impl FnOnce() -> Result<()>,
    notes: impl FnOnce() -> Result<()>,
    forge: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if let Err(error) = graphs() {
        warn(&format!(
            "graph observation indexing failed for {project}: {error}; will retry"
        ));
    }
    if let Err(error) = notes() {
        warn(&format!(
            "note observation indexing failed for {project}: {error}; will retry"
        ));
    }
    forge()
}

fn poll_one(
    roots: &Roots,
    project: ProjectId,
    path: &Path,
    subscription: &Subscription,
    request: &provenant_config::ports::Request,
    progress: &mut PollProgress,
) -> Result<()> {
    let mut observer = crate::pr::observer::Observer::open(
        roots.state.as_path(),
        path,
        &subscription.repository,
        request,
    )?;
    let (status, checks) = crate::pr::status::poll_line(
        &mut observer,
        subscription.number,
        subscription.head.as_deref(),
        request,
        Duration::ZERO,
    )?;
    let detail = poll_detail(subscription, &status, &checks)?;
    project_poll(roots, project, subscription, progress, detail)
}
fn poll_detail(
    subscription: &Subscription,
    status: &crate::pr::status::Line,
    checks: &[crate::pr::observer::Check],
) -> Result<Value> {
    // Bound display metadata while retaining every verdict bucket. A large
    // check matrix must not prevent its terminal outcome entering the journal.
    let mut projected_checks: Vec<Value> = checks
        .iter()
        .take(32)
        .map(|check| {
            json!({
                "name":provenant_cli::bounded_text(&check.name, LABEL_BYTES),"head":check.head,
                "bucket":check.bucket,"app":check.app,"conclusion":check.conclusion
            })
        })
        .collect();
    let buckets: std::collections::BTreeSet<_> = checks.iter().map(|check| &check.bucket).collect();
    for bucket in buckets {
        projected_checks.push(json!({"bucket":bucket}));
    }
    let mut projected_status = serde_json::to_value(status)?;
    if let Some(names) = projected_status["failing_checks"].as_array_mut() {
        names.truncate(16);
        for name in names {
            if let Some(text) = name.as_str() {
                *name = json!(provenant_cli::bounded_text(text, LABEL_BYTES));
            }
        }
    }
    let detail = json!({"repository":subscription.repository,"number":subscription.number,
        "head":subscription.head.as_deref().unwrap_or(&status.head),"pull_head":status.head,
        "checks":projected_checks,"checks_truncated":checks.len()>32,"status":projected_status,"source":"host_poller"});
    Ok(detail)
}
fn project_poll(
    roots: &Roots,
    project: ProjectId,
    subscription: &Subscription,
    progress: &mut PollProgress,
    detail: Value,
) -> Result<()> {
    let closed = matches!(
        detail["status"]["state"].as_str(),
        Some("CLOSED" | "MERGED")
    ) && detail["checks"]
        .as_array()
        .is_some_and(|checks| checks.iter().all(|check| check["bucket"] != "pending"));
    let digest = key(&detail)?;
    if progress.last.as_ref() != Some(&digest) {
        let generation = progress
            .generation
            .checked_add(1)
            .ok_or("forge generation exhausted")?;
        let id = format!("forge-poll:{}:{generation}:{digest}", key(subscription)?);
        append(roots, project, id, ObservationKind::CheckRun, detail)?;
        progress.generation = generation;
        progress.last = Some(digest);
    }
    progress.closed = closed;
    progress.last_success_micros = SystemClock.wall_now()?.as_micros();
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
struct Selection {
    requested: String,
    selected: String,
    source: String,
    warning: Option<String>,
    retry_micros: i64,
}
fn choose_source(
    requested: &str,
    probe: impl FnOnce() -> std::result::Result<(), &'static str>,
) -> (&'static str, Option<&'static str>) {
    if requested == "poll" {
        return ("poll", None);
    }
    match probe() {
        Ok(()) => ("webhook", None),
        Err(reason) => ("poll", Some(reason)),
    }
}
fn source(
    dir: &Directory,
    repository: &str,
    requested: &str,
    origin: &str,
    context: &provenant_cli::fabric::Context,
    request: &provenant_config::ports::Request,
    now: i64,
) -> Result<String> {
    let path = context.project_root.as_path();
    let state = context.roots.state.as_path();
    let name = format!("source-{}.json", key(&repository)?);
    let previous: Selection = read(dir, &name)?;
    if previous.requested == requested && now < previous.retry_micros {
        return Ok(previous.selected);
    }
    let mut selection = Selection {
        requested: requested.into(),
        selected: "poll".into(),
        source: origin.into(),
        warning: None,
        retry_micros: now.saturating_add(3_600_000_000),
    };
    let (selected, warning) = choose_source(requested, || {
        let ports = provenant_config::PortTable::read_for(Some(path), state).map_err(
            |_| "forge adapter configuration unavailable; using shared conditional poller",
        )?;
        if !matches!(ports.forge.backend, provenant_config::PortBackend::Builtin) {
            return Err(
                "configured forge adapter has no streaming forwarder; using shared conditional poller",
            );
        }
        let probe = crate::pr::forge_execute(
            state,
            path,
            &["gh", "webhook", "forward", "--help"],
            request,
            false,
            None,
        );
        if !probe.is_ok_and(|output| output.termination == provenant_os::exec::Termination::Exit(0))
        {
            return Err("gh-webhook extension unavailable; using shared conditional poller");
        }
        let admin = crate::pr::forge_execute(
            state,
            path,
            &[
                "gh",
                "api",
                &format!("repos/{repository}"),
                "--jq",
                ".permissions.admin",
            ],
            request,
            false,
            None,
        )
        .and_then(crate::pr::successful);
        if admin.is_ok_and(|bytes| String::from_utf8_lossy(&bytes).trim() == "true") {
            Ok(())
        } else {
            Err("repository admin rights unavailable; using shared conditional poller")
        }
    });
    selection.selected = selected.into();
    selection.warning = warning.map(str::to_owned);

    if let Some(warning) = &selection.warning {
        warn(&format!("{warning}; {origin}"));
    }
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "forge source={} requested={requested}; {origin}",
        selection.selected
    );
    save(dir, &name, &selection)?;
    Ok(selection.selected)
}
fn start_forwarder(roots: &Roots, project: ProjectId, path: &Path, repository: &str) -> Result<()> {
    let dir = directory(roots, project)?;
    let lock = format!("forward-{}.lock", key(&repository)?);
    match dir.try_lock(lock.as_ref(), LockMode::Exclusive) {
        Ok(guard) => drop(guard),
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    // The child takes the long-lived lock; duplicate launches are cheap no-ops.
    let argv = [
        std::env::current_exe()?.into_os_string(),
        "--state-dir".into(),
        roots.state.as_path().as_os_str().to_owned(),
        "--project".into(),
        path.as_os_str().to_owned(),
        "internal".into(),
        "event-listen".into(),
        repository.into(),
    ];
    provenant_os::exec::spawn_detached(&argv, path)?;
    Ok(())
}

/// gh-webhook writes complete JSON payloads to stdout when --url is omitted.
/// Decode a JSON stream rather than assuming that payloads occupy one line.
pub(crate) fn listen(repository: &str, globals: &provenant_cli::Globals) -> u8 {
    match listen_inner(repository, globals) {
        Ok(()) => 0,
        Err(error) => {
            warn(&format!(
                "forge forwarder: {error}; host reconciliation continues"
            ));
            1
        }
    }
}
fn listen_inner(repository: &str, globals: &provenant_cli::Globals) -> Result<()> {
    use std::{
        io::BufReader,
        process::{Command, Stdio},
    };
    if provenant_cli::fabric::in_lane() || !valid_repository(repository) {
        return Err("invalid host forwarder context".into());
    }
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let context = provenant_cli::fabric::Context::open_inspection(
        resolved.roots.clone(),
        &resolved.home,
        &resolved.project,
    )?;
    let bound_repository = crate::pr::status::wait_repository(
        resolved.roots.state.as_path(),
        &resolved.project,
        &provenant_config::ports::Request::within(Duration::from_secs(60)),
    )?;
    if !bound_repository.eq_ignore_ascii_case(repository) {
        return Err("forwarder repository is outside the bound project".into());
    }
    let dir = directory(&resolved.roots, context.identity.project)?;
    let lock = format!("forward-{}.lock", key(&repository)?);
    let _guard = match dir.try_lock(lock.as_ref(), LockMode::Exclusive) {
        Ok(guard) => guard,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let ports = provenant_config::PortTable::read_for(
        Some(&resolved.project),
        resolved.roots.state.as_path(),
    )?;
    if !matches!(ports.forge.backend, provenant_config::PortBackend::Builtin) {
        return Err("configured forge adapter has no streaming forwarder".into());
    }
    let mut child = Command::new("gh")
        .args([
            "webhook",
            "forward",
            "--repo",
            repository,
            "--events",
            "check_run,check_suite,workflow_run,pull_request,issue_comment",
        ])
        .current_dir(&resolved.project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let output = child.stdout.take().ok_or("missing forwarder stdout")?;
    let result = (|| -> Result<()> {
        for payload in
            serde_json::Deserializer::from_reader(BufReader::new(output)).into_iter::<Value>()
        {
            let payload = payload?;
            if let Some((kind, detail)) = normalise_webhook(repository, payload) {
                let id = format!("forge-webhook:{}", key(&detail)?);
                append(&resolved.roots, context.identity.project, id, kind, detail)?;
                // Refresh every subscription on this repository on the next host
                // tick. Only the host poller builds complete PR/CI projections.
                let Ok(_poll) = dir.try_lock("poll.lock".as_ref(), LockMode::Exclusive) else {
                    continue;
                };
                for name in dir.entries()? {
                    let name = name?;
                    let Some(name) = name
                        .to_str()
                        .filter(|name| name.starts_with("pr-") && name.ends_with(".json"))
                    else {
                        continue;
                    };
                    let subscription: Subscription = read(&dir, name)?;
                    if subscription.repository.eq_ignore_ascii_case(repository) {
                        let progress_name = format!("progress-{name}");
                        let mut progress: PollProgress = read(&dir, &progress_name)?;
                        progress.next_micros = 0;
                        save(&dir, &progress_name, &progress)?;
                    }
                }
            }
        }
        Ok(())
    })();
    let _ = child.kill();
    let status = child.wait();
    // An account switch or stream disconnect must not leave a cached webhook
    // selection repeatedly launching a dead forwarder. Poll while retrying it.
    let reason = match &result {
        Err(error) => format!("forwarder disconnected: {error}; using conditional poller"),
        Ok(()) => format!("forwarder exited ({status:?}); using conditional poller"),
    };
    record_forwarder_exit(
        &dir,
        repository,
        SystemClock.wall_now()?.as_micros(),
        &reason,
    )?;
    match result {
        Err(error) => Err(error),
        Ok(()) => Err(reason.into()),
    }
}
fn record_forwarder_exit(dir: &Directory, repository: &str, now: i64, reason: &str) -> Result<()> {
    let name = format!("source-{}.json", key(&repository)?);
    let mut selection: Selection = read(dir, &name)?;
    selection.selected = "poll".into();
    selection.warning = Some(reason.into());
    selection.retry_micros = now.saturating_add(60_000_000);
    save(dir, &name, &selection)
}
fn normalise_webhook(repository: &str, payload: Value) -> Option<(ObservationKind, Value)> {
    let kind = if payload.get("check_run").is_some() {
        ObservationKind::CheckRun
    } else if payload.get("check_suite").is_some() {
        ObservationKind::CheckSuite
    } else if payload.get("workflow_run").is_some() {
        ObservationKind::WorkflowRun
    } else if payload.get("comment").is_some() {
        ObservationKind::IssueComment
    } else if payload.get("pull_request").is_some() {
        ObservationKind::PullRequest
    } else {
        return None;
    };
    // Retain identifiers/state only: comments, PR bodies and user text are not
    // required for delivery and can be unbounded or contain credentials.
    let entity = match kind {
        ObservationKind::CheckRun => &payload["check_run"],
        ObservationKind::CheckSuite => &payload["check_suite"],
        ObservationKind::WorkflowRun => &payload["workflow_run"],
        ObservationKind::IssueComment => &payload["comment"],
        _ => &payload["pull_request"],
    };
    let mut pull_requests: Vec<_> = entity["pull_requests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pull| pull["number"].as_u64())
        .take(MAX_PR_ASSOCIATIONS + 1)
        .collect();
    let pull_requests_truncated = pull_requests.len() > MAX_PR_ASSOCIATIONS;
    pull_requests.truncate(MAX_PR_ASSOCIATIONS);
    let name = entity["name"].as_str();
    let name_truncated = name.is_some_and(|name| name.len() > LABEL_BYTES);
    let name = name.map(|name| provenant_cli::bounded_text(name, LABEL_BYTES));
    let number = payload["number"]
        .as_u64()
        .or_else(|| payload["issue"]["number"].as_u64())
        .or_else(|| entity["number"].as_u64())
        .or_else(|| pull_requests.first().copied());
    Some((
        kind,
        json!({"repository":repository,"action":payload["action"],"number":number,
            "pull_requests":pull_requests,"pull_requests_truncated":pull_requests_truncated,
            "id":entity["id"],"name":name,"name_truncated":name_truncated,
            "head":entity["head_sha"].as_str().or_else(||entity["head"]["sha"].as_str()),
            "state":entity["state"],"status":entity["status"],"conclusion":entity["conclusion"],
            "updated_at":entity["updated_at"],"source":"webhook"}),
    ))
}

pub(crate) fn ingest_lifecycle(
    roots: &Roots,
    project: ProjectId,
    host: provenant_core::HostId,
    runs: &provenant_run::RunStore,
) -> Result<()> {
    let dir = directory(roots, project)?;
    let _guard = match dir.try_lock("lifecycle.lock".as_ref(), LockMode::Exclusive) {
        Ok(guard) => guard,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let run_request = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = Store::open(&roots.state, &request)?;
    let mut cursor: u64 = read(&dir, "lifecycle-cursor.json")?;
    // Bounded catch-up per tick; source records and cursor remain replayable.
    for _ in 0..16 {
        let events = runs.events(cursor, 64, None, &run_request)?;
        if events.is_empty() {
            break;
        }
        store.ingest_run_events(project, host, &events, &request)?;
        cursor = events.last().ok_or("missing lifecycle page")?.sequence;
        save(&dir, "lifecycle-cursor.json", &cursor)?;
        if events.len() < 64 {
            break;
        }
    }
    Ok(())
}

fn index_graphs(roots: &Roots, project: ProjectId) -> Result<()> {
    use provenant_fabric_runtime::graph::FileJournal;
    for graph in FileJournal::graph_ids(&roots.state, project)? {
        FileJournal::index_observation(&roots.state, project, &graph)?;
    }
    Ok(())
}

/// Reconcile immutable note files into the shared stream. The source-local
/// sequence is only a progress marker: each note is reread from Board and the
/// store's stable observation identity makes a retry after a crash idempotent.
fn index_notes(roots: &Roots, project: ProjectId) -> Result<()> {
    use provenant_fabric_runtime::notes::{Board, Host, Input, Scope};

    let dir = directory(roots, project)?;
    let _guard = match dir.try_lock("notes-index.lock".as_ref(), LockMode::Exclusive) {
        Ok(guard) => guard,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = Store::open(&roots.state, &request)?;
    let board = Board::open(
        roots.state.as_path(),
        project,
        provenant_fabric_runtime::notes::default_read_cap(),
        provenant_fabric_runtime::notes::default_byte_cap(),
    )?;
    let host = Host::chair(project, None);
    let mut cursor: i64 = read(&dir, "notes-cursor.json")?;
    for _ in 0..16 {
        let page: Value = serde_json::from_value(board.execute(
            &host,
            Input::Read {
                reader: "observation-stream".into(),
                scope: Scope::Project,
                after: Some(cursor),
                limit: Some(64),
            },
            &clock,
            &cancel,
        )?)?;
        let notes = page["events"].as_array().ok_or("invalid notes page")?;
        for value in notes {
            let note: provenant_fabric_runtime::notes::Note =
                serde_json::from_value(value.clone())?;
            let observation = note.observation(ObservationCursor { project, seq: 0 })?;
            // Match the source identity and envelope on replay. The note's own
            // provenance time remains in detail and its lineage is preserved.
            store.append_observation(&observation, WallTime::from_micros(0)?, &request)?;
        }
        cursor = page["next"].as_i64().ok_or("invalid notes cursor")?;
        save(&dir, "notes-cursor.json", &cursor)?;
        if page["truncated"] != true || notes.is_empty() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)] // The custom worktree driver compiles without a test harness.
    fn fixture() -> (tempfile::TempDir, Roots, ProjectId) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        provenant_os::fixture::private_state_dir(roots.state.as_path()).unwrap();
        (temp, roots, ProjectId::from_bytes([34; 16]))
    }

    #[test]
    fn failed_local_indexes_are_independent_and_nonfatal() {
        let project = ProjectId::from_bytes([36; 16]);
        let graph_calls = std::cell::Cell::new(0);
        let note_calls = std::cell::Cell::new(0);
        let forge_calls = std::cell::Cell::new(0);
        let tick = || {
            continue_after_index(
                project,
                || {
                    graph_calls.set(graph_calls.get() + 1);
                    Err("corrupt graph checkpoint".into())
                },
                || {
                    note_calls.set(note_calls.get() + 1);
                    Err("corrupt note file".into())
                },
                || {
                    forge_calls.set(forge_calls.get() + 1);
                    Ok(())
                },
            )
        };
        tick().unwrap();
        assert_eq!(graph_calls.get(), 1);
        assert_eq!(note_calls.get(), 1);
        assert_eq!(forge_calls.get(), 1);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn note_and_correction_reconcile_once_across_consumer_reconnects() -> Result<()> {
        use provenant_fabric_runtime::notes::{Board, Host, Input, Scope};
        use provenant_records::lane_contract::ObservationKind;

        let (_temp, roots, project) = fixture();
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let board = Board::open(
            roots.state.as_path(),
            project,
            provenant_fabric_runtime::notes::default_read_cap(),
            provenant_fabric_runtime::notes::default_byte_cap(),
        )?;
        let host = Host::chair(project, None);
        let add = |text: &str, resolves| Input::Add {
            text: text.into(),
            tags: vec![],
            scope: Scope::Project,
            resolves,
        };
        let first: provenant_fabric_runtime::notes::Note = serde_json::from_value(board.execute(
            &host,
            add("first note", None),
            &clock,
            &cancel,
        )?)?;
        index_notes(&roots, project)?;

        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = Store::open(&roots.state, &request)?;
        let consumer = provenant_core::ActorId::from_bytes([35; 16]);
        let first_page = store.observations(project, 0, 10, &request)?;
        assert_eq!(first_page.items.len(), 1);
        assert_eq!(first_page.items[0].id, first.id);
        assert_eq!(first_page.items[0].lineage, first.lineage);
        assert_eq!(first_page.items[0].kind, ObservationKind::Note);
        store.mark_events_read(project, consumer, first_page.items[0].cursor.seq, &request)?;
        let resumed = store.event_cursor(project, consumer, &request)?;
        assert_eq!(resumed, first_page.items[0].cursor.seq);

        let correction: provenant_fabric_runtime::notes::Note = serde_json::from_value(
            board.execute(&host, add("corrected note", Some(1)), &clock, &cancel)?,
        )?;
        index_notes(&roots, project)?;
        // Simulate retry after the source was indexed but before its cursor was
        // trusted by a restarted indexer.
        let dir = directory(&roots, project)?;
        save(&dir, "notes-cursor.json", &0_i64)?;
        index_notes(&roots, project)?;

        let after_reconnect = store.observations(project, resumed, 10, &request)?;
        assert_eq!(after_reconnect.items.len(), 1);
        assert_eq!(after_reconnect.items[0].id, correction.id);
        assert_eq!(after_reconnect.items[0].kind, ObservationKind::Note);
        assert_eq!(after_reconnect.items[0].detail["resolves"], 1);
        store.mark_events_read(
            project,
            consumer,
            after_reconnect.items[0].cursor.seq,
            &request,
        )?;
        assert_eq!(
            store
                .observations(
                    project,
                    store.event_cursor(project, consumer, &request)?,
                    10,
                    &request,
                )?
                .items
                .len(),
            0
        );
        let replay = store.observations(project, 0, 10, &request)?;
        assert_eq!(
            replay
                .items
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.id.as_str(), correction.id.as_str()]
        );
        Ok(())
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn journal_subscriptions_register_projects_for_host_discovery() -> Result<()> {
        if crate::fabric::peer_tests::host_test(
            "event_delivery::tests::journal_subscriptions_register_projects_for_host_discovery",
        )? {
            return Ok(());
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let journal = Journal::open(&rig.state, &rig.project)?;
        journal.register_pr("owner/repo", 7, None)?;
        let bindings = provenant_config::registered_projects(&journal.roots)?;
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].project_id(), journal.project);
        let resumed = Journal::open(&rig.state, &rig.project)?;
        assert_eq!(resumed.project, journal.project);
        assert_eq!(
            provenant_config::registered_projects(&resumed.roots)?.len(),
            bindings.len()
        );
        Ok(())
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn host_projection_deduplicates_reconnects_and_preserves_subscription_after_gap() {
        let (_temp, roots, project) = fixture();
        let subscription = Subscription {
            repository: "owner/repo".into(),
            number: 7,
            head: None,
        };
        let mut journal = Journal::for_project(roots.clone(), project).unwrap();
        journal.register_pr("owner/repo", 7, None).unwrap();
        let cancel = CancelToken::new();
        assert!(
            journal
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap()
                .is_none()
        );
        let mut progress = PollProgress::default();
        let pending = json!({"repository":"owner/repo","number":7,"head":"abc","checks":[{"bucket":"pending"}],"status":{"state":"OPEN"}});
        project_poll(
            &roots,
            project,
            &subscription,
            &mut progress,
            pending.clone(),
        )
        .unwrap();
        // A crash before progress publication retries the same source identity.
        project_poll(
            &roots,
            project,
            &subscription,
            &mut PollProgress::default(),
            pending.clone(),
        )
        .unwrap();
        assert_eq!(
            journal
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap(),
            Some(pending)
        );
        let cursor = journal.cursor;
        assert!(
            journal
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap()
                .is_none()
        );
        drop(journal);
        let complete = json!({"repository":"owner/repo","number":7,"head":"abc","checks":[{"bucket":"pass"}],"status":{"state":"CLOSED"}});
        project_poll(
            &roots,
            project,
            &subscription,
            &mut progress,
            complete.clone(),
        )
        .unwrap();
        assert!(
            progress.closed,
            "closed outcomes stop active polling without deleting subscriptions"
        );
        let mut resumed = Journal::resume_graphs(roots.clone(), project, cursor).unwrap();
        assert_eq!(
            resumed
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap(),
            Some(complete)
        );
        let dir = directory(&roots, project).unwrap();
        let kept: Subscription =
            read(&dir, &format!("pr-{}.json", key(&subscription).unwrap())).unwrap();
        assert_eq!(
            kept.number, subscription.number,
            "expiry keeps the host interest alive"
        );
    }

    #[test]
    fn webhook_pr_events_wake_waiters_with_identifier_only_markers() -> Result<()> {
        let (_temp, roots, project) = fixture();
        let mut journal = Journal::for_project(roots.clone(), project).unwrap();
        let payload = json!({"check_run":{"id":42,"head_sha":"abc",
            "status":"completed","conclusion":"success",
            "pull_requests":[{"number":7},{"number":9}]}});
        let (kind, detail) = normalise_webhook("owner/repo", payload).unwrap();
        append(&roots, project, "webhook:check-run:42".into(), kind, detail)?;

        let marker = journal
            .next_pr("owner/repo", 9, Duration::ZERO, &CancelToken::new())?
            .expect("webhook association wakes the matching PR waiter");
        assert_eq!(marker, json!({"repository":"owner/repo","number":9}));
        assert!(marker.get("status").is_none());
        assert!(marker.get("head").is_none());
        Ok(())
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn large_check_matrices_preserve_failure_and_the_pinned_head_in_a_bounded_projection() {
        let subscription = Subscription {
            repository: "owner/repo".into(),
            number: 7,
            head: Some("old-head".into()),
        };
        let mut checks = vec![
            crate::pr::observer::Check {
                head: "old-head".into(),
                name: "x".repeat(700),
                bucket: "pass".into(),
                app: None,
                app_slug: None,
                conclusion: None,
                registered: None,
                url: None,
                excerpt: String::new(),
            };
            512
        ];
        checks.last_mut().unwrap().bucket = "fail".into();
        let status: crate::pr::status::Line = serde_json::from_value(json!({
            "number":7,"state":"OPEN","head":"new-head","draft":false,
            "checks_passed":511,"checks_total":512,"checks_failed":1,"checks_pending":0,
            "failing_checks":vec!["x".repeat(700);512],"merge":"UNKNOWN","review":"unknown","error":null
        })).unwrap();
        let detail = poll_detail(&subscription, &status, &checks).unwrap();
        assert_eq!(detail["head"], "old-head");
        assert_eq!(detail["pull_head"], "new-head");
        assert_eq!(detail["checks_truncated"], true);
        assert!(
            detail["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["bucket"] == "fail")
        );
        assert!(
            serde_json::to_vec(&detail).unwrap().len() < provenant_fabric_store::MAX_BODY_BYTES
        );
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn journal_drains_full_pages_before_waiting_for_another_hint() {
        let (_temp, roots, project) = fixture();
        let mut journal = Journal::for_project(roots.clone(), project).unwrap();
        for number in 1..=REPLAY_PAGE + 2 {
            append(
                &roots,
                project,
                format!("page-{number}"),
                ObservationKind::CheckRun,
                json!({"repository":"owner/repo","number":number,"status":{}}),
            )
            .unwrap();
        }
        assert!(
            journal
                .next_pr(
                    "owner/repo",
                    REPLAY_PAGE + 2,
                    Duration::ZERO,
                    &CancelToken::new()
                )
                .unwrap()
                .is_some()
        );
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn unavailable_webhook_capabilities_warn_and_poll_without_refusal() {
        for requested in ["auto", "webhook", "poll"] {
            for capability in [
                Ok(()),
                Err("missing extension"),
                Err("missing admin rights"),
            ] {
                let probed = std::cell::Cell::new(false);
                let (source, warning) = choose_source(requested, || {
                    probed.set(true);
                    capability
                });
                assert_eq!(probed.get(), requested != "poll");
                assert_eq!(
                    source,
                    if requested != "poll" && capability.is_ok() {
                        "webhook"
                    } else {
                        "poll"
                    }
                );
                assert_eq!(
                    warning.is_some(),
                    requested != "poll" && capability.is_err()
                );
            }
        }
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn unchanged_host_reads_keep_waiters_local_until_a_real_source_gap() {
        let (_temp, roots, project) = fixture();
        let journal = Journal::for_project(roots.clone(), project).unwrap();
        let subscription = Subscription {
            repository: "owner/repo".into(),
            number: 7,
            head: None,
        };
        let dir = directory(&roots, project).unwrap();
        let name = format!("progress-pr-{}.json", key(&subscription).unwrap());
        let mut progress = PollProgress {
            last_success_micros: 100,
            ..Default::default()
        };
        save(&dir, &name, &progress).unwrap();
        assert!(
            journal
                .source_current_at("owner/repo", 7, None, 101)
                .unwrap()
        );
        let tick = i64::from(crate::tick_agent::INTERVAL) * 1_000_000;
        assert!(
            journal
                .source_current_at("owner/repo", 7, None, 100 + tick)
                .unwrap()
        );
        assert!(
            !journal
                .source_current_at("owner/repo", 7, None, 100 + 3 * tick)
                .unwrap()
        );
        progress.failures = 1;
        save(&dir, &name, &progress).unwrap();
        assert!(
            !journal
                .source_current_at("owner/repo", 7, None, 101)
                .unwrap()
        );
        progress.failures = 0;
        progress.closed = true;
        save(&dir, &name, &progress).unwrap();
        assert!(
            !journal
                .source_current_at("owner/repo", 7, None, 101)
                .unwrap()
        );
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn forwarder_disconnect_switches_to_polling_and_preserves_the_config_source() {
        let (_temp, roots, project) = fixture();
        let dir = directory(&roots, project).unwrap();
        let repository = "owner/repo";
        let name = format!("source-{}.json", key(&repository).unwrap());
        save(
            &dir,
            &name,
            &Selection {
                requested: "auto".into(),
                selected: "webhook".into(),
                source: "configured fixture origin".into(),
                warning: None,
                retry_micros: i64::MAX,
            },
        )
        .unwrap();
        record_forwarder_exit(&dir, repository, 10, "account switch").unwrap();
        let selected: Selection = read(&dir, &name).unwrap();
        assert_eq!(selected.requested, "auto");
        assert_eq!(selected.source, "configured fixture origin");
        assert_eq!(selected.selected, "poll");
        assert!(selected.warning.is_some());
        assert!(selected.retry_micros > 10 && selected.retry_micros < i64::MAX);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn host_poll_failures_back_off_and_success_restores_cadence() {
        let mut progress = PollProgress::default();
        let interval = Duration::from_secs(5);
        progress.schedule(0, false, interval);
        let first = progress.next_micros;
        progress.schedule(0, false, interval);
        assert!(progress.next_micros > first);
        for _ in 0..100 {
            progress.schedule(0, false, interval);
        }
        assert!(progress.next_micros <= 900_000_000);
        progress.schedule(0, true, interval);
        assert_eq!(progress.failures, 0);
        assert_eq!(progress.next_micros, interval.as_micros() as i64);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn oversized_forge_metadata_is_bounded_with_explicit_truncation() {
        let payload = json!({"check_run":{"id":1,"head_sha":"abc",
            "name":"n".repeat(provenant_fabric_store::MAX_BODY_BYTES),
            "pull_requests":(1..=MAX_PR_ASSOCIATIONS+1).map(|number| json!({"number":number})).collect::<Vec<_>>()}});
        let (_, detail) = normalise_webhook("owner/repo", payload).unwrap();
        assert_eq!(detail["name_truncated"], true);
        assert_eq!(detail["pull_requests_truncated"], true);
        assert_eq!(
            detail["pull_requests"].as_array().unwrap().len(),
            MAX_PR_ASSOCIATIONS
        );
        assert!(detail["name"].as_str().unwrap().len() <= LABEL_BYTES);
        assert_eq!(detail["number"], 1);
        assert_eq!(detail["id"], 1);
        assert!(
            serde_json::to_vec(&detail).unwrap().len() < provenant_fabric_store::MAX_BODY_BYTES
        );
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn webhook_normalisation_keeps_forge_kinds_and_discards_user_text() {
        for (field, kind) in [
            ("check_run", ObservationKind::CheckRun),
            ("check_suite", ObservationKind::CheckSuite),
            ("workflow_run", ObservationKind::WorkflowRun),
            ("pull_request", ObservationKind::PullRequest),
            ("comment", ObservationKind::IssueComment),
        ] {
            let payload = json!({field:{"id":1,"head_sha":"abc","body":"not indexed","status":"completed"},"repository":{"full_name":"owner/repo"},"number":7,"action":"completed"});
            let (actual, detail) = normalise_webhook("owner/repo", payload).unwrap();
            assert_eq!(actual, kind);
            assert_eq!(detail["repository"], "owner/repo");
            assert!(!detail.to_string().contains("not indexed"));
        }
        for field in ["check_run", "check_suite", "workflow_run"] {
            let name = (field != "check_suite").then_some("build");
            let payload = json!({field:{"id":1,"name":name,"head_sha":"abc",
                "pull_requests":[{"number":7},{"number":9}],"status":"completed"},"action":"completed"});
            let (_, detail) = normalise_webhook("owner/repo", payload).unwrap();
            assert_eq!(detail["number"], 7);
            assert_eq!(detail["pull_requests"], json!([7, 9]));
            assert_eq!(detail["name"], json!(name));
            assert_eq!(detail["head"], "abc");
        }
    }
}
