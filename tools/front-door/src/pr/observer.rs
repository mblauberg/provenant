//! The one reader of CI for a repository. Every waiter, whether `pr status`,
//! a graph gate, `land` waiting or the landing gate, reads check runs and
//! commit statuses for an exact commit through [`Observer::checks`].
//!
//! Reads are cached in host state by repository and commit, so waiters in any
//! process share one answer per configured observation interval. Settled CI
//! uses the fast interval; pending CI slows until near the same check's recent
//! median completion time. A zero caller max age always revalidates. An older
//! answer is refreshed conditionally, so an unchanged commit costs a `304`. A one-shot
//! decision accepts no age and always revalidates. A rate-limited
//! response is recorded once for the repository with its reset time, and
//! every waiter holds off until then instead of retrying on its own.
//!
//! A commit is any head: a pull request head, a `land/**` candidate or a
//! landed commit.
#[cfg(test)]
use super::gh::Response;
use super::gh::rate_limit_reset;
use provenant_config::ports::Request;
use provenant_config::{Forge, ForgePort};
use provenant_core::GitOid;
use provenant_records::command_result::{CommandError, ErrorClass, Subject};
pub(crate) mod journal;
mod port;
mod retry;
use provenant_os::fs::{Directory, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

/// The interval of waiters that set none of their own. It is the default of
/// the one sourced retry family, not a private copy in a caller.
pub(crate) const POLL: Duration = retry::DEFAULT_POLL;
const PAGE: usize = 100;
const RATE_FILE: &str = "rate-limit.json";
const RATE_LOCK: &str = "rate-limit.lock";
const CACHE_LIMIT: u64 = 16 * 1024 * 1024;
const REPOSITORY_CONTEXT_PATH: &str = "repos/{owner}/{repo}";
const RECENT_CACHE_LIMIT: usize = 64;
const RUN_HISTORY_LIMIT: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredCheck {
    pub name: String,
    pub app: Option<u64>,
}

/// One reported check run or the latest state of one status context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    /// Exact commit SHA returned by GitHub for this check.
    pub head: String,
    pub name: String,
    /// GitHub's check-run ID, which lets a repair identify the exact failed job.
    pub job_id: Option<String>,
    /// `pass`, `fail`, `cancel`, `pending` or `skipping`.
    pub bucket: String,
    /// The GitHub App that reported a check run; legacy statuses have none.
    pub app: Option<u64>,
    pub app_slug: Option<String>,
    pub conclusion: Option<String>,
    /// The latest time GitHub recorded activity for this check.
    pub registered: Option<jiff::Timestamp>,
    pub url: Option<String>,
    pub excerpt: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ObserveError {
    #[error("GitHub rate limit; reset {reset}")]
    RateLimited { reset: jiff::Timestamp },
    /// The request's deadline passed or its caller cancelled it.
    #[error("CI wait expired")]
    Expired,
    #[error("{0}")]
    Unavailable(String),
}

/// The one error-contract projection for an exhausted forge allowance: a
/// transient condition carrying the reset, never a CI verdict and never bad
/// input. Spec: docs/specs/agent-native.md#33-error-and-warning-contract
pub(crate) fn forge_quota_error(
    reset: jiff::Timestamp,
    pr: Option<u64>,
    cause: Option<&str>,
) -> CommandError {
    let mut error = CommandError::new(
        "FORGE_QUOTA_EXHAUSTED",
        format!("forge rate limit; retry after {reset}"),
        cause,
        format!(
            "wait until Unix timestamp {} ({reset}), then retry the head observation",
            reset.as_second()
        ),
    );
    error = error
        .with_class(ErrorClass::Transient {
            retry_at: Some(reset),
        })
        .with_subject(Subject {
            host: None,
            project: None,
            graph: None,
            node: None,
            attempt: None,
            item: None,
            pr,
            head: None,
        });
    error
}

/// What GitHub reports about a pull request's ability to merge. UNKNOWN is a
/// real reading: GitHub has not finished computing mergeability for the head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeState {
    Mergeable,
    Conflicting,
    Unknown,
}

impl MergeState {
    /// A waiting reader's diagnosis of a head that reported no checks: its
    /// cause code and the next step an operator should take. A conflicting head
    /// never starts `pull_request` workflows, so the trigger advice is wrong
    /// for it; a mergeable head keeps that advice.
    pub(crate) fn no_checks_cause(self, base: Option<&str>) -> PrCause {
        if self == Self::Conflicting {
            let next = match base.filter(|base| !base.is_empty()) {
                Some(base) => format!(
                    "merge the base into the branch: git merge origin/{base} then push the merge"
                ),
                None => "merge the base into the branch: git merge origin/BASE then push the merge"
                    .to_owned(),
            };
            PrCause {
                code: "merge_conflict",
                next,
            }
        } else {
            PrCause {
                code: "not_scheduled",
                next: "check the workflow triggers or push an empty commit to start CI".to_owned(),
            }
        }
    }
}

/// A cause code and its next step, surfaced by `wait --pr` and `pr status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrCause {
    pub code: &'static str,
    pub next: String,
}

/// One immutable, head-bound reading for queue and landing decisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadReading {
    pub expected_head: GitOid,
    pub branch_ref: Option<String>,
    pub pr_head: Option<GitOid>,
    pub mergeable: MergeState,
    /// Required identities from the branch protection or ruleset.
    pub required_policy: Vec<RequiredCheck>,
    /// The checks observed for the expected commit, including optional checks.
    pub observed: Vec<Check>,
    /// Observed checks matching the required identities.
    pub required: Vec<Check>,
    pub observed_at: jiff::Timestamp,
    pub complete: bool,
    pub pending_reason: Option<String>,
    pub retry_at: Option<jiff::Timestamp>,
}

/// The oldest a head-bound observation may be before it can settle a verdict.
/// The one reader uses this as its `max_age` when a caller supplies no tighter
/// bound; it is the batch's `observed_at` freshness rule.
pub(crate) const HEAD_MAX_AGE: Duration = Duration::from_secs(60);

impl HeadReading {
    /// An answer that cannot settle a verdict, with the reason it is pending.
    fn pending(expected: GitOid, reason: impl Into<String>) -> Self {
        Self {
            expected_head: expected,
            branch_ref: None,
            pr_head: None,
            mergeable: MergeState::Unknown,
            required_policy: Vec::new(),
            observed: Vec::new(),
            required: Vec::new(),
            observed_at: WallClock.now(),
            complete: false,
            pending_reason: Some(reason.into()),
            retry_at: None,
        }
    }

    /// A quota-bound answer. The reset is carried as `retry_at` so callers hold
    /// the stage until it passes rather than turning quota into a CI verdict.
    fn limited(expected: GitOid, reset: jiff::Timestamp, reason: impl Into<String>) -> Self {
        let mut reading = Self::pending(expected, reason);
        reading.retry_at = Some(reset);
        reading
    }

    /// Whether this reading was sampled within `max_age` of `now`. A stale
    /// observation cannot bind a verdict to an exact head.
    pub(crate) fn fresh_at(&self, now: jiff::Timestamp, max_age: Duration) -> bool {
        let Ok(age) = jiff::SignedDuration::try_from(max_age) else {
            return false;
        };
        now >= self.observed_at && now.duration_since(self.observed_at) <= age
    }

