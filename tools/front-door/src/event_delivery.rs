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
    warning::{Warning, warn},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const LIMIT: u64 = 4 * 1024 * 1024;
const REPLAY_PAGE: u64 = 100;
const LABEL_BYTES: usize = 128;
/// Failed checks a journal event keeps so a wait that settles from it can
/// still name them; names and links are already bounded at the source.
const MAX_FAILURE_DETAILS: usize = 16;
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
pub(crate) fn tick(
    resolved: &crate::roots::Resolved,
    deadline: provenant_core::Deadline,
) -> Result<()> {
    if provenant_cli::fabric::in_lane() {
        return Err("forge ingestion requires host custody".into());
    }
    let mut errors = Vec::new();
    for binding in provenant_config::registered_projects(&resolved.roots)? {
        if deadline.is_expired(SystemClock.mono_now()) {
            break;
        }
        // A vanished checkout or common directory has no forge to poll. Report
        // it once with the unregister command rather than failing every tick.
        if crate::project_root::absent(&binding) {
            crate::project_root::warn_vanished(&binding);
            continue;
        }
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
            fetch_then_index(
                project,
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
                                warn(
                                    Warning::new(
                                        provenant_records::warning::FORGE_DISCOVERY_UNAVAILABLE,
                                        format!(
                                            "forge discovery unavailable for {project}: {error}; local journal sources continue; {}",
                                            origin.describe("domains.wait.forge_source")
                                        ),
                                     provenant_records::warning::detected_source(provenant_records::warning::FORGE_DISCOVERY_UNAVAILABLE))
                                    .subject(project.to_string()),
                                );
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
                                warn(Warning::new(
                                    provenant_records::warning::FORWARDER_UNAVAILABLE,
                                    format!(
                                        "forwarder unavailable: {error}; conditional reconciliation continues"
                                    ),
                                    provenant_records::warning::detected_source(
                                        provenant_records::warning::FORWARDER_UNAVAILABLE,
                                    ),
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
                            warn(Warning::new(
                                provenant_records::warning::FORWARDER_UNAVAILABLE,
                                format!("forwarder unavailable: {error}; using conditional poller"),
                                provenant_records::warning::detected_source(
                                    provenant_records::warning::FORWARDER_UNAVAILABLE,
                                ),
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
                || index_graphs(&resolved.roots, project),
                || index_notes(&resolved.roots, project),
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

/// The network page fetch runs first; local, replayable indexing follows in
/// the same task. A broken local journal never stops the authoritative fetch,
/// and a slow page delays only the `forge_events` task.
fn fetch_then_index<T>(
    project: impl std::fmt::Display,
    forge: impl FnOnce() -> Result<T>,
    graphs: impl FnOnce() -> Result<()>,
    notes: impl FnOnce() -> Result<()>,
) -> Result<T> {
    let fetched = forge();
    if let Err(error) = graphs() {
        let quarantined = error.is::<provenant_fabric_runtime::graph::UnreadableCheckpointNotice>();
        warn(
            Warning::new(
                provenant_records::warning::GRAPH_OBSERVATION_DEFERRED,
                format!(
                    "graph observation indexing failed for {project}: {error}{}",
                    if quarantined {
                        "; quarantined, skipped on later ticks"
                    } else {
                        "; will retry"
                    }
                ),
                provenant_records::warning::detected_source(
                    provenant_records::warning::GRAPH_OBSERVATION_DEFERRED,
                ),
            )
            .subject(project.to_string()),
        );
    }
    if let Err(error) = notes() {
        warn(
            Warning::new(
                provenant_records::warning::NOTE_OBSERVATION_DEFERRED,
                format!("note observation indexing failed for {project}: {error}; will retry"),
                provenant_records::warning::detected_source(
                    provenant_records::warning::NOTE_OBSERVATION_DEFERRED,
                ),
            )
            .subject(project.to_string()),
        );
    }
    fetched
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
    // Keep a bounded set of failure details so a default wait that settles
    // from this event still names the failed checks. Full details are re-read
    // from the observer at completion; capping the count keeps a large check
    // matrix from overflowing the event journal. Names and links are bounded
    // when the status line is built, so only the count needs bounding here.
    if let Some(details) = projected_status
        .get_mut("failed_check_details")
        .and_then(Value::as_array_mut)
    {
        details.truncate(MAX_FAILURE_DETAILS);
    }
    if let Some(names) = projected_status
        .get_mut("failing_checks")
        .and_then(Value::as_array_mut)
    {
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
        warn(Warning::new(
            provenant_records::warning::FORGE_SOURCE_UNRESOLVED,
            format!("{warning}; {origin}"),
            provenant_records::warning::detected_source(
                provenant_records::warning::FORGE_SOURCE_UNRESOLVED,
            ),
        ));
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
            warn(Warning::new(
                provenant_records::warning::FORWARDER_UNAVAILABLE,
                format!("forge forwarder: {error}; host reconciliation continues"),
                provenant_records::warning::detected_source(
                    provenant_records::warning::FORWARDER_UNAVAILABLE,
                ),
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
include!("event_delivery_tests.rs");
