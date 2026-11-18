//! Several pull requests in one GraphQL query, with quota-aware fallbacks.
//!
//! `pr status` and `ci failures` read pull-request facts and check rollups
//! through one aliased GraphQL query instead of one REST read per pull
//! request. The result is cached in the shared CI observer state for one fast
//! observation interval, keyed by the head SHA each entry belongs to. When the
//! GraphQL quota is exhausted, or the query cannot answer, callers fall back
//! to the per-pull-request REST reads; when a REST read is rate limited,
//! `single` answers it from GraphQL instead.
use super::{Line, View, status_line};
use crate::pr::gh::{Response, rate_limit_reset};
use crate::pr::observer::{self, shared_dir};
use crate::pr::{forge_execute, successful};
use provenant_config::{Forge, ForgeOperation, ForgePort, HeadersObservation, PortTable};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, io, path::Path, time::Duration};

/// How long one batched read may take, shared with one-shot REST reads.
const READ_BUDGET: Duration = Duration::from_secs(60);
const CACHE_FILE: &str = "pr-status.json";

/// One pull request's facts and check rollup, transport-neutral.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub number: u64,
    pub state: String,
    pub head: String,
    pub draft: bool,
    pub merge: String,
    pub review: Option<String>,
    pub url: String,
    /// Every reported check in the observer's bucket vocabulary, in rollup
    /// order. Empty when the rollup declared nothing.
    pub checks: Vec<RawCheck>,
    /// The rollup declared more contexts than one page holds, so `checks` is
    /// incomplete and callers must re-read exactly through REST.
    pub truncated: bool,
}

/// One rollup check without observer-only detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RawCheck {
    pub name: String,
    pub bucket: String,
    pub url: Option<String>,
}

/// The outcome of one attempt at a batched read.
pub(crate) enum Batch {
    Entries(Vec<Entry>),
    /// The batch transport cannot answer now; callers fall back to REST.
    Unavailable(String),
    /// The batch transport is rate limited until `1`.
    RateLimited(jiff::Timestamp),
}

/// The requested pull requests' facts from the shared cache, else one GraphQL
/// query, else a batch-level refusal the caller resolves per pull request.
#[allow(
    clippy::disallowed_methods,
    reason = "the batch cache TTL boundary owns the wall clock"
)]
pub(crate) fn snapshot(state: &Path, project: &Path, numbers: &[u64]) -> Batch {
    if numbers.is_empty() {
        return Batch::Entries(Vec::new());
    }
    let repository = match repository(state, project) {
        Ok(repository) => repository,
        Err(message) => return Batch::Unavailable(message),
    };
    let dir = match shared_dir(state, &repository) {
        Ok(dir) => dir,
        Err(message) => return Batch::Unavailable(message),
    };
    let ttl = observer::fast_interval(state, project).unwrap_or(Duration::from_secs(5));
    let now = jiff::Timestamp::now();
    if let Some(entries) = fresh_cache(&dir, numbers, now, ttl) {
        return Batch::Entries(entries);
    }
    match read(state, project, &repository, numbers) {
        Ok(entries) => {
            let _ = store_cache(&dir, &entries, now);
            Batch::Entries(entries)
        }
        Err(error) => error,
    }
}

/// `snapshot` for exactly one pull request, used when a REST read is limited.
pub(crate) fn single(state: &Path, project: &Path, number: u64) -> Batch {
    snapshot(state, project, &[number])
}

/// The canonical repository name, one cheap read shared with every transport.
pub(crate) fn repository(state: &Path, project: &Path) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Repo {
        #[serde(rename = "nameWithOwner")]
        name: String,
    }
    let bytes = successful(
        forge_execute(
            state,
            project,
            &["gh", "repo", "view", "--json", "nameWithOwner"],
            &provenant_config::ports::Request::within(READ_BUDGET),
            false,
            None,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let repo: Repo = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    Ok(repo.name)
}

fn fresh_cache(
    dir: &provenant_os::fs::Directory,
    numbers: &[u64],
    now: jiff::Timestamp,
    ttl: Duration,
) -> Option<Vec<Entry>> {
    let bytes = dir
        .read_bounded(std::ffi::OsStr::new(CACHE_FILE), 4 * 1024 * 1024)
        .ok()?;
    let cache: Cache = serde_json::from_slice(&bytes).ok()?;
    let age = now.as_millisecond().saturating_sub(cache.fetched_at);
    if !(0..i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)).contains(&age) {
        return None;
    }
    let mut by_number: BTreeMap<u64, Entry> = cache
        .entries
        .into_iter()
        .map(|entry| (entry.number, entry))
        .collect();
    let mut entries = Vec::with_capacity(numbers.len());
    for &number in numbers {
        let entry = by_number.remove(&number)?;
        entries.push(entry);
    }
    Some(entries)
}

