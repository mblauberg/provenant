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

mod contributions;
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
/// The pull request whose head is `branch`, as `gh pr view` selects it.
fn number_for_branch(project: &Path, branch: &str) -> Result<u64> {
    #[derive(Deserialize)]
    struct PullRequest {
        number: u64,
    }
    let bytes = successful(execute(
        project,
        &["gh", "pr", "view", branch, "--json", "number"],
        false,
    )?)?;
    Ok(serde_json::from_slice::<PullRequest>(&bytes)?.number)
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