    /// The error contract for a quota-bound reading: a transient condition
    /// carrying the forge's reset, never a CI verdict and never bad input.
    /// Spec: docs/specs/agent-native.md#33-error-and-warning-contract
    pub(crate) fn quota_error(&self) -> Option<CommandError> {
        let retry_at = self.retry_at?;
        let mut error = forge_quota_error(retry_at, None, self.pending_reason.as_deref());
        error.subject = error.subject.take().map(|mut subject| {
            subject.head = Some(self.expected_head);
            subject
        });
        Some(error)
    }
}

/// Read mergeability from one PR resource. GraphQL `mergeable` and
/// `mergeStateStatus`, and the REST `mergeable`/`mergeable_state` pair, all name
/// the same two settled states; anything else stays UNKNOWN for a bounded reread.
fn merge_state(value: &Value) -> MergeState {
    let conflicting = matches!(value["mergeable"].as_str(), Some("CONFLICTING"))
        || matches!(value["mergeable"].as_bool(), Some(false))
        || matches!(
            value["mergeStateStatus"].as_str(),
            Some("DIRTY" | "CONFLICTING")
        )
        || value["mergeable_state"]
            .as_str()
            .is_some_and(|state| state.eq_ignore_ascii_case("dirty"));
    if conflicting {
        return MergeState::Conflicting;
    }
    let known = matches!(value["mergeable"].as_str(), Some("MERGEABLE"))
        || matches!(value["mergeable"].as_bool(), Some(true))
        || matches!(
            value["mergeStateStatus"].as_str(),
            Some("CLEAN" | "MERGEABLE" | "BLOCKED" | "BEHIND" | "HAS_HOOKS")
        )
        || value["mergeable_state"]
            .as_str()
            .is_some_and(|state| !state.eq_ignore_ascii_case("unknown"));
    if known {
        MergeState::Mergeable
    } else {
        MergeState::Unknown
    }
}

/// Wall time shared with other processes, and the pause between reads.
pub(crate) trait Clock {
    fn now(&self) -> jiff::Timestamp;
    fn sleep(&self, duration: Duration);
}
pub(crate) struct WallClock;
impl Clock for WallClock {
    #[allow(clippy::disallowed_methods, reason = "CI observer clock boundary")]
    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::now()
    }
    #[allow(clippy::disallowed_methods, reason = "CI observer clock boundary")]
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Resolve the selected repository through the selected observation transport context.
pub(crate) fn repository_for_project(
    state: &Path,
    project: &Path,
    request: &Request,
) -> Result<String, ObserveError> {
    if request.remaining().is_zero() {
        return Err(ObserveError::Expired);
    }
    let mut forge = port::select_transport(state, project, request)?;
    loop {
        if request.is_cancelled() {
            return Err(ObserveError::Expired);
        }
        if request.remaining().is_zero() {
            return Err(ObserveError::Expired);
        }
        let response = port::get(&mut forge, REPOSITORY_CONTEXT_PATH, None).map_err(|error| {
            ObserveError::Unavailable(format!("cannot identify repository: {error}"))
        })?;
        let now = WallClock.now();
        if let Some(reset) = rate_limit_reset(&response, now.as_second()) {
            let reset = reset.max(now.as_second().saturating_add(1));
            let reset_at = timestamp(reset);
            let delay = Duration::try_from(reset_at.duration_since(now)).unwrap_or_default();
            if delay >= request.remaining() {
                return Err(ObserveError::RateLimited { reset: reset_at });
            }
            WallClock.sleep(delay);
            continue;
        }
        if !(200..=299).contains(&response.status) {
            return Err(ObserveError::Unavailable(format!(
                "cannot identify repository: HTTP {}",
                response.status
            )));
        }
        let value: Value = serde_json::from_slice(&response.body).map_err(|error| {
            ObserveError::Unavailable(format!("invalid repository response: {error}"))
        })?;
        return value
            .get("full_name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ObserveError::Unavailable("repository response has no canonical name".into())
            });
    }
}

/// The production entry to the one head-bound reader. `repo` names the host's
/// repository, `pr` the pull whose head is being judged, and `expected` the
/// exact commit the caller will act on. The batched GraphQL row is consulted
/// only when a REST read is quota-bound, and only its head is used here: a
/// transport that names another commit cannot confirm this observation.
pub(crate) fn observe_head(
    state: &Path,
    project: &Path,
    repo: &str,
    pr: u64,
    expected: GitOid,
    request: &Request,
) -> HeadReading {
    observe_head_with_required_names(state, project, repo, pr, expected, request, &[])
}