fn store_cache(dir: &provenant_os::fs::Directory, entries: &[Entry], now: jiff::Timestamp) {
    let cache = Cache {
        fetched_at: now.as_millisecond(),
        // Entries carry the head SHA their facts belong to; the TTL bounds how
        // long a moved head keeps serving.
        entries: entries.to_vec(),
    };
    if let Ok(bytes) = serde_json::to_vec(&cache) {
        let _ = dir.atomic_write(
            std::ffi::OsStr::new(CACHE_FILE),
            ".pr-status-",
            &bytes,
            provenant_os::fs::PublishMode::Replace,
        );
    }
}

#[derive(Serialize, Deserialize)]
struct Cache {
    /// Unix milliseconds.
    fetched_at: i64,
    entries: Vec<Entry>,
}

fn read(
    state: &Path,
    project: &Path,
    repository: &str,
    numbers: &[u64],
) -> Result<Vec<Entry>, Batch> {
    let Some((owner, name)) = repository.split_once('/') else {
        return Err(Batch::Unavailable(format!(
            "not a GitHub repository: {repository}"
        )));
    };
    let request = provenant_config::ports::Request::within(READ_BUDGET);
    let ports = PortTable::read_for(Some(project), state).map_err(|error| {
        Batch::Unavailable(format!("forge port configuration unavailable: {error}"))
    })?;
    let mut forge = ForgePort::new(
        ports.forge,
        "forge",
        project,
        state,
        // TODO(#3371 seam): caller owner supplies the operation-scoped policy.
        std::sync::Arc::new(provenant_records::policy::Policy::default()),
    )
    .map_err(|error| Batch::Unavailable(format!("cannot start forge adapter: {error}")))?;
    forge.bind(&request);
    let query = rollup_query(owner, name, numbers);
    let response = <ForgePort as Forge>::request(
        &mut forge,
        ForgeOperation::Graphql { query },
        request.remaining(),
    )
    .map_err(|error| Batch::Unavailable(format!("cannot read pull requests: {error}")))?;
    #[allow(
        clippy::disallowed_methods,
        reason = "rate-limit reset comparison needs the wall clock"
    )]
    let now = jiff::Timestamp::now();
    let headers = match &response.headers {
        HeadersObservation::Present { status, values } => {
            if let Some(reset) = rate_limit_reset(
                &Response {
                    status: *status,
                    headers: values.clone(),
                    body: Vec::new(),
                },
                now.as_second(),
            ) {
                return Err(Batch::RateLimited(
                    jiff::Timestamp::from_second(reset).unwrap_or(jiff::Timestamp::MAX),
                ));
            }
            Some(*status)
        }
        HeadersObservation::Absent => response.status,
    };
    if !headers.is_some_and(|status| (200..=299).contains(&status)) {
        return Err(Batch::Unavailable(format!(
            "pull-request read returned HTTP {}",
            headers.unwrap_or(0)
        )));
    }
    let reply: Reply = serde_json::from_slice(&response.body)
        .map_err(|error| Batch::Unavailable(format!("invalid pull-request response: {error}")))?;
    let data = reply
        .data
        .ok_or_else(|| Batch::Unavailable("pull-request response has no data".into()))?;
    let repo = data
        .repository
        .ok_or_else(|| Batch::Unavailable(format!("repository unavailable: {repository}")))?;
    let mut entries = Vec::with_capacity(numbers.len());
    for (index, &number) in numbers.iter().enumerate() {
        let pull = repo.pulls.get(&alias(index, numbers.len())).cloned();
        entries.push(entry(number, pull));
    }
    Ok(entries)
}

/// Zero-padded aliases keep map order equal to the request order.
fn alias(index: usize, total: usize) -> String {
    let width = total.to_string().len().max(2);
    format!("a{:0width$}", index, width = width)
}

