//! GC composition for terminal provider state, including nested run stores.
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, Roots};
use provenant_os::{clock::SystemClock, fs::Directory};
use provenant_run::{ExecutionContext, ProviderStateGc, ProviderStateIssue, Request, RunStore};
use std::{ffi::OsStr, path::Path};

#[cfg(test)]
pub(crate) fn collect(
    roots: &Roots,
    home: &Path,
    assets: &Path,
    days: u64,
    dry_run: bool,
) -> ProviderStateGc {
    collect_with_logs(roots, home, assets, days, 30, dry_run)
}

pub(crate) fn collect_with_logs(
    roots: &Roots,
    home: &Path,
    assets: &Path,
    days: u64,
    logs_days: u64,
    dry_run: bool,
) -> ProviderStateGc {
    collect_retention(roots, home, assets, days, logs_days, dry_run, false, None)
}

pub(crate) fn collect_pressure(
    projects: Option<&[ProjectId]>,
    roots: &Roots,
    home: &Path,
    assets: &Path,
    dry_run: bool,
) -> ProviderStateGc {
    collect_retention(roots, home, assets, 7, 30, dry_run, true, projects)
}

#[allow(clippy::too_many_arguments)]
fn collect_retention(
    roots: &Roots,
    home: &Path,
    assets: &Path,
    days: u64,
    logs_days: u64,
    dry_run: bool,
    temporary_only: bool,
    projects: Option<&[ProjectId]>,
) -> ProviderStateGc {
    let mut report = ProviderStateGc::default();
    collect_store(
        roots,
        roots.state.as_path(),
        home,
        assets,
        days,
        logs_days,
        dry_run,
        temporary_only,
        projects,
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
    logs_days: u64,
    dry_run: bool,
    temporary_only: bool,
    projects: Option<&[ProjectId]>,
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
        if let Some(expected) = expected
            && !directory.metadata()?.same_identity(&expected.metadata()?)
        {
            return Err("child store changed identity".into());
        }
        let state_root = crate::roots::child(home, assets, path.to_owned())?.state;
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
            cancel: &cancel,
        };
        if let Some(reader) = RunStore::open_read_only(&context, &call)? {
            // Applied cleanup updates the temporary-retained hint. Keep previews
            // read-only and never create a run store just to collect it.
            let store = if dry_run {
                reader
            } else {
                drop(reader);
                RunStore::open(&context, &call)?
            };
            let store = store.with_writer_registry(host.state.as_path());
            let mut collected = if temporary_only {
                match projects {
                    Some(projects) => store.gc_temporary_for_projects(
                        &directory,
                        clock.wall_now()?,
                        dry_run,
                        projects,
                        &call,
                    )?,
                    None => store.gc_temporary(&directory, clock.wall_now()?, dry_run, &call)?,
                }
            } else {
                store.gc_provider_state_with_logs(
                    &directory,
                    days,
                    logs_days,
                    clock.wall_now()?,
                    dry_run,
                    &call,
                )?
            };
            report.removed.append(&mut collected.removed);
            report.attempt_tmp.append(&mut collected.attempt_tmp);
            report.attempt_logs.append(&mut collected.attempt_logs);
            report.expired_events += collected.expired_events;
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
                logs_days,
                dry_run,
                temporary_only,
                projects,
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
    use provenant_core::{AttemptId, OperationId, RootInputs, RunId, TaskId, WallTime};
    use provenant_records::CanonicalV1;
    use std::{fs, os::unix::fs::DirBuilderExt};
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let (home, assets) = (base.join("home"), base.join("release"));
    fs::create_dir(&home)?;
    fs::create_dir(&assets)?;
    let mut inputs = RootInputs::new(home.clone(), assets.clone());
    inputs.state_dir = Some(base.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let child_path = roots.state.as_path().join("children/child");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&child_path)?;
    inputs.state_dir = Some(child_path.clone());
    let context = ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let (clock, cancel) = (SystemClock, CancelToken::new());
    let call = Request {
        clock: &clock,
        cancel: &cancel,
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
        provenant_run::AttemptState::Queued {
            extra: Default::default(),
        },
        provenant_run::Outcome::Cancelled,
        "fixture",
        WallTime::from_micros(200)?,
        &call,
    )?;
    let events_before = store.events(0, 64, Some(attempt), &call)?;
    assert!(
        events_before.len() > 1,
        "child attempt records multiple events"
    );
    let index_call = provenant_fabric_runtime::chairs::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let index = provenant_fabric_runtime::chairs::ChairIndex::open(&roots.state, &index_call)?;
    index.submit(
        context.project_id,
        run,
        &store.request(run, &call)?,
        &index_call,
    )?;
    let association = index.read(context.project_id, run, &index_call)?;
    drop(store);
    let temporary = candidate
        .parent()
        .ok_or("attempt path")?
        .parent()
        .ok_or("attempt path")?
        .join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), vec![1; 8192])?;
    let preview = collect(&roots, &home, &assets, 7, true);
    assert!(preview.errors.is_empty(), "{:?}", preview.errors);
    assert_eq!(preview.expired_events, (events_before.len() - 1) as u64);
    let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("child store")?;
    assert_eq!(
        reader.events(0, 64, Some(attempt), &call)?.len(),
        events_before.len()
    );
    drop(reader);
    assert_eq!(preview.attempt_tmp.len(), 1);
    assert_eq!(preview.attempt_tmp[0].path, temporary);
    assert!(preview.attempt_tmp[0].bytes >= 8192);
    assert!(temporary.exists());
    let report = collect(&roots, &home, &assets, 7, false);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.expired_events, preview.expired_events);
    assert_eq!(report.attempt_tmp.len(), 1);
    assert_eq!(report.attempt_tmp[0].path, temporary);
    assert_eq!(report.attempt_tmp[0].bytes, preview.attempt_tmp[0].bytes);
    assert!(!temporary.exists());
    assert!(!candidate.exists());
    let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("child store")?;
    assert_eq!(
        reader.run(run, &call)?.run_id,
        run,
        "payload GC retains run metadata"
    );
    assert_eq!(
        index.read(context.project_id, run, &index_call)?,
        association,
        "retained runs keep chair associations"
    );
    let retained = reader.events(0, 64, Some(attempt), &call)?;
    assert_eq!(retained.len(), 1, "child store keeps newest event");
    assert_eq!(
        retained[0].sequence,
        events_before.last().ok_or("last event")?.sequence
    );
    drop(reader);
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
        30,
        false,
        false,
        None,
        1,
        Some(&expected),
        &mut report,
    );
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].reason.contains("identity"));
    assert!(moved.join("run.sqlite3").exists());
    Ok(())
}
