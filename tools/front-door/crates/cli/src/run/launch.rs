use super::*;

pub fn preview_project_id(roots: &Roots, cwd: &Path) -> Option<provenant_core::ProjectId> {
    if crate::fabric::in_lane() {
        let gitdir = std::env::var_os("PROVENANT_WORKTREE_GITDIR").map(PathBuf::from)?;
        return project_id_for_lane_gitdir(roots, &gitdir);
    }
    let git = provenant_os::git::git_executable()?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(git).ok()?;
    let executor = provenant_os::exec::SystemExecutor::new(1).ok()?;
    let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5)).ok()?;
    let location = runtime
        .project_location(&executor, cwd, budget, &CancelToken::new())
        .ok()?
        .location;
    provenant_config::lookup_project(roots, &location)
        .ok()?
        .map(|binding| binding.project_id())
}

pub(super) fn project_id_for_lane_gitdir(
    roots: &Roots,
    gitdir: &Path,
) -> Option<provenant_core::ProjectId> {
    let mut matches = provenant_config::registered_projects(roots)
        .ok()?
        .into_iter()
        .filter(|binding| {
            let common = binding.common_dir();
            gitdir == common
                || gitdir
                    .strip_prefix(common.join("worktrees"))
                    .is_ok_and(|rest| {
                        let mut components = rest.components();
                        matches!(components.next(), Some(std::path::Component::Normal(_)))
                            && components.next().is_none()
                    })
        });
    let project = matches.next()?.project_id();
    matches.next().is_none().then_some(project)
}

pub fn nested_dispatch_context(roots: &Roots) -> Result<(u64, Option<AttemptId>), Fail> {
    let parent_depth = environment_text(run::LANE_DEPTH_ENV)?;
    let parent_attempt = environment_text(run::PARENT_ATTEMPT_ENV)?;
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::config(Status::Error, error))?;
    let max_depth = config
        .knobs
        .get("lanes.max_depth")
        .and_then(|knob| knob.value.as_u64())
        .ok_or_else(|| {
            Fail::new(
                Status::Error,
                ErrorKind::Config,
                "lanes.max_depth is missing or invalid",
            )
        })?;
    lane_dispatch_values(
        parent_depth.as_deref(),
        parent_attempt.as_deref(),
        max_depth,
        roots.state.as_path(),
        std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV)
            .as_deref()
            .map(Path::new),
    )
}

pub(super) fn worktree_was_pruned(receipt: &Value, path: &Path) -> bool {
    receipt["state"] == "pruned"
        || receipt["state"] == "pruning"
            && path
                .to_str()
                .is_some_and(|path| receipt["worktree"] == path)
            && std::fs::symlink_metadata(path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

pub(super) fn environment_text(name: &str) -> Result<Option<String>, Fail> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(Fail::new(
            Status::Invalid,
            ErrorKind::LaneContext,
            "lane context environment value is not valid text",
        )),
    }
}

pub(super) fn lane_dispatch_values(
    parent_depth: Option<&str>,
    parent_attempt: Option<&str>,
    max_depth: u64,
    state: &Path,
    run_state: Option<&Path>,
) -> Result<(u64, Option<AttemptId>), Fail> {
    let parent_depth = parent_depth
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| {
            Fail::new(
                Status::Invalid,
                ErrorKind::LaneContext,
                "lane depth environment value must be a non-negative integer",
            )
        })?;
    let parent_attempt = parent_attempt
        .map(AttemptId::parse)
        .transpose()
        .map_err(|_| {
            Fail::new(
                Status::Invalid,
                ErrorKind::ParentAttempt,
                "parent attempt environment value is invalid",
            )
        })?;
    if let Some(run_state) = run_state {
        let invalid = || {
            Fail::new(
                Status::Refused,
                ErrorKind::LaneContext,
                "lane environment must match its host state and include lineage",
            )
        };
        // The marker selects the host, never a writable child database. The
        // relay authenticates lineage from its host-bound attempt socket.
        if run_state != state || parent_depth.is_none() || parent_attempt.is_none() {
            return Err(invalid());
        }
    }
    let parent_depth = match (parent_depth, parent_attempt) {
        (None, None) => 0,
        (Some(depth), Some(_)) if depth >= 1 => depth,
        (Some(_), Some(_)) => {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::LaneContext,
                "nested lane context requires a depth of at least 1",
            ));
        }
        _ => {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::LaneContext,
                "lane depth and parent attempt must be supplied together",
            ));
        }
    };
    let lane_depth = run::next_lane_depth(parent_depth, max_depth)?;
    Ok((lane_depth, parent_attempt))
}

