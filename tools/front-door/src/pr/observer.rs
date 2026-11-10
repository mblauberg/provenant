//! The one reader of CI for a repository. Every waiter, whether `pr status`,
//! a graph gate, `land` waiting or the landing gate, reads check runs and
//! commit statuses for an exact commit through [`Observer::checks`].
//!
//! Reads are cached in host state by repository and commit, so waiters in any
//! process share one answer per poll interval: a waiter accepts an answer
//! younger than its own interval. An older answer is refreshed by a
//! conditional request, so an unchanged commit costs a `304`. A one-shot
//! decision accepts no age and always revalidates. A rate-limited
//! response is recorded once for the repository with its reset time, and
//! every waiter holds off until then instead of retrying on its own.
//!
//! A commit is any head: a pull request head, a `land/**` candidate or a
//! landed commit.
use crate::event_intake::gh::{Forge, Gh, Response, rate_limit_reset};
use provenant_core::{CancelToken, Clock as _, DurationMicros, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, LockMode, PublishMode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::Duration,
};

/// The interval of waiters that set none of their own.
pub(crate) const POLL: Duration = Duration::from_secs(5);
const PAGE: usize = 100;
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LAST: Duration = Duration::from_secs(15);
const RATE_FILE: &str = "rate-limit.json";
const RATE_LOCK: &str = "rate-limit.lock";
const CACHE_LIMIT: u64 = 16 * 1024 * 1024;

/// One reported check run or the latest state of one status context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub name: String,
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
    #[error("CI wait expired")]
    Expired,
    #[error("{0}")]
    Unavailable(String),
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

/// `gh api` in the project checkout, each request bounded by the read budget.
pub(crate) struct GhTransport {
    project: PathBuf,
    left: Duration,
}
impl Forge for GhTransport {
    fn bound(&mut self, left: Duration) {
        self.left = left;
    }
    #[allow(clippy::disallowed_methods, reason = "forge transport timing boundary")]
    fn get(&mut self, path: &str, etag: Option<&str>) -> std::io::Result<Response> {
        let left = self
            .left
            .clamp(Duration::from_secs(1), Duration::from_secs(60));
        Gh::in_checkout(self.project.clone(), left).get(path, etag)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Page {
    etag: Option<String>,
    body: Value,
}
#[derive(Default, Serialize, Deserialize)]
struct Cached {
    /// Unix milliseconds.
    fetched_at: i64,
    runs: Vec<Page>,
    statuses: Vec<Page>,
}
#[derive(Serialize, Deserialize)]
struct RateLimit {
    reset: i64,
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
}

impl Observer<GhTransport, WallClock> {
    /// The observer for `repository` (`owner/name`), reading through `gh` in
    /// `project` within `budget`, with its cache under the state root.
    #[allow(clippy::disallowed_methods, reason = "forge transport timing boundary")]
    pub(crate) fn open(
        state: &Path,
        project: &Path,
        repository: &str,
        budget: Duration,
    ) -> Result<Self, ObserveError> {
        let transport = GhTransport {
            project: project.to_owned(),
            left: budget,
        };
        Self::with(state, repository, transport, WallClock)
    }
}

impl<F: Forge, C: Clock> Observer<F, C> {
    pub(crate) fn with(
        state: &Path,
        repository: &str,
        forge: F,
        clock: C,
    ) -> Result<Self, ObserveError> {
        let repository = repository
            .strip_prefix("repos/")
            .unwrap_or(repository)
            .to_owned();
        let parts: Vec<_> = repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            })
        {
            return Err(ObserveError::Unavailable(format!(
                "not a GitHub repository: {repository}"
            )));
        }
        let key = repository.to_ascii_lowercase().replace('/', "--");
        let dir = open_state(state, &key).map_err(|error| {
            ObserveError::Unavailable(format!("cannot open CI observer state: {error}"))
        })?;
        Ok(Self {
            dir,
            repository,
            forge,
            clock,
        })
    }

