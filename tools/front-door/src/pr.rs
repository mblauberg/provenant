//! Thin GitHub CLI edge, with CI evidence bound to the merge head.
use crate::{emit, fail, roots};
use provenant_cli::{Globals, Pr, domains::MergeMethod};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, GitOid};
use provenant_os::exec::{
    ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
};
use provenant_records::{CommandResult, Outcome};
use serde::Deserialize;
use std::{ffi::OsString, path::Path, time::Duration};

pub(super) mod contributions;
mod gh;
mod git;
pub(crate) mod land;
pub(crate) mod landing;
pub(crate) mod observer;
mod open;
pub(crate) mod status;
mod wait;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A refusal with a stable machine code; the message stays free text.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Refused {
    pub(crate) code: &'static str,
    message: String,
}

impl Refused {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The refusal's code, or `fallback` for any other failure.
pub(crate) fn code_of(
    error: &(dyn std::error::Error + 'static),
    fallback: &'static str,
) -> &'static str {
    error
        .downcast_ref::<Refused>()
        .map_or(fallback, |refused| refused.code)
}

/// An external command exceeded its time limit. Reads may retry this; a
/// mutation must first re-read the state it meant to change.
#[derive(Debug, thiserror::Error)]
#[error("external command timed out")]
pub(crate) struct TimedOut;

/// Run an external command to completion within `timeout`. A `watch` run
/// discards repeated progress so a long CI wait cannot fill capture.
pub(crate) fn execute_for(
    project: &Path,
    argv: &[&str],
    timeout: Duration,
    watch: bool,
) -> Result<ExecOutcome> {
    execute_with(project, argv, timeout, watch, None)
}
fn execute_with(
    project: &Path,
    argv: &[&str],
    timeout: Duration,
    watch: bool,
    stdin: Option<&[u8]>,
) -> Result<ExecOutcome> {
    execute_cancellable(project, argv, timeout, watch, stdin, None)
}
fn execute_cancellable(
    project: &Path,
    argv: &[&str],
    timeout: Duration,
    watch: bool,
    stdin: Option<&[u8]>,
    cancel: Option<&provenant_core::CancelToken>,
) -> Result<ExecOutcome> {
    if argv.first() == Some(&"gh") {
        return Err("forge commands require the resolved forge port".into());
    }
    raw_execute_cancellable(project, argv, timeout, watch, stdin, cancel)
}

fn raw_execute_cancellable(
    project: &Path,
    argv: &[&str],
    timeout: Duration,
    watch: bool,
    stdin: Option<&[u8]>,
    cancel: Option<&provenant_core::CancelToken>,
) -> Result<ExecOutcome> {
    let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
    let mut options = ExecOptions::new(&argv, project, timeout);
    options.stdin = stdin;
    options.cancel = cancel;
    options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
    options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
    // Discard repeated progress so long CI runs cannot fill capture.
    if watch {
        options.stdout = StreamPolicy::Discard;
    }
    options.stop_on_output_limit = true;
    let output = SystemExecutor::new(1)?
        .execute(options)
        .map_err(|error| match error {
            provenant_os::exec::ExecError::Io { operation, source } => {
                format!("cannot {operation} {}: {source}", argv[0].to_string_lossy())
            }
            error => error.to_string(),
        })?;
    if output.timed_out {
        return Err(Box::new(TimedOut));
    }
    if output.cancelled
        || output.drain_truncated
        || output.stdout.truncated
        || output.stdout.read_failed
        || output.stderr.truncated
        || output.stderr.read_failed
    {
        return Err("external command did not complete with intact output".into());
    }
    Ok(output)
}
/// Route forge operations through the sourced adapter while retaining the
/// bounded command result required by the PR policy readers.
pub(crate) fn forge_execute(
    state: &Path,
    project: &Path,
    argv: &[&str],
    request: &provenant_config::ports::Request,
    watch: bool,
    stdin: Option<&[u8]>,
) -> Result<ExecOutcome> {
    if argv.first() != Some(&"gh") {
        return Err("forge operation must name the builtin forge executable".into());
    }
    let ports = provenant_config::PortTable::read_for(Some(project), state)?;
    let mut forge = provenant_config::ForgePort::new(
        ports.forge,
        "forge",
        project,
        state,
        // TODO(#3371 seam): caller owner supplies the operation-scoped policy.
        std::sync::Arc::new(provenant_records::policy::Policy::default()),
    )?;
    provenant_config::Forge::bind(&mut forge, request);
    let arguments: Vec<_> = argv[1..].iter().map(OsString::from).collect();
    let execution = forge
        .execute_with(
            provenant_config::ForgeOperation::Cli {
                arguments,
                input: stdin.map(<[u8]>::to_vec),
            },
            request.remaining(),
            |_, _, _, budget| {
                raw_execute_cancellable(project, argv, budget, watch, stdin, Some(request.cancel()))
                    .map_err(|error| {
                        let kind = if error.is::<TimedOut>() {
                            std::io::ErrorKind::TimedOut
                        } else {
                            std::io::ErrorKind::Other
                        };
                        std::io::Error::new(kind, error.to_string())
                    })
            },
        )
        .map_err(|error| -> Box<dyn std::error::Error> {
            match error {
                provenant_config::PortError::Disposition(
                    provenant_config::PortCommandDisposition::TimedOut,
                ) => Box::new(TimedOut),
                provenant_config::PortError::Executor(error)
                    if error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    Box::new(TimedOut)
                }
                error => Box::new(error),
            }
        })?;
    use provenant_config::PortCommandDisposition as D;
    if !execution.capture_complete() && matches!(execution.disposition, D::Succeeded | D::Refused) {
        return Err(Box::new(provenant_config::PortError::Disposition(
            D::Unknown,
        )));
    }
    match execution.disposition {
        D::TimedOut => Err(Box::new(TimedOut)),
        D::Succeeded => {
            let mut outcome = execution
                .outcome
                .ok_or("forge port returned no process outcome")?;
            // The port's declared success codes define operation success. Exact
            // command termination is retained by the shared command receipt.
            outcome.termination = Termination::Exit(0);
            Ok(outcome)
        }
        D::Refused => {
            let mut outcome = execution
                .outcome
                .ok_or("forge port refused without a process outcome")?;
            if outcome.termination == Termination::Exit(0) {
                outcome.termination = Termination::Exit(1);
            }
            Ok(outcome)
        }
        disposition => Err(Box::new(provenant_config::PortError::Disposition(
            disposition,
        ))),
    }
}