/// Hand every queued attempt of `run` to its own guardian. Fabric's facade
/// shares this path with the CLI leaves.
pub fn launch(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
) -> Result<Vec<run::Started>, Fail> {
    launch_prepared(session, run, edge, call, None)
}

/// Launch the holder-admitted budget continuation with its original host
/// executable authority, after predecessor custody has been released.
pub fn launch_budget_wrap_up(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    host: &run::HostLaunchContext,
) -> Result<Vec<run::Started>, Fail> {
    launch_on_host_with_wait(session, run, edge, call, Some(host), None, None)
}

/// The detached launcher's entry point. It reopens the admitted run and uses
/// the ordinary host launch path, including writer custody and guardian handoff.
pub fn launch_worker(id: RunId, provider_lease_wait: Option<u64>, edge: &Edge<'_>) -> Reply {
    let outcome = (|| {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel)?;
        let session = open(edge, &call)?;
        let started =
            launch_prepared_with_wait(&session, id, edge, &call, None, provider_lease_wait)?;
        Ok((json!({"started": started}), String::new()))
    })();
    reply("internal launch", outcome)
}

/// A waited caller gives launch to a separately reaped process, so returning
/// cannot drop a writer lease while preparation is still in progress.
pub(super) fn launch_with_deadline(
    session: &Session,
    record: &run::RunRecord,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    wait: Duration,
) -> Result<Vec<run::Started>, Fail> {
    let executable = std::env::current_exe()
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    let argv = vec![
        executable.into_os_string(),
        "--state-dir".into(),
        edge.roots.state.as_path().as_os_str().into(),
        "internal".into(),
        "launch".into(),
        record.run_id.to_string().into(),
        // The caller's wait also bounds a wait for an updating provider.
        "--provider-lease-wait".into(),
        wait.as_secs().max(1).to_string().into(),
    ];
    // Process startup and each admitted task need a usable receipt window
    // even when the requested observation wait is very short.
    let task_seconds = u64::try_from(record.task_ids.len()).unwrap_or(u64::MAX);
    let preparation_budget =
        Duration::from_secs(5_u64.saturating_add(task_seconds.saturating_sub(1)));
    let result =
        provenant_os::exec::detached_receipt(&argv, edge.cwd, wait.max(preparation_budget));
    // Unit tests run inside a test harness, which cannot re-execute itself as
    // the CLI. End-to-end tests exercise the actual worker process.
    #[cfg(any(test, feature = "fixture"))]
    if matches!(result, Err(provenant_os::exec::ExecError::TestHarness)) {
        return launch_prepared(session, record.run_id, edge, call, None);
    }
    let _ = (session, call);
    let receipt =
        result.map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    let unknown = || {
        Fail::new(
            Status::Error,
            ErrorKind::LaunchUnknown,
            format!(
                "launch preparation: outcome unknown; rerun with --operation-id {} to recover the receipt",
                record.operation_id
            ),
        )
    };
    // The worker holds custody and keeps preparing after this wait ends, so a
    // launch that outlasts the wait is a lane still waiting, not a failure. The
    // caller reports the lane's current phase from the store.
    let Some(receipt) = receipt else {
        return Ok(Vec::new());
    };
    let value: Value = serde_json::from_slice(&receipt).map_err(|_| unknown())?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let kind = error["code"]
            .as_str()
            .and_then(|code| {
                ErrorKind::ALL
                    .iter()
                    .copied()
                    .find(|kind| kind.code() == code)
            })
            .unwrap_or(ErrorKind::Io);
        let status = match kind.category() {
            "invalid" => Status::Invalid,
            "refused" => Status::Refused,
            _ => Status::Error,
        };
        return Err(Fail::new(
            status,
            kind,
            error["message"].as_str().unwrap_or("launch worker failed"),
        ));
    }
    serde_json::from_value(value["data"]["started"].clone()).map_err(|_| unknown())
}

