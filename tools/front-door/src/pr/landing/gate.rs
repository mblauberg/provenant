//! Gate evidence: validation, command execution and pull-request CI waiting.
use super::{LandingError, model::*, ports::Forge};
use provenant_cli::domains::{GateKind, PrCiMode, PrCiSettings};
use std::time::Duration;

/// Upper bound on waiting for one pull request's own CI run.
const PR_CI_WAIT_LIMIT: Duration = Duration::from_secs(6 * 60 * 60);

fn failed(message: impl Into<String>) -> LandingError {
    LandingError::Failed(message.into())
}

/// Evaluate the gate named by `definition` against `evidence`, ignoring commit binding.
fn evaluate(
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> Result<GateResult, LandingError> {
    if evidence.definition_digest != definition.digest {
        return Err(failed(
            "gate evidence was produced for a different gate definition",
        ));
    }
    match definition.kind {
        GateKind::Checks => evaluate_checks(definition, evidence),
        GateKind::Commands => evaluate_commands(definition, evidence),
    }
}

fn evaluate_checks(
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> Result<GateResult, LandingError> {
    if definition.checks.is_empty() {
        return Err(failed("unmet gate: no required checks are configured"));
    }
    let mut failed_check = false;
    let mut pending = false;
    for name in &definition.checks {
        let mut matching = evidence.checks.iter().filter(|check| {
            check.name == *name
                && definition
                    .issuer
                    .as_ref()
                    .is_none_or(|issuer| check.issuer.as_ref() == Some(issuer))
        });
        // Several runs of one name can exist; a pass needs every run to pass.
        let mut seen = false;
        for check in matching.by_ref() {
            seen = true;
            match check.result {
                GateResult::Passed => {}
                GateResult::Failed => {
                    failed_check = true;
                }
                GateResult::Pending => {
                    pending = true;
                }
                GateResult::Missing | GateResult::Skipped | GateResult::Neutral => {
                    return Err(failed(
                        format!("unmet gate: required check {name} is {:?}", check.result)
                            .to_lowercase(),
                    ));
                }
            }
        }
        if !seen {
            let issuer = definition
                .issuer
                .as_ref()
                .map_or(String::new(), |issuer| format!(" from issuer {issuer}"));
            return Err(failed(format!(
                "unmet gate: required check {name}{issuer} is missing"
            )));
        }
    }
    Ok(if failed_check {
        GateResult::Failed
    } else if pending {
        GateResult::Pending
    } else {
        GateResult::Passed
    })
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

/// Validate provenance and binding, returning the observed gate result; invalid evidence is an error.
pub fn validate_evidence(
    candidate: &Candidate,
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> Result<GateResult, LandingError> {
    if evidence.commit != candidate.commit || evidence.tree != candidate.tree {
        return Err(failed(format!(
            "gate evidence is bound to commit {} tree {}, not candidate {} tree {}",
            evidence.commit, evidence.tree, candidate.commit, candidate.tree
        )));
    }
    evaluate(definition, evidence)
}

/// Prove whether a pull request run covers the candidate tree and definition.
///
/// Only a singleton batch can be proven: the pull request head's tree must equal
/// the candidate tree and the passing evidence must satisfy the same definition.
pub fn pr_run_covers(
    candidate: &Candidate,
    definition: &GateDefinition,
    evidence: &GateEvidence,
) -> Result<bool, LandingError> {
    let [step] = candidate.steps.as_slice() else {
        return Ok(false);
    };
    if step.tree != candidate.tree
        || evidence.commit != step.pr.head
        || evidence.tree != candidate.tree
    {
        return Ok(false);
    }
    Ok(matches!(
        evaluate(definition, evidence),
        Ok(GateResult::Passed)
    ))
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
    reason = "gate command execution owns its wall-clock timeout"
)]
fn run_command(
    checkout: &std::path::Path,
    command: &str,
    deadline: std::time::Instant,
) -> Result<Run, LandingError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    // Command output goes to stderr so the JSON report on stdout stays clean.
    let script = format!("exec 1>&2\n{command}");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(checkout)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| failed(format!("cannot start gate command: {error}")))?;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| failed(format!("cannot wait for gate command: {error}")))?
        {
            return Ok(if status.success() {
                Run::Passed
            } else {
                Run::Failed
            });
        }
        if std::time::Instant::now() >= deadline {
            let group = format!("-{}", child.id());
            let _ = Command::new("kill").args(["-KILL", "--", &group]).output();
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Run::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Execute a configured command plan in an isolated candidate checkout.
#[allow(
    clippy::disallowed_methods,
    reason = "gate command execution owns its wall-clock timeout and git boundary"
)]
pub fn execute_commands(
    project: &std::path::Path,
    candidate: &Candidate,
    definition: &GateDefinition,
) -> Result<GateEvidence, LandingError> {
    if definition.commands.is_empty() {
        return Err(failed("unmet gate: no gate commands are configured"));
    }
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_secs(definition.timeout_seconds);
    let root = project.join(".worktrees");
    let short: String = candidate.commit.chars().take(12).collect();
    let checkout = root.join(format!("land-gate-{short}-{}", std::process::id()));
    if checkout.exists() {
        return Err(failed("gate checkout already exists"));
    }
    std::fs::create_dir_all(&root)
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
        match run_command(&checkout, command, deadline) {
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
    let executor = definition
        .executor
        .clone()
        .unwrap_or_else(|| "local".to_owned());
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
        pool: None,
        duration_seconds: Some(started.elapsed().as_secs()),
    })
}

