//! Thin GitHub CLI edge, with CI evidence bound to the merge head.
use crate::{emit, fail, roots};
use provenant_cli::{Globals, Pr, domains::MergeMethod};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::GitOid;
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

fn execute(project: &Path, argv: &[&str]) -> Result<ExecOutcome> {
    execute_for(project, argv, Duration::from_secs(60), false)
}
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
fn head(project: &Path, number: &str) -> Result<GitOid> {
    Ok(rest_pull(&|argv| execute(project, argv), number)?.head.sha)
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
fn pull_for_branch(project: &Path, branch: &str, head: GitOid) -> Result<BranchPull> {
    pull_for_branch_with(branch, head, &|argv| successful(execute(project, argv)?))
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
/// How long `pr watch` waits for CI to report its first check, and how often
/// it asks. A push is followed by a gap before any workflow registers.
const UNREPORTED_WAIT: Duration = Duration::from_secs(180);
const UNREPORTED_POLL: Duration = Duration::from_secs(10);
/// The longest `pr watch` follows the CI of one head.
const WATCH_BUDGET: Duration = Duration::from_secs(24 * 60 * 60);

type Observed = std::result::Result<Vec<observer::Check>, observer::ObserveError>;

/// Follow the CI of one head until no check is pending, then require that
/// every check passed or was skipped and that at least one passed. A head
/// with no check yet, or only skipped ones, waits `UNREPORTED_WAIT` for one
/// that runs.
fn watch_with(
    read: &mut dyn FnMut() -> Observed,
    sleep: &dyn Fn(Duration),
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
            sleep(UNREPORTED_POLL);
            waited += UNREPORTED_POLL;
            continue;
        }
        if checks.iter().any(|check| check.bucket == "pending") {
            sleep(observer::POLL);
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
// The `pr watch` edge owns its real clock and sleep.
#[allow(clippy::disallowed_methods)]
fn watch(project: &Path, state: &Path, number: &str, head: GitOid, repository: &str) -> Result<()> {
    let started = std::time::Instant::now();
    let mut ci = observer::Observer::open(state, project, repository, WATCH_BUDGET)
        .map_err(|error| wait::ci_failure(error, &format!("provenant pr watch {number}")))?;
    let commit = head.to_string();
    watch_with(
        &mut || {
            ci.checks(
                &commit,
                observer::POLL,
                WATCH_BUDGET.saturating_sub(started.elapsed()),
            )
        },
        &std::thread::sleep,
        number,
    )?;
    if self::head(project, number)? != head {
        return Err(format!("pull request head moved from {head}").into());
    }
    Ok(())
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
            });
        if status == Some(409) {
            return Err(wait::failure(
                "HEAD_MOVED",
                format!("pull request head moved from {head}"),
            ));
        }
        if status == Some(405) {
            return Err(wait::failure(
                "MERGE_NOT_ALLOWED",
                "pull request is not mergeable; next: resolve the mergeability problem and retry",
            ));
        }
        return Err(format!("merge request failed: {error}").into());
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
            let pull = rest_pull(&|argv| execute(&project, argv), &number)?;
            let current = pull.head.sha;
            captured = Some(current);
            let repository = pull
                .base
                .repo
                .map(|repo| repo.full_name)
                .ok_or("pull request base repository is unavailable")?;
            let state =
                roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional)?
                    .roots
                    .state;
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
        let status = if result.is_ok() { "pass" } else { "fail" };
        if let Err(error) = result {
            failures.push(format!("fail #{number} {head}: {error}"));
        }
        human.push_str(&format!("{status} #{number} {head}\n"));
        pulls.push(serde_json::json!({"number": number, "head": head, "result": status}));
    }
    let outcome = if failures.is_empty() {
        Outcome::Ok
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
    use super::observer::{Check, ObserveError};
    use super::*;
    use std::cell::Cell;

    fn check(bucket: &str) -> Check {
        Check {
            head: "head".into(),
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
            "7",
        )
        .expect("checks registered after three empty reads and then passed");
        assert_eq!(reads.get(), 5);
        assert_eq!(slept.get(), UNREPORTED_POLL * 3 + observer::POLL);
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
            "7",
        )
        .expect("the real workflow registered and passed");
        let error = watch_with(&mut || Ok(vec![check("skipping")]), &|_| {}, "7");
        assert!(error.is_err(), "only skipped checks at the bound");
    }

    #[test]
    fn watch_reports_unscheduled_checks_at_the_bound() {
        let slept = Cell::new(Duration::ZERO);
        let error = watch_with(
            &mut || Ok(Vec::new()),
            &|delay| slept.set(slept.get() + delay),
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
            "7",
        )
        .expect_err("the rate limit outlasts the budget");
        assert_eq!(reads.get(), 1);
        let failure = error
            .downcast_ref::<wait::Failure>()
            .expect("typed failure");
        assert_eq!(failure.code, "RATE_LIMITED");
    }
}