pub(super) fn launch_prepared(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    ready: Option<&run::PreparedStart>,
) -> Result<Vec<run::Started>, Fail> {
    launch_prepared_with_wait(session, run, edge, call, ready, None)
}

pub(super) fn launch_prepared_with_wait(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    ready: Option<&run::PreparedStart>,
    provider_lease_wait: Option<u64>,
) -> Result<Vec<run::Started>, Fail> {
    // Every lane-facing launch path is submission-only. Only its host holder
    // prepares providers, including credentials and the child's OS profile.
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() {
        return Ok(Vec::new());
    }
    launch_on_host_with_wait(session, run, edge, call, None, ready, provider_lease_wait)
}

pub(super) struct PreparedProvider<'a> {
    fallback: &'a Registry<'a>,
    ready: Option<&'a run::PreparedStart>,
    provider_lease_wait: Option<u64>,
}

impl run::Providers for PreparedProvider<'_> {
    fn write_grant_limit(&self) -> run::WriteGrantLimit {
        self.fallback.write_grant_limit()
    }
    fn priority(&self, task: &run::TaskRequest) -> Result<u8, String> {
        self.fallback.priority(task)
    }
    fn check_policy(&self, task: &run::TaskRequest) -> Result<(), run::PrepareRefusal> {
        if let Some(ready) = self.ready {
            if ready.task != *task {
                return Err(run::PrepareRefusal::new(
                    provenant_records::run::TerminalCause::HandoffRefused,
                    "prepared continuation task changed",
                ));
            }
            return Ok(());
        }
        self.fallback.check_policy(task)
    }
    fn prepare(
        &self,
        task: &run::TaskRequest,
        prompt: &str,
        paths: &run::LaunchPaths,
        resume: Option<&run::AttemptRecord>,
    ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
        if let Some(ready) = self.ready {
            if ready.attempt != paths.attempt_id || ready.task != *task || resume.is_some() {
                return Err(run::PrepareRefusal::new(
                    provenant_records::run::TerminalCause::HandoffRefused,
                    "prepared continuation identity changed",
                ));
            }
            return ready.plan.borrow_mut().take().ok_or_else(|| {
                run::PrepareRefusal::new(
                    provenant_records::run::TerminalCause::HandoffRefused,
                    "prepared continuation consumed",
                )
            });
        }
        self.fallback
            .prepare_with_wait(task, prompt, paths, resume, self.provider_lease_wait)
    }
    fn decode(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
    ) -> Result<run::Decoded, String> {
        self.fallback.decode(route, stdout, exit, provider_state)
    }
    fn failure_diagnostic(&self, route: &run::RouteRecord, stderr: &[u8]) -> Option<String> {
        self.fallback.failure_diagnostic(route, stderr)
    }
    fn decode_captures(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        stderr: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
        context: Option<&run::HolderContext>,
    ) -> Result<run::Decoded, String> {
        self.fallback
            .decode_captures(route, stdout, stderr, exit, provider_state, context)
    }
}

