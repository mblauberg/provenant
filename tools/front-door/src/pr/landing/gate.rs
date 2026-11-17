//! Gate evidence: validation, command execution and pull-request CI waiting.
use super::{LandingError, model::*, ports::Forge};
use crate::pr::status::{CiCheck, CiCompletion, Required, ci_completion};
use provenant_cli::domains::{GateKind, PrCiMode, PrCiSettings};
use std::time::{Duration, Instant};

/// Reports independently of forge reads and stops promptly when the wait ends.
pub(super) struct ProgressReporter {
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    state: std::sync::Arc<std::sync::Mutex<String>>,
}
impl ProgressReporter {
    pub(super) fn new(subject: String, interval: Duration) -> Self {
        Self::with_sink(
            subject,
            interval,
            Box::new(|line| {
                use std::io::Write;
                let _ = writeln!(std::io::stderr().lock(), "{line}");
            }),
        )
    }

    #[allow(clippy::disallowed_methods, reason = "wall-clock progress boundary")]
    fn with_sink(subject: String, interval: Duration, sink: Box<dyn Fn(String) + Send>) -> Self {
        let started = Instant::now();
        sink(format!("land: waiting on {subject}, state pending"));
        let (stop, receiver) = std::sync::mpsc::channel();
        let state = std::sync::Arc::new(std::sync::Mutex::new("pending".to_owned()));
        let worker_state = std::sync::Arc::clone(&state);
        let interval = interval.max(Duration::from_millis(1));
        let worker = std::thread::spawn(move || {
            while matches!(
                receiver.recv_timeout(interval),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                let state = worker_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                sink(format!(
                    "land: waiting on {subject} for {}s, state {state}",
                    started.elapsed().as_secs()
                ));
            }
        });
        Self {
            stop,
            worker: Some(worker),
            state,
        }
    }