fn rollup_query(owner: &str, name: &str, numbers: &[u64]) -> String {
    let mut query = format!("query{{repository(owner:\"{owner}\",name:\"{name}\"){{");
    for (index, &number) in numbers.iter().enumerate() {
        query.push_str(&format!(
            "{}:pullRequest(number:{number}){{number state isDraft mergeStateStatus \
             reviewDecision url headRefOid statusCheckRollup{{\
             contexts(last:100){{totalCount nodes{{__typename ...on CheckRun{{name status conclusion \
             detailsUrl}} ...on StatusContext{{context state targetUrl}}}}}}}}}}}",
            alias(index, numbers.len()),
        ));
    }
    query.push_str("}}");
    query
}

#[derive(Deserialize)]
struct Reply {
    data: Option<Data>,
}
#[derive(Deserialize)]
struct Data {
    repository: Option<Repo>,
}
#[derive(Deserialize)]
struct Repo {
    #[serde(flatten)]
    pulls: BTreeMap<String, Option<Pull>>,
}
#[derive(Clone, Deserialize)]
struct Pull {
    #[serde(default)]
    number: u64,
    #[serde(default)]
    state: String,
    #[serde(rename = "isDraft", default)]
    draft: bool,
    #[serde(rename = "mergeStateStatus", default)]
    merge: String,
    #[serde(rename = "reviewDecision", default)]
    review: Option<String>,
    #[serde(default)]
    url: String,
    #[serde(rename = "headRefOid", default)]
    head: String,
    #[serde(rename = "statusCheckRollup", default)]
    rollup: Option<Rollup>,
}
#[derive(Clone, Deserialize)]
struct Rollup {
    contexts: Option<Contexts>,
}
#[derive(Clone, Deserialize)]
struct Contexts {
    #[serde(rename = "totalCount", default)]
    total: u64,
    #[serde(default)]
    nodes: Option<Vec<Value>>,
}

/// A missing pull request keeps its row, marked unavailable like a failed read.
fn entry(number: u64, pull: Option<Pull>) -> Entry {
    let Some(pull) = pull else {
        return Entry {
            number,
            state: "UNAVAILABLE".into(),
            head: "unknown".into(),
            draft: false,
            merge: "unknown".into(),
            review: None,
            url: String::new(),
            checks: Vec::new(),
            truncated: false,
        };
    };
    let rollup = pull.rollup.as_ref();
    let declared = rollup
        .and_then(|rollup| rollup.contexts.as_ref())
        .map_or(0, |contexts| contexts.total);
    let mut checks = Vec::new();
    let mut lost = 0;
    for node in rollup
        .and_then(|rollup| rollup.contexts.as_ref())
        .and_then(|contexts| contexts.nodes.as_ref())
        .into_iter()
        .flatten()
    {
        if let Some(check) = raw_check(node) {
            checks.push(check);
        } else {
            lost += 1;
        }
    }
    Entry {
        number,
        state: pull.state,
        head: pull.head,
        draft: pull.draft,
        merge: pull.merge,
        review: pull.review,
        url: pull.url,
        checks: checks.clone(),
        // A node the rollup cannot speak for, or more contexts than one page
        // holds, makes this rollup incomplete; callers then read REST exactly.
        truncated: lost > 0 || u64::try_from(checks.len()).is_ok_and(|count| count < declared),
    }
}

/// One rollup node in the observer's bucket vocabulary.
fn raw_check(node: &Value) -> Option<RawCheck> {
    let kind = node.get("__typename")?.as_str()?;
    let bucket = match kind {
        "CheckRun" => match node.get("status")?.as_str()? {
            "COMPLETED" => match node.get("conclusion")?.as_str()? {
                "SUCCESS" => "pass",
                "NEUTRAL" | "SKIPPED" => "skipping",
                "CANCELLED" => "cancel",
                "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE" => "fail",
                _ => return None,
            },
            "QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED" => "pending",
            _ => return None,
        },
        "StatusContext" => match node.get("state")?.as_str()? {
            "SUCCESS" => "pass",
            "PENDING" | "EXPECTED" => "pending",
            "FAILURE" | "ERROR" => "fail",
            _ => return None,
        },
        _ => return None,
    };
    let name = match kind {
        "CheckRun" => node.get("name")?,
        _ => node.get("context")?,
    }
    .as_str()?;
    let url = match kind {
        "CheckRun" => node.get("detailsUrl"),
        _ => node.get("targetUrl"),
    }
    .and_then(Value::as_str)
    .map(str::to_owned);
    Some(RawCheck {
        name: name.into(),
        bucket: bucket.into(),
        url,
    })
}