pub(super) fn with_chair_index<T>(
    roots: &Roots,
    call: &run::Request<'_>,
    action: impl FnOnce(
        &provenant_fabric_runtime::chairs::ChairIndex,
        &provenant_fabric_runtime::chairs::Request<'_>,
    ) -> Result<T, provenant_fabric_runtime::chairs::Error>,
) -> Result<T, Fail> {
    use provenant_fabric_runtime::chairs::{ChairIndex, Request};
    let index_call = Request {
        clock: call.clock,
        cancel: call.cancel,
    };
    ChairIndex::open(&roots.state, &index_call)
        .and_then(|index| action(&index, &index_call))
        .map_err(|error| {
            Fail::new(
                match &error {
                    provenant_fabric_runtime::chairs::Error::Conflict
                    | provenant_fabric_runtime::chairs::Error::NotFound
                    | provenant_fabric_runtime::chairs::Error::Invalid(_) => Status::Refused,
                    _ => Status::Error,
                },
                match &error {
                    provenant_fabric_runtime::chairs::Error::Conflict => ErrorKind::Conflict,
                    provenant_fabric_runtime::chairs::Error::NotFound => ErrorKind::NotFound,
                    provenant_fabric_runtime::chairs::Error::Invalid(_) => ErrorKind::Refused,
                    provenant_fabric_runtime::chairs::Error::Busy => ErrorKind::Contention,
                    provenant_fabric_runtime::chairs::Error::Cancelled => ErrorKind::Cancelled,
                    _ => ErrorKind::Fabric,
                },
                format!("chair association: {error}"),
            )
        })
}

/// Resolve the graph label for a run at the CLI boundary. The run store still
/// exposes its short-lived run adapter in a few launch APIs; chair records use
/// the attempt's canonical work key.
pub(super) fn graph_for_run(
    session: &Session,
    run: RunId,
    call: &run::Request<'_>,
) -> Result<String, Fail> {
    let page = session.store.lanes(
        &run::LaneQuery {
            targets: vec![run::LaneTarget {
                run,
                task: None,
                attempt: None,
            }],
            all: true,
            limit: 1,
            after: 0,
            newest_first: false,
            owner: None,
            unseen_after: None,
        },
        call,
    )?;
    let attempt = page.lanes.first().ok_or(run::RunError::NotFound)?;
    Ok(session
        .store
        .work_key(attempt.attempt_id, call)?
        .graph()
        .to_owned())
}

fn graph_for_attempt(
    session: &Session,
    attempt: AttemptId,
    call: &run::Request<'_>,
) -> Result<String, Fail> {
    Ok(session.store.work_key(attempt, call)?.graph().to_owned())
}