    pub(super) fn set_state(&self, state: &GateResult) {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = format!("{state:?}");
    }
}
impl Drop for ProgressReporter {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn failed(message: impl Into<String>) -> LandingError {
    LandingError::Failed(message.into())
}

fn check_issues(definition: &GateDefinition, evidence: &GateEvidence) -> Vec<(String, GateResult)> {
    let names = if definition.kind == GateKind::Commands {
        &definition.commands
    } else {
        &definition.checks
    };
    names
        .iter()
        .filter_map(|name| {
            let named: Vec<_> = evidence
                .checks
                .iter()
                .filter(|check| check.name == *name)
                .collect();
            let matching: Vec<_> = named
                .iter()
                .filter(|check| {
                    definition.kind == GateKind::Commands
                        || definition
                            .issuer
                            .as_ref()
                            .is_none_or(|issuer| check.issuer.as_ref() == Some(issuer))
                })
                .collect();
            if matching.is_empty() {
                return Some((
                    format!(
                        "{name}: {}",
                        if named.is_empty() {
                            "missing"
                        } else {
                            "wrong issuer"
                        }
                    ),
                    GateResult::Missing,
                ));
            }
            let issues: Vec<_> = matching
                .iter()
                .filter(|check| check.result != GateResult::Passed)
                .collect();
            if issues.is_empty() {
                None
            } else {
                let result = if issues.iter().any(|check| {
                    matches!(
                        check.result,
                        GateResult::Missing | GateResult::Skipped | GateResult::Neutral
                    )
                }) {
                    GateResult::Missing
                } else if issues
                    .iter()
                    .any(|check| check.result == GateResult::Failed)
                {
                    GateResult::Failed
                } else {
                    GateResult::Pending
                };
                Some((
                    format!(
                        "{name}: {}",
                        issues
                            .iter()
                            .map(|check| format!("{:?}", check.result).to_lowercase())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    result,
                ))
            }
        })
        .collect()
}

fn evaluate_checks(
    definition: &GateDefinition,
    issues: &[(String, GateResult)],
) -> Result<GateResult, LandingError> {
    if definition.checks.is_empty() {
        return Err(failed("unmet gate: no required checks are configured"));
    }
    if issues
        .iter()
        .any(|(_, state)| *state == GateResult::Missing)
    {
        return Err(failed(format!(
            "unmet gate: {}",
            issues
                .iter()
                .map(|(message, _)| message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    Ok(
        if issues.iter().any(|(_, state)| *state == GateResult::Failed) {
            GateResult::Failed
        } else if !issues.is_empty() {
            GateResult::Pending
        } else {
            GateResult::Passed
        },
    )
}

fn evaluate_commands(
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> Result<GateResult, LandingError> {
    if definition.commands.is_empty() {
        return Err(failed("unmet gate: no gate commands are configured"));
    }
    if let Some(executor) = &definition.executor
        && evidence.issuer.as_ref() != Some(executor)
    {
        return Err(failed(format!(
            "unmet gate: evidence was not produced by executor {executor}"
        )));
    }
    for item in &definition.coverage {
        if !evidence.coverage.contains(item) {
            return Err(failed(format!(
                "unmet gate: coverage {item} is missing from the evidence"
            )));
        }
    }
    match evidence.result {
        GateResult::Passed | GateResult::Failed | GateResult::Pending => {
            Ok(evidence.result.clone())
        }
        GateResult::Missing | GateResult::Skipped | GateResult::Neutral => Err(failed(
            format!("unmet gate: command plan result is {:?}", evidence.result).to_lowercase(),
        )),
    }
}

/// Evaluate provenance, binding and check diagnostics once for the driver.
/// Evidence already on record for the candidate's tree: a member head whose
/// tree is the candidate's and whose required checks passed. The evidence is
/// rebound to the candidate and names the head it came from.
pub fn reusable_evidence(
    forge: &mut dyn super::ports::Forge,
    candidate: &Candidate,
    definition: &GateDefinition,
) -> Option<GateEvidence> {
    if definition.kind != GateKind::Checks {
        return None;
    }
    for step in &candidate.steps {
        let head = &step.pr.head;
        let Some(mut observed) = forge
            .checks(head)
            .ok()
            .and_then(|runs| runs.into_iter().next())
        else {
            continue;
        };
        if observed.commit != *head || observed.tree != candidate.tree {
            continue;
        }
        observed.commit = candidate.commit.clone();
        let Ok(mut evidence) = super::adapters::bind_evidence(candidate, definition, &observed)
        else {
            continue;
        };
        if evidence.result != GateResult::Passed {
            continue;
        }
        evidence
            .extra
            .insert("reused_from_commit".into(), head.clone().into());
        return Some(evidence);
    }
    None
}

pub fn assess_evidence(
    candidate: &Candidate,
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> (Result<GateResult, LandingError>, String) {
    let issues = check_issues(definition, evidence);
    let names = if definition.kind == GateKind::Commands {
        &definition.commands
    } else {
        &definition.checks
    };
    let wrong_tree = evidence.commit != candidate.commit || evidence.tree != candidate.tree;
    let unmet = if wrong_tree {
        names
            .iter()
            .map(|name| format!("{name}: evidence is for another commit or tree"))
            .collect::<Vec<_>>()
            .join("; ")
    } else {
        issues
            .iter()
            .map(|(message, _)| message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    };
    let result = if wrong_tree {
        Err(failed(format!(
            "gate evidence is bound to commit {} tree {}, not candidate {} tree {}",
            evidence.commit, evidence.tree, candidate.commit, candidate.tree
        )))
    } else if evidence.definition_digest != definition.digest {
        Err(failed(
            "gate evidence was produced for a different gate definition",
        ))
    } else {
        match definition.kind {
            GateKind::Checks => evaluate_checks(definition, &issues),
            GateKind::Commands => evaluate_commands(definition, evidence),
        }
    };
    (result, unmet)
}

/// Remove a gate checkout through git only; never delete a path directly.
fn remove_checkout(project: &std::path::Path, checkout: &std::path::Path) {
    #[allow(clippy::disallowed_methods, reason = "git boundary for gate checkout")]
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["worktree", "remove", "--force"])
        .arg(checkout)
        .output();
    #[allow(clippy::disallowed_methods, reason = "git boundary for gate checkout")]
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["worktree", "prune"])
        .output();
}

enum Run {
    Passed,
    Failed,
    TimedOut,
}

#[allow(
    clippy::disallowed_methods,
    reason = "bound each command to the gate deadline"
)]
fn run_command(
    checkout: &std::path::Path,
    command: &str,
    deadline: std::time::Instant,
    passthrough: &[String],
) -> Result<Run, LandingError> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let timeout = deadline.saturating_duration_since(std::time::Instant::now());
    if timeout.is_zero() {
        return Ok(Run::TimedOut);
    }
    let argv = ["sh".into(), "-c".into(), command.into()];
    let env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            !["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"]
                .iter()
                .any(|name| key == name && !passthrough.iter().any(|allowed| allowed == name))
        })
        .collect();
    let mut options = ExecOptions::new(&argv, checkout, timeout);
    options.env = Some(&env);
    options.cleanup_process_group = true;
    let outcome = SystemExecutor::new(1)
        .and_then(|executor| executor.execute(options))
        .map_err(|error| failed(format!("cannot execute gate command: {error:?}")))?;
    use std::io::Write;
    let _ = std::io::stderr().write_all(&outcome.stdout.bytes);
    let _ = std::io::stderr().write_all(&outcome.stderr.bytes);
    Ok(if outcome.timed_out {
        Run::TimedOut
    } else if outcome.termination == Termination::Exit(0) {
        Run::Passed
    } else {
        Run::Failed
    })
}

/// Execute a configured command plan in an isolated candidate checkout.
#[allow(
    clippy::disallowed_methods,
    reason = "gate command execution owns its wall-clock timeout and git boundary"
)]
pub fn execute_commands(
    project: &std::path::Path,
    state: &std::path::Path,
    candidate: &Candidate,
    definition: &GateDefinition,
) -> Result<GateEvidence, LandingError> {
    if definition.commands.is_empty() {
        return Err(failed("unmet gate: no gate commands are configured"));
    }
    if definition
        .executor
        .as_deref()
        .is_some_and(|executor| executor != "local")
    {
        return Err(failed(
            "unsupported command gate executor; only local is available",
        ));
    }
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_secs(definition.timeout_seconds);
    let ports = provenant_config::PortTable::read_for(Some(project), state)
        .map_err(|error| failed(format!("cannot resolve worktree layout port: {error}")))?;
    let short: String = candidate.commit.chars().take(12).collect();
    let branch = format!("land-gate-{short}-{}", std::process::id());
    let relative = ports
        .managed_writer_path(&branch)
        .map_err(|error| failed(format!("cannot resolve gate checkout path: {error}")))?;
    let checkout = project.join(relative);
    let root = checkout
        .parent()
        .ok_or_else(|| failed("configured gate checkout has no parent"))?;
    if checkout.exists() {
        return Err(failed("gate checkout already exists"));
    }
    std::fs::create_dir_all(root)
        .map_err(|error| failed(format!("cannot prepare gate checkout directory: {error}")))?;
    let added = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["worktree", "add", "--detach"])
        .arg(&checkout)
        .arg(&candidate.commit)
        .output()
        .map_err(|error| failed(format!("cannot start git: {error}")))?;
    if !added.status.success() {
        return Err(failed(format!(
            "cannot create gate checkout: {}",
            String::from_utf8_lossy(&added.stderr).trim()
        )));
    }
    let mut checks = Vec::new();
    let mut result = GateResult::Passed;
    let mut outcome = Ok(());
    for command in &definition.commands {
        match run_command(&checkout, command, deadline, &definition.env_passthrough) {
            Ok(Run::Passed) => checks.push(CheckEvidence {
                name: command.clone(),
                issuer: None,
                result: GateResult::Passed,
            }),
            Ok(Run::Failed) => {
                checks.push(CheckEvidence {
                    name: command.clone(),
                    issuer: None,
                    result: GateResult::Failed,
                });
                result = GateResult::Failed;
                break;
            }
            Ok(Run::TimedOut) => {
                outcome = Err(failed(format!(
                    "gate command timed out after {}s: {command}",
                    definition.timeout_seconds
                )));
                break;
            }
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    remove_checkout(project, &checkout);
    outcome?;
    let executor = "local".to_owned();
    for check in &mut checks {
        check.issuer = Some(executor.clone());
    }
    Ok(GateEvidence {
        commit: candidate.commit.clone(),
        tree: candidate.tree.clone(),
        definition_digest: definition.digest.clone(),
        checks,
        issuer: Some(executor),
        coverage: definition.coverage.clone(),
        result,
        pool: definition.pool.clone(),
        duration_seconds: Some(started.elapsed().as_secs()),
        extra: Default::default(),
    })
}

/// Whether a pull request's own CI run is still outstanding under `policy`.
fn ci_outstanding(
    forge: &mut dyn Forge,
    pr: &EligiblePr,
    policy: &PrCiSettings,
    required_names: &[String],
) -> Result<bool, LandingError> {
    let wait = match policy.mode {
        PrCiMode::None => return Ok(false),
        PrCiMode::OptIn => {
            let label = policy.label.as_deref().unwrap_or_default();
            !label.is_empty() && forge.labels(pr.number)?.iter().any(|have| have == label)
        }
        PrCiMode::Always | PrCiMode::Affected => true,
    };
    if !wait {
        return Ok(false);
    }
    let runs = forge.checks(&pr.head)?;
    if runs.is_empty() {
        return Ok(policy.mode == PrCiMode::Always);
    }
    if runs.iter().any(|run| run.commit != pr.head) {
        return Ok(true);
    }
    let checks: Vec<_> = runs
        .iter()
        .flat_map(|run| {
            if run.checks.is_empty() {
                vec![CiCheck {
                    head: run.commit.clone(),
                    name: String::new(),
                    bucket: match &run.result {
                        GateResult::Passed => "pass",
                        GateResult::Failed => "fail",
                        GateResult::Pending | GateResult::Missing => "pending",
                        GateResult::Skipped | GateResult::Neutral => "skipping",
                    }
                    .into(),
                    app: None,
                }]
            } else {
                run.checks
                    .iter()
                    .map(|check| CiCheck {
                        head: run.commit.clone(),
                        name: check.name.clone(),
                        bucket: match &check.result {
                            GateResult::Passed => "pass",
                            GateResult::Failed => "fail",
                            GateResult::Pending | GateResult::Missing => "pending",
                            GateResult::Skipped | GateResult::Neutral => "skipping",
                        }
                        .into(),
                        app: None,
                    })
                    .collect()
            }
        })
        .collect();
    let required: Vec<_> = required_names
        .iter()
        .map(|name| Required {
            name: name.clone(),
            app: None,
        })
        .collect();
    let pending = ci_completion(&pr.head, &checks, &required) == CiCompletion::Pending;
    // `always` waits for a run to appear; `affected` waits only for runs that exist.
    Ok(
        pending
            || (policy.mode == PrCiMode::Always && runs.iter().all(|run| run.checks.is_empty())),
    )
}

/// Follow the configured pull-request CI evidence policy without bypassing forge requirements.
///
/// Waiting never alters what the forge requires: required checks remain enforced
/// by the forge and by the candidate gate, so skipping an optional wait loses nothing.
/// The wait ends at `patience_seconds`, the configured gate bound, as
/// [`LandingError::Pending`]: the landing owner records the member as pending
/// and a later submission resumes it. A rate-limited read skips one poll.
pub fn await_pr_ci(
    forge: &mut dyn Forge,
    prs: &[EligiblePr],
    policy: &PrCiSettings,
    required_names: &[String],
    poll_seconds: u64,
    patience_seconds: u64,
    progress_interval: Duration,
) -> Result<(), LandingError> {
    #[allow(clippy::disallowed_methods, reason = "polling boundary")]
    await_pr_ci_with(
        forge,
        prs,
        policy,
        required_names,
        (poll_seconds, patience_seconds),
        &std::thread::sleep,
        &|number| ProgressReporter::new(format!("pull request #{number} CI"), progress_interval),
    )
}

fn await_pr_ci_with(
    forge: &mut dyn Forge,
    prs: &[EligiblePr],
    policy: &PrCiSettings,
    required_names: &[String],
    (poll_seconds, patience_seconds): (u64, u64),
    sleep: &dyn Fn(Duration),
    progress: &dyn Fn(u64) -> ProgressReporter,
) -> Result<(), LandingError> {
    let poll = Duration::from_secs(poll_seconds);
    let patience = Duration::from_secs(patience_seconds);
    let mut waited = Duration::ZERO;
    for pr in prs {
        if policy.mode == PrCiMode::None {
            break;
        }
        let _progress = progress(pr.number);
        loop {
            match ci_outstanding(forge, pr, policy, required_names) {
                Ok(false) => break,
                Ok(true) => {}
                Err(LandingError::RateLimited(message)) => {
                    use std::io::Write;
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "warning: pull request #{} CI read skipped: {message}",
                        pr.number
                    );
                }
                Err(error) => return Err(error),
            }
            let remaining = patience.saturating_sub(waited);
            if remaining.is_zero() {
                return Err(LandingError::Pending(format!(
                    "warning: pull request #{} CI is still running after {}s; resubmit to resume",
                    pr.number,
                    waited.as_secs()
                )));
            }
            let pause = forge.observation_interval().unwrap_or(poll).min(remaining);
            sleep(pause);
            waited += pause;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::landing::fakes::FakeForge;
    use crate::pr::landing::test_support::LandingFixture;
    use std::cell::Cell;

    fn validate_evidence(
        candidate: &Candidate,
        definition: &GateDefinition,
        evidence: &GateEvidence,
    ) -> Result<GateResult, LandingError> {
        assess_evidence(candidate, definition, evidence).0
    }
    fn candidate(steps: usize) -> Candidate {
        LandingFixture::candidate_with_steps(steps)
    }
    fn definition() -> GateDefinition {
        LandingFixture::definition()
    }
    fn evidence(result: GateResult, issuer: Option<&str>) -> GateEvidence {
        LandingFixture::evidence("c", result, issuer)
    }
    fn pr(number: u64, head: &str) -> EligiblePr {
        LandingFixture::pr(number, head)
    }
    #[test]
    fn gate_check_results_follow_the_declared_result_states() {
        let cases = [
            (Some(GateResult::Passed), Some(GateResult::Passed)),
            (Some(GateResult::Failed), Some(GateResult::Failed)),
            (Some(GateResult::Pending), Some(GateResult::Pending)),
            (Some(GateResult::Skipped), None),
            (Some(GateResult::Neutral), None),
            (Some(GateResult::Missing), None),
            (None, None),
        ];
        for (observed, expected) in cases {
            let mut evidence =
                LandingFixture::evidence("c", observed.clone().unwrap_or(GateResult::Passed), None);
            if observed.is_none() {
                evidence.checks.clear();
            }
            let result = validate_evidence(
                &LandingFixture::candidate_with_steps(1),
                &LandingFixture::definition(),
                &evidence,
            );
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), expected),
                None => assert!(matches!(result, Err(LandingError::Failed(_)))),
            }
        }
    }
    #[test]
    fn unexpected_issuer_does_not_satisfy_a_pinned_gate() {
        let mut pinned = LandingFixture::definition();
        pinned.issuer = Some("15368".into());
        let wrong = LandingFixture::evidence("c", GateResult::Passed, Some("1"));
        assert!(validate_evidence(&candidate(1), &pinned, &wrong).is_err());
        let right = LandingFixture::evidence("c", GateResult::Passed, Some("15368"));
        assert_eq!(
            validate_evidence(&candidate(1), &pinned, &right).unwrap(),
            GateResult::Passed
        );
    }
    #[test]
    fn evidence_for_another_commit_tree_or_definition_is_rejected() {
        let mut other = LandingFixture::evidence("c", GateResult::Passed, None);
        other.commit = "x".into();
        assert!(validate_evidence(&candidate(1), &definition(), &other).is_err());
        let mut other = LandingFixture::evidence("c", GateResult::Passed, None);
        other.tree = "x".into();
        assert!(validate_evidence(&candidate(1), &definition(), &other).is_err());
        let mut other = LandingFixture::evidence("c", GateResult::Passed, None);
        other.definition_digest = "x".into();
        assert!(validate_evidence(&candidate(1), &definition(), &other).is_err());
    }
    #[test]
    fn command_gate_reports_the_failed_command() {
        let mut def = LandingFixture::definition();
        def.kind = GateKind::Commands;
        def.commands = vec!["cargo test --example fixture".into()];
        let mut red = LandingFixture::evidence("c", GateResult::Failed, None);
        red.result = GateResult::Failed;
        red.checks[0].name = def.commands[0].clone();
        let (result, unmet) = assess_evidence(&candidate(1), &def, &red);
        assert_eq!(result.unwrap(), GateResult::Failed);
        assert!(unmet.contains(&def.commands[0]));
    }

