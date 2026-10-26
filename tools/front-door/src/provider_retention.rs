//! GC composition for terminal provider state, including nested run stores.
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, RootInputs, Roots, WaitBudget};
use provenant_run::{ExecutionContext, ProviderStateGc, ProviderStateIssue, Request, RunStore};
use provenant_supervisor::{
    clock::SystemClock,
    fs::{Directory, LocalFs},
};
use std::{ffi::OsStr, path::Path};

pub(crate) fn collect(
    roots: &Roots,
    home: &Path,
    assets: &Path,
    days: u64,
    dry_run: bool,
) -> ProviderStateGc {
    let mut report = ProviderStateGc::default();
    collect_store(
        roots,
        roots.state.as_path(),
        home,
        assets,
        days,
        dry_run,
        0,
        None,
        &mut report,
    );
    report
}

#[allow(clippy::too_many_arguments)]
fn collect_store(
    host: &Roots,
    path: &Path,
    home: &Path,
    assets: &Path,
    days: u64,
    dry_run: bool,
    depth: usize,
    expected: Option<&Directory>,
    report: &mut ProviderStateGc,
) {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if depth > 32 {
            return Err("child store depth limit".into());
        }
        let directory = match Directory::open_anchor(path) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut inputs = RootInputs::new(home.to_owned(), assets.to_owned());
        if let Some(expected) = expected
            && !directory.metadata()?.same_identity(&expected.metadata()?)
        {
            return Err("child store changed identity".into());
        }
        inputs.state_dir = Some(path.to_owned());
        let state_root = inputs.resolve(&LocalFs)?.state;
        if state_root.as_path() != path {
            return Err("child store path was redirected".into());
        }
        let context = ExecutionContext {
            state_root,
            project_id: ProjectId::from_bytes([0; 16]),
            actor_id: ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        let (clock, cancel) = (SystemClock, CancelToken::new());
        let call = Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 5)?,
            cancel: &cancel,
        };
        if let Some(store) = RunStore::open_read_only(&context, &call)? {
            let mut collected = store
                .with_writer_registry(host.state.as_path())
                .gc_provider_state(&directory, days, clock.wall_now()?, dry_run, &call)?;
            report.removed.append(&mut collected.removed);
            report.kept.append(&mut collected.kept);
            report.errors.append(&mut collected.errors);
        }
        let children = match directory.open_verified_directory(OsStr::new("children")) {
            Ok(children) => children,
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for name in children.entries()? {
            let name = name?;
            // Keep the verified descriptor live while its child store is read.
            let child = children.open_verified_directory(&name)?;
            collect_store(
                host,
                &path.join("children").join(&name),
                home,
                assets,
                days,
                dry_run,
                depth + 1,
                Some(&child),
                report,
            );
        }
        Ok(())
    })();
    if let Err(error) = result {
        report.errors.push(ProviderStateIssue {
            path: path.into(),
            reason: error.to_string(),
        });
    }
}

#[cfg(test)]
pub(crate) fn provider_state_gc_sweeps_children_and_refuses_replaced_roots()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, OperationId, RunId, TaskId, WallTime};
    use provenant_records::CanonicalV1;
    use std::{fs, os::unix::fs::DirBuilderExt};
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let (home, assets) = (base.join("home"), base.join("release"));
    fs::create_dir(&home)?;
    fs::create_dir(&assets)?;
    let mut inputs = RootInputs::new(home.clone(), assets.clone());
    inputs.state_dir = Some(base.join("state"));
    let roots = inputs.resolve(&LocalFs)?;
    let child_path = roots.state.as_path().join("children/child");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&child_path)?;
    inputs.state_dir = Some(child_path.clone());
    let context = ExecutionContext {
        state_root: inputs.resolve(&LocalFs)?.state,
        project_id: ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let (clock, cancel) = (SystemClock, CancelToken::new());
    let call = Request {
        clock: &clock,
        cancel: &cancel,
        budget: WaitBudget::cli(clock.mono_now(), 5)?,
    };
    let store = RunStore::open(&context, &call)?;
    let (run, attempt, task) = (
        RunId::from_bytes([3; 16]),
        AttemptId::from_bytes([4; 16]),
        TaskId::from_bytes([5; 16]),
    );
    store.create(&provenant_run::PreparedRun {
        run_id: run, operation_id: OperationId::parse("retention-child")?, created_at: WallTime::from_micros(100)?,
        attempts: vec![provenant_run::NewAttempt { attempt_id: attempt, task_id: task, prompt: String::new() }],
        request: serde_json::from_value(serde_json::json!({
            "schema_version":1, "concurrency":1, "parent":null, "tasks":[{
                "task_id": task, "prompt_digest": CanonicalV1::file_digest(b""),
                "route": {"kind":"model","provider":"codex","model":"codex/fixture"}, "fallback":[],
                "authority_digest": provenant_core::Digest::from_bytes([0;32]),
                "provider_timeout_seconds":20, "delivery_id":null }]
        }))?,
    }, &call)?;
    let candidate = child_path.join(format!(
        "runs/{run}/attempts/{attempt}/provider-state/candidate-0"
    ));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(candidate.join("sessions"))?;
    fs::write(candidate.join("provider.txt"), "codex")?;
    fs::write(candidate.join("sessions/session.jsonl"), "transcript")?;
    store.withdraw(
        attempt,
        provenant_run::AttemptState::Queued,
        provenant_run::Outcome::Cancelled,
        "fixture",
        WallTime::from_micros(200)?,
        &call,
    )?;
    drop(store);
    let report = collect(&roots, &home, &assets, 7, false);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!candidate.exists());
    assert!(
        child_path
            .join(format!(
                "runs/{run}/attempts/{attempt}/provider-history/candidate-0/sessions/session.jsonl"
            ))
            .exists()
    );
    let expected = Directory::open_anchor(&child_path)?;
    let moved = child_path.with_file_name("moved");
    fs::rename(&child_path, &moved)?;
    fs::DirBuilder::new().mode(0o700).create(&child_path)?;
    let mut report = ProviderStateGc::default();
    collect_store(
        &roots,
        &child_path,
        &home,
        &assets,
        7,
        false,
        1,
        Some(&expected),
        &mut report,
    );
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].reason.contains("identity"));
    assert!(moved.join("run.sqlite3").exists());
    Ok(())
}