pub(super) fn seal_launch_task(
    request_digest: provenant_core::Digest,
    task: &run::TaskRequest,
) -> run::SealedTask {
    run::SealedTask {
        request_digest,
        task_id: task.task_id,
        effective_network: task.access.network(task.writer.is_some()),
        effective_database_ipc: task.access.database_ipc,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn launch_on_host_with_wait(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
    ready: Option<&run::PreparedStart>,
    provider_lease_wait: Option<u64>,
) -> Result<Vec<run::Started>, Fail> {
    let inherited_host = host.is_some();
    let host = match host {
        Some(host) => host.clone(),
        None => run::HostLaunchContext {
            graph_directory: None,
            configuration_root: edge.roots.state.as_path().into(),
            cwd: edge.cwd.into(),
            executables: std::collections::BTreeMap::new(),
            executable_search_path: std::env::var_os("PATH")
                .map(|path| {
                    std::env::split_paths(&path)
                        .filter(|path| path.is_absolute())
                        .collect()
                })
                .unwrap_or_default(),
            git: provenant_os::git::git_executable()
                .ok_or_else(|| Fail::new(Status::Refused, ErrorKind::Git, "Git unavailable"))?,
            default_account_directories: run::HostLaunchContext::default_account_directories(),
            credential_roots: run::HostLaunchContext::credential_roots(),
            github_roots: run::HostLaunchContext::github_roots(),
            provider_path_environment: run::HostLaunchContext::provider_path_environment(),
            cargo_build_jobs: std::env::var("CARGO_BUILD_JOBS").ok(),
            #[cfg(feature = "fixture")]
            fixture_state_write: false,
        },
    };
    if session.context.state_root != edge.roots.state {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::Refused,
            "launch requires the host run store",
        ));
    }
    let read = session.store.request(run, call)?;
    let request = &read;
    let request_digest = request.digest().map_err(run::RunError::from)?;

    {
        let graph = graph_for_run(session, run, call)?;
        with_chair_index(edge.roots, call, |index, index_call| {
            if let Some(prior) = request.parent {
                let parent = session.store.record(prior, call).map_err(|_| {
                    provenant_fabric_runtime::chairs::Error::Invalid("continuation parent")
                })?;
                let original = session.store.request(parent.run_id, call).map_err(|_| {
                    provenant_fabric_runtime::chairs::Error::Invalid("continuation request")
                })?;
                let parent_graph = graph_for_attempt(session, prior, call).map_err(|_| {
                    provenant_fabric_runtime::chairs::Error::Invalid("continuation work key")
                })?;
                index.inherit(
                    session.context.project_id,
                    (&parent_graph, original.digest()?),
                    &graph,
                    request,
                    index_call,
                )
            } else {
                match index.read(session.context.project_id, &graph, index_call)? {
                    Some(value) if value.request_digest != request_digest => {
                        Err(provenant_fabric_runtime::chairs::Error::Conflict)
                    }
                    // Only the host root's immutable request may repair an
                    // interrupted submission, including owned-worktree replay.
                    None => index.submit(session.context.project_id, &graph, request, index_call),
                    _ => Ok(()),
                }
            }
        })?;
    }
    let seed = run::RootSeed {
        home: edge.home.into(),
        checkout: edge.assets.into(),
        state_root: session.context.state_root.as_path().into(),
    };
    let holder = |attempt: &run::AttemptRecord, requester| {
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == attempt.task_id)
            .ok_or(run::RunError::Validation(
                "attempt task missing from request",
            ))?;
        let mut host = host.clone();
        if !inherited_host {
            // Called after preparation, while its selected provider lease is held.
            for registration in REGISTRY {
                let name = registration.adapter.id();
                if let Ok(path) = provider::find_executable(name) {
                    host.executables.insert(name.into(), path);
                }
            }
        }
        if let Some(writer) = request
            .tasks
            .iter()
            .find(|task| task.task_id == attempt.task_id)
            .and_then(|task| task.writer.as_ref())
        {
            host.cwd = writer.worktree.clone();
            provenant_confinement::worktrees::with_context(
                edge.roots,
                edge.home,
                &writer.project,
                call.clock,
                call.cancel,
                |context| {
                    use provenant_confinement::worktrees;
                    let tree =
                        provenant_confinement::verify_worktree(context.repo, &writer.worktree)?;
                    if let Some(mut owner) = worktrees::load_owner(&tree)?
                        && owner.origin == worktrees::WorktreeOrigin::Lane
                        && (owner.operation_id.as_ref() == Some(&attempt.operation_id)
                            || owner.attempt_id.is_some() && owner.attempt_id == attempt.retry_of)
                    {
                        let expected = owner.clone();
                        owner.attempt_id = Some(attempt.attempt_id);
                        owner.operation_id = Some(attempt.operation_id.clone());
                        worktrees::update_owner_when_unchanged(
                            context,
                            &tree,
                            &expected,
                            &owner,
                            call.cancel,
                        )?;
                    }
                    Ok(())
                },
            )
            .map_err(run::RunError::from)?;
        }
        run::HolderContext {
            schema_version: 1,
            roots: seed.clone(),
            project_id: attempt.project_id,
            actor_id: requester,
            run_id: attempt.run_id,
            attempt_id: attempt.attempt_id,
            host_delivery: request
                .tasks
                .iter()
                .find(|task| task.task_id == attempt.task_id)
                .and_then(|task| task.deliver.as_ref())
                .map(|_| run::HostDeliveryContext::capture()),
            launch: Some(host),
            sealed: Some(seal_launch_task(
                request_digest,
                &session.store.effective_launch_task(request, task),
            )),
        }
        .encode()
    };
    let registry = Registry {
        roots: Some(edge.roots),
        home: Some(edge.home),
        assets: Some(edge.assets),
        cwd: Some(edge.cwd),
        state_root: Some(session.context.state_root.as_path()),
        project_id: Some(session.context.project_id),
        // Top-level discovery runs in prepare under its lease. Child launches
        // continue to use their sealed host executable authority.
        host: inherited_host.then_some(&host),
        cancel: Some(call.cancel),
        repo_facts: session.store.repo_facts().clone(),
    };
    let providers = PreparedProvider {
        fallback: &registry,
        ready,
        provider_lease_wait,
    };
    // Fixture guardians observe a recorded boot key, so the reservation must be
    // taken against the same key as on the lane-launch path of the test host.
    #[cfg(feature = "fixture")]
    if host.fixture_state_write {
        let started = session.store.start_with_writer_lease(
            run,
            &providers,
            &holder,
            &|directory, scope, binding| {
                provenant_supervisor::fixture::acquire_writer_with_binding(
                    directory,
                    scope,
                    binding,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
            },
            Some(request),
            call,
        )?;
        check_launch_policy(&started)?;
        return Ok(started);
    }
    let started = session
        .service()
        .start(run, &providers, &holder, request, call)?;
    check_launch_policy(&started)?;
    Ok(started)
}