    #[test]
    fn command_gate_checks_executor_coverage_and_result() {
        let mut def = definition();
        def.kind = GateKind::Commands;
        def.commands = vec!["true".into()];
        def.executor = Some("pool".into());
        def.coverage = vec!["unit".into()];
        let mut good = evidence(GateResult::Passed, None);
        good.issuer = Some("pool".into());
        good.coverage = vec!["unit".into()];
        assert_eq!(
            validate_evidence(&candidate(1), &def, &good).unwrap(),
            GateResult::Passed
        );
        let mut wrong = good.clone();
        wrong.issuer = Some("other".into());
        assert!(validate_evidence(&candidate(1), &def, &wrong).is_err());
        let mut uncovered = good.clone();
        uncovered.coverage.clear();
        assert!(validate_evidence(&candidate(1), &def, &uncovered).is_err());
        let mut red = good.clone();
        red.result = GateResult::Failed;
        assert_eq!(
            validate_evidence(&candidate(1), &def, &red).unwrap(),
            GateResult::Failed
        );
        let mut skipped = good;
        skipped.result = GateResult::Skipped;
        assert!(validate_evidence(&candidate(1), &def, &skipped).is_err());
    }
    fn pending_forge() -> FakeForge {
        let mut forge = FakeForge::default();
        forge
            .check_runs
            .insert("h1".into(), vec![evidence(GateResult::Pending, None)]);
        forge
    }
    // Spec: docs/specs/cli.md#landing-adds-no-merge-authority
    #[test]
    fn pending_pr_ci_reports_repeatedly_while_waiting() {
        let (tx, rx) = std::sync::mpsc::channel();
        let progress = |number| {
            ProgressReporter::with_sink(
                format!("pull request #{number} CI"),
                Duration::from_millis(1),
                Box::new({
                    let tx = tx.clone();
                    move |line| {
                        tx.send(line).unwrap();
                    }
                }),
            )
        };
        let mut forge = pending_forge();
        let first = Cell::new(true);
        let result = await_pr_ci_with(
            &mut forge,
            &[pr(1, "h1")],
            &PrCiSettings {
                mode: PrCiMode::Always,
                label: None,
            },
            &[],
            (1, 2),
            &|_| {
                if first.replace(false) {
                    rx.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
            },
            &progress,
        );
        assert!(matches!(result, Err(LandingError::Pending(_))));
    }
    fn waits(mode: PrCiMode, label: Option<&str>, labelled: bool, runs: bool) -> u32 {
        let mut forge = pending_forge();
        if !runs {
            forge.check_runs.clear();
        }
        if labelled {
            forge.labels.insert(1, vec!["full-ci".into()]);
        }
        let policy = PrCiSettings {
            mode,
            label: label.map(str::to_owned),
        };
        let slept = Cell::new(0u32);
        // The run never resolves, so the wait ends only at the limit.
        let result = await_pr_ci_with(
            &mut forge,
            &[pr(1, "h1")],
            &policy,
            &[],
            (3600, 7200),
            &|_| slept.set(slept.get() + 1),
            &|number| ProgressReporter::new(format!("PR #{number}"), Duration::from_secs(60)),
        );
        if slept.get() > 0 {
            // An unfinished run is pending at the bound, never a failure.
            assert!(matches!(result, Err(LandingError::Pending(_))));
        } else {
            assert!(result.is_ok());
        }
        slept.get()
    }
    #[test]
    fn pr_ci_modes_decide_whether_to_wait() {
        assert!(waits(PrCiMode::Always, None, false, true) > 0);
        assert!(waits(PrCiMode::Always, None, false, false) > 0);
        assert_eq!(waits(PrCiMode::None, None, false, true), 0);
        assert_eq!(waits(PrCiMode::OptIn, Some("full-ci"), false, true), 0);
        assert!(waits(PrCiMode::OptIn, Some("full-ci"), true, true) > 0);
        assert!(waits(PrCiMode::Affected, None, false, true) > 0);
        assert_eq!(waits(PrCiMode::Affected, None, false, false), 0);
    }

    #[test]
    fn pr_ci_wait_uses_adapter_cadence_and_bounds_it_by_patience() {
        let settings = provenant_cli::wait::Settings {
            pr_interval_ms: 37,
            ..Default::default()
        };
        let fast = Duration::from_millis(settings.pr_interval_ms);
        let mut forge = pending_forge();
        forge.next_observation_interval = Some(fast);
        let pauses = std::cell::RefCell::new(Vec::new());
        let result = await_pr_ci_with(
            &mut forge,
            &[pr(1, "h1")],
            &PrCiSettings {
                mode: PrCiMode::Always,
                label: None,
            },
            &[],
            (3, 1),
            &|pause| pauses.borrow_mut().push(pause),
            &|number| ProgressReporter::new(format!("PR #{number}"), Duration::from_secs(60)),
        );
        assert!(matches!(result, Err(LandingError::Pending(_))));
        let pauses = pauses.borrow();
        assert!(pauses.iter().all(|pause| *pause <= fast));
        assert!(pauses.iter().copied().sum::<Duration>() <= Duration::from_secs(1));
    }

    #[test]
    fn configured_check_that_has_not_reported_keeps_land_waiting() {
        let mut forge = FakeForge::default();
        let mut passing_other_check = evidence(GateResult::Passed, None);
        passing_other_check.commit = "h1".into();
        passing_other_check.checks[0].name = "optional".into();
        forge
            .check_runs
            .insert("h1".into(), vec![passing_other_check]);
        let policy = PrCiSettings {
            mode: PrCiMode::Always,
            label: None,
        };
        let required = vec!["ci-status".to_owned()];
        let slept = Cell::new(0_u32);
        let result = await_pr_ci_with(
            &mut forge,
            &[pr(1, "h1")],
            &policy,
            &required,
            (3600, 7200),
            &|_| slept.set(slept.get() + 1),
            &|number| ProgressReporter::new(format!("PR #{number}"), Duration::from_secs(60)),
        );
        assert!(matches!(result, Err(LandingError::Pending(_))));
        assert!(slept.get() > 0);
    }

    #[test]
    #[cfg(unix)]
    #[allow(
        clippy::disallowed_methods,
        reason = "exercise command process isolation"
    )]
    fn command_scrubs_credentials_and_cleans_background_on_success() {
        use std::process::Command;
        const MARKER: &str = "PROVENANT_GATE_HYGIENE_CHILD";
        if std::env::var_os(MARKER).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "pr::landing::gate::tests::command_scrubs_credentials_and_cleans_background_on_success", "--nocapture"])
                .env(MARKER, "1").env("GH_TOKEN", "gate-test").env("GITHUB_TOKEN", "gate-test").env("GH_ENTERPRISE_TOKEN", "gate-test").output().unwrap();
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
                "child did not run the hygiene test"
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        assert!(matches!(
            run_command(
                std::path::Path::new("."),
                "test -z \"$GH_TOKEN$GITHUB_TOKEN$GH_ENTERPRISE_TOKEN\"",
                deadline,
                &[]
            )
            .unwrap(),
            Run::Passed
        ));
        assert!(matches!(
            run_command(
                std::path::Path::new("."),
                "test \"$GH_TOKEN\" = gate-test && test -z \"$GITHUB_TOKEN$GH_ENTERPRISE_TOKEN\"",
                deadline,
                &["GH_TOKEN".into()]
            )
            .unwrap(),
            Run::Passed
        ));
        let dir = tempfile::tempdir().unwrap();
        let wait_for_marker = |marker: &std::path::Path, timeout: Duration| {
            provenant_test_support::wait_for(
                "gate background marker",
                timeout,
                || marker.exists(),
                |exists| *exists,
            )
            .is_ok()
        };
        let control_trigger = dir.path().join("control-trigger");
        let control_marker = dir.path().join("control-marker");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "while [ ! -e '{}' ]; do sleep 0.01; done; echo alive > '{}'",
                control_trigger.display(),
                control_marker.display()
            ))
            .spawn()
            .unwrap();
        std::fs::write(&control_trigger, b"").unwrap();
        assert!(
            wait_for_marker(&control_marker, Duration::from_secs(2)),
            "live child did not write its marker after the trigger"
        );
        assert!(child.wait().unwrap().success());

        for (suffix, expected_success) in [("0", true), ("1", false)] {
            let trigger_path = dir.path().join(format!("trigger-{suffix}"));
            let marker_path = dir.path().join(format!("background-{suffix}"));
            let command = format!(
                "(while [ ! -e '{}' ]; do sleep 0.01; done; echo alive > '{}') & test -z \"$GH_TOKEN$GITHUB_TOKEN$GH_ENTERPRISE_TOKEN\"; exit {suffix}",
                trigger_path.display(),
                marker_path.display()
            );
            let result = run_command(dir.path(), &command, deadline, &[]).unwrap();
            assert!(matches!(result, Run::Passed) == expected_success);
            std::fs::write(&trigger_path, b"").unwrap();
            assert!(
                !wait_for_marker(&marker_path, Duration::from_secs(1)),
                "background process outlived command completion"
            );
        }
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "test builds a scratch repository"
    )]
    fn commands_run_in_an_isolated_checkout_and_clean_up() {
        let dir = std::env::temp_dir().join(format!("gate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = provenant_os::fixture::git_command()
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-qm", "c"]);
        let commit = git(&["rev-parse", "HEAD"]);
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        let mut cand = candidate(1);
        cand.commit = commit;
        cand.tree = tree;
        let mut def = definition();
        def.kind = GateKind::Commands;
        def.commands = vec!["test -f f".into()];
        def.executor = Some("local".into());
        let got = execute_commands(&dir, &dir, &cand, &def).unwrap();
        assert_eq!(got.result, GateResult::Passed);
        assert_eq!(got.issuer.as_deref(), Some("local"));
        assert!(got.duration_seconds.is_some());
        def.commands = vec!["test -f missing".into()];
        assert_eq!(
            execute_commands(&dir, &dir, &cand, &def).unwrap().result,
            GateResult::Failed
        );
        def.commands = vec!["sleep 30".into()];
        def.timeout_seconds = 1;
        assert!(execute_commands(&dir, &dir, &cand, &def).is_err());
        let ports = provenant_config::PortTable::read_for(Some(&dir), &dir).unwrap();
        let branch = format!("land-gate-{}-{}", &cand.commit[..12], std::process::id());
        let checkout = dir.join(ports.managed_writer_path(&branch).unwrap());
        let leftover = std::fs::read_dir(checkout.parent().unwrap())
            .unwrap()
            .count();
        assert_eq!(leftover, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