/// Production entry for a PR observation with an explicit graph gate check set.
/// An empty set preserves branch-protection and ruleset policy lookup.
pub(crate) fn observe_head_with_required_names(
    state: &Path,
    project: &Path,
    repo: &str,
    pr: u64,
    expected: GitOid,
    request: &Request,
    required_names: &[String],
) -> HeadReading {
    let mut reader = match Observer::open(state, project, repo, request) {
        Ok(reader) => reader,
        Err(ObserveError::RateLimited { reset }) => {
            return HeadReading::limited(expected, reset, "forge rate limit; observer setup held");
        }
        Err(error) => return HeadReading::pending(expected, error.to_string()),
    };
    let graphql_head = || match crate::pr::status::batch::single_for_repository_with_request(
        state, project, repo, pr, request,
    ) {
        crate::pr::status::batch::Batch::Entries(entries) => entries
            .first()
            .and_then(|entry| GitOid::parse(&entry.head).ok())
            .map(|head| head.to_string()),
        _ => None,
    };
    if required_names.is_empty() {
        reader.observe_head(pr, expected, request, HEAD_MAX_AGE, &graphql_head)
    } else {
        reader.observe_head_with_required_names(
            pr,
            expected,
            request,
            HEAD_MAX_AGE,
            Some(required_names),
            &graphql_head,
        )
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Page {
    etag: Option<String>,
    body: Value,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}
#[derive(Serialize, Deserialize)]
struct ResourcePage {
    page: Page,
    fetched_at: i64,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}
#[derive(Default, Serialize, Deserialize)]
struct Cached {
    /// Unix milliseconds.
    fetched_at: i64,
    runs: Vec<Page>,
    statuses: Vec<Page>,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

#[derive(Clone)]
struct RunDuration {
    id: u64,
    name: String,
    duration_ms: i64,
    completed_at_ms: i64,
}

#[derive(Clone, Copy)]
struct Cadence {
    fast: Duration,
    slow: Duration,
    near_end: Duration,
    conditional_requests: bool,
}

fn adaptive_freshness(
    cached: &Cached,
    history: &[RunDuration],
    caller_max_age: Duration,
    cadence: Cadence,
    now_ms: i64,
) -> Duration {
    if caller_max_age.is_zero() {
        return Duration::ZERO;
    }
    adaptive_interval(cached, history, cadence, now_ms)
}

fn adaptive_interval(
    cached: &Cached,
    history: &[RunDuration],
    cadence: Cadence,
    now_ms: i64,
) -> Duration {
    let fast = cadence.fast;
    let pending = cached
        .runs
        .iter()
        .filter_map(|page| page.body.get("check_runs")?.as_array())
        .flat_map(|runs| runs.iter())
        .filter(|run| {
            matches!(
                run.get("status").and_then(Value::as_str),
                Some("queued" | "in_progress" | "pending" | "waiting" | "requested")
            )
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return fast;
    }
    let near_end = pending.iter().any(|run| {
        let Some(name) = run.get("name").and_then(Value::as_str) else {
            return false;
        };
        let Some(started) = time(run.get("started_at")).flatten() else {
            return false;
        };
        let mut durations = history
            .iter()
            .filter(|sample| sample.name == name)
            .map(|sample| sample.duration_ms)
            .collect::<Vec<_>>();
        if durations.is_empty() {
            return false;
        }
        durations.sort_unstable();
        let expected_ms = durations[durations.len() / 2];
        let elapsed_ms = now_ms.saturating_sub(started.as_millisecond()).max(0);
        elapsed_ms.saturating_add(i64::try_from(cadence.near_end.as_millis()).unwrap_or(i64::MAX))
            >= expected_ms
    });
    if near_end {
        fast
    } else {
        cadence.slow.max(fast)
    }
}

fn durations_from_cache(cached: &Cached) -> Vec<RunDuration> {
    cached
        .runs
        .iter()
        .filter_map(|page| page.body.get("check_runs")?.as_array())
        .flat_map(|runs| runs.iter())
        .filter(|run| run.get("status").and_then(Value::as_str) == Some("completed"))
        .filter_map(|run| {
            let started = time(run.get("started_at"))??.as_millisecond();
            let completed = time(run.get("completed_at"))??.as_millisecond();
            let duration_ms = completed
                .checked_sub(started)
                .filter(|duration| *duration > 0)?;
            Some(RunDuration {
                id: run.get("id")?.as_u64()?,
                name: run.get("name")?.as_str()?.to_owned(),
                duration_ms,
                completed_at_ms: completed,
            })
        })
        .collect()
}

fn unique_run_history(samples: impl IntoIterator<Item = RunDuration>) -> Vec<RunDuration> {
    let mut by_id = std::collections::BTreeMap::<u64, RunDuration>::new();
    for sample in samples {
        match by_id.entry(sample.id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(sample);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let old = entry.get();
                if sample.completed_at_ms > old.completed_at_ms
                    || (sample.completed_at_ms == old.completed_at_ms
                        && sample.duration_ms > old.duration_ms)
                {
                    entry.insert(sample);
                }
            }
        }
    }
    let mut history = by_id.into_values().collect::<Vec<_>>();
    history.sort_by_key(|sample| sample.completed_at_ms);
    if history.len() > RUN_HISTORY_LIMIT {
        history.drain(..history.len() - RUN_HISTORY_LIMIT);
    }
    history
}

fn cached_run_history(dir: &Directory) -> Option<Vec<RunDuration>> {
    let entries = dir.entries().ok()?;
    let mut names = Vec::<(i128, OsString)>::new();
    for entry in entries {
        let name = entry.ok()?;
        let Some(text) = name.to_str() else {
            continue;
        };
        let Some(commit) = text.strip_suffix(".json") else {
            continue;
        };
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            continue;
        }
        let metadata = dir.metadata_at(&name, false).ok()?;
        if metadata.kind == provenant_os::fs::FileKind::Regular {
            names.push((metadata.modified_ns, name));
        }
    }
    names.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let mut history = Vec::new();
    for (_, name) in names.into_iter().take(RECENT_CACHE_LIMIT) {
        let bytes = dir.read_bounded(&name, CACHE_LIMIT).ok()?;
        let cached: Cached = serde_json::from_slice(&bytes).ok()?;
        history.extend(durations_from_cache(&cached));
    }
    Some(unique_run_history(history))
}

fn settings_roots(state: &Path, project: &Path) -> Result<provenant_core::Roots, ObserveError> {
    let home = std::env::var_os(provenant_core::consumer_env::HOME)
        .map(PathBuf::from)
        .ok_or_else(|| {
            ObserveError::Unavailable("cannot resolve CI cadence configuration roots".into())
        })?;
    let mut inputs = provenant_core::RootInputs::new(home, project.to_owned());
    inputs.state_dir = Some(state.to_owned());
    inputs.workspaces.push(project.to_owned());
    inputs
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|_| {
            ObserveError::Unavailable("cannot resolve CI cadence configuration roots".into())
        })
}

fn request_etag(cadence: Cadence, previous: Option<&Page>) -> Option<&str> {
    if cadence.conditional_requests {
        previous.and_then(|page| page.etag.as_deref())
    } else {
        None
    }
}
#[derive(Serialize, Deserialize)]
struct RateLimit {
    reset: i64,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

enum Fetch {
    Limited(i64),
    Transient(String),
    Failed(String),
}

pub(crate) struct Observer<F, C> {
    dir: Directory,
    repository: String,
    forge: F,
    clock: C,
    cadence: Cadence,
    retry: retry::Retry,
    history: Vec<RunDuration>,
    next_poll_interval: Duration,
    latest_observed_at: Option<jiff::Timestamp>,
    review_gate_contexts: Vec<String>,
}

impl Observer<ForgePort, WallClock> {
    /// Select the configured CI transport once, then share the observer cache.
    pub(crate) fn open(
        state: &Path,
        project: &Path,
        repository: &str,
        request: &Request,
    ) -> Result<Self, ObserveError> {
        let transport = port::select_transport(state, project, request)?;
        let roots = settings_roots(state, project).map_err(|error| {
            ObserveError::Unavailable(format!("cannot read CI cadence configuration: {error}"))
        })?;
        let (settings, _) = provenant_cli::wait::Settings::resolve(&roots).map_err(|error| {
            ObserveError::Unavailable(format!("cannot read CI cadence configuration: {error}"))
        })?;
        let mut observer = Self::with_settings(state, repository, transport, WallClock, settings)?;
        observer.review_gate_contexts = crate::pr::status::gate_contexts(project, state);
        Ok(observer)
    }
}

impl<F: Forge, C: Clock> Observer<F, C> {
    /// The `owner/repo` this observer reads.
    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    #[cfg(test)]
    pub(crate) fn with(
        state: &Path,
        repository: &str,
        forge: F,
        clock: C,
    ) -> Result<Self, ObserveError> {
        Self::with_settings(
            state,
            repository,
            forge,
            clock,
            provenant_cli::wait::Settings::default(),
        )
    }

    fn with_settings(
        state: &Path,
        repository: &str,
        forge: F,
        clock: C,
        settings: provenant_cli::wait::Settings,
    ) -> Result<Self, ObserveError> {
        let dir = shared_dir(state, repository).map_err(ObserveError::Unavailable)?;
        let history = match cached_run_history(&dir) {
            Some(history) => history,
            None => {
                provenant_records::warning::warn(
                    provenant_records::warning::Warning::new(
                        provenant_records::warning::CI_DURATION_CACHE_UNAVAILABLE,
                        "recent CI duration cache unavailable; using slower observation cadence",
                        provenant_records::warning::detected_source(
                            provenant_records::warning::CI_DURATION_CACHE_UNAVAILABLE,
                        ),
                    )
                    .subject(repository),
                );
                Vec::new()
            }
        };
        let cadence = Cadence {
            fast: Duration::from_millis(settings.pr_interval_ms),
            slow: Duration::from_millis(settings.pr_slow_interval_ms),
            near_end: Duration::from_millis(settings.pr_near_end_window_ms),
            conditional_requests: settings.pr_conditional_requests,
        };
        let next_poll_interval = cadence.fast;
        Ok(Self {
            dir,
            repository: repository.to_owned(),
            forge,
            clock,
            cadence,
            retry: retry::Retry::default(),
            history,
            next_poll_interval,
            latest_observed_at: None,
            review_gate_contexts: Vec::new(),
        })
    }

    /// The configured fast interval for PR and CI observation.
    pub(crate) fn fast_interval(&self) -> Duration {
        self.cadence.fast
    }

    /// The interval selected from the last CI observation for the next read.
    pub(crate) fn next_poll_interval(&self) -> Duration {
        self.next_poll_interval
    }

    /// Read one repository-relative forge resource through the selected port.
    /// Its cache is separate from the check-run cache shape.
    pub(crate) fn resource(
        &mut self,
        relative_path: &str,
        max_age: Duration,
        request: &Request,
    ) -> Result<Value, ObserveError> {
        if !valid_resource_path(relative_path) {
            return Err(ObserveError::Unavailable(
                "invalid repository resource path".into(),
            ));
        }
        let path = format!("repos/{}/{relative_path}", self.repository);
        let name = format!(
            "resource-{}.json",
            provenant_records::CanonicalV1::file_digest(path.as_bytes())
        );
        let start = self.clock.now();
        let deadline = start
            .checked_add(
                jiff::SignedDuration::try_from(request.remaining())
                    .unwrap_or(jiff::SignedDuration::MAX),
            )
            .unwrap_or(jiff::Timestamp::MAX);
        loop {
            let now = self.clock.now();
            if request.is_cancelled() {
                return Err(ObserveError::Expired);
            }
            if now >= deadline {
                return Err(ObserveError::Expired);
            }
            let cached: Option<ResourcePage> = self
                .dir
                .read_bounded(OsStr::new(&name), CACHE_LIMIT)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            if let Some(entry) = cached.as_ref() {
                let age = now.as_millisecond().saturating_sub(entry.fetched_at);
                if (0..i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX)).contains(&age) {
                    self.mark_observed_at(entry.fetched_at);
                    return Ok(entry.page.body.clone());
                }
            }
            if let Some(reset) = self.rate_limited(now) {
                self.pause_for_reset(reset, deadline, request)?;
                continue;
            }
            self.forge.bind(request);
            let (previous, extra) = cached
                .map(|entry| (Some(entry.page), entry.extra))
                .unwrap_or_default();
            let fetched = match self.page(&path, previous.as_ref(), identity) {
                Ok(fetched) => fetched,
                Err(Fetch::Limited(reset)) => {
                    let reset = self.record_reset(reset.max(now.as_second().saturating_add(1)));
                    self.pause_for_reset(reset, deadline, request)?;
                    continue;
                }
                Err(Fetch::Transient(message) | Fetch::Failed(message)) => {
                    return Err(ObserveError::Unavailable(message));
                }
            };
            let entry = ResourcePage {
                page: fetched,
                fetched_at: self.clock.now().as_millisecond(),
                extra,
            };
            self.mark_observed_at(entry.fetched_at);
            let bytes = serde_json::to_vec(&entry).map_err(|error| {
                ObserveError::Unavailable(format!("cannot encode repository resource: {error}"))
            })?;
            self.dir
                .atomic_write(
                    OsStr::new(&name),
                    ".resource-",
                    &bytes,
                    PublishMode::Replace,
                )
                .map_err(|error| {
                    ObserveError::Unavailable(format!("cannot record repository resource: {error}"))
                })?;
            return Ok(entry.page.body);
        }
    }

    /// Observe every Actions run for this exact head through the shared cache.
    pub(crate) fn active_workflows(
        &mut self,
        head: &str,
        max_age: Duration,
        request: &Request,
    ) -> Result<Option<bool>, ObserveError> {
        if !matches!(head.len(), 40 | 64) || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ObserveError::Unavailable(format!(
                "not a commit id: {head}"
            )));
        }
        let mut page = 1_u64;
        let mut seen = false;
        let mut active = false;
        loop {
            let value = self.resource(
                &format!("actions/runs?head_sha={head}&per_page=100&page={page}"),
                max_age,
                request,
            )?;
            let runs = value["workflow_runs"]
                .as_array()
                .ok_or_else(|| ObserveError::Unavailable("invalid Actions runs response".into()))?;
            for run in runs {
                let status = run["status"]
                    .as_str()
                    .ok_or_else(|| ObserveError::Unavailable("Actions run has no status".into()))?;
                seen = true;
                active |= status != "completed";
            }
            if runs.len() < 100 {
                return Ok(seen.then_some(active));
            }
            page += 1;
        }
    }

    /// Read a pull request's mergeability before diagnosing its missing checks.
    /// `first` is the resource the caller already read; only an UNKNOWN result
    /// rereads, at most the sourced `mergeable_attempts` times, within the
    /// caller's request. A still-UNKNOWN result stays UNKNOWN rather than being
    /// mistaken for a mergeable head.
    pub(crate) fn pull_merge_state(
        &mut self,
        number: u64,
        first: Value,
        request: &Request,
    ) -> Result<(MergeState, Value), ObserveError> {
        let path = format!("pulls/{number}");
        let mut value = first;
        let mut retries = 0_u8;
        let attempts = self.retry.mergeable_attempts.value;
        let pause = self.retry.mergeable_pause.value;
        loop {
            let state = merge_state(&value);
            if state != MergeState::Unknown
                || retries >= attempts
                || request.remaining().is_zero()
                || request.is_cancelled()
            {
                return Ok((state, value));
            }
            retries = retries.saturating_add(1);
            self.clock.sleep(pause);
            value = self.resource(&path, Duration::ZERO, request)?;
        }
    }

    /// Every check reported for `commit`. A zero `max_age` forces a fresh read;
    /// any nonzero value enables the configured adaptive cadence. Transient
    /// failures and quota holds are retried within `request`'s remaining time,
    /// and every page read shares its deadline and cancellation.
    pub(crate) fn checks(
        &mut self,
        commit: &str,
        max_age: Duration,
        request: &Request,
    ) -> Result<Vec<Check>, ObserveError> {
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ObserveError::Unavailable(format!(
                "not a commit id: {commit}"
            )));
        }
        let start = self.clock.now();
        let deadline = start
            .checked_add(
                jiff::SignedDuration::try_from(request.remaining())
                    .unwrap_or(jiff::SignedDuration::MAX),
            )
            .unwrap_or(jiff::Timestamp::MAX);
        self.forge.bind(request);
        let mut retries = 0_u32;
        loop {
            if request.is_cancelled() {
                return Err(ObserveError::Expired);
            }
            let now = self.clock.now();
            if let Some(reset) = self.rate_limited(now) {
                self.pause_for_reset(reset, deadline, request)?;
                continue;
            }
            let left = Duration::try_from(deadline.duration_since(now)).unwrap_or_default();
            if left.is_zero() {
                return Err(ObserveError::Expired);
            }
            // The per-commit lock only shares one read between waiters. A busy
            // lock is skipped: this waiter reads unlocked rather than queue.
            let lock = format!("{commit}.lock");
            let guard = self
                .dir
                .try_lock(OsStr::new(&lock), LockMode::Exclusive)
                .ok();
            let now = self.clock.now();
            // Another waiter may have met the limit while this one queued.
            if self.rate_limited(now).is_some() {
                drop(guard);
                continue;
            }
            let name = format!("{commit}.json");
            let cached: Option<Cached> = self
                .dir
                .read_bounded(OsStr::new(&name), CACHE_LIMIT)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            let freshness = cached.as_ref().map_or_else(
                || {
                    if max_age.is_zero() {
                        Duration::ZERO
                    } else {
                        self.cadence.fast
                    }
                },
                |cached| {
                    adaptive_freshness(
                        cached,
                        &self.history,
                        max_age,
                        self.cadence,
                        now.as_millisecond(),
                    )
                },
            );
            let fresh = cached.as_ref().is_some_and(|cached| {
                let age = now.as_millisecond().saturating_sub(cached.fetched_at);
                (0..i64::try_from(freshness.as_millis()).unwrap_or(i64::MAX)).contains(&age)
            });
            let cached = if fresh {
                cached.unwrap_or_default()
            } else {
                match self.fetch(commit, cached.unwrap_or_default()) {
                    Ok(mut fetched) => {
                        self.history = unique_run_history(
                            std::mem::take(&mut self.history)
                                .into_iter()
                                .chain(durations_from_cache(&fetched)),
                        );
                        fetched.fetched_at = now.as_millisecond();
                        let bytes = serde_json::to_vec(&fetched).map_err(|error| {
                            ObserveError::Unavailable(format!("cannot encode CI: {error}"))
                        })?;
                        // A lost cache write costs one more read, never a wrong answer.
                        let _ = self.dir.atomic_write(
                            OsStr::new(&name),
                            ".ci-",
                            &bytes,
                            PublishMode::Replace,
                        );
                        fetched
                    }
                    Err(Fetch::Limited(reset)) => {
                        // A reset already past still pauses, and a later
                        // recorded reset is never replaced by an earlier one.
                        let reset = self.record_reset(reset.max(now.as_second().saturating_add(1)));
                        drop(guard);
                        self.pause_for_reset(reset, deadline, request)?;
                        retries = retries.saturating_add(1);
                        continue;
                    }
                    Err(Fetch::Transient(message)) => {
                        drop(guard);
                        let delay = self
                            .retry
                            .first
                            .value
                            .saturating_mul(2_u32.saturating_pow(retries))
                            .min(self.retry.last.value);
                        let left = Duration::try_from(deadline.duration_since(self.clock.now()))
                            .unwrap_or_default();
                        if left <= delay {
                            return Err(ObserveError::Unavailable(message));
                        }
                        self.pause(delay, request);
                        retries = retries.saturating_add(1);
                        continue;
                    }
                    Err(Fetch::Failed(message)) => return Err(ObserveError::Unavailable(message)),
                }
            };
            self.mark_observed_at(cached.fetched_at);
            self.next_poll_interval = adaptive_interval(
                &cached,
                &self.history,
                self.cadence,
                self.clock.now().as_millisecond(),
            );
            drop(guard);
            return parse(&cached, commit).ok_or_else(|| {
                ObserveError::Unavailable(
                    "CI evidence is incomplete or belongs to another head".into(),
                )
            });
        }
    }

    /// The one head-bound reading for queue and landing decisions. A verdict
    /// binds to the PR head, the required set and every check page through this
    /// observer's single cache and rate-limit hold-off, so no waiter can settle
    /// on a head that another read has already superseded. `graphql_head` is
    /// consulted only when a REST read is quota-bound: a GraphQL row answers a
    /// rate-limited read only when it names the very same head.
    ///
    /// Spec: docs/specs/agent-native.md#39-delivery-queue-absorbs-the-fleet
    /// Resolve the PR's current head through this observer, then bind checks
    /// and a final head read to the resolved identity.
    pub(crate) fn observe_current_head(
        &mut self,
        number: u64,
        request: &Request,
        max_age: Duration,
        required_names: Option<&[String]>,
        graphql_head: &dyn Fn() -> Option<String>,
    ) -> Result<HeadReading, ObserveError> {
        let pull = match self.resource(&format!("pulls/{number}"), max_age, request) {
            Ok(pull) => pull,
            Err(ObserveError::RateLimited { reset }) => {
                let Some(head) = graphql_head().and_then(|head| GitOid::parse(&head).ok()) else {
                    return Err(ObserveError::RateLimited { reset });
                };
                let reading = match required_names {
                    Some(names) => self.observe_head_with_required_names(
                        number,
                        head,
                        request,
                        max_age,
                        Some(names),
                        graphql_head,
                    ),
                    None => self.observe_head(number, head, request, max_age, graphql_head),
                };
                return Ok(reading);
            }
            Err(error) => return Err(error),
        };
        let expected = pull
            .pointer("/head/sha")
            .and_then(Value::as_str)
            .and_then(|sha| GitOid::parse(sha).ok())
            .ok_or_else(|| ObserveError::Unavailable("pull request has no valid head".into()))?;
        let reading = match required_names {
            Some(names) => self.observe_head_with_required_names(
                number,
                expected,
                request,
                max_age,
                Some(names),
                graphql_head,
            ),
            None => self.observe_head(number, expected, request, max_age, graphql_head),
        };
        Ok(reading)
    }

    pub(crate) fn observe_head(
        &mut self,
        number: u64,
        expected: GitOid,
        request: &Request,
        max_age: Duration,
        graphql_head: &dyn Fn() -> Option<String>,
    ) -> HeadReading {
        self.observe_head_with_required_names(
            number,
            expected,
            request,
            max_age,
            None,
            graphql_head,
        )
    }

    /// Observe a PR head against either its declared branch policy or an
    /// explicit graph gate check set. Both paths share the same head, cache,
    /// page reads and quota hold.
    pub(crate) fn observe_head_with_required_names(
        &mut self,
        number: u64,
        expected: GitOid,
        request: &Request,
        max_age: Duration,
        required_names: Option<&[String]>,
        graphql_head: &dyn Fn() -> Option<String>,
    ) -> HeadReading {
        let start = self.clock.now();
        let deadline = start
            .checked_add(
                jiff::SignedDuration::try_from(request.remaining())
                    .unwrap_or(jiff::SignedDuration::MAX),
            )
            .unwrap_or(jiff::Timestamp::MAX);
        loop {
            if request.is_cancelled() {
                return HeadReading::pending(expected, "CI wait cancelled");
            }
            let now = self.clock.now();
            if now >= deadline {
                return HeadReading::pending(expected, "CI wait deadline expired");
            }
            self.latest_observed_at = None;
            let reading = self.observe_head_once(
                number,
                expected,
                request,
                max_age,
                required_names,
                graphql_head,
            );
            let Some(reset) = reading.retry_at else {
                return reading;
            };
            let reset = reset.as_second();
            if self.pause_for_reset(reset, deadline, request).is_err() {
                return reading;
            }
        }
    }

    fn observe_head_once(
        &mut self,
        number: u64,
        expected: GitOid,
        request: &Request,
        max_age: Duration,
        required_names: Option<&[String]>,
        graphql_head: &dyn Fn() -> Option<String>,
    ) -> HeadReading {
        // No observation older than the batch's freshness bound can settle a
        // verdict, however long a caller is willing to wait for one.
        let freshness = if max_age.is_zero() {
            HEAD_MAX_AGE
        } else {
            max_age.min(HEAD_MAX_AGE)
        };
        let pull = match self.resource(&format!("pulls/{number}"), max_age, request) {
            Ok(pull) => pull,
            Err(ObserveError::RateLimited { reset }) => {
                return match graphql_head().and_then(|head| GitOid::parse(&head).ok()) {
                    // The fallback answered for the same head, but the REST
                    // read is still quota-bound: hold until the reset passes.
                    Some(head) if head == expected => HeadReading::limited(
                        expected,
                        reset,
                        "forge rate limit; REST head read held",
                    ),
                    // Two transports disagree about the head. That is pending,
                    // never a verdict for either head.
                    Some(head) => HeadReading::pending(
                        expected,
                        format!("rest/graphql head mismatch: graphql {head}"),
                    ),
                    None => HeadReading::limited(
                        expected,
                        reset,
                        "forge rate limit; no alternate head reading",
                    ),
                };
            }
            Err(error) => return HeadReading::pending(expected, error.to_string()),
        };
        let pr_head = pull
            .pointer("/head/sha")
            .and_then(Value::as_str)
            .and_then(|sha| GitOid::parse(sha).ok());
        let branch_ref = pull
            .pointer("/head/ref")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mergeable = merge_state(&pull);
        // A moved head cannot produce a verdict for the expected commit. Keep
        // the observed identity even if checks for that old commit are gone.
        if pr_head != Some(expected) {
            let mut reading = HeadReading::pending(
                expected,
                format!("pull head {pr_head:?} is not the expected {expected}"),
            );
            reading.pr_head = pr_head;
            reading.branch_ref = branch_ref;
            reading.mergeable = mergeable;
            reading.observed_at = self.clock.now();
            return reading;
        }
        let base = pull
            .pointer("/base/ref")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let required_policy = required_names
            .map(|names| {
                names
                    .iter()
                    .map(|name| RequiredCheck {
                        name: name.clone(),
                        app: None,
                    })
                    .collect()
            })
            .map(Ok)
            .unwrap_or_else(|| self.required_head_checks(base, request, max_age));
        let required_policy = match required_policy {
            Ok(required) => required,
            Err(ObserveError::RateLimited { reset }) => {
                return HeadReading::limited(
                    expected,
                    reset,
                    "forge rate limit; required policy held",
                );
            }
            Err(error) => return HeadReading::pending(expected, error.to_string()),
        };
        let observed = match self.checks(&expected.to_string(), max_age, request) {
            Ok(checks) => checks,
            Err(ObserveError::RateLimited { reset }) => {
                return HeadReading::limited(expected, reset, "forge rate limit; check pages held");
            }
            Err(error) => return HeadReading::pending(expected, error.to_string()),
        };
        let is_review_gate = |name: &str| self.review_gate_contexts.iter().any(|gate| gate == name);
        let ci_required: Vec<_> = required_policy
            .iter()
            .filter(|need| !is_review_gate(&need.name))
            .collect();
        let required_observed: Vec<Check> = observed
            .iter()
            .filter(|check| {
                ci_required.iter().any(|need| {
                    check.name == need.name && need.app.is_none_or(|app| check.app == Some(app))
                })
            })
            .cloned()
            .collect();
        let review_only_policy = !required_policy.is_empty() && ci_required.is_empty();
        let completion_checks: Vec<&Check> = if review_only_policy {
            observed
                .iter()
                .filter(|check| !is_review_gate(&check.name))
                .collect()
        } else {
            required_observed.iter().collect()
        };
        // Re-read the PR after every check page. If its head moved during the
        // observation, the earlier checks cannot settle a verdict.
        let current_pull = match self.resource(&format!("pulls/{number}"), Duration::ZERO, request)
        {
            Ok(pull) => pull,
            Err(ObserveError::RateLimited { reset }) => {
                return HeadReading::limited(
                    expected,
                    reset,
                    "forge rate limit; final head read held",
                );
            }
            Err(error) => return HeadReading::pending(expected, error.to_string()),
        };
        let current_head = current_pull
            .pointer("/head/sha")
            .and_then(Value::as_str)
            .and_then(|sha| GitOid::parse(sha).ok());
        let current_branch = current_pull
            .pointer("/head/ref")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let current_mergeable = merge_state(&current_pull);
        let observed_at = self.latest_observed_at.unwrap_or_else(|| self.clock.now());
        let sampled_at = self.clock.now();
        let head_agrees = pr_head == Some(expected) && current_head == Some(expected);
        let covered = (!ci_required.is_empty()
            && ci_required.iter().all(|need| {
                observed.iter().any(|check| {
                    check.name == need.name && need.app.is_none_or(|app| check.app == Some(app))
                })
            }))
            || (review_only_policy && !completion_checks.is_empty());
        let settled = !completion_checks
            .iter()
            .any(|check| matches!(check.bucket.as_str(), "pending" | "queued" | "in_progress"));
        let complete = head_agrees && covered && settled;
        let pending_reason = if complete {
            None
        } else if !head_agrees {
            Some(if pr_head != current_head {
                format!("pull head moved during observation: {pr_head:?} -> {current_head:?}")
            } else {
                format!("pull head {current_head:?} is not the expected {expected}")
            })
        } else if required_policy.is_empty() {
            Some("no required checks from branch protection or a ruleset".into())
        } else if !covered {
            Some("a required check has no observation for this head".into())
        } else {
            Some("a required check is queued or in progress".into())
        };
        let mut reading = HeadReading {
            expected_head: expected,
            branch_ref: if current_head == pr_head {
                branch_ref
            } else {
                current_branch
            },
            pr_head: current_head,
            mergeable: current_mergeable,
            required_policy,
            observed,
            required: required_observed,
            observed_at,
            complete,
            pending_reason,
            retry_at: None,
        };
        // A reading that has already aged past the bound cannot settle a
        // verdict, even when every other condition held when it was sampled.
        if reading.complete && !reading.fresh_at(sampled_at, freshness) {
            reading.complete = false;
            reading.pending_reason = Some(format!("observation older than {freshness:?}"));
        }
        reading
    }

    /// Wait for the shared quota reset only when it fits inside the caller's
    /// deadline. A caller with a shorter budget receives the quota reading and
    /// can report its retry time without extending that budget.
    fn pause_for_reset(
        &self,
        reset: i64,
        deadline: jiff::Timestamp,
        request: &Request,
    ) -> Result<(), ObserveError> {
        let reset_at = timestamp(reset);
        let now = self.clock.now();
        let delay = Duration::try_from(reset_at.duration_since(now)).unwrap_or_default();
        let left = Duration::try_from(deadline.duration_since(now)).unwrap_or_default();
        if delay >= left {
            return Err(ObserveError::RateLimited { reset: reset_at });
        }
        self.pause(delay, request);
        if request.is_cancelled() {
            return Err(ObserveError::Expired);
        }
        if self.clock.now() < reset_at {
            return Err(ObserveError::RateLimited { reset: reset_at });
        }
        Ok(())
    }

    /// The required check identities declared by branch protection or a
    /// ruleset. A pinned GitHub App remains part of the identity, so another
    /// app's same-named check cannot satisfy it.
    fn required_head_checks(
        &mut self,
        base: &str,
        request: &Request,
        max_age: Duration,
    ) -> Result<Vec<RequiredCheck>, ObserveError> {
        let mut required = Vec::new();
        if base.is_empty() {
            return Ok(required);
        }
        let mut add = |name: &Value, app: Option<u64>| {
            if let Some(name) = name.as_str() {
                required.push(RequiredCheck {
                    name: name.to_owned(),
                    app,
                });
            }
        };
        match self.resource(&format!("branches/{base}/protection"), max_age, request) {
            Ok(protection) => {
                if let Some(checks) = protection
                    .pointer("/required_status_checks/checks")
                    .and_then(Value::as_array)
                {
                    for check in checks {
                        add(
                            &check["context"],
                            check["app_id"]
                                .as_i64()
                                .and_then(|id| u64::try_from(id).ok()),
                        );
                    }
                }
                if let Some(contexts) = protection
                    .pointer("/required_status_checks/contexts")
                    .and_then(Value::as_array)
                {
                    for context in contexts {
                        add(context, None);
                    }
                }
            }
            Err(ObserveError::Unavailable(message)) if message.contains("HTTP 404") => {}
            Err(error) => return Err(error),
        }
        let rules = self.resource(
            &format!("rules/branches/{base}?per_page=100"),
            max_age,
            request,
        )?;
        if let Some(rows) = rules.as_array() {
            for rule in rows {
                if let Some(checks) = rule
                    .pointer("/parameters/required_status_checks")
                    .and_then(Value::as_array)
                {
                    for check in checks {
                        add(
                            &check["context"],
                            check["integration_id"]
                                .as_i64()
                                .and_then(|id| u64::try_from(id).ok()),
                        );
                    }
                }
            }
        }
        required.sort_by(|left, right| (&left.name, left.app).cmp(&(&right.name, right.app)));
        required.dedup();
        let pinned: std::collections::BTreeSet<String> = required
            .iter()
            .filter(|check| check.app.is_some())
            .map(|check| check.name.clone())
            .collect();
        required.retain(|check| check.app.is_some() || !pinned.contains(&check.name));
        Ok(required)
    }

    fn mark_observed_at(&mut self, fetched_at: i64) {
        let Some(sample) = jiff::Timestamp::from_millisecond(fetched_at).ok() else {
            return;
        };
        if self.latest_observed_at.is_none_or(|known| sample < known) {
            self.latest_observed_at = Some(sample);
        }
    }

    /// Pause between retries on the observer's clock, ending early when the
    /// request is cancelled. The slice between cancellation checks comes from
    /// the one sourced retry family. Callers that wait on CI use this rather
    /// than sleeping themselves, so every pause is cancellable.
    pub(crate) fn pause(&self, delay: Duration, request: &Request) {
        let mut left = delay;
        while !left.is_zero() && !request.is_cancelled() {
            let slice = left.min(self.retry.slice.value);
            self.clock.sleep(slice);
            left = left.saturating_sub(slice);
        }
    }

    /// Record a rate limit's reset, keeping the later of it and any already
    /// recorded. The read-max-write runs under a repository lock, because the
    /// per-commit lock does not order writers on different commits.
    fn record_reset(&self, reset: i64) -> i64 {
        // A lost lock only risks an earlier reset replacing a later one; the
        // next limited read records the limit again.
        let _guard = self
            .dir
            .try_lock(OsStr::new(RATE_LOCK), LockMode::Exclusive)
            .ok();
        let reset = reset.max(self.recorded_reset());
        let extra = self
            .dir
            .read_bounded(OsStr::new(RATE_FILE), 4096)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RateLimit>(&bytes).ok())
            .map(|limit| limit.extra)
            .unwrap_or_default();
        if let Ok(bytes) = serde_json::to_vec(&RateLimit { reset, extra }) {
            let _ = self.dir.atomic_write(
                OsStr::new(RATE_FILE),
                ".rate-",
                &bytes,
                PublishMode::Replace,
            );
        }
        reset
    }

    /// The reset recorded in the rate-limit file, or 0 when none is.
    fn recorded_reset(&self) -> i64 {
        self.dir
            .read_bounded(OsStr::new(RATE_FILE), 4096)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RateLimit>(&bytes).ok())
            .map_or(0, |limit| limit.reset)
    }

    /// The reset of a rate limit another waiter recorded, while it lasts.
    fn rate_limited(&self, now: jiff::Timestamp) -> Option<i64> {
        let bytes = self.dir.read_bounded(OsStr::new(RATE_FILE), 4096).ok()?;
        let limit: RateLimit = serde_json::from_slice(&bytes).ok()?;
        (limit.reset > now.as_second()).then_some(limit.reset)
    }

    fn fetch(&mut self, commit: &str, previous: Cached) -> Result<Cached, Fetch> {
        let root = format!("repos/{}/commits/{commit}", self.repository);
        // Read until every declared run is listed or a page comes back empty;
        // parsing then refuses a listing that lost a page.
        let mut runs = Vec::new();
        let (mut listed, mut declared) = (0_u64, 0_u64);
        loop {
            let path = format!(
                "{root}/check-runs?per_page={PAGE}&filter=latest&page={}",
                runs.len() + 1
            );
            let page = self.page(&path, previous.runs.get(runs.len()), prune_runs)?;
            let count = page.body["check_runs"].as_array().map_or(0, Vec::len) as u64;
            declared = declared.max(page.body["total_count"].as_u64().unwrap_or(0));
            listed += count;
            runs.push(page);
            if count == 0 || listed >= declared {
                break;
            }
        }
        let mut statuses = Vec::new();
        loop {
            let path = format!(
                "{root}/statuses?per_page={PAGE}&page={}",
                statuses.len() + 1
            );
            let page = self.page(&path, previous.statuses.get(statuses.len()), prune_statuses)?;
            let count = page.body.as_array().map_or(0, Vec::len);
            statuses.push(page);
            if count < PAGE {
                break;
            }
        }
        Ok(Cached {
            fetched_at: 0,
            runs,
            statuses,
            extra: previous.extra,
        })
    }

    fn page(
        &mut self,
        path: &str,
        previous: Option<&Page>,
        prune: fn(Value) -> Value,
    ) -> Result<Page, Fetch> {
        let etag = request_etag(self.cadence, previous);
        let response = port::get(&mut self.forge, path, etag).map_err(|error| {
            let message = format!("cannot read CI: {error}");
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidData
            ) {
                Fetch::Failed(message)
            } else {
                Fetch::Transient(message)
            }
        })?;
        if let Some(reset) = rate_limit_reset(&response, self.clock.now().as_second()) {
            return Err(Fetch::Limited(reset));
        }
        match response.status {
            304 => match previous {
                Some(page) if etag.is_some() => Ok(Page {
                    etag: page.etag.clone(),
                    body: page.body.clone(),
                    extra: page.extra.clone(),
                }),
                _ => Err(Fetch::Failed("GitHub answered 304 to a fresh read".into())),
            },
            200..=299 => {
                let body = serde_json::from_slice(&response.body)
                    .map_err(|_| Fetch::Failed("CI did not return valid JSON".into()))?;
                Ok(Page {
                    etag: response.headers.get("etag").cloned(),
                    body: prune(body),
                    extra: previous.map(|page| page.extra.clone()).unwrap_or_default(),
                })
            }
            500..=599 => Err(Fetch::Transient(format!(
                "cannot read CI: HTTP {}",
                response.status
            ))),
            status => Err(Fetch::Failed(format!(
                "cannot read CI for the commit: HTTP {status}"
            ))),
        }
    }
}

