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
mod git;
pub(crate) mod land;
pub(crate) mod landing;
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

fn execute(project: &Path, argv: &[&str], watch: bool) -> Result<ExecOutcome> {
    execute_for(
        project,
        argv,
        Duration::from_secs(if watch { 86_400 } else { 60 }),
        watch,
    )
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
    mergeable: Option<bool>,
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
    Ok(rest_pull(&|argv| execute(project, argv, false), number)?
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
}
fn pull_for_branch(project: &Path, branch: &str, head: GitOid) -> Result<BranchPull> {
    pull_for_branch_with(branch, head, &|argv| {
        successful(execute(project, argv, false)?)
    })
}
/// The one same-repository pull request whose head branch is `branch` and
/// that is open or already at `head`. If neither exists, a unique merged PR
/// reveals a moved head. A fork's namesake never matches; multiple candidates
/// are ambiguous and refuse reconciliation.
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
    let candidates: Vec<PullRequest> = serde_json::from_slice::<Vec<Vec<PullRequest>>>(&bytes)?
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
        })
        .collect();
    let has_current = candidates
        .iter()
        .any(|pull| pull.state == "open" || pull.head.sha == head);
    let mut matching: Vec<_> = candidates
        .into_iter()
        .filter(|pull| {
            if has_current {
                pull.state == "open" || pull.head.sha == head
            } else {
                pull.merged_at.is_some()
            }
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
            })
        }
        0 => Err(format!("no same-repository pull request has head branch {branch}").into()),
        count => Err(format!(
            "{count} same-repository pull requests have head branch {branch}; land one by number"
        )
        .into()),
    }
}
fn checks(project: &Path, number: &str, head: GitOid, watch: bool) -> Result<()> {
    if watch {
        successful(execute(
            project,
            &["gh", "pr", "checks", number, "--watch"],
            true,
        )?)?;
    }
    #[derive(Deserialize)]
    struct Check {
        bucket: String,
    }
    let bytes = successful(execute(
        project,
        &["gh", "pr", "checks", number, "--json", "bucket"],
        false,
    )?)?;
    let checks: Vec<Check> = serde_json::from_slice(&bytes)?;
    if !checks.iter().any(|check| check.bucket == "pass")
        || checks
            .iter()
            .any(|check| !matches!(check.bucket.as_str(), "pass" | "skipping"))
    {
        return Err("CI checks have not passed".into());
    }
    if self::head(project, number)? != head {
        return Err(format!("pull request head moved from {head}").into());
    }
    Ok(())
}
fn merge(reader: &wait::Reader<'_>, number: &str, head: GitOid) -> Result<()> {
    merge_with(reader, number, head, MergeMethod::Merge).map(|_| ())
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
    let (leaf, number) = match command {
        Pr::Watch { number } => ("pr watch", number),
        Pr::Open(command) => return open::run(*command, globals),
        Pr::Status { numbers, wait } => return status::run(numbers, wait, globals),
        Pr::Contributions { number, bind } => return contributions::run(number, bind, globals),
    };
    let mut captured = None;
    let result = (|| -> Result<()> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let number = number.to_string();
        let current = head(&project, &number)?;
        captured = Some(current);
        checks(&project, &number, current, true)?;
        Ok(())
    })();
    let Some(head) = captured else {
        return fail(
            leaf,
            Outcome::Error,
            "PR",
            &result.err().map_or_else(
                || "pull request head is unavailable".into(),
                |error| error.to_string(),
            ),
            globals,
        );
    };
    let (outcome, status) = if result.is_ok() {
        (Outcome::Ok, "pass")
    } else {
        (Outcome::Error, "fail")
    };
    let mut report = CommandResult::new(
        leaf,
        outcome,
        serde_json::json!({"number": number, "head": head, "result": status}),
    );
    if let Err(error) = result {
        report.error = Some(provenant_records::CommandError {
            diagnostic: None,
            code: "PR".into(),
            message: format!("fail #{number} {head}: {error}"),
            cause: None,
            repair: None,
        });
    }
    emit(
        &report,
        globals,
        &format!("{status} #{number} {head}\n"),
        true,
    )
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
        let moved = select(serde_json::json!([pull(457, "MERGED", OTHER, false)])).unwrap();
        assert_eq!(
            (
                moved.number,
                moved.state.as_str(),
                moved.head.to_string().as_str()
            ),
            (457, "MERGED", OTHER)
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
