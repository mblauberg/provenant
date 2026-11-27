use super::*;
use crate::tests::{call, clock, context, input};
use provenant_core::{CancelToken, Clock as _};
use provenant_os::exec::{
    CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
};
use std::{collections::VecDeque, sync::Mutex};

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

struct GateExecutor {
    exits: Mutex<VecDeque<i64>>,
    calls: Mutex<Vec<(String, String, String)>>,
}

impl GateExecutor {
    fn new(exits: impl IntoIterator<Item = i64>) -> Self {
        Self {
            exits: Mutex::new(exits.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl Executor for GateExecutor {
    fn execute(&self, options: ExecOptions<'_>) -> std::result::Result<ExecOutcome, ExecError> {
        let program = options
            .argv
            .first()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        let environment = options.env.unwrap_or(&[]);
        let value = |name: &std::ffi::OsStr| {
            environment
                .iter()
                .find(|(key, _)| key.as_os_str() == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let phase = value(std::ffi::OsStr::new(
            provenant_core::consumer_env::PORT_PHASE,
        ));
        let epoch = value(std::ffi::OsStr::new(
            provenant_core::consumer_env::ADMISSION_EPOCH,
        ));
        self.calls
            .lock()
            .expect("gate calls mutex")
            .push((program, phase, epoch));
        let code = self
            .exits
            .lock()
            .expect("gate exits mutex")
            .pop_front()
            .unwrap_or(0);
        Ok(ExecOutcome {
            termination: Termination::Exit(code),
            stdout: Default::default(),
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 1,
        })
    }
}

fn declaration(program: &str, source: &str) -> String {
    serde_json::json!({
        "schema_version": 2,
        "admission": {
            "source": source,
            "backend": {
                "kind": "command",
                "command": {
                    "argv": [program],
                    "exit_codes": {
                        "success": [0],
                        "unknown": [75],
                        "other": "refused"
                    }
                }
            }
        }
    })
    .to_string()
}

#[test]
fn admission_binding_retries_busy_release_and_survives_config_rotation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let project = temp.path().join("project");
    let project_ports = project.join(".provenant/ports.json");
    std::fs::create_dir_all(
        project_ports
            .parent()
            .ok_or_else(|| std::io::Error::other("ports parent missing"))?,
    )?;
    std::fs::create_dir_all(context.state_root.as_path())?;
    std::fs::write(&project_ports, declaration("repo-gate-old", "project"))?;
    std::fs::write(
        context.state_root.as_path().join("ports.json"),
        r#"{"schema_version":3}"#,
    )?;

    let mut prepared = input();
    let attempt = prepared.attempt_id;
    prepared.request.task.writer = Some(provenant_records::run::WriterRequest {
        extra: Default::default(),
        custody_project: Some(crate::tests::writer_project()),
        project: project.clone(),
        worktree: project.join("lane"),
    });
    crate::tests::rekey(&mut prepared);
    store.create(&prepared, &call)?;
    let request = store.request(attempt, &call)?;
    let record = store.record(attempt, &call)?;
    let (record, binding) = store
        .admission_binding(&request, &record, &call)
        .map_err(|error| std::io::Error::other(error.to_owned()))?;
    let binding =
        binding.ok_or_else(|| std::io::Error::other("project admission command was not bound"))?;
    assert_eq!(binding.source, provenant_records::Layer::Project);
    assert_eq!(binding.phase, RepositoryAdmissionPhase::AcquirePending);
    assert!(record.repository_admission_binding.is_some());

    // Busy acquisition is followed by a failed release. Both remain durable
    // in the same attempt binding and the attempt stays queued for retry.
    let executor = GateExecutor::new([1, 1, 0, 0, 0]);
    let (record, skipped) = store.admission_skip_with_phase(
        &request,
        &record,
        &call,
        &mut |request, id, binding, phase, cancel| {
            store.run_admission_phase_with_executor(request, id, binding, phase, cancel, &executor)
        },
    );
    assert!(skipped.is_some());
    assert!(matches!(record.state, AttemptState::Queued { .. }));
    let stored: RepositoryAdmissionBinding = record
        .repository_admission_binding
        .clone()
        .ok_or("missing repository admission binding")?;
    assert_eq!(stored.phase, RepositoryAdmissionPhase::ReleasePending);

    // The release retry and the next acquisition still use the bound project
    // command after the effective source rotates to the host declaration.
    std::fs::write(&project_ports, r#"{"schema_version":3}"#)?;
    std::fs::write(
        context.state_root.as_path().join("ports.json"),
        declaration("repo-gate-new", "host"),
    )?;
    let effective =
        provenant_config::PortTable::read_for(Some(&project), context.state_root.as_path())?;
    assert_eq!(effective.admission.source, provenant_records::Layer::Host);
    let rotated_program = match effective.admission.backend {
        provenant_config::PortBackend::Command { command } => command.argv[0].clone(),
        _ => return Err(std::io::Error::other("host admission command was not active").into()),
    };
    assert_eq!(rotated_program, "repo-gate-new");

    let (record, skipped) = store.admission_skip_with_phase(
        &request,
        &record,
        &call,
        &mut |request, id, binding, phase, cancel| {
            store.run_admission_phase_with_executor(request, id, binding, phase, cancel, &executor)
        },
    );
    assert!(skipped.is_some());
    assert!(matches!(record.state, AttemptState::Queued { .. }));
    let (record, skipped) = store.admission_skip_with_phase(
        &request,
        &record,
        &call,
        &mut |request, id, binding, phase, cancel| {
            store.run_admission_phase_with_executor(request, id, binding, phase, cancel, &executor)
        },
    );
    assert!(skipped.is_none());
    let mut terminal_intent = record.clone();
    mark_admission_release_pending(&mut terminal_intent)?;
    let terminal_binding: RepositoryAdmissionBinding = terminal_intent
        .repository_admission_binding
        .clone()
        .ok_or("missing repository admission binding")?;
    assert_eq!(
        terminal_binding.phase,
        RepositoryAdmissionPhase::ReleasePending
    );
    let record =
        store.record_admission_phase(&record, RepositoryAdmissionPhase::ReleasePending, &call)?;
    assert!(store.release_admission_with_phase(
        &record,
        &call,
        &mut |request, id, binding, phase, cancel| {
            store.run_admission_phase_with_executor(request, id, binding, phase, cancel, &executor)
        },
    ));
    let final_record = store.record(attempt, &call)?;
    let binding: RepositoryAdmissionBinding = final_record
        .repository_admission_binding
        .clone()
        .ok_or("missing repository admission binding")?;
    assert_eq!(binding.phase, RepositoryAdmissionPhase::Released);
    assert_eq!(binding.lease_epoch, 2);
    assert!(matches!(final_record.state, AttemptState::Queued { .. }));
    let calls = executor.calls.lock().expect("gate calls mutex");
    assert!(
        calls
            .iter()
            .all(|(program, _, _)| program == "repo-gate-old")
    );
    for (phase, epoch) in [
        ("acquire", "1"),
        ("release", "1"),
        ("acquire", "2"),
        ("release", "2"),
    ] {
        assert!(
            calls
                .iter()
                .any(|(_, observed, key)| observed == phase && key == epoch)
        );
    }
    drop(calls);

    // A release fence published while acquisition is in flight must win
    // over the late successful acquire result.
    let mut observed = Vec::new();
    let (record, skipped) = store.admission_skip_with_phase(
        &request,
        &final_record,
        &call,
        &mut |_, id, binding, phase, _| {
            observed.push((phase, binding.lease_epoch));
            if phase == RepositoryAdmissionPhase::AcquirePending {
                let current = store.record(id, &call).expect("acquiring record");
                store
                    .record_admission_phase(
                        &current,
                        RepositoryAdmissionPhase::ReleasePending,
                        &call,
                    )
                    .expect("concurrent release fence");
            }
            true
        },
    );
    assert!(skipped.is_some());
    let fenced: RepositoryAdmissionBinding = record
        .repository_admission_binding
        .clone()
        .ok_or("missing repository admission binding")?;
    assert_eq!(fenced.phase, RepositoryAdmissionPhase::Released);
    assert!(observed.contains(&(RepositoryAdmissionPhase::ReleasePending, fenced.lease_epoch)));
    Ok(())
}
#[test]
fn starting_owner_waits_for_release_proof_and_unwinds_cancellation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let project = temp.path().join("project");
    let project_ports = project.join(".provenant/ports.json");
    std::fs::create_dir_all(project_ports.parent().ok_or("ports parent missing")?)?;
    std::fs::create_dir_all(context.state_root.as_path())?;
    std::fs::write(&project_ports, declaration("repo-gate-bound", "project"))?;
    std::fs::write(
        context.state_root.as_path().join("ports.json"),
        r#"{"schema_version":3}"#,
    )?;

    let mut input = input();
    let attempt = input.attempt_id;
    input.request.task.writer = Some(provenant_records::run::WriterRequest {
        extra: Default::default(),
        custody_project: Some(crate::tests::writer_project()),
        project: project.clone(),
        worktree: project.join("lane"),
    });
    crate::tests::rekey(&mut input);
    store.create(&input, &call)?;
    let request = store.request(attempt, &call)?;
    let queued = store.record(attempt, &call)?;
    let (queued, frozen) = store
        .admission_binding(&request, &queued, &call)
        .map_err(|error| std::io::Error::other(error.to_owned()))?;
    let frozen = frozen.ok_or("project admission command was not bound")?;
    let attempt_dir = store.attempt_dir(attempt, true)?;
    let launcher = attempt_dir.try_lock(
        std::ffi::OsStr::new("launcher.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let starting = store.claim_starting_for_preparation(&queued, &call)?;
    assert!(matches!(starting.state, AttemptState::Starting { .. }));
    assert!(starting.resolved_route.is_none());

    // A configuration rotation after the owner claim cannot replace the
    // declaration already bound to this attempt.
    std::fs::write(&project_ports, r#"{"schema_version":3}"#)?;
    std::fs::write(
        context.state_root.as_path().join("ports.json"),
        declaration("repo-gate-rotated", "host"),
    )?;
    let mut calls = Vec::new();
    let (pending, skipped) = store.admission_skip_with_phase_bound(
        &request,
        &starting,
        Some(frozen),
        &call,
        &mut |_, _, binding, phase, _| {
            calls.push((binding.command.argv[0].clone(), phase));
            false
        },
    );
    assert!(skipped.is_some());
    assert!(matches!(pending.state, AttemptState::Starting { .. }));
    assert!(
        calls
            .iter()
            .all(|(program, _)| program == "repo-gate-bound")
    );
    assert!(matches!(
        store.requeue_starting_after_admission_skip(&pending, &call),
        Err(RunError::Conflict)
    ));
    assert!(matches!(
        store.record(attempt, &call)?.state,
        AttemptState::Starting { .. }
    ));

    let binding: RepositoryAdmissionBinding = pending
        .repository_admission_binding
        .clone()
        .ok_or("missing repository admission binding")?;
    let (released, skipped) = store.admission_skip_with_phase_bound(
        &request,
        &pending,
        Some(binding),
        &call,
        &mut |_, _, _, phase, _| phase == RepositoryAdmissionPhase::ReleasePending,
    );
    assert!(skipped.is_some());
    let queued = store
        .requeue_starting_after_admission_skip(&released, &call)?
        .ok_or("cancellation unexpectedly won the requeue fence")?;
    assert!(matches!(queued.state, AttemptState::Queued { .. }));
    drop(launcher);

    let launcher = attempt_dir.try_lock(
        std::ffi::OsStr::new("launcher.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let starting = store.claim_starting_for_preparation(&queued, &call)?;
    custody::request_stop(
        attempt_dir.try_clone()?,
        RunStore::scope(&starting),
        starting.verified_owner.unwrap_or(context.actor_id),
        provenant_supervisor::machine::StopReason::ExplicitCancellation,
    )?;
    assert!(
        store
            .requeue_starting_after_admission_skip(&starting, &call)?
            .is_none()
    );
    assert!(matches!(
        store.record(attempt, &call)?.state,
        AttemptState::Starting { .. }
    ));
    drop(launcher);
    assert!(store.cancel_preparation_if_requested(
        attempt,
        &starting,
        &attempt_dir,
        clock.wall_now()?,
        &call,
    )?);
    assert!(matches!(
        store.record(attempt, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    Ok(())
}

#[test]
fn caller_cancellation_still_settles_the_preparation_owner() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let never = CancelToken::new();
    let inspection = call(&clock, &never);
    let store = RunStore::open(&context, &inspection)?;
    let input = input();
    let attempt = input.attempt_id;
    store.create(&input, &inspection)?;
    let queued = store.record(attempt, &inspection)?;
    let directory = store.attempt_dir(attempt, true)?;
    let _launcher = directory.try_lock(
        std::ffi::OsStr::new("launcher.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let starting = store.claim_starting_for_preparation(&queued, &inspection)?;
    let cancelled = CancelToken::new();
    cancelled.cancel();
    let cancelled_call = call(&clock, &cancelled);
    assert!(store.cancel_preparation_if_requested(
        attempt,
        &starting,
        &directory,
        clock.wall_now()?,
        &cancelled_call,
    )?);
    assert!(matches!(
        store.record(attempt, &inspection)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    Ok(())
}

#[test]
fn terminal_admission_release_uses_receipt_without_mutating_attempt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    let attempt = input.attempt_id;
    store.create(&input, &call)?;
    let queued = store.record(attempt, &call)?;
    let command = provenant_records::PortCommand {
        argv: vec![
            temp.path()
                .join("not-executed-admission-command")
                .display()
                .to_string(),
        ],
        exit_codes: provenant_records::CommandExitCodes {
            success: vec![0],
            unknown: vec![75],
            other: provenant_records::ExitDisposition::Refused,
        },
    };
    let binding = RepositoryAdmissionBinding::new(provenant_records::Layer::Project, &command);
    store.record_admission_binding(&queued, binding, &call)?;
    let terminal = store
        .withdraw(
            attempt,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "cancel before preparation",
            clock.wall_now()?,
            &call,
        )?
        .ok_or("attempt did not settle")?;
    let terminal_binding = terminal
        .repository_admission_binding
        .clone()
        .ok_or("missing repository admission binding")?;
    assert_eq!(
        terminal_binding.phase,
        RepositoryAdmissionPhase::ReleasePending
    );

    // A matching sidecar receipt proves an idempotent release while the
    // immutable terminal AttemptRecord remains byte-for-byte unchanged.
    let key = format!("{attempt}:{}", terminal_binding.lease_epoch);
    let execution = provenant_config::PortCommandExecution {
        command_digest: terminal_binding.command_digest,
        disposition: provenant_config::PortCommandDisposition::Succeeded,
        outcome: Some(ExecOutcome {
            termination: Termination::Exit(0),
            stdout: CapturedStream::default(),
            stderr: CapturedStream::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 1,
        }),
    };
    provenant_config::record_port_command(
        context.state_root.as_path(),
        "admission",
        terminal_binding.source,
        "release",
        &key,
        &execution,
        provenant_config::PortReceiptLifetime::AttemptTerminalAndRelease {
            attempt_id: attempt,
            project_id: context.project_id,
            actor_id: context.actor_id,
            lease_epoch: terminal_binding.lease_epoch,
        },
    )?;
    assert!(store.admission_release_receipt_confirmed(attempt, &terminal_binding));
    assert!(
        store.release_admission_with_phase(&terminal, &call, &mut |_, _, _, _, _| panic!(
            "confirmed receipt must avoid re-running release"
        ),)
    );
    assert!(matches!(
        store.record_admission_phase(&terminal, RepositoryAdmissionPhase::Released, &call),
        Err(RunError::Conflict)
    ));
    assert_eq!(store.record(attempt, &call)?, terminal);
    Ok(())
}
