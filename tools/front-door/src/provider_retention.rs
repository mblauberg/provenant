//! GC adapters for provider state, attempt scratch and payload caps.
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, Roots};
use provenant_os::{clock::SystemClock, fs::Directory};
use provenant_run::{ExecutionContext, ProviderStateGc, ProviderStateIssue, Request, RunStore};
use std::path::Path;

#[derive(Clone, Copy)]
enum RetentionPass {
    ProviderState { days: u64, logs_days: u64 },
    AttemptScratch { idle_seconds: u64 },
    PayloadCap { cap_bytes: u64, measure: bool },
}

pub(crate) fn collect_provider_state(
    roots: &Roots,
    home: &Path,
    assets: &Path,
    days: u64,
    logs_days: u64,
    dry_run: bool,
) -> ProviderStateGc {
    collect_retention(
        roots,
        home,
        assets,
        RetentionPass::ProviderState { days, logs_days },
        dry_run,
        None,
    )
}

pub(crate) fn collect_attempt_scratch(
    roots: &Roots,
    home: &Path,
    assets: &Path,
    idle_seconds: u64,
    projects: Option<&[ProjectId]>,
    dry_run: bool,
) -> ProviderStateGc {
    collect_retention(
        roots,
        home,
        assets,
        RetentionPass::AttemptScratch { idle_seconds },
        dry_run,
        projects,
    )
}

pub(crate) fn collect_payload_cap(
    projects: Option<&[ProjectId]>,
    roots: &Roots,
    home: &Path,
    assets: &Path,
    cap_bytes: u64,
    measure: bool,
    dry_run: bool,
) -> ProviderStateGc {
    collect_retention(
        roots,
        home,
        assets,
        RetentionPass::PayloadCap { cap_bytes, measure },
        dry_run,
        projects,
    )
}

