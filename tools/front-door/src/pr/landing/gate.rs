//! Gate evidence: validation, command execution and pull-request CI waiting.
use super::{LandingError, model::*, ports::Forge};
use provenant_cli::domains::{GateKind, PrCiMode, PrCiSettings};
use std::time::Duration;

/// Upper bound on waiting for one pull request's own CI run.
const PR_CI_WAIT_LIMIT: Duration = Duration::from_secs(6 * 60 * 60);

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
    Ok(
        pending
            || (policy.mode == PrCiMode::Always && runs.iter().all(|run| run.checks.is_empty())),
    )
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

    fn validate_evidence(
        candidate: &Candidate,
        definition: &GateDefinition,
        evidence: &GateEvidence,
    ) -> Result<GateResult, LandingError> {
        assess_evidence(candidate, definition, evidence).0
    }
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
            pool: None,
            env_passthrough: Vec::new(),
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
    fn command_gate_reports_the_failed_command() {
        let mut def = definition();
        def.kind = GateKind::Commands;
        def.commands = vec!["cargo test --example fixture".into()];
        let mut red = evidence(GateResult::Failed, None);
        red.result = GateResult::Failed;
        red.checks[0].name = def.commands[0].clone();
        let (result, unmet) = assess_evidence(&candidate(1), &def, &red);
        assert_eq!(result.unwrap(), GateResult::Failed);
        assert_eq!(unmet, "cargo test --example fixture: failed");
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
        let got = execute_commands(&dir, &cand, &def).unwrap();
        assert_eq!(got.result, GateResult::Passed);
        assert_eq!(got.issuer.as_deref(), Some("local"));
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