/// Whether a pull request's own CI run is still outstanding under `policy`.
fn ci_outstanding(
    forge: &mut dyn Forge,
    pr: &EligiblePr,
    policy: &PrCiSettings,
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
    let pending = runs.iter().any(|run| {
        run.result == GateResult::Pending
            || run
                .checks
                .iter()
                .any(|check| check.result == GateResult::Pending)
    });
    // `always` waits for a run to appear; `affected` waits only for runs that exist.
    Ok(pending || (policy.mode == PrCiMode::Always && runs.is_empty()))
}

/// Follow the configured pull-request CI evidence policy without bypassing forge requirements.
///
/// Waiting never alters what the forge requires: required checks remain enforced
/// by the forge and by the candidate gate, so skipping an optional wait loses nothing.
pub fn await_pr_ci(
    forge: &mut dyn Forge,
    prs: &[EligiblePr],
    policy: &PrCiSettings,
    poll_seconds: u64,
) -> Result<(), LandingError> {
    #[allow(clippy::disallowed_methods, reason = "polling boundary")]
    await_pr_ci_with(forge, prs, policy, poll_seconds, &std::thread::sleep)
}

fn await_pr_ci_with(
    forge: &mut dyn Forge,
    prs: &[EligiblePr],
    policy: &PrCiSettings,
    poll_seconds: u64,
    sleep: &dyn Fn(Duration),
) -> Result<(), LandingError> {
    let poll = Duration::from_secs(poll_seconds.max(1));
    for pr in prs {
        let mut waited = Duration::ZERO;
        while ci_outstanding(forge, pr, policy)? {
            if waited >= PR_CI_WAIT_LIMIT {
                return Err(failed(format!(
                    "pull request {} CI did not finish in time",
                    pr.number
                )));
            }
            sleep(poll);
            waited += poll;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::landing::fakes::FakeForge;
    use std::cell::Cell;

    fn definition() -> GateDefinition {
        GateDefinition {
            kind: GateKind::Checks,
            digest: "d".into(),
            checks: vec!["ci-status".into()],
            commands: Vec::new(),
            issuer: None,
            executor: None,
            coverage: Vec::new(),
            timeout_seconds: 60,
        }
    }
    fn pr(number: u64, head: &str) -> EligiblePr {
        EligiblePr {
            number,
            head: head.into(),
            approval: "a".into(),
            base_branch: "main".into(),
        }
    }
    fn candidate(steps: usize) -> Candidate {
        Candidate {
            base: "base".into(),
            steps: (0..steps)
                .map(|n| CandidateStep {
                    pr: pr(n as u64, &format!("h{n}")),
                    commit: "c".into(),
                    tree: "t".into(),
                })
                .collect(),
            commit: "c".into(),
            tree: "t".into(),
            ref_name: "land/1".into(),
        }
    }
    fn evidence(result: GateResult, issuer: Option<&str>) -> GateEvidence {
        GateEvidence {
            commit: "c".into(),
            tree: "t".into(),
            definition_digest: "d".into(),
            checks: vec![CheckEvidence {
                name: "ci-status".into(),
                issuer: issuer.map(str::to_owned),
                result,
            }],
            issuer: None,
            coverage: Vec::new(),
            result: GateResult::Passed,
            pool: None,
            duration_seconds: None,
        }
    }

    #[test]
    fn passing_check_on_the_candidate_passes() {
        let got = validate_evidence(
            &candidate(1),
            &definition(),
            &evidence(GateResult::Passed, None),
        );
        assert_eq!(got.unwrap(), GateResult::Passed);
    }
    #[test]
    fn failed_check_is_a_red_result() {
        let got = validate_evidence(
            &candidate(1),
            &definition(),
            &evidence(GateResult::Failed, None),
        );
        assert_eq!(got.unwrap(), GateResult::Failed);
    }
    #[test]
    fn skipped_neutral_and_absent_checks_are_missing() {
        for result in [
            GateResult::Skipped,
            GateResult::Neutral,
            GateResult::Missing,
        ] {
            let got = validate_evidence(&candidate(1), &definition(), &evidence(result, None));
            assert!(got.unwrap_err().to_string().contains("ci-status"));
        }
        let mut empty = evidence(GateResult::Passed, None);
        empty.checks.clear();
        assert!(validate_evidence(&candidate(1), &definition(), &empty).is_err());
    }
    #[test]
    fn unexpected_issuer_does_not_satisfy_a_pinned_gate() {
        let mut pinned = definition();
        pinned.issuer = Some("15368".into());
        let wrong = evidence(GateResult::Passed, Some("1"));
        assert!(validate_evidence(&candidate(1), &pinned, &wrong).is_err());
        let right = evidence(GateResult::Passed, Some("15368"));
        assert_eq!(
            validate_evidence(&candidate(1), &pinned, &right).unwrap(),
            GateResult::Passed
        );
    }
    #[test]
    fn evidence_for_another_commit_tree_or_definition_is_rejected() {
        let mut other = evidence(GateResult::Passed, None);
        other.commit = "x".into();
        assert!(validate_evidence(&candidate(1), &definition(), &other).is_err());
        let mut other = evidence(GateResult::Passed, None);
        other.tree = "x".into();
        assert!(validate_evidence(&candidate(1), &definition(), &other).is_err());
        let mut other = evidence(GateResult::Passed, None);
        other.definition_digest = "x".into();
        assert!(validate_evidence(&candidate(1), &definition(), &other).is_err());
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
    #[test]
    fn pr_run_covers_only_a_singleton_with_the_same_tree() {
        let mut run = evidence(GateResult::Passed, None);
        run.commit = "h0".into();
        assert!(pr_run_covers(&candidate(1), &definition(), &run).unwrap());
        assert!(!pr_run_covers(&candidate(2), &definition(), &run).unwrap());
        run.tree = "other".into();
        assert!(!pr_run_covers(&candidate(1), &definition(), &run).unwrap());
        let mut failing = evidence(GateResult::Failed, None);
        failing.commit = "h0".into();
        assert!(!pr_run_covers(&candidate(1), &definition(), &failing).unwrap());
    }

    fn pending_forge() -> FakeForge {
        let mut forge = FakeForge::default();
        forge
            .check_runs
            .insert("h1".into(), vec![evidence(GateResult::Pending, None)]);
        forge
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
        let result = await_pr_ci_with(&mut forge, &[pr(1, "h1")], &policy, 3600, &|_| {
            slept.set(slept.get() + 1)
        });
        if slept.get() > 0 {
            assert!(result.is_err());
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
    #[allow(
        clippy::disallowed_methods,
        reason = "test builds a scratch repository"
    )]
    fn commands_run_in_an_isolated_checkout_and_clean_up() {
        let dir = std::env::temp_dir().join(format!("gate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
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
        def.executor = Some("local-pool".into());
        let got = execute_commands(&dir, &cand, &def).unwrap();
        assert_eq!(got.result, GateResult::Passed);
        assert_eq!(got.issuer.as_deref(), Some("local-pool"));
        assert!(got.duration_seconds.is_some());
        def.commands = vec!["test -f missing".into()];
        assert_eq!(
            execute_commands(&dir, &cand, &def).unwrap().result,
            GateResult::Failed
        );
        def.commands = vec!["sleep 30".into()];
        def.timeout_seconds = 1;
        assert!(execute_commands(&dir, &cand, &def).is_err());
        let leftover = std::fs::read_dir(dir.join(".worktrees")).unwrap().count();
        assert_eq!(leftover, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