/// The sourced retry family for readers that resolve no observer of their own
/// (for example review subject selection). The `ci.retry.*` knobs supply each
/// value with its layer and origin; an unreadable config keeps the compiled
/// defaults.
/// Spec: docs/specs/cli.md#ci-observations
pub(crate) fn retry(roots: &provenant_core::Roots) -> retry::Retry {
    let knobs = provenant_config::read_knobs(roots)
        .map(|config| config.knobs)
        .unwrap_or_default();
    retry::Retry::from_knobs(&knobs)
}

fn identity(body: Value) -> Value {
    body
}

fn valid_resource_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'-' | b'_' | b'.' | b'?' | b'=' | b'&' | b'%')
                })
        })
}

/// `state/ci-observer/<repository>`, private to this user, so its locks hold.
fn open_state(state: &Path, key: &str) -> Result<Directory, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(state)?;
    let mut dir = Directory::open_anchor(state)?;
    for name in ["ci-observer", key] {
        match dir.mkdir(OsStr::new(name), 0o700) {
            Ok(()) => {}
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        dir = dir.open_verified_directory(OsStr::new(name))?;
    }
    Ok(dir)
}

/// The shared per-repository cache directory, keyed like every CI read.
/// Batched forge reads and the per-commit observer cache share one home so a
/// rate limit recorded by one transport holds off the other.
pub(crate) fn shared_dir(state: &Path, repository: &str) -> Result<Directory, String> {
    let repository = repository.strip_prefix("repos/").unwrap_or(repository);
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
    {
        return Err(format!("not a GitHub repository: {repository}"));
    }
    let key = repository.to_ascii_lowercase().replace('/', "--");
    open_state(state, &key).map_err(|error| error.to_string())
}

