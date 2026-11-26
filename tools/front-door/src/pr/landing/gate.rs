//! Gate evidence: validation, command execution and pull-request CI waiting.
use super::{LandingError, model::*, ports::Forge};
use crate::pr::observer::{self, AwaitOutcome, Observation};
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
    #[allow(
        clippy::disallowed_methods,
        reason = "progress output writes to stderr at the gate boundary"
    )]
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
        evidence.reused_from_commit = Some(head.clone());
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

/// Execute a configured command plan in an isolated candidate checkout.
#[allow(
    clippy::disallowed_methods,
    reason = "gate command execution owns its wall-clock timeout, git boundary and stderr output"
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
    let checkout =
        crate::verify::DetachedCheckout::new(project, &ports, &candidate.commit, "land-gate")
            .map_err(|error| failed(format!("cannot create gate checkout: {error}")))?;
    let common =
        provenant_os::git::repository_path(project, provenant_os::git::RepositoryPath::CommonDir)
            .map_err(|error| failed(format!("cannot resolve gate repository: {error}")))?;
    let env = std::env::vars_os()
        .filter(|(key, _)| {
            key != "PROVENANT_BUILD_LOCAL_ROOT"
                && !["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"]
                    .iter()
                    .any(|name| {
                        key == name
                            && !definition
                                .env_passthrough
                                .iter()
                                .any(|allowed| allowed == name)
                    })
        })
        .chain(std::iter::once((
            "PROVENANT_BUILD_LOCAL_ROOT".into(),
            checkout.path.as_os_str().to_owned(),
        )))
        .collect();
    use provenant_run::build::{
        self, Job, JobKind, Origin, Output, Priority, SnapshotSource, Step,
    };
    if deadline <= std::time::Instant::now() {
        return Err(failed(format!(
            "gate command timed out after {}s: {}",
            definition.timeout_seconds, definition.commands[0]
        )));
    }
    let job = Job {
        id: format!(
            "gate-{}-{}",
            std::process::id(),
            provenant_os::clock::now_unix_millis()
        ),
        project_key: provenant_config::project_key_for(project)
            .unwrap_or_else(|| provenant_config::project_key(&common)),
        project: project.to_path_buf(),
        origin: Origin::Landing,
        owner: format!("landing-{}", std::process::id()),
        kind: JobKind::Gate {
            name: "commands".into(),
        },
        priority: Priority::Train,
        source: SnapshotSource::Commit {
            repo_common_dir: common,
            commit: candidate.commit.clone(),
        },
        verify_base: None,
        steps: definition
            .commands
            .iter()
            .map(|command| Step {
                cwd_rel: Default::default(),
                argv: vec!["sh".into(), "-c".into(), command.into()],
            })
            .collect(),
        stop_on_failure: true,
        env,
        output: Output::Capture {
            limit_bytes: 8 * 1024 * 1024,
        },
        timeout: Some(deadline.saturating_duration_since(std::time::Instant::now())),
        sandboxed: false,
        prelude: None,
    };
    let context = build::RunContext {
        state: state.to_path_buf(),
        home: std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| project.to_path_buf()),
        strategy: build::strategy(&ports.build).value,
        cancel: provenant_core::CancelToken::new(),
    };
    // The gate deadline includes admission, not just the spawned commands.
    let (finished, wait) = std::sync::mpsc::channel::<()>();
    let cancel = context.cancel.clone();
    let watchdog = std::thread::spawn(move || {
        if wait
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .is_err()
        {
            cancel.cancel();
        }
    });
    let execution = build::run(job, &context);
    let _ = finished.send(());
    let _ = watchdog.join();
    let expired = context.cancel.is_cancelled();
    if expired && execution.is_err() {
        return Err(failed(format!(
            "gate command timed out after {}s: {}",
            definition.timeout_seconds, definition.commands[0]
        )));
    }
    let execution =
        execution.map_err(|error| failed(format!("cannot execute gate command: {error}")))?;
    if let Some(capture) = &execution.captured {
        use std::io::Write;
        let _ = std::io::stderr().write_all(&capture.stdout);
        let _ = std::io::stderr().write_all(&capture.stderr);
    }
    if execution.outcome.timed_out || expired {
        let index = execution.step_exits.len().saturating_sub(1);
        let command = &definition.commands[index.min(definition.commands.len() - 1)];
        return Err(failed(format!(
            "gate command timed out after {}s: {command}",
            definition.timeout_seconds
        )));
    }
    let mut checks = Vec::new();
    let mut result = GateResult::Passed;
    for (command, exit) in definition.commands.iter().zip(&execution.step_exits) {
        let passed = exit.is_some_and(|exit| exit.code == Some(0));
        let verdict = if passed {
            GateResult::Passed
        } else {
            GateResult::Failed
        };
        checks.push(CheckEvidence {
            name: command.clone(),
            issuer: None,
            app_id: None,
            result: verdict,
        });
        if !passed {
            result = GateResult::Failed;
            break;
        }
    }
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
        reused_from_commit: None,
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
    // The same completion definition wait and status use decides the gate. A
    // terminal PR settles before a pending check, and contexts the project
    // declares as a review gate are not CI.
    let state = match forge.pull_request(pr.number)?.state {
        super::ports::PullRequestState::Open => "OPEN",
        super::ports::PullRequestState::Merged => "MERGED",
        super::ports::PullRequestState::Closed => "CLOSED",
    };
    let pending = ci_completion(
        state,
        &pr.head,
        &checks,
        &required,
        forge.review_gate_contexts(),
    ) == CiCompletion::Pending;
    // `always` waits for a run to appear; `affected` waits only for runs that exist.
    Ok(
        pending
            || (policy.mode == PrCiMode::Always && runs.iter().all(|run| run.checks.is_empty())),
    )
}