fn collect_retention(
    roots: &Roots,
    home: &Path,
    assets: &Path,
    pass: RetentionPass,
    dry_run: bool,
    projects: Option<&[ProjectId]>,
) -> ProviderStateGc {
    let mut report = ProviderStateGc::default();
    collect_store(
        roots,
        roots.state.as_path(),
        home,
        assets,
        pass,
        dry_run,
        projects,
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
    pass: RetentionPass,
    dry_run: bool,
    projects: Option<&[ProjectId]>,
    expected: Option<&Directory>,
    report: &mut ProviderStateGc,
) {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let directory = match Directory::open_anchor(path) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if let Some(expected) = expected
            && !directory.metadata()?.same_identity(&expected.metadata()?)
        {
            return Err("run store changed identity".into());
        }
        let state_root = crate::roots::child(home, assets, path.to_owned())?.state;
        if state_root.as_path() != path {
            return Err("run store path was redirected".into());
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
            // Applied cleanup needs a writable store. Previews stay read-only
            // and never create a run store just to collect it.
            let store = if dry_run {
                reader
            } else {
                drop(reader);
                RunStore::open(&context, &call)?
            };
            let store = store.with_writer_registry(host.state.as_path());
            let now = clock.wall_now()?;
            match pass {
                RetentionPass::ProviderState { days, logs_days } => {
                    let mut collected = store.gc_provider_state_with_logs(
                        &directory, days, logs_days, now, dry_run, &call,
                    )?;
                    report.removed.append(&mut collected.removed);
                    report.attempt_tmp.append(&mut collected.attempt_tmp);
                    report.attempt_logs.append(&mut collected.attempt_logs);
                    report.expired_events += collected.expired_events;
                    report.kept.append(&mut collected.kept);
                    report.errors.append(&mut collected.errors);
                }
                RetentionPass::AttemptScratch { idle_seconds } => {
                    let owner = crate::scratch_owner::Owner::new(host, home, assets);
                    let decide =
                        |record: &provenant_run::AttemptRecord,
                         request: &provenant_run::CanonicalRequest| {
                            owner.disposition(record, request)
                        };
                    let policy = provenant_run::ScratchPolicy {
                        idle_seconds,
                        projects,
                        attempts: None,
                        owner: Some(&decide),
                    };
                    let mut collected =
                        store.gc_temporary(&directory, &policy, now, dry_run, &call)?;
                    report.removed.append(&mut collected.removed);
                    report.attempt_tmp.append(&mut collected.attempt_tmp);
                    report.attempt_logs.append(&mut collected.attempt_logs);
                    report.expired_events += collected.expired_events;
                    report.kept.append(&mut collected.kept);
                    report.errors.append(&mut collected.errors);
                }
                RetentionPass::PayloadCap { cap_bytes, measure } => {
                    report.payload.absorb(store.gc_payloads(
                        &directory, cap_bytes, now, dry_run, measure, projects, &call,
                    )?);
                }
            }
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

/// Look up the exact merged branch; no directory or run-store inventory.
pub(crate) fn branch_attempts(
    roots: &Roots,
    project: ProjectId,
    branch: &str,
) -> Result<Vec<provenant_core::AttemptId>, Box<dyn std::error::Error>> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    match provenant_run::Contributions::open_read_only(roots.state.as_path(), &call)? {
        Some(index) => Ok(index.attempts_for_branch(project, branch, &call)?),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn attempts_released(
    roots: &Roots,
    project: ProjectId,
    ids: &[provenant_core::AttemptId],
    _worktree: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: project,
        actor_id: ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let Some(store) = RunStore::open_disk_read_only(&context, &call)? else {
        return Ok(ids.is_empty());
    };
    for id in ids {
        let record = store.record(*id, &call)?;
        if !matches!(record.state, provenant_run::AttemptState::Terminal { .. })
            || !store.custody_released(&record, &call)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn spawn_branch_scratch(
    roots: &Roots,
    home: &Path,
    project: &Path,
    branch: &str,
    tip: provenant_core::GitOid,
    integration: &str,
    attempts: &[provenant_core::AttemptId],
) -> Result<(), Box<dyn std::error::Error>> {
    let target = serde_json::to_string(&(branch, tip, integration, attempts))?;
    let args = [
        std::env::current_exe()?.into_os_string(),
        "--state-dir".into(),
        roots.state.as_path().as_os_str().to_owned(),
        "--project".into(),
        project.as_os_str().to_owned(),
        "gc".into(),
        "--finished-branch".into(),
        target.into(),
        "--apply".into(),
    ];
    provenant_os::exec::spawn_detached(&args, home)?;
    Ok(())
}

/// The land command already removed the exact worktree. Its branch index
/// selects attempts; publication/custody locks and absent-owner proof are
/// rechecked by RunStore at mutation. No shared cache adapters run here.
pub(crate) fn run_branch_scratch(globals: &provenant_cli::Globals, branch: &str) -> u8 {
    let collect = || -> Result<(), Box<dyn std::error::Error>> {
        if provenant_cli::fabric::in_lane() {
            return Err("branch scratch collection requires host custody".into());
        }
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Checkout,
            crate::roots::Product::Required,
        )?;
        let identity = provenant_cli::fabric::Context::open(
            resolved.roots.clone(),
            &resolved.home,
            &resolved.project,
        )?;
        let project = identity.identity.project;
        let (branch, tip, integration, requested): (
            String,
            provenant_core::GitOid,
            String,
            Vec<provenant_core::AttemptId>,
        ) = serde_json::from_str(branch)?;
        let executor = provenant_os::exec::SystemExecutor::new(1)?;
        let target = if integration.starts_with("refs/") {
            provenant_confinement::repo::GitRefName::new(&integration)?
        } else {
            provenant_confinement::repo::GitRefName::remote_tracking(&integration)?
        };
        let landed = crate::gc_support::host_git_with_stderr(
            &resolved.project,
            &executor,
            &[
                "merge-base",
                "--is-ancestor",
                &tip.to_string(),
                target.as_str(),
            ],
            std::time::Duration::from_secs(5),
            false,
        )?;
        let landed = landed.termination == provenant_os::exec::Termination::Exit(0);
        let indexed = branch_attempts(&resolved.roots, project, &branch)?;
        let ids: Vec<_> = requested
            .into_iter()
            .filter(|id| indexed.contains(id))
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let context = ExecutionContext {
            state_root: resolved.roots.state.clone(),
            project_id: project,
            actor_id: ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        let directory = Directory::open_anchor(resolved.roots.state.as_path())?;
        let store = RunStore::open(&context, &call)?;
        let owner =
            crate::scratch_owner::Owner::new(&resolved.roots, &resolved.home, &resolved.assets);
        let finished = |record: &provenant_run::AttemptRecord,
                        request: &provenant_run::CanonicalRequest| {
            let path = request
                .task
                .writer
                .as_ref()
                .map(|writer| &writer.worktree)
                .or(request.task.access.source_root.as_ref());
            if path.is_some_and(|path| {
                std::fs::symlink_metadata(path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            }) {
                return provenant_run::ScratchOwner::Finished;
            }
            if !landed {
                // Squash merges and non-origin targets may not have local
                // ancestry. Reuse scoped graph/forge proof for these IDs only.
                return owner.disposition(record, request);
            }
            // The launch captured these IDs before deleting this branch's
            // checkout. A queued worker cannot expand to a reused branch.
            let Some(head) = record.head.or(record.subject) else {
                return owner.disposition(record, request);
            };
            let related = crate::gc_support::host_git_with_stderr(
                &resolved.project,
                &executor,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &head.to_string(),
                    &tip.to_string(),
                ],
                std::time::Duration::from_secs(5),
                false,
            )
            .is_ok_and(|output| output.termination == provenant_os::exec::Termination::Exit(0));
            if !related {
                return owner.disposition(record, request);
            }
            provenant_run::ScratchOwner::Finished
        };
        let policy = provenant_run::ScratchPolicy {
            idle_seconds: 0,
            projects: None,
            attempts: Some(&ids),
            owner: Some(&finished),
        };
        let report = store.gc_temporary(&directory, &policy, clock.wall_now()?, false, &call)?;
        if !report.errors.is_empty() {
            return Err(
                "branch scratch collection incomplete; retry with provenant gc --apply".into(),
            );
        }
        Ok(())
    };
    if collect().is_ok() { 0 } else { 1 }
}

#[cfg(test)]
pub(crate) fn provider_retention_classes_sweep_host_and_refuse_replaced_roots()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, OperationId, RootInputs, WaitBudget, WallTime};
    use provenant_fabric_runtime::graph::{
        FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
    };
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
    let child_path = roots.state.as_path().to_owned();
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
    let attempt = AttemptId::from_bytes([4; 16]);
    store.create(&provenant_run::PreparedRun {
        attempt_id: attempt, operation_id: OperationId::parse("retention-child")?, created_at: WallTime::from_micros(100)?,
        prompt: String::new(),
        request: serde_json::from_value(serde_json::json!({
            "schema_version":1, "parent":null, "task":{
                "context": {"graph_id":"lane-retention","node_id":"retention","attempt":1},
                "prompt_digest": CanonicalV1::file_digest(b""),
                "route": {"kind":"model","provider":"codex","model":"codex/fixture"}, "authority_digest": provenant_core::Digest::from_bytes([0;32]),
                "provider_timeout_seconds":20, "delivery_id":null }
        }))?,
    }, &call)?;
    let reaction = provenant_run::Reaction {
        schema_version: 1,
        attempt,
        project: context.project_id,
        sender: context.actor_id,
        notice: "fixture".into(),
        writer: false,
        finished_micros: clock.wall_now()?.as_micros() - 10 * 86_400 * 1_000_000,
        pr_ids: Vec::new(),
        issue_ids: Vec::new(),
        graph: false,
        resolved: false,
        chair: None,
        session: None,
        graph_done: true,
        notice_done: true,
        log_done: true,
        ledger_done: true,
        observation_done: true,
    };
    store.owe_reaction(&reaction, &call)?;
    store.save_reaction(&reaction, &call)?;
    let kept = collect_provider_state(&roots, &home, &assets, 7, 20, false);
    assert!(kept.errors.is_empty(), "{:?}", kept.errors);
    assert!(store.reaction(attempt, &call)?.is_some());
    let expired = collect_provider_state(&roots, &home, &assets, 7, 5, false);
    assert!(expired.errors.is_empty(), "{:?}", expired.errors);
    assert!(store.reaction(attempt, &call)?.is_none());
    let candidate = child_path.join(format!("attempts/{attempt}/provider-state/candidate-0"));
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
    let key = store.work_key(attempt, &call)?;
    let graph = key.graph().to_owned();
    let index = provenant_fabric_runtime::chairs::ChairIndex::open(&roots.state, &index_call)?;
    index.submit(
        context.project_id,
        &graph,
        &store.request(attempt, &call)?,
        &index_call,
    )?;
    let association = index.read(context.project_id, &graph, &index_call)?;
    // Age alone cannot finish graph-owned work. Give retention the durable
    // cancellation proof that makes this graph's scratch collectable.
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": graph.clone(),
        "nodes": [{"id":"retention", "access":"read_only", "prompt":"fixture", "route":{"alias":"senior"}}]
    }))?;
    let digest = provenant_fabric_runtime::graph::graph_plan_digest(&document)?.to_string();
    let budget = WaitBudget::cli(clock.mono_now(), 5)?;
    let mut journal = FileJournal::open(
        &roots.state,
        context.project_id,
        &graph,
        budget,
        &cancel,
        &clock,
    )?;
    journal.bind(GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: base.clone(),
        document,
    })?;
    let checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": graph.clone(),
        "plan_digest": digest,
        "cancelled": true,
        "nodes": [{"id":"retention", "state":"skipped", "attempts":[]}]
    }))?;
    journal.save(&checkpoint)?;
    drop(journal);
    assert!(checkpoint.cancelled);
    assert!(
        FileJournal::read_status_checkpoint(&roots.state, context.project_id, &graph)?
            .is_some_and(|stored| stored.cancelled)
    );
    drop(store);
    let temporary = candidate
        .parent()
        .ok_or("attempt path")?
        .parent()
        .ok_or("attempt path")?
        .join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), vec![1; 8192])?;
    let preview = collect_provider_state(&roots, &home, &assets, 7, 30, true);
    let preview_scratch = collect_attempt_scratch(&roots, &home, &assets, 7, None, true);
    let preview_payload = collect_payload_cap(None, &roots, &home, &assets, 16 << 30, false, true);
    assert!(preview.errors.is_empty(), "{:?}", preview.errors);
    assert!(preview.attempt_tmp.is_empty());
    assert_eq!(preview.payload.cap_bytes, 0);
    assert!(
        preview_scratch.errors.is_empty(),
        "{:?}",
        preview_scratch.errors
    );
    assert_eq!(preview.expired_events, (events_before.len() - 1) as u64);
    let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("run store")?;
    assert_eq!(
        reader.events(0, 64, Some(attempt), &call)?.len(),
        events_before.len()
    );
    drop(reader);
    assert_eq!(preview_scratch.attempt_tmp.len(), 1);
    assert_eq!(preview_scratch.attempt_tmp[0].path, temporary);
    assert!(preview_scratch.attempt_tmp[0].bytes >= 8192);
    assert!(
        preview_payload.errors.is_empty(),
        "{:?}",
        preview_payload.errors
    );
    assert_eq!(preview_payload.payload.cap_bytes, 16 << 30);
    assert!(temporary.exists());
    let report = collect_provider_state(&roots, &home, &assets, 7, 30, false);
    let scratch = collect_attempt_scratch(&roots, &home, &assets, 7, None, false);
    let payload = collect_payload_cap(None, &roots, &home, &assets, 16 << 30, true, false);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.attempt_tmp.is_empty());
    assert_eq!(report.payload.cap_bytes, 0);
    assert!(scratch.errors.is_empty(), "{:?}", scratch.errors);
    assert_eq!(report.expired_events, preview.expired_events);
    assert_eq!(scratch.attempt_tmp.len(), 1);
    assert_eq!(scratch.attempt_tmp[0].path, temporary);
    assert_eq!(
        scratch.attempt_tmp[0].bytes,
        preview_scratch.attempt_tmp[0].bytes
    );
    assert!(payload.errors.is_empty(), "{:?}", payload.errors);
    assert_eq!(payload.payload.cap_bytes, 16 << 30);
    assert!(!temporary.exists());
    assert!(!candidate.exists());
    let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("run store")?;
    let retained_key = reader.work_key(attempt, &call)?;
    assert_eq!(
        reader.attempt_by_key(&retained_key, &call)?.attempt_id,
        attempt,
        "payload GC retains the attempt under its work key"
    );
    assert_eq!(
        index.read(context.project_id, &graph, &index_call)?,
        association,
        "retained runs keep chair associations"
    );
    let retained = reader.events(0, 64, Some(attempt), &call)?;
    assert_eq!(retained.len(), 1, "run store keeps newest event");
    assert_eq!(
        retained[0].sequence,
        events_before.last().ok_or("last event")?.sequence
    );
    drop(reader);
    assert!(
        child_path
            .join(format!(
                "attempts/{attempt}/provider-history/candidate-0/sessions/session.jsonl"
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
        RetentionPass::ProviderState {
            days: 7,
            logs_days: 30,
        },
        false,
        None,
        Some(&expected),
        &mut report,
    );
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].reason.contains("identity"));
    assert!(moved.join("run.sqlite3").exists());
    Ok(())
}