pub(crate) fn successful(output: ExecOutcome) -> Result<Vec<u8>> {
    if output.termination != Termination::Exit(0) {
        return Err(format!(
            "external command failed: {}",
            String::from_utf8_lossy(&output.stderr.bytes).trim()
        )
        .into());
    }
    Ok(output.stdout.bytes)
}
#[derive(Deserialize)]
struct RestBranch {
    sha: GitOid,
    #[serde(rename = "ref")]
    branch: String,
    repo: Option<RestRepository>,
}
#[derive(Deserialize)]
struct RestRepository {
    full_name: String,
}
#[derive(Deserialize)]
struct RestPull {
    state: String,
    head: RestBranch,
    base: RestBranch,
    #[allow(dead_code)]
    mergeable_state: Option<String>,
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    merge_commit_sha: Option<String>,
    title: String,
    #[serde(default)]
    user: Option<RestUser>,
    #[serde(default)]
    stack: Option<RestStack>,
    #[serde(default)]
    pub(crate) draft: bool,
}
#[derive(Deserialize)]
struct RestStack {
    number: u64,
    size: u64,
    position: u64,
    base: RestStackBase,
}
#[derive(Deserialize)]
struct RestStackBase {
    #[serde(rename = "ref")]
    branch: String,
}
#[derive(Deserialize)]
struct RestUser {
    login: String,
}
fn pull_endpoint(number: &str) -> String {
    format!("repos/{{owner}}/{{repo}}/pulls/{number}")
}
fn rest_pull(read: &impl Fn(&[&str]) -> Result<ExecOutcome>, number: &str) -> Result<RestPull> {
    let endpoint = pull_endpoint(number);
    let bytes = successful(read(&[
        "gh",
        "api",
        &endpoint,
        "-H",
        "X-GitHub-Api-Version:2026-03-10",
    ])?)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn head(state: &Path, project: &Path, number: &str) -> Result<GitOid> {
    Ok(rest_pull(
        &|argv| {
            forge_execute(
                state,
                project,
                argv,
                &provenant_config::ports::Request::within(Duration::from_secs(60)),
                false,
                None,
            )
        },
        number,
    )?
    .head
    .sha)
}
/// The pull request a graph lands: found by head branch, never by `gh pr
/// view BRANCH`, which reads a numeric branch as a pull request number.
#[derive(Debug, PartialEq, Eq)]
struct BranchPull {
    number: u64,
    state: String,
    head: GitOid,
    /// The branch the pull request merges into.
    base_branch: String,
}
fn pull_for_branch(state: &Path, project: &Path, branch: &str, head: GitOid) -> Result<BranchPull> {
    pull_for_branch_with(branch, head, &|argv| {
        successful(forge_execute(
            state,
            project,
            argv,
            &provenant_config::ports::Request::within(Duration::from_secs(60)),
            false,
            None,
        )?)
    })
}
/// The one same-repository pull request whose head branch is `branch` and
/// that is open or already at `head`. A fork's namesake, or an earlier pull
/// request from a reused branch name, never matches.
fn pull_for_branch_with(
    branch: &str,
    head: GitOid,
    read: &dyn Fn(&[&str]) -> Result<Vec<u8>>,
) -> Result<BranchPull> {
    #[derive(Deserialize)]
    struct PullRequest {
        number: u64,
        state: String,
        merged_at: Option<String>,
        head: RestBranch,
        base: RestBranch,
    }
    #[derive(Deserialize)]
    struct RepositoryOwner {
        owner: RestUser,
    }
    let repository: RepositoryOwner =
        serde_json::from_slice(&read(&["gh", "api", "repos/{owner}/{repo}"])?)?;
    let head_filter = format!("head={}:{}", repository.owner.login, branch);
    let bytes = read(&[
        "gh",
        "api",
        "--method",
        "GET",
        "--paginate",
        "--slurp",
        "repos/{owner}/{repo}/pulls",
        "-f",
        "state=all",
        "-f",
        "per_page=100",
        "-f",
        &head_filter,
    ])?;
    let mut matching: Vec<PullRequest> = serde_json::from_slice::<Vec<Vec<PullRequest>>>(&bytes)?
        .into_iter()
        .flatten()
        .filter(|pull| {
            pull.head.branch == branch
                && pull
                    .head
                    .repo
                    .as_ref()
                    .zip(pull.base.repo.as_ref())
                    .is_some_and(|(head, base)| {
                        head.full_name.eq_ignore_ascii_case(&base.full_name)
                    })
                && (pull.state == "open" || pull.head.sha == head)
        })
        .collect();
    match matching.len() {
        1 => {
            let pull = matching.remove(0);
            Ok(BranchPull {
                number: pull.number,
                state: if pull.merged_at.is_some() {
                    "MERGED".into()
                } else {
                    pull.state.to_uppercase()
                },
                head: pull.head.sha,
                base_branch: pull.base.branch,
            })
        }
        0 => Err(format!("no same-repository pull request has head branch {branch}").into()),
        count => Err(format!(
            "{count} same-repository pull requests have head branch {branch}; land one by number"
        )
        .into()),
    }
}
/// How long `pr watch` waits for CI to report its first check. Its poll
/// interval comes from the observer's configured adaptive cadence.
const UNREPORTED_WAIT: Duration = Duration::from_secs(180);
/// The longest `pr watch` follows the CI of one head.
const WATCH_BUDGET: Duration = Duration::from_secs(24 * 60 * 60);

type Observed = std::result::Result<Vec<observer::Check>, observer::ObserveError>;

/// Follow the CI of one head until no check is pending, then require that
/// every check passed or was skipped and that at least one passed. A head
/// with no check yet, or only skipped ones, waits `UNREPORTED_WAIT` for one
/// that runs.
#[cfg(test)] // Retained as the pre-journal polling baseline.
#[allow(dead_code)] // The custom worktree driver compiles without a test harness.
fn watch_with(
    read: &mut dyn FnMut() -> Observed,
    sleep: &dyn Fn(Duration),
    poll_interval: &dyn Fn() -> Duration,
    number: &str,
) -> Result<()> {
    let mut waited = Duration::ZERO;
    loop {
        let checks = read()
            .map_err(|error| wait::ci_failure(error, &format!("provenant pr watch {number}")))?;
        // Skipped jobs finish at once, so right after a push they can be the
        // only checks reported; wait for the real workflows as for none.
        if checks.iter().all(|check| check.bucket == "skipping") {
            if waited >= UNREPORTED_WAIT && !checks.is_empty() {
                return Err("CI checks have not passed".into());
            }
            if waited >= UNREPORTED_WAIT {
                return Err(wait::failure(
                    "CI_NOT_SCHEDULED",
                    format!(
                        "no checks were reported for #{number} within {}s; next: check the workflow triggers or push an empty commit to start CI",
                        UNREPORTED_WAIT.as_secs()
                    ),
                ));
            }
            let delay = poll_interval()
                .max(Duration::from_millis(1))
                .min(UNREPORTED_WAIT.saturating_sub(waited));
            sleep(delay);
            waited += delay;
            continue;
        }
        if checks.iter().any(|check| check.bucket == "pending") {
            sleep(poll_interval().max(Duration::from_millis(1)));
            continue;
        }
        if !checks.iter().any(|check| check.bucket == "pass")
            || checks
                .iter()
                .any(|check| !matches!(check.bucket.as_str(), "pass" | "skipping"))
        {
            return Err("CI checks have not passed".into());
        }
        return Ok(());
    }
}
fn watch_timeout(number: &str, head: &str, checks: &[String]) -> Box<dyn std::error::Error> {
    let states: std::collections::BTreeSet<_> = checks.iter().map(String::as_str).collect();
    wait::failure(
        "CI_TIMEOUT",
        format!(
            "watch timed out for #{number}; last known head={head} check_states={states:?}; subscription retained; next: provenant pr watch {number}"
        ),
    )
}
fn indexed_watch(
    read: &mut dyn FnMut() -> Observed,
    next_event: &mut dyn FnMut(Duration) -> Result<Option<serde_json::Value>>,
    request: &provenant_config::ports::Request,
    expected_head: &str,
    number: &str,
) -> Result<()> {
    fn buckets(checks: Vec<observer::Check>) -> Vec<String> {
        checks.into_iter().map(|check| check.bucket).collect()
    }
    fn evaluate(checks: &[String], unreported: Duration, number: &str) -> Result<bool> {
        if checks.iter().all(|check| check == "skipping") {
            if unreported >= UNREPORTED_WAIT && !checks.is_empty() {
                return Err("CI checks have not passed".into());
            }
            if unreported >= UNREPORTED_WAIT {
                return Err(wait::failure(
                    "CI_NOT_SCHEDULED",
                    format!(
                        "no checks were reported for #{number} within {}s; next: check the workflow triggers or push an empty commit to start CI",
                        UNREPORTED_WAIT.as_secs()
                    ),
                ));
            }
            return Ok(false);
        }
        if checks.iter().any(|check| check == "pending") {
            return Ok(false);
        }
        if !checks.iter().any(|check| check == "pass")
            || checks
                .iter()
                .any(|check| !matches!(check.as_str(), "pass" | "skipping"))
        {
            return Err("CI checks have not passed".into());
        }
        Ok(true)
    }

    let mut checks = buckets(
        read().map_err(|error| wait::ci_failure(error, &format!("provenant pr watch {number}")))?,
    );
    let mut unreported = Duration::ZERO;
    loop {
        if evaluate(&checks, unreported, number)? {
            return Ok(());
        }
        let remaining = request.remaining();
        if remaining.is_zero() {
            return Err(watch_timeout(number, expected_head, &checks));
        }
        let maximum = remaining.min(Duration::from_secs(60));
        let detail = next_event(maximum)?;
        let waited = remaining.saturating_sub(request.remaining()).min(maximum);
        if checks.iter().all(|check| check == "skipping") {
            unreported = unreported.saturating_add(waited);
        }
        match detail {
            Some(detail) if detail["heartbeat"] == true => continue,
            Some(detail)
                if detail["head"].as_str() == Some(expected_head)
                    && detail["checks"].is_array() =>
            {
                let rows = detail["checks"]
                    .as_array()
                    .ok_or_else(|| "PR journal event has no checks array".to_owned())?;
                checks = rows
                    .iter()
                    .map(|row| {
                        row["bucket"]
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "PR journal check has no bucket".into())
                    })
                    .collect::<Result<Vec<_>>>()?;
            }
            Some(_) | None => {
                if request.remaining().is_zero() {
                    return Err(watch_timeout(number, expected_head, &checks));
                }
                checks = buckets(read().map_err(|error| match error {
                    observer::ObserveError::Expired => {
                        watch_timeout(number, expected_head, &checks)
                    }
                    error => wait::ci_failure(error, &format!("provenant pr watch {number}")),
                })?);
            }
        }
    }
}

