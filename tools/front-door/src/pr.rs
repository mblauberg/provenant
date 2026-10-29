//! Thin GitHub CLI edge, with CI evidence bound to the merge head.
use crate::{emit, fail, roots};
use provenant_cli::{Globals, Pr};
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
mod open;
pub(crate) mod status;
mod wait;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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
fn execute_for(
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
    let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
    let mut options = ExecOptions::new(&argv, project, timeout);
    options.stdin = stdin;
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
fn successful(output: ExecOutcome) -> Result<Vec<u8>> {
    if output.termination != Termination::Exit(0) {
        return Err(format!(
            "external command failed: {}",
            String::from_utf8_lossy(&output.stderr.bytes).trim()
        )
        .into());
    }
    Ok(output.stdout.bytes)
}
fn head(project: &Path, number: &str) -> Result<GitOid> {
    #[derive(Deserialize)]
    struct PullRequest {
        #[serde(rename = "headRefOid")]
        head: GitOid,
    }
    let bytes = successful(execute(
        project,
        &["gh", "pr", "view", number, "--json", "headRefOid"],
        false,
    )?)?;
    Ok(serde_json::from_slice::<PullRequest>(&bytes)?.head)
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
        #[serde(rename = "headRefOid")]
        head: GitOid,
        #[serde(rename = "isCrossRepository")]
        cross_repository: bool,
    }
    let bytes = read(&[
        "gh",
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "all",
        "--json",
        "number,state,headRefOid,isCrossRepository",
    ])?;
    let mut matching: Vec<PullRequest> = serde_json::from_slice::<Vec<PullRequest>>(&bytes)?
        .into_iter()
        .filter(|pull| !pull.cross_repository && (pull.state == "OPEN" || pull.head == head))
        .collect();
    match matching.len() {
        1 => {
            let pull = matching.remove(0);
            Ok(BranchPull {
                number: pull.number,
                state: pull.state,
                head: pull.head,
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
fn merge(project: &Path, number: &str, head: GitOid) -> Result<()> {
    let git = provenant_os::git::git_executable().ok_or("Git executable is unavailable")?;
    let git = git.to_str().ok_or("Git executable path is not UTF-8")?;
    let configured = execute(project, &[git, "config", "--get", "user.email"], false)?;
    let email = match configured.termination {
        Termination::Exit(0) => Some(
            String::from_utf8(configured.stdout.bytes)?
                .trim()
                .to_owned(),
        ),
        Termination::Exit(1) => None,
        _ => return Err("cannot read Git user.email".into()),
    };
    let head = head.to_string();
    let mut argv = vec![
        "gh",
        "pr",
        "merge",
        number,
        "--merge",
        "--match-head-commit",
        &head,
    ];
    if let Some(email) = email.as_deref().filter(|email| !email.is_empty()) {
        argv.extend(["--author-email", email]);
    }
    successful(execute(project, &argv, false)?)?;
    Ok(())
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

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn select(pulls: serde_json::Value) -> Result<BranchPull> {
        pull_for_branch_with("123", HEAD.parse().unwrap(), &|argv| {
            // `gh pr view 123` would select pull request #123, not branch 123.
            assert_eq!(&argv[..5], ["gh", "pr", "list", "--head", "123"]);
            Ok(serde_json::to_vec(&pulls)?)
        })
    }

    fn pull(number: u64, state: &str, head: &str, cross: bool) -> serde_json::Value {
        serde_json::json!({"number":number, "state":state, "headRefOid":head, "isCrossRepository":cross})
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