pub(super) fn launch_unknown(started: &[run::Started]) -> Vec<String> {
    started
        .iter()
        .filter_map(|s| match s {
            run::Started::Unknown { attempt, error } => Some(format!("{attempt}: {error}")),
            _ => None,
        })
        .collect()
}

pub(super) fn check_launch_policy(started: &[run::Started]) -> Result<(), Fail> {
    if !started.is_empty()
        && started.iter().all(|item| {
            matches!(
                item,
                run::Started::Refused { .. } | run::Started::Blocked { .. }
            )
        })
        && let Some((attempt, error)) = started.iter().find_map(|item| match item {
            run::Started::Blocked { attempt, error } => Some((attempt, error)),
            _ => None,
        })
    {
        let reason = format!("{attempt}: launch blocked: {error}");
        let mut failure = Fail::new(Status::Refused, ErrorKind::Refused, &reason);
        failure.detail = Some(Box::new(
            provenant_fabric_runtime::coordination::facade::FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason,
            },
        ));
        return Err(failure);
    }
    // Preserve the full receipt whenever a sibling can still make progress
    // or needs unknown-handoff recovery, including Fabric's operation replay.
    if started
        .iter()
        .any(|started| !matches!(started, run::Started::Refused { .. }))
    {
        return Ok(());
    }
    if let Some((run, attempt, worktree)) = started.iter().find_map(|started| match started {
        run::Started::Refused {
            kind:
                run::RefusalKind::RetainedImport {
                    run,
                    attempt,
                    worktree,
                },
            ..
        } => Some((*run, *attempt, worktree.clone())),
        _ => None,
    }) {
        return Err(run::RunError::RetainedWriterImport {
            run,
            attempt,
            worktree,
        }
        .into());
    }
    // Report any all-refused batch using its typed diagnostic where available.
    if let Some((attempt, reason, kind)) = started.iter().find_map(|started| match started {
        run::Started::Refused {
            attempt,
            reason,
            kind,
        } => Some((attempt, reason, kind)),
        _ => None,
    }) {
        let mut fail = Fail::new(
            Status::Refused,
            ErrorKind::Refused,
            format!("{attempt}: {reason}"),
        );
        if let Some(mut projection) = kind.diagnostic() {
            projection.code = ErrorKind::Refused.code().into();
            fail.projection = Some(Box::new(projection));
        }
        return Err(fail);
    }
    Ok(())
}