/// Require the saved check policy to pass on the PR's current head itself.
/// Queue landing uses this after candidate checks and again for each member
/// immediately before its match-head merge.
pub(super) fn require_delivery_queue_ci(
    forge: &mut dyn Forge,
    pr: &EligiblePr,
) -> Result<(), LandingError> {
    let required = match forge.required_ci_policy(&pr.base_branch) {
        Ok(Some(required)) => required,
        Ok(None) => {
            return Err(LandingError::Pending(format!(
                "required CI policy is unavailable for base {}",
                pr.base_branch
            )));
        }
        Err(error) => {
            return Err(LandingError::Pending(format!(
                "could not read required CI policy for base {}: {error}",
                pr.base_branch
            )));
        }
    };
    let observations = forge.checks(&pr.head).map_err(|error| {
        LandingError::Pending(format!(
            "could not observe CI for current pull request head {}: {error}",
            pr.head
        ))
    })?;
    let checks: Vec<_> = observations
        .iter()
        .flat_map(|observation| {
            observation.checks.iter().map(|check| CiCheck {
                head: observation.commit.clone(),
                name: check.name.clone(),
                bucket: match &check.result {
                    GateResult::Passed => "pass",
                    GateResult::Failed => "fail",
                    GateResult::Pending => "pending",
                    GateResult::Skipped | GateResult::Neutral => "skipping",
                    GateResult::Missing => "missing",
                }
                .into(),
                app: check.app_id,
            })
        })
        .collect();
    let required: Vec<_> = required
        .into_iter()
        .map(|check| Required {
            name: check.name,
            app: check.app_id,
        })
        .collect();
    match ci_completion(&pr.head, &checks, &required) {
        CiCompletion::Green => Ok(()),
        CiCompletion::Failed => Err(LandingError::UnmetGate(format!(
            "required CI failed on current pull request head {}",
            pr.head
        ))),
        CiCompletion::Pending | CiCompletion::Unavailable => Err(LandingError::Pending(format!(
            "required CI is absent, stale or inconclusive on current pull request head {}",
            pr.head
        ))),
    }
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
    pause: &dyn Fn(Duration),
    progress: &dyn Fn(u64) -> ProgressReporter,
) -> Result<(), LandingError> {
    let poll = Duration::from_secs(poll_seconds);
    let patience = Duration::from_secs(patience_seconds);
    for pr in prs {
        if policy.mode == PrCiMode::None {
            break;
        }
        let _progress = progress(pr.number);
        // The observer owns the cadence and the bound; this gate only decides
        // when a PR's required CI is outstanding and how a timeout is reported.
        let outcome = observer::await_ci(
            || {
                let interval = forge.observation_interval().unwrap_or(poll);
                match ci_outstanding(forge, pr, policy, required_names) {
                    Ok(true) => Ok(Observation::Pending(interval)),
                    Ok(false) => Ok(Observation::Settled),
                    Err(LandingError::RateLimited(message)) => {
                        provenant_records::warning::warn(
                            provenant_records::warning::Warning::new(
                                provenant_records::warning::GH_RATE_LIMITED,
                                format!("pull request #{} CI read skipped: {message}", pr.number),
                                provenant_records::warning::detected_source(
                                    provenant_records::warning::GH_RATE_LIMITED,
                                ),
                            )
                            .subject(format!("#{}", pr.number))
                            .once_per_change(),
                        );
                        Ok(Observation::Skipped(interval))
                    }
                    Err(error) => Err(error),
                }
            },
            patience,
            pause,
        )?;
        if let AwaitOutcome::TimedOut(waited) = outcome {
            return Err(LandingError::Pending(format!(
                "warning: pull request #{} CI is still running after {}s; resubmit to resume",
                pr.number,
                waited.as_secs()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::landing::fakes::FakeForge;
    use crate::pr::landing::ports::{PullRequestInfo, PullRequestState};
    use crate::pr::landing::test_support::LandingFixture;
    use std::cell::Cell;

    fn open_info(number: u64, head: &str) -> PullRequestInfo {
        PullRequestInfo {
            number,
            head: head.into(),
            base_branch: "main".into(),
            author: "author".into(),
            state: PullRequestState::Open,
            merged_commit: None,
            stack: None,
            draft: false,
        }
    }

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
        forge.pull_requests.insert(1, open_info(1, "h1"));
        forge
            .check_runs
            .insert("h1".into(), vec![evidence(GateResult::Pending, None)]);
        forge
    }
    #[test]
    fn required_review_gate_does_not_hold_landing_ci_pending() {
        use crate::pr::landing::ports::RequiredCiCheck;

        let mut forge = FakeForge::default();
        forge.pull_requests.insert(1, open_info(1, "h1"));
        forge.required_ci_policies.insert(
            "main".into(),
            Some(vec![RequiredCiCheck {
                name: "verdict".into(),
                app_id: None,
            }]),
        );
        forge.review_gate_contexts = vec!["verdict".into()];
        let mut build = evidence(GateResult::Passed, None);
        build.checks[0].name = "build".into();
        let mut verdict = evidence(GateResult::Pending, None);
        verdict.checks[0].name = "verdict".into();
        forge.check_runs.insert("h1".into(), vec![build, verdict]);

        let result = require_delivery_queue_ci(&mut forge, &pr(1, "h1"));

        assert!(result.is_ok(), "a review gate listed as required is not CI");
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
        forge.pull_requests.insert(1, open_info(1, "h1"));
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
        let repository = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = provenant_os::fixture::git_command()
                .arg("-C")
                .arg(repository.path())
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        std::fs::write(repository.path().join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-qm", "fixture"]);
        let mut cand = candidate(1);
        cand.commit = git(&["rev-parse", "HEAD"]);
        cand.tree = git(&["rev-parse", "HEAD^{tree}"]);
        let mut def = definition();
        def.kind = GateKind::Commands;
        def.executor = Some("local".into());
        def.commands = vec!["test -z \"$GH_TOKEN$GITHUB_TOKEN$GH_ENTERPRISE_TOKEN\"".into()];
        assert_eq!(
            execute_commands(repository.path(), repository.path(), &cand, &def)
                .unwrap()
                .result,
            GateResult::Passed
        );
        def.env_passthrough = vec!["GH_TOKEN".into()];
        def.commands = vec![
            "test \"$GH_TOKEN\" = gate-test && test -z \"$GITHUB_TOKEN$GH_ENTERPRISE_TOKEN\""
                .into(),
        ];
        assert_eq!(
            execute_commands(repository.path(), repository.path(), &cand, &def)
                .unwrap()
                .result,
            GateResult::Passed
        );
        def.env_passthrough.clear();
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
            def.commands = vec![command];
            let result =
                execute_commands(repository.path(), repository.path(), &cand, &def).unwrap();
            assert_eq!(result.result == GateResult::Passed, expected_success);
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
        assert_eq!(got.commit, cand.commit);
        assert_eq!(got.tree, cand.tree);
        assert_eq!(got.definition_digest, def.digest);
        assert_eq!(got.checks.len(), 1);
        assert_eq!(got.checks[0].name, def.commands[0]);
        assert_eq!(got.checks[0].result, GateResult::Passed);
        assert_eq!(got.checks[0].issuer.as_deref(), Some("local"));
        assert!(got.duration_seconds.is_some());
        def.commands = vec!["test -f f".into(), "test -f missing".into(), "false".into()];
        let failed = execute_commands(&dir, &dir, &cand, &def).unwrap();
        assert_eq!(failed.result, GateResult::Failed);
        assert_eq!(failed.checks.len(), 2);
        assert_eq!(failed.checks[0].result, GateResult::Passed);
        assert_eq!(failed.checks[1].result, GateResult::Failed);
        assert_eq!(failed.checks[1].name, def.commands[1]);
        assert_eq!(failed.checks[1].issuer.as_deref(), Some("local"));
        assert_eq!(failed.commit, cand.commit);
        assert_eq!(failed.tree, cand.tree);
        assert_eq!(failed.definition_digest, def.digest);
        // #3790: the chair owns one warm-check strategy. The gate runs under
        // `slots`; the retired command/lane wrapper strategies no longer exist,
        // so no configured wrapper transforms a step.
        let ports = provenant_config::PortTable::read_for(Some(&dir), &dir).unwrap();
        assert_eq!(
            provenant_run::build::strategy(&ports.build).value.name,
            provenant_run::build::StrategyName::Slots
        );
        // The gate deadline covers admission and execution and still cleans up.
        def.commands = vec!["sleep 30".into()];
        def.timeout_seconds = 1;
        assert!(execute_commands(&dir, &dir, &cand, &def).is_err());
        let branch = format!("land-gate-{}-{}", &cand.commit[..12], std::process::id());
        let checkout = dir.join(ports.managed_writer_path(&branch).unwrap());
        let leftover = std::fs::read_dir(checkout.parent().unwrap())
            .unwrap()
            .count();
        assert_eq!(leftover, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