// The `pr watch` edge owns its real clock and sleep.
#[allow(clippy::disallowed_methods)]
fn watch(project: &Path, state: &Path, number: &str, head: GitOid, repository: &str) -> Result<()> {
    // One request context bounds the whole watch: every checks read and the
    // final pull request read share its deadline.
    let request = provenant_config::ports::Request::within(WATCH_BUDGET);
    let mut ci = observer::Observer::open(state, project, repository, &request)
        .map_err(|error| wait::ci_failure(error, &format!("provenant pr watch {number}")))?;
    let commit = head.to_string();
    let number_value = number.parse::<u64>()?;
    let mut journal = crate::event_delivery::Journal::open(state, project)?;
    journal.register_pr(repository, number_value, Some(&commit))?;
    let cancel = CancelToken::new();
    indexed_watch(
        &mut || ci.checks(&commit, Duration::ZERO, &request),
        &mut |maximum| {
            let event = journal.next_pr(repository, number_value, maximum, &cancel)?;
            if event.is_none()
                && journal
                    .source_current(repository, number_value, Some(&commit))
                    .unwrap_or(false)
            {
                Ok(Some(serde_json::json!({"heartbeat":true})))
            } else {
                Ok(event)
            }
        },
        &request,
        &commit,
        number,
    )?;
    let pull = ci
        .resource(&format!("pulls/{number}"), Duration::ZERO, &request)
        .map_err(|error| wait::ci_failure(error, &format!("provenant pr watch {number}")))?;
    let expected_head = head.to_string();
    if pull["head"]["sha"].as_str() != Some(expected_head.as_str()) {
        return Err(format!("pull request head moved from {head}").into());
    }
    Ok(())
}
fn extract_github_message(output: &provenant_os::exec::ExecOutcome) -> Option<String> {
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stdout.bytes)
        && let Some(msg) = val.get("message").and_then(|m| m.as_str())
        && !msg.trim().is_empty()
    {
        return Some(msg.trim().to_owned());
    }
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stderr.bytes)
        && let Some(msg) = val.get("message").and_then(|m| m.as_str())
        && !msg.trim().is_empty()
    {
        return Some(msg.trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr.bytes);
    for line in stderr.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{')
            && trimmed.ends_with('}')
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
            && let Some(msg) = val.get("message").and_then(|m| m.as_str())
            && !msg.trim().is_empty()
        {
            return Some(msg.trim().to_owned());
        }
    }
    let stdout = String::from_utf8_lossy(&output.stdout.bytes);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{')
            && trimmed.ends_with('}')
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
            && let Some(msg) = val.get("message").and_then(|m| m.as_str())
            && !msg.trim().is_empty()
        {
            return Some(msg.trim().to_owned());
        }
    }
    let text_lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("HTTP"))
        .collect();
    if !text_lines.is_empty() {
        let text = text_lines.join(" ");
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

fn merge_failure_from_output(
    output: &provenant_os::exec::ExecOutcome,
    head: GitOid,
) -> Box<dyn std::error::Error> {
    let error = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stderr.bytes),
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let status = error
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|words| {
            (words[0] == "HTTP")
                .then(|| {
                    words[1]
                        .trim_end_matches(|ch: char| !ch.is_ascii_digit())
                        .parse::<u16>()
                        .ok()
                })
                .flatten()
        })
        .or_else(|| {
            serde_json::from_slice::<serde_json::Value>(&output.stdout.bytes)
                .ok()
                .and_then(|val| {
                    val.get("status").and_then(|s| {
                        s.as_str()
                            .and_then(|s| s.parse::<u16>().ok())
                            .or_else(|| s.as_u64().map(|n| n as u16))
                    })
                })
        });
    if status == Some(409) {
        return wait::failure("HEAD_MOVED", format!("pull request head moved from {head}"));
    }
    if status == Some(405) {
        let message = match extract_github_message(output) {
            Some(msg)
                if msg
                    .to_ascii_lowercase()
                    .starts_with("pull request is not mergeable") =>
            {
                msg
            }
            Some(msg) => format!("pull request is not mergeable: {msg}"),
            None => "pull request is not mergeable".to_owned(),
        };
        return wait::failure("MERGE_NOT_ALLOWED", message);
    }
    format!("merge request failed: {error}").into()
}