    /// Every check reported for `commit`. An answer any waiter on the host
    /// read less than `max_age` ago is served without a request. Transient
    /// failures are retried, and a rate limit is waited out, within `budget`.
    pub(crate) fn checks(
        &mut self,
        commit: &str,
        max_age: Duration,
        budget: Duration,
    ) -> Result<Vec<Check>, ObserveError> {
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ObserveError::Unavailable(format!(
                "not a commit id: {commit}"
            )));
        }
        let start = self.clock.now();
        let deadline = start
            .checked_add(
                jiff::SignedDuration::try_from(budget).unwrap_or(jiff::SignedDuration::MAX),
            )
            .unwrap_or(jiff::Timestamp::MAX);
        let mut retries = 0_u32;
        loop {
            let now = self.clock.now();
            if let Some(reset) = self.rate_limited(now) {
                let at = timestamp(reset);
                if at > deadline {
                    return Err(ObserveError::RateLimited { reset: at });
                }
                self.clock
                    .sleep(seconds(reset.saturating_sub(now.as_second()).max(1)));
                continue;
            }
            let left = Duration::try_from(deadline.duration_since(now)).unwrap_or_default();
            if left.is_zero() {
                return Err(ObserveError::Expired);
            }
            self.forge.bound(left);
            let lock = format!("{commit}.lock");
            let guard = self
                .dir
                .lock(
                    OsStr::new(&lock),
                    LockMode::Exclusive,
                    WaitBudget::up_to(
                        SystemClock.mono_now(),
                        DurationMicros::from_micros(
                            u64::try_from(left.as_micros()).unwrap_or(u64::MAX),
                        ),
                    ),
                    &CancelToken::new(),
                    &SystemClock,
                )
                .map_err(|error| {
                    if self.clock.now() >= deadline {
                        ObserveError::Expired
                    } else {
                        ObserveError::Unavailable(format!("cannot lock CI observer state: {error}"))
                    }
                })?;
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
            let fresh = cached.as_ref().is_some_and(|cached| {
                let age = now.as_millisecond().saturating_sub(cached.fetched_at);
                (0..i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX)).contains(&age)
            });
            let cached = if fresh {
                cached.unwrap_or_default()
            } else {
                match self.fetch(commit, cached.unwrap_or_default()) {
                    Ok(mut fetched) => {
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
                        if timestamp(reset) > deadline {
                            return Err(ObserveError::RateLimited {
                                reset: timestamp(reset),
                            });
                        }
                        continue;
                    }
                    Err(Fetch::Transient(message)) => {
                        drop(guard);
                        let delay = RETRY_FIRST
                            .saturating_mul(2_u32.saturating_pow(retries))
                            .min(RETRY_LAST);
                        let left = Duration::try_from(deadline.duration_since(self.clock.now()))
                            .unwrap_or_default();
                        if left <= delay {
                            return Err(ObserveError::Unavailable(message));
                        }
                        self.clock.sleep(delay);
                        retries = retries.saturating_add(1);
                        continue;
                    }
                    Err(Fetch::Failed(message)) => return Err(ObserveError::Unavailable(message)),
                }
            };
            drop(guard);
            return parse(&cached, commit).ok_or_else(|| {
                ObserveError::Unavailable(
                    "CI evidence is incomplete or belongs to another head".into(),
                )
            });
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
            .lock(
                OsStr::new(RATE_LOCK),
                LockMode::Exclusive,
                WaitBudget::up_to(
                    SystemClock.mono_now(),
                    DurationMicros::from_micros(5_000_000),
                ),
                &CancelToken::new(),
                &SystemClock,
            )
            .ok();
        let reset = reset.max(self.recorded_reset());
        if let Ok(bytes) = serde_json::to_vec(&RateLimit { reset }) {
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
        })
    }

    fn page(
        &mut self,
        path: &str,
        previous: Option<&Page>,
        prune: fn(Value) -> Value,
    ) -> Result<Page, Fetch> {
        let etag = previous.and_then(|page| page.etag.as_deref());
        let response = self
            .forge
            .get(path, etag)
            .map_err(|error| Fetch::Transient(format!("cannot read CI: {error}")))?;
        if let Some(reset) = rate_limit_reset(&response, self.clock.now().as_second()) {
            return Err(Fetch::Limited(reset));
        }
        match response.status {
            304 => match previous {
                Some(page) if etag.is_some() => Ok(Page {
                    etag: page.etag.clone(),
                    body: page.body.clone(),
                }),
                _ => Err(Fetch::Failed("GitHub answered 304 to a fresh read".into())),
            },
            200..=299 => {
                let body = serde_json::from_slice(&response.body)
                    .map_err(|_| Fetch::Failed("CI did not return valid JSON".into()))?;
                Ok(Page {
                    etag: response.headers.get("etag").cloned(),
                    body: prune(body),
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

fn seconds(value: i64) -> Duration {
    Duration::from_secs(u64::try_from(value).unwrap_or(0))
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
                name: run.get("name")?.as_str()?.into(),
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
                name: name.into(),
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
#[allow(dead_code, unused_imports, clippy::disallowed_methods)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        hash::{Hash, Hasher},
        io,
        sync::{Arc, Condvar, Mutex},
    };

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";
    /// 2026-01-01T00:00:00Z.
    const START: i64 = 1_767_225_600_000;

    /// One forge and one clock shared by every waiter in a test.
    #[derive(Default)]
    struct World {
        now_ms: i64,
        requests: Vec<(String, Option<String>)>,
        full_reads: usize,
        runs: BTreeMap<String, Value>,
        statuses: BTreeMap<String, Value>,
        /// Unix second until which every request is rate limited.
        limited_until: Option<i64>,
        /// The next request is limited with a reset already past.
        past_limit: bool,
        delay: Duration,
    }
    /// Holds a forge read open until enough waiters have entered `checks`.
    #[derive(Default)]
    struct Gate {
        entered: Mutex<usize>,
        needed: Mutex<usize>,
        changed: Condvar,
    }
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<World>>, Arc<Gate>);
    impl Shared {
        fn hold_reads_until(&self, waiters: usize) {
            *self.1.needed.lock().expect("gate") = waiters;
        }
        fn enter(&self) {
            *self.1.entered.lock().expect("gate") += 1;
            self.1.changed.notify_all();
        }
        fn pass(&self) {
            let needed = *self.1.needed.lock().expect("gate");
            let entered = self.1.entered.lock().expect("gate");
            drop(
                self.1
                    .changed
                    .wait_while(entered, |entered| *entered < needed)
                    .expect("gate"),
            );
        }
        fn new() -> Self {
            let shared = Self::default();
            shared.with(|world| world.now_ms = START);
            shared
        }
        fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
            f(&mut self.0.lock().expect("world"))
        }
        fn requests(&self) -> usize {
            self.with(|world| world.requests.len())
        }
        fn observer(&self, state: &Path) -> Observer<Shared, Shared> {
            Observer::with(state, "Owner/Repo", self.clone(), self.clone()).expect("observer")
        }
    }
    impl Forge for Shared {
        fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
            self.pass();
            let delay = self.with(|world| {
                world.requests.push((path.into(), etag.map(str::to_owned)));
                world.delay
            });
            self.sleep(delay);
            self.with(|world| {
                let mut headers = BTreeMap::new();
                let now = world.now_ms / 1000;
                if std::mem::take(&mut world.past_limit) {
                    headers.insert("retry-after".into(), "0".into());
                    return Ok(Response {
                        status: 429,
                        headers,
                        body: Vec::new(),
                    });
                }
                if let Some(until) = world.limited_until.filter(|until| *until > now) {
                    headers.insert("x-ratelimit-remaining".into(), "0".into());
                    headers.insert("x-ratelimit-reset".into(), until.to_string());
                    return Ok(Response {
                        status: 403,
                        headers,
                        body: br#"{"message":"API rate limit exceeded"}"#.to_vec(),
                    });
                }
                let commit = path
                    .split("/commits/")
                    .nth(1)
                    .and_then(|rest| rest.split('/').next())
                    .unwrap_or_default();
                let body = if path.contains("/check-runs?") {
                    world.runs.get(commit).cloned()
                } else {
                    world.statuses.get(commit).cloned()
                }
                .unwrap_or_else(|| json!([]));
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                body.to_string().hash(&mut hasher);
                let tag = format!("W/\"{:x}\"", hasher.finish());
                headers.insert("etag".into(), tag.clone());
                if etag == Some(tag.as_str()) {
                    return Ok(Response {
                        status: 304,
                        headers,
                        body: Vec::new(),
                    });
                }
                world.full_reads += 1;
                Ok(Response {
                    status: 200,
                    headers,
                    body: serde_json::to_vec(&body)?,
                })
            })
        }
    }
    impl Clock for Shared {
        fn now(&self) -> jiff::Timestamp {
            self.with(|world| jiff::Timestamp::from_millisecond(world.now_ms).expect("time"))
        }
        fn sleep(&self, duration: Duration) {
            let millis = i64::try_from(duration.as_millis()).expect("sleep");
            self.with(|world| world.now_ms += millis);
        }
    }

    fn run(name: &str, status: &str, conclusion: Option<&str>) -> Value {
        json!({"name":name,"head_sha":HEAD,"status":status,"conclusion":conclusion,
            "started_at":"2020-01-01T00:00:00Z","app":{"id":15368,"slug":"github-actions"}})
    }
    fn page(runs: Vec<Value>) -> Value {
        json!({"total_count":runs.len(),"check_runs":runs})
    }
    fn state() -> tempfile::TempDir {
        tempfile::tempdir().expect("state")
    }

    #[test]
    fn two_concurrent_waiters_on_one_head_share_one_read_per_poll() {
        let shared = Shared::new();
        shared.with(|world| {
            world
                .runs
                .insert(HEAD.into(), page(vec![run("build", "in_progress", None)]));
            world.delay = Duration::from_millis(100);
        });
        // The first read stays open until both waiters are inside `checks`,
        // so the second must queue behind it and be served from its cache.
        shared.hold_reads_until(2);
        let dir = state();
        let poll = |shared: &Shared| {
            std::thread::scope(|scope| {
                let waiters: Vec<_> = (0..2)
                    .map(|_| {
                        let mut observer = shared.observer(dir.path());
                        let shared = shared.clone();
                        scope.spawn(move || {
                            shared.enter();
                            observer.checks(HEAD, POLL, Duration::from_secs(60))
                        })
                    })
                    .collect();
                waiters
                    .into_iter()
                    .map(|waiter| waiter.join().expect("waiter").expect("checks"))
                    .collect::<Vec<_>>()
            })
        };
        let first = poll(&shared);
        assert_eq!(first[0], first[1]);
        assert_eq!(first[0][0].bucket, "pending");
        // One read of the head is one check-run request and one status request.
        assert_eq!(shared.requests(), 2);
        // Within the interval, neither waiter reaches the forge.
        shared.with(|world| world.now_ms += 1_000);
        poll(&shared);
        assert_eq!(shared.requests(), 2);
        // The next interval costs one more read for both.
        shared.with(|world| world.now_ms += 5_000);
        poll(&shared);
        assert_eq!(shared.requests(), 4);
    }

    #[test]
    fn an_unchanged_answer_is_revalidated_by_a_304() {
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
        });
        let dir = state();
        let first = shared
            .observer(dir.path())
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect("first");
        assert_eq!(shared.with(|world| world.full_reads), 2);
        // A one-shot reader in another process accepts no age, yet an unchanged
        // head costs only conditional requests.
        let again = shared
            .observer(dir.path())
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect("again");
        assert_eq!(again, first);
        assert_eq!(shared.with(|world| world.full_reads), 2);
        let conditional = shared.with(|world| world.requests[2..].to_vec());
        assert_eq!(conditional.len(), 2);
        assert!(
            conditional.iter().all(|(_, etag)| etag.is_some()),
            "{conditional:?}"
        );
        // A changed answer is read in full.
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("failure"))]),
            );
        });
        let changed = shared
            .observer(dir.path())
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect("changed");
        assert_eq!(changed[0].bucket, "fail");
        assert_eq!(shared.with(|world| world.full_reads), 3);
    }

    #[test]
    fn a_rate_limit_stops_every_waiter_until_its_reset() {
        let shared = Shared::new();
        let reset = START / 1000 + 120;
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
            world.limited_until = Some(reset);
        });
        let dir = state();
        let error = shared
            .observer(dir.path())
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect_err("limited");
        let at = jiff::Timestamp::from_second(reset).expect("reset");
        assert!(
            matches!(error, ObserveError::RateLimited { reset } if reset == at),
            "{error}"
        );
        assert!(error.to_string().contains(&at.to_string()), "{error}");
        assert_eq!(shared.requests(), 1);
        // Another waiter, on another head, holds off without asking again.
        let error = shared
            .observer(dir.path())
            .checks(OTHER, POLL, Duration::from_secs(60))
            .expect_err("still limited");
        assert!(matches!(error, ObserveError::RateLimited { .. }), "{error}");
        assert_eq!(shared.requests(), 1);
        // A waiter whose budget outlasts the reset sleeps until it, then reads.
        let checks = shared
            .observer(dir.path())
            .checks(HEAD, POLL, Duration::from_secs(300))
            .expect("after reset");
        assert_eq!(checks[0].bucket, "pass");
        assert!(shared.with(|world| world.now_ms) >= reset * 1000);
        assert_eq!(shared.requests(), 3);
    }

    #[test]
    fn a_rate_limit_whose_reset_is_past_still_pauses() {
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
            world.past_limit = true;
        });
        let dir = state();
        let checks = shared
            .observer(dir.path())
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect("after pause");
        assert_eq!(checks[0].bucket, "pass");
        assert!(shared.with(|world| world.now_ms) >= START + 1000);
    }

    #[test]
    fn trimming_keeps_the_ids_a_rerun_needs() {
        let shared = Shared::new();
        let mut kept = run("build", "completed", Some("failure"));
        kept["id"] = json!(42);
        kept["check_suite"] = json!({"id": 7, "app": {"id": 1}});
        shared.with(|world| {
            world.runs.insert(HEAD.into(), page(vec![kept]));
        });
        let dir = state();
        shared
            .observer(dir.path())
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect("checks");
        let repository = std::fs::read_dir(dir.path().join("ci-observer"))
            .expect("root")
            .next()
            .expect("repository")
            .expect("entry")
            .path();
        let text = std::fs::read_to_string(repository.join(format!("{HEAD}.json"))).expect("cache");
        let cached: Cached = serde_json::from_str(&text).expect("cached");
        let run = &cached.runs[0].body["check_runs"][0];
        assert_eq!(run["id"], 42);
        assert_eq!(run["check_suite"], json!({"id": 7}));
    }

    #[test]
    fn pr_status_land_wait_and_the_landing_gate_agree_on_one_observation() {
        let shared = Shared::new();
        let dir = state();
        let now: jiff::Timestamp = "2026-01-01T00:00:00Z".parse().expect("now");
        for (runs, expected) in [
            (
                vec![run("build", "in_progress", None)],
                ("pending", "pending", "pending"),
            ),
            (
                vec![run("build", "completed", Some("failure"))],
                ("failing", "failed", "failed"),
            ),
            (
                vec![run("build", "completed", Some("success"))],
                ("ready", "passed", "passed"),
            ),
            (
                vec![
                    run("lint", "in_progress", None),
                    run("test", "completed", Some("failure")),
                    run("build", "completed", Some("success")),
                ],
                ("failing", "failed", "failed"),
            ),
        ] {
            shared.with(|world| {
                world.runs.insert(HEAD.into(), page(runs));
            });
            let checks = shared
                .observer(dir.path())
                .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
                .expect("checks");
            let status = crate::pr::status::class_of(HEAD, &checks);
            let wait = crate::pr::wait::verdict(&checks, now);
            let gate = match crate::pr::landing::adapters::gate_checks(checks).1 {
                crate::pr::landing::model::GateResult::Passed => "passed",
                crate::pr::landing::model::GateResult::Pending => "pending",
                crate::pr::landing::model::GateResult::Failed => "failed",
                other => panic!("unexpected gate result {other:?}"),
            };
            assert_eq!((status, wait, gate), expected);
        }
    }

    #[test]
    fn every_page_is_read_and_a_lost_one_is_refused() {
        let shared = Shared::new();
        let dir = state();
        let runs: Vec<_> = (0..PAGE + 1)
            .map(|index| run(&format!("job-{index}"), "completed", Some("success")))
            .collect();
        let mut first = page(runs[..PAGE].to_vec());
        first["total_count"] = json!(PAGE + 1);
        let mut second = page(runs[PAGE..].to_vec());
        second["total_count"] = json!(PAGE + 1);
        let mut world_pages = vec![first, second];
        struct Paged(Shared, Vec<Value>);
        impl Forge for Paged {
            fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
                let index: usize = path
                    .rsplit("&page=")
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(1);
                if path.contains("/check-runs?") {
                    let body = self
                        .1
                        .get(index - 1)
                        .cloned()
                        .unwrap_or_else(|| json!({"check_runs":[]}));
                    self.0.with(|world| world.runs.insert(HEAD.into(), body));
                }
                self.0.get(path, etag)
            }
        }
        let mut paged = Observer::with(
            dir.path(),
            "owner/repo",
            Paged(shared.clone(), world_pages.clone()),
            shared.clone(),
        )
        .expect("observer");
        let checks = paged
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect("pages");
        assert_eq!(checks.len(), PAGE + 1);
        world_pages.truncate(1);
        let mut lost = Observer::with(
            dir.path(),
            "owner/repo",
            Paged(shared.clone(), world_pages),
            shared.clone(),
        )
        .expect("observer");
        let error = lost
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect_err("lost page");
        assert!(matches!(error, ObserveError::Unavailable(_)), "{error}");
    }

    #[test]
    fn evidence_for_another_head_or_an_unknown_state_is_refused() {
        let ok = parse_pages(
            &[page(vec![run("build", "in_progress", None)])],
            &[json!([])],
            HEAD,
        )
        .expect("valid");
        assert_eq!(ok[0].bucket, "pending");
        assert_eq!(ok[0].app, Some(15368));
        assert_eq!(ok[0].app_slug.as_deref(), Some("github-actions"));
        let mut moved = run("build", "completed", Some("success"));
        moved["head_sha"] = json!(OTHER);
        assert!(parse_pages(&[page(vec![moved])], &[json!([])], HEAD).is_none());
        assert!(
            parse_pages(
                &[page(vec![run("build", "completed", None)])],
                &[json!([])],
                HEAD
            )
            .is_none()
        );
        let mut bad_time = run("build", "completed", Some("success"));
        bad_time["started_at"] = json!("yesterday");
        assert!(parse_pages(&[page(vec![bad_time])], &[json!([])], HEAD).is_none());
    }

    #[test]
    fn only_the_latest_state_of_a_status_context_counts() {
        let statuses = json!([
            {"context":"legacy","state":"success","target_url":"https://ci.example/1"},
            {"context":"legacy","state":"failure","description":"obsolete failure"},
            {"context":"external","state":"error","description":"broken"},
        ]);
        let checks = parse_pages(&[page(Vec::new())], &[statuses], HEAD).expect("valid");
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].bucket, "pass");
        assert_eq!(checks[0].url.as_deref(), Some("https://ci.example/1"));
        assert_eq!(checks[1].bucket, "fail");
        assert_eq!(checks[1].excerpt, "broken");
    }

    #[test]
    fn a_failed_check_keeps_its_link_and_output_excerpt() {
        let mut failed = run("build", "completed", Some("failure"));
        failed["details_url"] = json!("https://github.com/owner/repo/actions/runs/1");
        failed["output"] = json!({"title":"Build failed","summary":"","text":"src/main.rs:12","annotations_count":3});
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(HEAD.into(), page(vec![failed]));
        });
        let dir = state();
        let checks = shared
            .observer(dir.path())
            .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
            .expect("checks");
        assert_eq!(checks[0].excerpt, "Build failed\nsrc/main.rs:12");
        assert!(
            checks[0]
                .url
                .as_deref()
                .is_some_and(|url| url.ends_with("/runs/1"))
        );
    }

    #[test]
    fn a_server_error_is_retried_and_a_refusal_is_not() {
        struct Failing(Shared, Vec<u16>);
        impl Forge for Failing {
            fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
                if let Some(status) = self.1.pop() {
                    self.0
                        .with(|world| world.requests.push((path.into(), None)));
                    return Ok(Response {
                        status,
                        headers: BTreeMap::new(),
                        body: b"{}".to_vec(),
                    });
                }
                self.0.get(path, etag)
            }
        }
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
        });
        let dir = state();
        let mut retried = Observer::with(
            dir.path(),
            "owner/repo",
            Failing(shared.clone(), vec![502]),
            shared.clone(),
        )
        .expect("observer");
        assert_eq!(
            retried
                .checks(HEAD, Duration::ZERO, Duration::from_secs(60))
                .expect("retried")
                .len(),
            1
        );
        let mut refused = Observer::with(
            dir.path(),
            "owner/repo",
            Failing(shared.clone(), vec![404]),
            shared.clone(),
        )
        .expect("observer");
        assert!(matches!(
            refused.checks(HEAD, Duration::ZERO, Duration::from_secs(60)),
            Err(ObserveError::Unavailable(_))
        ));
    }
}