/// The configured fast observation interval, the shared short TTL for batched
/// reads that have no conditional revalidation.
pub(crate) fn fast_interval(state: &Path, project: &Path) -> Option<Duration> {
    let roots = settings_roots(state, project).ok()?;
    let (settings, _) = provenant_cli::wait::Settings::resolve(&roots).ok()?;
    Some(Duration::from_millis(settings.pr_interval_ms))
}

fn timestamp(second: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(second).unwrap_or(jiff::Timestamp::MAX)
}

/// Keep only the fields CI evidence uses, so the cache stays small.
fn pick(value: &Value, keys: &[&str]) -> Value {
    let mut kept = Map::new();
    for key in keys {
        if let Some(field) = value.get(key) {
            kept.insert((*key).into(), field.clone());
        }
    }
    Value::Object(kept)
}
fn prune_runs(body: Value) -> Value {
    let mut kept = pick(&body, &["total_count"]);
    let runs = body["check_runs"].as_array().map(|runs| {
        runs.iter()
            .map(|run| {
                let mut entry = pick(
                    run,
                    &[
                        "name",
                        "head_sha",
                        "status",
                        "conclusion",
                        "started_at",
                        "completed_at",
                        "details_url",
                        "id",
                    ],
                );
                entry["check_suite"] = pick(&run["check_suite"], &["id"]);
                entry["app"] = pick(&run["app"], &["id", "slug"]);
                entry["output"] = pick(&run["output"], &["title", "summary", "text"]);
                entry
            })
            .collect::<Vec<_>>()
    });
    if let Some(runs) = runs {
        kept["check_runs"] = Value::Array(runs);
    }
    kept
}
fn prune_statuses(body: Value) -> Value {
    match body {
        Value::Array(statuses) => Value::Array(
            statuses
                .iter()
                .map(|status| {
                    pick(
                        status,
                        &[
                            "context",
                            "state",
                            "sha",
                            "target_url",
                            "description",
                            "created_at",
                            "updated_at",
                        ],
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

/// `null` is absent; any other malformed time makes the evidence unusable.
fn time(value: Option<&Value>) -> Option<Option<jiff::Timestamp>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(text) => text.as_str()?.parse().ok().map(Some),
    }
}

/// Check runs and the latest state of each status context. Evidence for
/// another commit, a lost page or an unknown state makes the whole read
/// unusable, since the missing check could be the failing one.
fn parse(cached: &Cached, commit: &str) -> Option<Vec<Check>> {
    parse_bodies(
        cached.runs.iter().map(|page| &page.body),
        cached.statuses.iter().map(|page| &page.body),
        commit,
    )
}

/// Parse check-run and status pages as GitHub serves them.
#[cfg(test)]
pub(crate) fn parse_pages(runs: &[Value], statuses: &[Value], commit: &str) -> Option<Vec<Check>> {
    parse_bodies(runs, statuses, commit)
}

fn parse_bodies<'a>(
    runs: impl IntoIterator<Item = &'a Value>,
    statuses: impl IntoIterator<Item = &'a Value>,
    commit: &str,
) -> Option<Vec<Check>> {
    let mut checks = Vec::new();
    let mut declared = 0;
    for page in runs {
        declared = page["total_count"]
            .as_u64()
            .map_or(declared, |count| count.max(declared));
        for run in page.get("check_runs")?.as_array()? {
            if run.get("head_sha")?.as_str()? != commit {
                return None;
            }
            let bucket = match run.get("status")?.as_str()? {
                "completed" => match run.get("conclusion")?.as_str()? {
                    "success" => "pass",
                    "neutral" | "skipped" => "skipping",
                    "cancelled" => "cancel",
                    "failure" | "timed_out" | "action_required" | "startup_failure" | "stale" => {
                        "fail"
                    }
                    _ => return None,
                },
                "queued" | "in_progress" | "pending" | "waiting" | "requested" => "pending",
                _ => return None,
            };
            let started = time(run.get("started_at"))?;
            let completed = time(run.get("completed_at"))?;
            let output = run.get("output");
            checks.push(Check {
                head: commit.into(),
                name: run.get("name")?.as_str()?.into(),
                job_id: run
                    .get("id")
                    .and_then(Value::as_u64)
                    .map(|id| id.to_string()),
                bucket: bucket.into(),
                app: run["app"]["id"].as_u64(),
                app_slug: run["app"]["slug"].as_str().map(str::to_owned),
                conclusion: run["conclusion"].as_str().map(str::to_owned),
                registered: started.max(completed),
                url: run["details_url"].as_str().map(str::to_owned),
                excerpt: ["title", "summary", "text"]
                    .into_iter()
                    .filter_map(|key| output?.get(key)?.as_str())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
            });
        }
    }
    if checks.len() < usize::try_from(declared).ok()? {
        return None;
    }
    // Statuses are newest first, including prior updates to the same context.
    // An older state cannot veto a successful retry.
    let mut seen = std::collections::BTreeSet::new();
    for page in statuses {
        for status in page.as_array()? {
            let name = status.get("context")?.as_str()?;
            if !seen.insert(name) {
                continue;
            }
            if status["sha"].as_str().is_some_and(|sha| sha != commit) {
                return None;
            }
            let bucket = match status.get("state")?.as_str()? {
                "success" => "pass",
                "pending" | "expected" => "pending",
                "failure" | "error" => "fail",
                _ => return None,
            };
            let created = time(status.get("created_at"))?;
            let updated = time(status.get("updated_at"))?;
            checks.push(Check {
                head: commit.into(),
                name: name.into(),
                job_id: None,
                bucket: bucket.into(),
                app: None,
                app_slug: None,
                conclusion: None,
                registered: created.max(updated),
                url: status["target_url"].as_str().map(str::to_owned),
                excerpt: status["description"].as_str().unwrap_or_default().into(),
            });
        }
    }
    Some(checks)
}

#[cfg(test)]
include!("observer_tests.rs");