/// The status line an entry carries, sharing the REST reader's rendering.
pub(crate) fn line_from_entry(entry: &Entry) -> Line {
    let checks: Vec<observer::Check> = entry
        .checks
        .iter()
        .map(|check| observer::Check {
            head: entry.head.clone(),
            name: check.name.clone(),
            bucket: check.bucket.clone(),
            app: None,
            app_slug: None,
            conclusion: None,
            registered: None,
            url: check.url.clone(),
            excerpt: String::new(),
        })
        .collect();
    status_line(
        entry.number,
        View {
            state: entry.state.clone(),
            head: entry.head.clone(),
            merge: entry.merge.clone(),
            review: entry.review.clone(),
            draft: entry.draft,
            url: entry.url.clone(),
        },
        (!entry.truncated && entry.state != "UNAVAILABLE").then_some(checks.as_slice()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pull(sha: &str, checks: Value, total: u64) -> Value {
        json!({
            "number": 7, "state": "OPEN", "isDraft": false,
            "mergeStateStatus": "CLEAN", "reviewDecision": "APPROVED",
            "url": "https://github.com/owner/repo/pull/7", "headRefOid": sha,
            "statusCheckRollup": {"contexts": {"totalCount": total, "nodes": checks}}
        })
    }

    #[test]
    fn the_rollup_query_aliases_one_pull_request_per_number() {
        let query = rollup_query("owner", "repo", &[1, 2]);
        assert!(query.contains("a00:pullRequest(number:1)"));
        assert!(query.contains("a01:pullRequest(number:2)"));
        assert!(query.contains("contexts(last:100)"));
        assert!(query.contains("...on CheckRun{name status conclusion detailsUrl}"));
        assert!(query.contains("...on StatusContext{context state targetUrl}"));
        assert_eq!(alias(0, 12), "a00");
        assert_eq!(alias(11, 12), "a11");
        assert_eq!(alias(0, 120), "a000");
    }

    #[test]
    fn rollup_buckets_match_the_rest_reader() {
        let sha = "a".repeat(40);
        let checks = json!([
            {"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://github.com/owner/repo/actions/runs/1"},
            {"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://github.com/owner/repo/actions/runs/2"},
            {"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS","conclusion":null,"detailsUrl":null},
            {"__typename":"CheckRun","name":"docs","status":"COMPLETED","conclusion":"SKIPPED","detailsUrl":null},
            {"__typename":"CheckRun","name":"deploy","status":"COMPLETED","conclusion":"CANCELLED","detailsUrl":null},
            {"__typename":"StatusContext","context":"deploy-status","state":"PENDING","targetUrl":null},
            {"__typename":"StatusContext","context":"coverage","state":"ERROR","targetUrl":null}
        ]);
        let entry = entry(
            7,
            Some(serde_json::from_value(pull(&sha, checks, 7)).unwrap()),
        );
        let buckets: Vec<_> = entry
            .checks
            .iter()
            .map(|check| check.bucket.as_str())
            .collect();
        assert_eq!(
            buckets,
            [
                "pass", "fail", "pending", "skipping", "cancel", "pending", "fail"
            ]
        );
        assert!(!entry.truncated);
        assert_eq!(entry.head, sha);
        assert_eq!(entry.review.as_deref(), Some("APPROVED"));
        assert_eq!(
            entry.checks[0].url.as_deref(),
            Some("https://github.com/owner/repo/actions/runs/1")
        );
        let line = line_from_entry(&entry);
        assert_eq!(line.class(), "failing");
        assert_eq!(line.failing_checks, ["test", "deploy", "coverage"]);
        assert!(line.text().contains("fail=3 names=test,deploy,coverage"));
    }

    #[test]
    fn a_truncated_or_unspeakable_rollup_falls_back_to_rest() {
        let sha = "b".repeat(40);
        let truncated = entry(
            7,
            Some(serde_json::from_value(pull(&sha, json!([]), 101)).unwrap()),
        );
        assert!(truncated.truncated);
        let unknown_kind = json!([
            {"__typename":"RequiredStatusCheck","context":"build","state":"SUCCESS"}
        ]);
        let foreign = entry(
            7,
            Some(serde_json::from_value(pull(&sha, unknown_kind, 1)).unwrap()),
        );
        assert!(foreign.checks.is_empty());
        assert!(foreign.truncated);
    }

    #[test]
    fn a_missing_pull_request_stays_a_row() {
        let missing = entry(7, None);
        assert_eq!(missing.state, "UNAVAILABLE");
        assert_eq!(missing.head, "unknown");
        assert!(missing.checks.is_empty());
    }
}