/// Merge a pull request pinned to `head` with `method`, returning the commit
/// GitHub created. A moved head (409) and a refused merge (405) are named.
fn merge_with(
    reader: &wait::Reader<'_>,
    number: &str,
    head: GitOid,
    method: MergeMethod,
) -> Result<Option<String>> {
    let method = match method {
        MergeMethod::Merge => "merge_method=merge",
        MergeMethod::Squash => "merge_method=squash",
        MergeMethod::Rebase => "merge_method=rebase",
    };
    let endpoint = format!("{}/merge", pull_endpoint(number));
    let sha = format!("sha={head}");
    let output = reader.request(
        &[
            "gh", "api", "--method", "PUT", &endpoint, "-f", &sha, "-f", method,
        ],
        true,
    )?;
    if output.termination != Termination::Exit(0) {
        return Err(merge_failure_from_output(&output, head));
    }
    let response: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    if response
        .get("merged")
        .and_then(serde_json::Value::as_bool)
        .is_none()
    {
        return Err("GitHub did not return a merge result".into());
    }
    Ok(response
        .get("sha")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}
pub fn run(command: Pr, globals: &Globals, _workers: &dyn WorkerObserver) -> u8 {
    let numbers = match command {
        Pr::Watch { numbers } => numbers,
        Pr::Open(command) => return open::run(*command, globals),
        Pr::Status { numbers } => return status::run(numbers, globals),
        Pr::Contributions { number, bind } => return contributions::run(number, bind, globals),
    };
    const LEAF: &str = "pr watch";
    // Each pull request is watched in turn; the result reports every head.
    let mut pulls = Vec::new();
    let mut human = String::new();
    let mut failures = Vec::new();
    for number in numbers {
        let mut captured = None;
        let result = (|| -> Result<()> {
            let project = roots::selected_project(globals.project.as_deref(), true)?;
            let number = number.to_string();
            let state =
                roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional)?
                    .roots
                    .state;
            let request = provenant_config::ports::Request::within(WATCH_BUDGET);
            let repository = status::wait_repository(state.as_path(), &project, &request)?;
            let mut observer =
                observer::Observer::open(state.as_path(), &project, &repository, &request)
                    .map_err(|error| {
                        wait::ci_failure(error, &format!("provenant pr watch {number}"))
                    })?;
            let pull = observer
                .resource(&format!("pulls/{number}"), Duration::ZERO, &request)
                .map_err(|error| {
                    wait::ci_failure(error, &format!("provenant pr watch {number}"))
                })?;
            let pull: RestPull = serde_json::from_value(pull)?;
            let current = pull.head.sha;
            captured = Some(current);
            watch(&project, state.as_path(), &number, current, &repository)?;
            Ok(())
        })();
        let Some(head) = captured else {
            return fail(
                LEAF,
                Outcome::Error,
                "PR",
                &result.err().map_or_else(
                    || format!("pull request #{number} head is unavailable"),
                    |error| format!("#{number}: {error}"),
                ),
                globals,
            );
        };
        let timeout = result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<wait::Failure>())
            .is_some_and(|error| matches!(error.code, "CI_TIMEOUT" | "CI_NOT_SCHEDULED"));
        let status = if result.is_ok() {
            "pass"
        } else if timeout {
            "timeout"
        } else {
            "fail"
        };
        if let Err(error) = result {
            failures.push(format!("{status} #{number} {head}: {error}"));
        }
        human.push_str(&format!("{status} #{number} {head}\n"));
        pulls.push(serde_json::json!({"number": number, "head": head, "result": status}));
    }
    let outcome = if failures.is_empty() {
        Outcome::Ok
    } else if pulls.iter().all(|pull| pull["result"] != "fail") {
        Outcome::Timeout
    } else {
        Outcome::Error
    };
    let mut report = CommandResult::new(LEAF, outcome, serde_json::json!({"pulls": pulls}));
    if !failures.is_empty() {
        report.error = Some(provenant_records::CommandError {
            diagnostic: None,
            code: "PR".into(),
            message: failures.join("; "),
            cause: None,
            repair: None,
        });
    }
    emit(&report, globals, &human, true)
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod branch_pull_tests {
    use super::*;
    mod rest_calls {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/rest_calls.rs"
        ));
    }

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn select(pulls: serde_json::Value) -> Result<BranchPull> {
        pull_for_branch_with("123", HEAD.parse().unwrap(), &|argv| {
            // `gh pr view 123` would select pull request #123, not branch 123.
            assert!(rest_calls::rest_only(&argv[1..].join(" ")));
            assert!(!rest_calls::rest_only("pr list --head 123"));
            if argv == ["gh", "api", "repos/{owner}/{repo}"] {
                return Ok(br#"{"owner":{"login":"owner"}}"#.to_vec());
            }
            assert_eq!(
                argv,
                [
                    "gh",
                    "api",
                    "--method",
                    "GET",
                    "--paginate",
                    "--slurp",
                    "repos/{owner}/{repo}/pulls",
                    "-f",
                    "state=all",
                    "-f",
                    "per_page=100",
                    "-f",
                    "head=owner:123",
                ]
            );
            Ok(serde_json::to_vec(&serde_json::json!([pulls]))?)
        })
    }

    fn pull(number: u64, state: &str, head: &str, cross: bool) -> serde_json::Value {
        serde_json::json!({"number":number, "state":if state == "OPEN" { "open" } else { "closed" }, "merged_at":if state == "MERGED" { Some("2020") } else { None }, "head":{"sha":head,"ref":"123","repo":{"full_name":if cross { "fork/repo" } else { "owner/repo" }}}, "base":{"sha":head,"ref":"main","repo":{"full_name":"owner/repo"}}})
    }

    #[test]
    fn branch_lookup_passes_the_head_filter_as_a_single_argument() {
        let result = pull_for_branch_with("feature/a&b", HEAD.parse().unwrap(), &|argv| {
            assert!(rest_calls::rest_only(&argv[1..].join(" ")));
            if argv == ["gh", "api", "repos/{owner}/{repo}"] {
                return Ok(br#"{"owner":{"login":"owner"}}"#.to_vec());
            }
            assert_eq!(argv.last(), Some(&"head=owner:feature/a&b"));
            Ok(b"[[]]".to_vec())
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no same-repository")
        );
    }

    #[test]
    fn a_numeric_branch_resolves_to_its_unique_same_repository_pull_request() {
        let found = select(serde_json::json!([
            pull(9, "OPEN", HEAD, true),
            pull(456, "OPEN", HEAD, false),
            pull(12, "MERGED", OTHER, false),
        ]))
        .unwrap();
        assert_eq!((found.number, found.state.as_str()), (456, "OPEN"));
        assert_eq!(
            select(serde_json::json!([pull(456, "MERGED", HEAD, false)]))
                .unwrap()
                .number,
            456
        );
        let ambiguous = select(serde_json::json!([
            pull(456, "OPEN", HEAD, false),
            pull(457, "OPEN", OTHER, false),
        ]))
        .unwrap_err();
        assert!(
            ambiguous.to_string().contains("2 same-repository"),
            "{ambiguous}"
        );
        assert!(select(serde_json::json!([pull(9, "OPEN", HEAD, true)])).is_err());
    }
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod unreported_checks_tests {
    const HEAD: &str = "watched-head";
    const OTHER: &str = "another-head";
    use super::observer::{Check, ObserveError};
    use super::*;
    use std::cell::Cell;

    fn check(bucket: &str) -> Check {
        Check {
            head: HEAD.into(),
            name: format!("ci-{bucket}"),
            bucket: bucket.into(),
            app: None,
            app_slug: None,
            conclusion: None,
            registered: None,
            url: None,
            excerpt: String::new(),
        }
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn indexed_watch_timeout_reports_the_last_snapshot_and_keeps_the_subscription() {
        let request = provenant_config::ports::Request::within(Duration::ZERO);
        let events = Cell::new(0);
        let error = indexed_watch(
            &mut || Ok(vec![check("pending")]),
            &mut |_| {
                events.set(events.get() + 1);
                Ok(None)
            },
            &request,
            HEAD,
            "7",
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<wait::Failure>().unwrap().code,
            "CI_TIMEOUT"
        );
        assert!(error.to_string().contains(HEAD));
        assert!(error.to_string().contains("pending"));
        assert!(error.to_string().contains("subscription retained"));
        assert_eq!(events.get(), 0);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn indexed_watch_advances_from_pending_to_pass_without_another_check_read() {
        use serde_json::json;

        let reads = Cell::new(0);
        let events = Cell::new(0);
        let request = provenant_config::ports::Request::within(Duration::from_secs(30));
        let mut read = || {
            reads.set(reads.get() + 1);
            Ok(vec![check("pending")])
        };
        let mut next = |maximum| {
            assert!(maximum > Duration::ZERO && maximum <= Duration::from_secs(30));
            events.set(events.get() + 1);
            Ok(Some(if events.get() == 1 {
                json!({"heartbeat":true})
            } else {
                json!({"head":HEAD,"checks":[{"bucket":"pass"}]})
            }))
        };
        indexed_watch(&mut read, &mut next, &request, HEAD, "7")
            .expect("the journal reports the passing checks");
        // One complete Observer::checks invocation happens for the initial
        // reconcile; journal delivery causes no further Forge reads.
        assert_eq!(reads.get(), 1);
        assert_eq!(events.get(), 2);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn indexed_watch_reconciles_after_a_quiet_journal_window() {
        let reads = Cell::new(0);
        let events = Cell::new(0);
        let request = provenant_config::ports::Request::within(Duration::from_secs(30));
        let mut read = || {
            reads.set(reads.get() + 1);
            Ok(vec![check(if reads.get() == 1 {
                "pending"
            } else {
                "pass"
            })])
        };
        let mut next = |maximum| {
            assert!(maximum > Duration::ZERO && maximum <= Duration::from_secs(30));
            events.set(events.get() + 1);
            Ok(None)
        };
        indexed_watch(&mut read, &mut next, &request, HEAD, "7")
            .expect("the quiet-window reconcile observes the settled checks");
        assert_eq!(events.get(), 1);
        assert_eq!(reads.get(), 2, "one initial read and one gap reconcile");
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn indexed_watch_reconciles_a_head_mismatch_before_accepting_a_result() {
        use serde_json::json;

        let reads = Cell::new(0);
        let events = Cell::new(0);
        let request = provenant_config::ports::Request::within(Duration::from_secs(30));
        let mut read = || {
            reads.set(reads.get() + 1);
            Ok(vec![check("pending")])
        };
        let mut next = |maximum| {
            assert!(maximum > Duration::ZERO && maximum <= Duration::from_secs(30));
            events.set(events.get() + 1);
            Ok(Some(if events.get() == 1 {
                json!({"head":OTHER,"checks":[{"bucket":"pass"}]})
            } else {
                json!({"head":HEAD,"checks":[{"bucket":"pass"}]})
            }))
        };
        indexed_watch(&mut read, &mut next, &request, HEAD, "7")
            .expect("only the watched head can settle the wait");
        assert_eq!(events.get(), 2);
        assert_eq!(reads.get(), 2, "the moved head triggered one reconcile");
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn indexed_watch_reconciles_webhook_markers_even_when_the_head_matches() {
        use serde_json::json;

        let reads = Cell::new(0);
        let events = Cell::new(0);
        let request = provenant_config::ports::Request::within(Duration::from_secs(30));
        let mut read = || {
            reads.set(reads.get() + 1);
            Ok(vec![check(if reads.get() == 1 {
                "pending"
            } else {
                "pass"
            })])
        };
        let mut next = |_| {
            events.set(events.get() + 1);
            Ok(Some(json!({
                "repository":"owner/repo",
                "number":7,
                "head":HEAD,
                "source":"webhook"
            })))
        };
        indexed_watch(&mut read, &mut next, &request, HEAD, "7")
            .expect("identifier marker triggers an authoritative check read");
        assert_eq!(events.get(), 1);
        assert_eq!(reads.get(), 2, "the webhook marker triggered one reconcile");
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn indexed_watch_stops_on_a_failed_event_without_reconciling() {
        use serde_json::json;

        let reads = Cell::new(0);
        let request = provenant_config::ports::Request::within(Duration::from_secs(30));
        let mut read = || {
            reads.set(reads.get() + 1);
            Ok(vec![check("pending")])
        };
        let mut next = |_| Ok(Some(json!({"head":HEAD,"checks":[{"bucket":"fail"}]})));
        let error = indexed_watch(&mut read, &mut next, &request, HEAD, "7")
            .expect_err("a failed check event is terminal");
        assert!(error.to_string().contains("CI checks have not passed"));
        assert_eq!(reads.get(), 1, "failure came from the journal event");
    }

    #[test]
    fn watch_waits_while_no_check_is_reported_then_proceeds() {
        let reads = Cell::new(0);
        let slept = Cell::new(Duration::ZERO);
        watch_with(
            &mut || {
                reads.set(reads.get() + 1);
                Ok(match reads.get() {
                    1..=3 => Vec::new(),
                    4 => vec![check("pending")],
                    _ => vec![check("pass"), check("skipping")],
                })
            },
            &|delay| slept.set(slept.get() + delay),
            &|| observer::POLL,
            "7",
        )
        .expect("checks registered after three empty reads and then passed");
        assert_eq!(reads.get(), 5);
        assert_eq!(slept.get(), observer::POLL * 4);
    }

    #[test]
    fn watch_uses_the_adapter_interval_selected_after_each_pending_read() {
        let reads = Cell::new(0);
        let interval = Cell::new(Duration::from_secs(1));
        let slept = Cell::new(Duration::ZERO);
        watch_with(
            &mut || {
                reads.set(reads.get() + 1);
                match reads.get() {
                    1 => {
                        interval.set(Duration::from_secs(15));
                        Ok(vec![check("pending")])
                    }
                    2 => {
                        interval.set(Duration::from_secs(2));
                        Ok(vec![check("pending")])
                    }
                    _ => Ok(vec![check("pass")]),
                }
            },
            &|delay| slept.set(slept.get() + delay),
            &|| interval.get(),
            "7",
        )
        .expect("checks settle after slow and fast pending intervals");
        assert_eq!(slept.get(), Duration::from_secs(17));
    }

    #[test]
    fn watch_waits_past_checks_that_skipped_before_ci_registered() {
        let reads = Cell::new(0);
        watch_with(
            &mut || {
                reads.set(reads.get() + 1);
                Ok(match reads.get() {
                    1 => vec![check("skipping")],
                    _ => vec![check("pass"), check("skipping")],
                })
            },
            &|_| {},
            &|| observer::POLL,
            "7",
        )
        .expect("the real workflow registered and passed");
        let error = watch_with(
            &mut || Ok(vec![check("skipping")]),
            &|_| {},
            &|| observer::POLL,
            "7",
        );
        assert!(error.is_err(), "only skipped checks at the bound");
    }

    #[test]
    fn watch_reports_unscheduled_checks_at_the_bound() {
        let slept = Cell::new(Duration::ZERO);
        let error = watch_with(
            &mut || Ok(Vec::new()),
            &|delay| slept.set(slept.get() + delay),
            &|| observer::POLL,
            "7",
        )
        .expect_err("checks never registered");
        assert_eq!(slept.get(), UNREPORTED_WAIT);
        let failure = error
            .downcast_ref::<wait::Failure>()
            .expect("typed failure");
        assert_eq!(failure.code, "CI_NOT_SCHEDULED");
        assert!(failure.message.contains("#7 within 180s"));
    }

    #[test]
    fn watch_fails_a_finished_run_that_did_not_pass_without_waiting() {
        for buckets in [
            vec!["fail"],
            vec!["pass", "cancel"],
            vec!["fail", "skipping"],
        ] {
            let error = watch_with(
                &mut || Ok(buckets.iter().map(|bucket| check(bucket)).collect()),
                &|_| panic!("a finished run must not wait"),
                &|| observer::POLL,
                "7",
            );
            assert!(error.is_err(), "{buckets:?}");
        }
    }

    #[test]
    fn watch_names_a_rate_limit_and_does_not_retry_it() {
        let reads = Cell::new(0);
        let error = watch_with(
            &mut || {
                reads.set(reads.get() + 1);
                Err(ObserveError::RateLimited {
                    reset: "2026-01-01T00:00:00Z".parse().unwrap(),
                })
            },
            &|_| panic!("the observer owns the rate-limit wait"),
            &|| observer::POLL,
            "7",
        )
        .expect_err("the rate limit outlasts the budget");
        assert_eq!(reads.get(), 1);
        let failure = error
            .downcast_ref::<wait::Failure>()
            .expect("typed failure");
        assert_eq!(failure.code, "RATE_LIMITED");
    }

    fn test_outcome(stdout: &[u8], stderr: &[u8]) -> provenant_os::exec::ExecOutcome {
        provenant_os::exec::ExecOutcome {
            termination: Termination::Exit(1),
            stdout: provenant_os::exec::CapturedStream {
                bytes: stdout.to_vec(),
                ..Default::default()
            },
            stderr: provenant_os::exec::CapturedStream {
                bytes: stderr.to_vec(),
                ..Default::default()
            },
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        }
    }

    #[test]
    fn merge_405_carries_github_message_to_refusal() {
        let output = test_outcome(
            br#"{"message":"Pull Request is still a draft","status":"405"}"#,
            b"HTTP 405\n",
        );
        let head = GitOid::parse("0123456789abcdef0123456789abcdef01234567").unwrap();
        let err = merge_failure_from_output(&output, head);
        let failure = err.downcast_ref::<wait::Failure>().expect("typed failure");
        assert_eq!(failure.code, "MERGE_NOT_ALLOWED");
        assert!(
            failure.message.contains("Pull Request is still a draft"),
            "message was: {}",
            failure.message
        );
    }

    #[test]
    fn merge_405_carries_stderr_text_to_refusal() {
        let output = test_outcome(b"", b"HTTP 405\nPull Request is still a draft\n");
        let head = GitOid::parse("0123456789abcdef0123456789abcdef01234567").unwrap();
        let err = merge_failure_from_output(&output, head);
        let failure = err.downcast_ref::<wait::Failure>().expect("typed failure");
        assert_eq!(failure.code, "MERGE_NOT_ALLOWED");
        assert!(
            failure.message.contains("Pull Request is still a draft"),
            "message was: {}",
            failure.message
        );
    }
}
