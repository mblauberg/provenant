use super::*;

/// The main CLI path reuses admission, managed worktrees and bounded waits.
pub fn run(mut command: Dispatch, edge: &Edge<'_>) -> Reply {
    if let Err(fail) = require_explicit_target(&command, edge.cwd) {
        return reply("run", Err(fail));
    }
    if command.host.is_none()
        && let Some(path) = &command.access.worktree
    {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            edge.cwd.join(path)
        };
        if !absolute.exists() {
            if path.is_absolute() || path.starts_with(".worktrees") {
                return reply(
                    "run",
                    Err(Fail::new(
                        Status::Invalid,
                        ErrorKind::Invalid,
                        format!(
                            "worktree {} does not exist; use --branch BRANCH to create an owned worktree",
                            path.display()
                        ),
                    )),
                );
            }
            command.worktree_branch = Some(path.to_string_lossy().into_owned());
            command.access.worktree = None;
        }
    }
    if command.route.effort.is_none()
        && let Some(model) = command.route.model.as_ref()
        && !model.0.contains('@')
        && let Ok(id) = ModelId::parse(&model.0)
    {
        let config =
            match provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS) {
                Ok(config) => config,
                Err(error) => {
                    return reply(
                        "run",
                        Err(Fail::new(
                            Status::Error,
                            ErrorKind::Config,
                            error.to_string(),
                        )),
                    );
                }
            };
        if let Ok(catalogue) = provenant_routing::Catalogue::bundled() {
            command.route.effort = catalogue.model_default_effort(&config.domains.routing, &id);
        }
    }
    render_run(dispatch(command, edge), edge)
}

// A run that names neither a mode nor a target, from a linked worktree, is the
// ambiguous case: the agent may have forgotten `--worktree`. Refuse before
// dispatch so it cannot silently become a read-only lane. An explicit mode
// (including `--mode read-only`) or a target is an intent and passes.
pub(super) fn require_explicit_target(command: &Dispatch, cwd: &Path) -> Result<(), Fail> {
    if command.access.mode.is_some()
        || command.access.worktree.is_some()
        || command.branch.is_some()
        || command.worktree_branch.is_some()
    {
        return Ok(());
    }
    require_worktree_target(cwd)
}

pub(super) fn require_worktree_target(cwd: &Path) -> Result<(), Fail> {
    let cwd = provenant_os::fs::canonicalize(cwd)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Invalid, error.to_string()))?;
    for path in cwd.ancestors() {
        match std::fs::symlink_metadata(path.join(".git")) {
            Ok(marker) if marker.is_file() => {
                if !is_linked_gitfile(path)? {
                    return Ok(());
                }
                return Err(Fail::new(
                    Status::Refused,
                    ErrorKind::Refused,
                    format!(
                        "run from a linked worktree needs an explicit target: use --worktree {} for a writer, or --mode read-only for a reader",
                        path.display()
                    ),
                ));
            }
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Fail::new(
                    Status::Error,
                    ErrorKind::Invalid,
                    error.to_string(),
                ));
            }
        }
    }
    Ok(())
}

// Git writes `gitdir: <common>/worktrees/<name>` into a linked checkout's
// gitfile. A submodule's gitfile names its own `.git/modules/<name>` instead.
pub(super) fn is_linked_gitfile(checkout: &Path) -> Result<bool, Fail> {
    let contents = std::fs::read_to_string(checkout.join(".git"))
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Invalid, error.to_string()))?;
    let Some(gitdir) = contents
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
    else {
        return Ok(false);
    };
    // A relative gitdir is resolved against the checkout that holds the gitfile.
    let gitdir = checkout.join(gitdir.trim());
    Ok(gitdir.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("worktrees")))
}

/// The full target of a lane this run just reported, read from its row.
fn lane_target(row: &Value) -> Option<Target> {
    Some(Target {
        selection: None,
        run: Some(row["run_id"].as_str()?.parse().ok()?),
        task: Some(row["task_id"].as_str()?.parse().ok()?),
        attempt: Some(row["attempt_id"].as_str()?.parse().ok()?),
    })
}

/// The current project's run store, opened read-only; `None` sends each lane
/// through the general `output` path, which reports the failure itself.
fn local_session(edge: &Edge<'_>) -> Option<Session> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).ok()?;
    let context = execution_context(edge).ok()?;
    let store = run::RunStore::open_read_only(&context, &call)
        .ok()??
        .with_writer_registry(edge.roots.state.as_path());
    Some(Session { store, context })
}

pub(super) fn render_run(mut result: Reply, edge: &Edge<'_>) -> Reply {
    result.result.command = "run".into();
    if result.result.error.is_some() {
        return result;
    }
    let rows = result.result.data["lanes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        // A route explanation has no lane rows; keep the preview text returned
        // by dispatch instead of rendering an empty run summary over it.
        return result;
    }
    let mut human = format!(
        "operation {}\n",
        result.result.data["operation_id"]
            .as_str()
            .unwrap_or("unknown")
    );
    if let (Some(path), Some(branch)) = (
        result.result.data["worktree"]["path"].as_str(),
        result.result.data["worktree"]["branch"].as_str(),
    ) {
        human.push_str(&format!(
            "worktree {} ({})\n",
            plain_text(path, usize::MAX),
            plain_text(branch, usize::MAX)
        ));
    }
    let timed_out = result.result.status == Status::Timeout;
    let mut next = None;
    let mut wait = None;
    // Every lane's result tail reads the same project's store, so resolve the
    // project and open it once for the whole run rather than once per lane.
    let tails = std::cell::OnceCell::new();
    for row in rows {
        let Some(id) = row["attempt_id"].as_str() else {
            continue;
        };
        let state = row["state"].as_str().unwrap_or("unknown");
        human.push_str(&format!(
            "{} {state} {} (effort: {}, network: {})\n",
            &id[..id.len().min(8)],
            row["applied_route"]["model"]
                .as_str()
                .unwrap_or("unresolved"),
            row["applied_route"]["effort"].as_str().unwrap_or(
                if row["applied_route"].is_object() {
                    "provider default"
                } else {
                    "unresolved"
                }
            ),
            row["network"].as_str().unwrap_or("unknown")
        ));
        if let Some(reason) = row["reason"].as_str() {
            human.push_str(&format!("{}\n", plain_text(reason, usize::MAX)));
        }
        if (row["result_path"].is_string()
            || (row["host"].is_string()
                && !matches!(state, "queued" | "starting" | "running" | "settling")))
            && let Ok(attempt) = id.parse()
        {
            let target = if row["result_path"].is_string() {
                lane_target(&row)
            } else {
                None
            };
            let local = target.and_then(|target| {
                tails
                    .get_or_init(|| local_session(edge))
                    .as_ref()
                    .map(|session| (session, target))
            });
            let tail = output_with(
                Output {
                    attempt,
                    host: None,
                    part: Part::Result,
                    offset: 0,
                    tail: Some(20),
                    max_bytes: 8192,
                    trace: false,
                    to: None,
                    path: false,
                },
                edge,
                local.as_ref().map(|(session, target)| (*session, target)),
            );
            if tail.result.error.is_none() {
                human.push_str(&tail.human);
                if !human.ends_with('\n') {
                    human.push('\n');
                }
                result.result.data["result_tails"][id] = tail.result.data;
            } else {
                human.push_str(&format!(
                    "result unavailable; run `provenant output {id}`\n"
                ));
            }
        }
        // The first lane still in flight when the wait ends names the wait to keep.
        if timed_out
            && wait.is_none()
            && matches!(state, "queued" | "starting" | "running" | "settling")
        {
            wait = Some(format!("provenant lanes --id {id} --wait 4m"));
        }
        // One typed next step: the first lane that needs one.
        if next.is_none() {
            if timed_out && matches!(state, "queued" | "starting" | "running" | "settling") {
                next = Some(format!("provenant lanes --id {id} --wait 4m"));
            } else if state == "input_required" {
                next = Some(format!("provenant resume {id} --prompt-file ANSWER"));
            } else if !matches!(state, "succeeded") {
                next = Some(format!("provenant output {id}"));
            }
        }
        if !timed_out
            && !row["superseded_by"].is_string()
            && !matches!(state, "succeeded" | "review_required" | "input_required")
        {
            result.result.status = Status::Failed;
        }
    }
    if timed_out {
        // The wait ended before the run settled. Its lanes keep running, so
        // this is not a failure: exit 0, and name the command that keeps waiting.
        result.result.status = Status::Waiting;
        if let Some(command) = &wait {
            human.push_str(&format!(
                "run is still running; keep waiting with `{command}`\n"
            ));
        }
    }
    human.push_str(&warnings_text(&result.result.data));
    result.summary = Some(crate::Summary::verbatim(&human, next));
    result.human = human;
    result.read_only = false;
    result
}

pub fn dispatch(command: Dispatch, edge: &Edge<'_>) -> Reply {
    dispatch_with_workers(
        command,
        edge,
        &provenant_confinement::worktrees::SystemWorkerObserver,
    )
}

pub(super) fn dispatch_with_workers(
    command: Dispatch,
    edge: &Edge<'_>,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Reply {
    let operation_id = command.operation_id.clone();
    let mut created = None;
    let mut result = dispatch_inner(command, edge, &mut created, workers);
    if let (Err(fail), Some((path, branch))) = (&mut result, created) {
        // A failed launch can follow admission. Those trees belong to the
        // accepted run and must remain available for resume.
        let cleanup = (|| -> Result<(), Fail> {
            let workers = provenant_confinement::worktrees::CensusPolicyWorkers::from_roots(
                workers, edge.roots,
            )
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
            let clock = SystemClock::new();
            let census = provenant_confinement::worktrees::CensusSession::new(&workers, &clock);
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let session = open(edge, &call)?;
            if session.store.operation(&operation_id, &call)?.is_some() {
                return Ok(());
            }
            provenant_confinement::worktrees::with_context_using_workers(
                edge.roots,
                edge.home,
                edge.cwd,
                &clock,
                &cancel,
                &census,
                |context| {
                    use provenant_confinement::worktrees;
                    let info = worktrees::list(context, &cancel)?
                        .into_iter()
                        .find(|tree| tree.path == path)
                        .ok_or(worktrees::WorktreeError::NotFound)?;
                    if info.dirty != worktrees::WorktreeCondition::Clean
                        || info.writer != worktrees::WriterState::Free
                    {
                        return Err(worktrees::WorktreeError::Dirty(String::new()));
                    }
                    let tree = provenant_confinement::verify_worktree(context.repo, &path)?;
                    if worktrees::load_owner(&tree)?.is_none_or(|owner| {
                        owner.origin != worktrees::WorktreeOrigin::Lane
                            || owner.operation_id.as_ref() != Some(&operation_id)
                    }) {
                        return Err(worktrees::WorktreeError::UnknownWriter);
                    }
                    worktrees::remove(
                        context,
                        worktrees::WorktreeRemove::Name {
                            name: branch,
                            discard: false,
                            integration: None,
                            expected_head: Some(info.head),
                        },
                        &cancel,
                    )?;
                    Ok(())
                },
            )
            .map_err(run::RunError::from)?;
            Ok(())
        })();
        if let Err(error) = cleanup {
            fail.message.push_str(&format!(
                "; new managed worktree retained for gc at {}: {}",
                path.display(),
                error.message
            ));
        }
    }
    let mut out = reply_waited("dispatch", result);
    operation_reply(&mut out, &operation_id);
    out
}

pub(super) fn operation_reply(reply: &mut Reply, id: &OperationId) {
    if reply.result.data.is_null() {
        reply.result.data = json!({});
    }
    reply.result.data["operation_id"] = json!(id);
    if let Some(error) = &mut reply.result.error {
        error.message.push_str(&format!("; operation {id}"));
        // The same operation replays its request, so a retry cannot admit twice.
        if error.code == ErrorKind::Contention.code() {
            error.repair = Some(format!("retry the same command with --operation-id {id}"));
        }
    } else {
        reply.human = format!("operation {id}\n{}", reply.human);
    }
}

pub(super) fn check_delivery_branch(project: &Path, branch: &str) -> Result<(), Fail> {
    run::HostDeliveryContext::check_delivery_branch(project, branch)
        .map_err(|error| Fail::new(Status::Refused, ErrorKind::Refused, error.to_string()))
}

/// Refuse flags outside the named adapters' launch tables before the first
/// side effect, including `--worktree-branch` creation. Manifest tasks and
/// named routes are checked again where their adapter is known.
pub(super) fn refused(error: provenant_records::UnsupportedLaunch) -> Fail {
    Fail::new(Status::Refused, ErrorKind::Unsupported, error.to_string())
}

/// A pool is refused an effort only when no member's adapter can take it.
pub(crate) fn pool_effort_refusal(
    adapters: &[&str],
    effort: &str,
) -> Option<provenant_records::UnsupportedLaunch> {
    let request = provenant_records::LaunchRequest {
        model: None,
        effort: Some(effort),
        ..Default::default()
    };
    let refusals: Vec<_> = adapters
        .iter()
        .filter_map(|name| provenant_records::AdapterSpec::get(name))
        .map(|spec| spec.check_launch(&request))
        .collect();
    let refused: Vec<_> = refusals.into_iter().filter_map(Result::err).collect();
    let known = adapters
        .iter()
        .filter(|name| provenant_records::AdapterSpec::get(name).is_some())
        .count();
    (known > 0 && refused.len() == known)
        .then(|| refused.into_iter().next())
        .flatten()
}

/// Refuse a model the catalogue does not record, or an effort its recorded
/// model does not take, before any run record exists. A typo gets the nearest
/// recorded IDs. Routing applies the same rules, but only after launch prep.
pub(super) fn catalogued(model: &str, effort: Option<&str>) -> Result<(), Fail> {
    let Ok(id) = ModelId::parse(model) else {
        return Ok(());
    };
    let catalogue = provenant_routing::Catalogue::bundled()
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
    let Some(entry) = catalogue.model(&id) else {
        let nearest = catalogue.nearest_models(&id);
        let hint = if nearest.is_empty() {
            String::new()
        } else {
            format!("; nearest catalogued IDs: {}", nearest.join(", "))
        };
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            format!("model {model} is not in the catalogue{hint}"),
        ));
    };
    match effort {
        Some(effort) if !entry.efforts.iter().any(|supported| supported == effort) => {
            let support = if entry.efforts.is_empty() {
                "this model takes no effort".to_owned()
            } else {
                format!("supported efforts: {}", entry.efforts.join(", "))
            };
            Err(Fail::new(
                Status::Refused,
                ErrorKind::Unsupported,
                format!("model {model} does not take effort {effort}; {support}"),
            ))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
pub(crate) fn check_flag_capabilities(
    command: &Dispatch,
    roots: Option<&Roots>,
) -> Result<(), Fail> {
    let has_writes = run::Service::has_writes(
        matches!(command.access.mode(), Mode::WorktreeWrite),
        &command.access.write_root,
    );
    check_flag_capabilities_with_writes(command, roots, has_writes)
}

pub(super) fn check_flag_capabilities_with_writes(
    command: &Dispatch,
    roots: Option<&Roots>,
    has_writes: bool,
) -> Result<(), Fail> {
    let route = requested(
        command.route.model.as_ref().map(|m| m.0.as_str()),
        command.route.route.as_deref(),
        command.route.effort.clone(),
    );
    let postgres = command
        .access
        .capability
        .iter()
        .any(|name| name == "postgres");
    let flag_network = network_access(command.access.network, postgres);
    let check = |provider: &str,
                 model: Option<&str>,
                 effort: Option<&str>,
                 network: Option<run::NetworkAccess>| {
        let Some(spec) = provenant_records::AdapterSpec::get(provider) else {
            return Ok(());
        };
        spec.check_launch(&provenant_records::LaunchRequest {
            model,
            effort,
            resume: false,
            network,
            write: has_writes,
        })
        .map_err(refused)
    };
    let check_route =
        |route: run::RequestedRoute, network: Option<run::NetworkAccess>| -> Result<(), Fail> {
            match run::Service::route(route) {
                Ok(run::RequestedRoute::Adapter { provider, .. }) => {
                    check(&provider, None, None, network)?
                }
                Ok(run::RequestedRoute::Model {
                    provider,
                    model,
                    effort,
                    ..
                }) => {
                    check(&provider, Some(&model), effort.as_deref(), network)?;
                    catalogued(&model, effort.as_deref())?;
                }
                // A pool whose every member's adapter takes no effort cannot serve one.
                Ok(run::RequestedRoute::Named {
                    name,
                    effort: Some(effort),
                    ..
                }) => {
                    if let Some(roots) = roots
                        && let Ok(members) = crate::route::effective_members(roots, &name)
                    {
                        let adapters: Vec<&str> = members
                            .iter()
                            .map(|member| member.model.adapter())
                            .collect();
                        pool_effort_refusal(&adapters, &effort)
                            .map_or(Ok(()), |error| Err(refused(error)))?;
                    }
                }
                _ => {}
            }
            Ok(())
        };
    let check_fallback = |model: &str,
                          effort: Option<&str>,
                          network: Option<run::NetworkAccess>|
     -> Result<(), Fail> {
        catalogued(model, None)?;
        match model.split_once('/') {
            Some((provider, _)) => check(provider, Some(model), effort, network),
            None => Ok(()),
        }
    };
    let effort_of = |route: &run::RequestedRoute| match route {
        run::RequestedRoute::Adapter { .. } => None,
        run::RequestedRoute::Model { effort, .. }
        | run::RequestedRoute::Named { effort, .. }
        | run::RequestedRoute::Review { effort, .. } => effort.clone(),
    };
    if let Ok(route) = route {
        let effort = effort_of(&route);
        check_route(route, flag_network)?;
        for model in &command.route.fallback {
            check_fallback(&model.0, effort.as_deref(), flag_network)?;
        }
    }
    // Manifest tasks carry their own routes; read them before any worktree exists.
    // A malformed manifest is reported later by the normal path.
    if let Some(path) = &command.input.manifest
        && let Ok(text) = read_bounded(path)
        && let Ok(manifest) = provenant_records::decode_v1::<Manifest, _>(
            text.as_bytes(),
            provenant_records::Limits {
                max_bytes: PROMPT_LIMIT,
                ..Default::default()
            },
        )
    {
        for task in manifest.tasks {
            let effort = effort_of(&task.route);
            let network = network_access(task.network, postgres).or(flag_network);
            check_route(task.route, network)?;
            for model in &task.fallback {
                check_fallback(model, effort.as_deref(), network)?;
            }
        }
    }
    Ok(())
}

enum WorktreeAdmissionError {
    Own(worktree::OwnWorktreeError),
    Run(run::RunError),
    Fail(Fail),
}

impl WorktreeAdmissionError {
    fn is_transient(&self) -> bool {
        use provenant_confinement::worktrees::WorktreeError;
        match self {
            Self::Own(worktree::OwnWorktreeError::Other(run::RunError::Writer(err)))
            | Self::Run(run::RunError::Writer(err)) => matches!(
                err,
                WorktreeError::RegistryLockTimeout | WorktreeError::UnknownWriter
            ),
            _ => false,
        }
    }
}

impl From<WorktreeAdmissionError> for Fail {
    fn from(error: WorktreeAdmissionError) -> Self {
        match error {
            WorktreeAdmissionError::Own(err) => err.into(),
            WorktreeAdmissionError::Run(err) => err.into(),
            WorktreeAdmissionError::Fail(fail) => fail,
        }
    }
}

pub(super) fn dispatch_inner(
    mut command: Dispatch,
    edge: &Edge<'_>,
    created: &mut Option<(PathBuf, String)>,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<(Value, String, bool), Fail> {
    // Explain is a preview-only operation. Return before worktree preflight,
    // operation replay, or any preparation that could lead to submission.
    if command.explain {
        if command.input.manifest.is_some() {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                "--explain applies to one --route or --model, or to --preset",
            ));
        }
        let run_config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
            .map(|config| config.domains.run)
            .unwrap_or_default();
        let project = preview_project_id(edge.roots, edge.cwd);
        let configured_admission = project
            .as_ref()
            .and_then(|id| run_config.admission_seconds_for(&id.to_string()))
            .or(run_config.admission_seconds);
        let effective_admission =
            run::AdmissionPolicy::resolve_timeout(command.admission_timeout, configured_admission);
        // The brief scan only reads the prompt file and resolves the roots; it
        // never prepares anything that could lead to a submission.
        let warnings = match command.input.prompt_file.as_deref() {
            Some(path) => {
                let prompt = read_bounded(path)?;
                let access = lane_access(&command.access, edge.roots.state.as_path(), edge.home)?;
                if brief_read_root_warnings_enabled(&run_config, edge.roots, edge.cwd) {
                    brief_read_root_warnings(
                        &prompt,
                        edge.cwd,
                        command.access.worktree.as_deref(),
                        &access.read_roots,
                        edge.home,
                    )
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        };
        return crate::route::explain_dispatch(&command.route, edge.roots)
            .map(|(mut data, mut text)| {
                data["admission_timeout"] = json!({
                    "seconds": effective_admission.seconds,
                    "origin": effective_admission.origin.as_str(),
                });
                text.push_str(&format!(
                    "admission timeout: {}s ({})\n",
                    effective_admission.seconds, effective_admission.origin
                ));
                record_warnings(&mut data, warnings);
                text.push_str(&warnings_text(&data));
                (data, text, false)
            })
            .map_err(|error| {
                let refused = matches!(
                    &error,
                    crate::route::EdgeError::Routing(routing)
                        if routing.code() == "no_eligible_pool_member"
                );
                Fail::new(
                    if refused {
                        Status::Refused
                    } else {
                        Status::Invalid
                    },
                    if refused {
                        ErrorKind::Refused
                    } else {
                        ErrorKind::Invalid
                    },
                    error.to_string(),
                )
            });
    }
    // Check explicit placement before local aliases are normalised away and
    // before writer validation or peer discovery can act on the request.
    if command.host.is_some() && crate::fabric::in_lane() {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::LaneCoordinationRefused,
            "coordination mutations are not allowed inside a lane",
        ));
    }
    // Validate roots before checking the capabilities they request.
    let mut lane_access = lane_access(&command.access, edge.roots.state.as_path(), edge.home)?;
    let has_writes = run::Service::has_writes(
        matches!(command.access.mode(), Mode::WorktreeWrite),
        &lane_access.write_roots,
    );
    check_flag_capabilities_with_writes(&command, Some(edge.roots), has_writes)?;
    let configured_workers =
        provenant_confinement::worktrees::CensusPolicyWorkers::for_run(workers, edge.roots)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
    let census_clock = SystemClock::new();
    let census =
        provenant_confinement::worktrees::CensusSession::new(&configured_workers, &census_clock);
    let workers: &dyn provenant_confinement::worktrees::WorkerObserver = &census;
    // Preflight. An owned worktree is the first side effect, so every check
    // that does not depend on the tree runs before it and a refusal leaves
    // no tree behind.
    let owned = if let Some(branch) = command.worktree_branch.clone() {
        if !matches!(command.access.mode(), Mode::WorktreeWrite) || crate::fabric::in_lane() {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Refused,
                "--worktree-branch requires a host worktree-write dispatch",
            ));
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        // Initialise the state before entering the shared confinement context.
        let call = request(&clock, &cancel)?;
        let session = open(edge, &call)?;
        if let Some(replayed) = replay_owned_dispatch(&command, edge, &session, &call, workers)? {
            return Ok(replayed);
        }
        if let Some(mode) = &command.deliver {
            if let Some(delivery) = run::HostDeliveryContext::destination(mode.clone(), &branch)? {
                check_delivery_branch(edge.cwd, &delivery.branch)?;
            }
        } else if let Some(target) = &command.deliver_branch {
            check_delivery_branch(edge.cwd, target)?;
        }
        let host = provenant_config::bind_host(edge.roots)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
        Some(OwnedWorktree {
            branch,
            host,
            created_at: now(&clock)?,
        })
    } else {
        worktree_access(
            matches!(command.access.mode(), Mode::WorktreeWrite) && command.branch.is_none(),
            command.access.worktree.as_deref(),
        )?;
        None
    };
    let unsupported = |what: &str| {
        Err(Fail::new(
            Status::Refused,
            ErrorKind::Unsupported,
            format!("{what} is not available in this build"),
        ))
    };
    let postgres = command
        .access
        .capability
        .iter()
        .any(|name| name == "postgres");
    // Naming this host is ordinary local placement in any mode. A peer writer
    // names a new branch the peer creates, since --worktree is a local path.
    let mut new_worktree = None;
    if let Some(host) = command.host.as_deref() {
        if remote::names_local_host(edge, host)? {
            command.host = None;
        } else if matches!(command.access.mode(), Mode::WorktreeWrite) {
            if !remote::names_peer(edge, host)? {
                return Err(remote::unknown_host(host));
            }
            new_worktree = Some(remote::new_worktree(&command)?);
        }
    }
    if command.branch.is_some() && new_worktree.is_none() {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "--branch and --base create a writer worktree on a peer --host with \
             --mode worktree-write; on this host use `provenant worktree create` and --worktree",
        ));
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    if command.route.read_heavy {
        command.knobs.push("routing.workload=read_heavy".into());
    }
    if !command.route.exclude.is_empty() {
        // Refuse an unknown selector before anything is submitted.
        crate::route::exclusion_rules(
            &command.route.exclude,
            provenant_routing::ExclusionSource::Flag,
            edge.roots,
        )
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
        command.knobs.push(format!(
            "routing.flag_exclude={}",
            serde_json::Value::from(command.route.exclude.clone())
        ));
    }
    let flag_route = (
        command.route.model.as_ref().map(|m| m.0.clone()),
        command.route.route.clone(),
        command.route.effort.clone(),
        command
            .route
            .fallback
            .iter()
            .map(|m| m.0.clone())
            .collect::<Vec<_>>(),
    );
    let mut tasks = Vec::new();
    let mut flag_delivery =
        command
            .deliver_remote
            .zip(command.deliver_branch)
            .map(|(remote, branch)| run::WriterDeliveryRequest {
                extra: Default::default(),
                mode: Default::default(),
                remote,
                branch,
                open_pr: command.open_pr,
                fast_forward_only: false,
            });
    if let Some(mode) = &command.deliver {
        let branch = if let Some(path) = &command.access.worktree {
            let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)?;
            run::prepare_writer(
                &writer,
                edge.roots.state.as_path(),
                execution_context(edge)?.project_id,
            )?
            .branch
            .as_str()
            .trim_start_matches("refs/heads/")
            .to_owned()
        } else if let Some(branch) = command.branch.as_ref().or(command.worktree_branch.as_ref()) {
            branch.clone()
        } else if matches!(mode, run::DeliveryMode::None) {
            String::new()
        } else {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Refused,
                "delivery needs a writer worktree",
            ));
        };
        flag_delivery = run::HostDeliveryContext::destination(mode.clone(), &branch)?;
    }
    if let Some(path) = &command.input.manifest {
        let manifest: Manifest = provenant_records::decode_v1(
            read_bounded(path)?.as_bytes(),
            provenant_records::Limits {
                max_bytes: PROMPT_LIMIT,
                ..Default::default()
            },
        )
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
        if manifest.schema_version != 1 || !(1..=64).contains(&manifest.tasks.len()) {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Invalid,
                "manifest needs schema_version 1 and 1-64 tasks",
            ));
        }
        let base = path.parent().unwrap_or(Path::new("."));
        for (index, task) in manifest.tasks.into_iter().enumerate() {
            let prompt = match (task.prompt, task.prompt_file) {
                (Some(prompt), None) => prompt,
                (None, Some(file))
                    if file
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))) =>
                {
                    read_bounded(&base.join(file))?
                }
                (None, Some(_)) => {
                    return Err(Fail::new(
                        Status::Invalid,
                        ErrorKind::Invalid,
                        "prompt_file must be relative to the manifest, without ..",
                    ));
                }
                _ => {
                    return Err(Fail::new(
                        Status::Invalid,
                        ErrorKind::Invalid,
                        "each task needs exactly one prompt or prompt_file",
                    ));
                }
            };
            let route = run::Service::route(task.route)?;
            let fallback = if task.fallback.is_empty() {
                flag_route.3.clone()
            } else {
                task.fallback
            };
            let task_id = match task.task_id {
                Some(id) => id,
                None => derived_task(&command.operation_id, index)?,
            };
            let network = network_access(task.network, postgres);
            tasks.push((
                task_id,
                task.title,
                prompt,
                route,
                fallback,
                task.delivery_id,
                task.account,
                task.deliver.or_else(|| flag_delivery.clone()),
                task.base.or_else(|| command.base.clone()),
                task.build_heavy || command.build_heavy,
                task.context,
                network,
            ));
        }
    } else {
        let prompt = match &command.input.prompt_file {
            Some(path) => read_bounded(path)?,
            None => read_text(std::io::stdin().lock())?,
        };
        let task_id = match command.task_id {
            Some(id) => id,
            None => derived_task(&command.operation_id, 0)?,
        };
        tasks.push((
            task_id,
            command.title.clone(),
            prompt,
            requested(
                flag_route.0.as_deref(),
                flag_route.1.as_deref(),
                flag_route.2.clone(),
            )?,
            flag_route.3.clone(),
            command.delivery_id,
            command.account.clone(),
            flag_delivery,
            command.base.clone(),
            command.build_heavy,
            None,
            None,
        ));
    }
    let concurrency = if command.input.manifest.is_some() {
        command.concurrency
    } else {
        Some(1)
    };
    if let Some(host) = &command.host {
        let mut placed = Vec::new();
        for (
            index,
            (
                task_id,
                title,
                prompt,
                route,
                fallback,
                delivery_id,
                account,
                deliver,
                base,
                build_heavy,
                context,
                network,
            ),
        ) in tasks.into_iter().enumerate()
        {
            // The peer derives task IDs from the operation and has no fallback field.
            if task_id != derived_task(&command.operation_id, index)? {
                return unsupported("an explicit task ID with --host");
            }
            if !fallback.is_empty() {
                return unsupported("route fallback with --host");
            }
            if !lane_access.is_default() || network.is_some() {
                return unsupported(
                    "network, capability, read-root or write-root access with --host",
                );
            }
            placed.push(remote::Task {
                context,
                build_heavy,
                title,
                base,
                head: command.head.clone(),
                prompt,
                route,
                account,
                delivery_id,
                deliver,
            });
        }
        return remote::dispatch(
            edge,
            host,
            remote::Batch {
                operation_id: command.operation_id,
                tasks: placed,
                concurrency,
                knobs: command.knobs,
                provider_timeout: command.provider_timeout,
                new_worktree,
            },
            command.wait,
        );
    }
    let mut session = open(edge, &call)?;
    // Protection governs new admission. Service::dispatch still compares the
    // immutable inputs when an existing operation is replayed.
    if session
        .store
        .operation(&command.operation_id, &call)?
        .is_none()
    {
        for (_, _, _, _, _, _, _, delivery, _, _, _, _) in &tasks {
            if let Some(delivery) = delivery {
                check_delivery_branch(edge.cwd, &delivery.branch)?;
            }
        }
    }
    let chair = if crate::fabric::in_lane() {
        None
    } else {
        let context = crate::fabric::Context::open(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Identity, error.to_string()))?;
        Some(run::ChairBinding {
            extra: Default::default(),
            session: crate::fabric::current_session(),
            host: context.identity.inbox.host,
            actor: crate::workflow::session_actor(&context, None)
                .map_err(|error| Fail::new(Status::Error, ErrorKind::Identity, error))?,
        })
    };
    // Validate deterministic admission inputs before creating or claiming a tree.
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    config
        .dispatch_profile(&command.knobs)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    for (_, title, prompt, _, models, _, _, _, _, _, _, _) in &tasks {
        run::Service::scan(prompt)?;
        if let Some(title) = title {
            provenant_core::validate_task_title(title)
                .map_err(|message| Fail::new(Status::Invalid, ErrorKind::Invalid, message))?;
        }
        fallback(models)?;
    }

    let configured_admission = config
        .domains
        .run
        .admission_seconds_for(&session.context.project_id.to_string());
    let effective_admission =
        run::AdmissionPolicy::resolve_timeout(command.admission_timeout, configured_admission);
    let mut policy = session.store.admission_policy();
    policy.admission_seconds = effective_admission.seconds;
    session.store.set_admission_policy(policy);

    let now_wall = clock
        .wall_now()
        .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "wall clock"))?;
    let initial_deadline = DurationMicros::from_seconds(effective_admission.seconds)
        .ok()
        .and_then(|duration| now_wall.checked_add(duration).ok())
        .ok_or_else(|| {
            Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                "invalid admission timeout",
            )
        })?;

    let mut admission_window = match session
        .store
        .admission_window(&command.operation_id, &call)?
    {
        Some(window) => window,
        None => session.store.save_admission_window(
            &command.operation_id,
            run::AdmissionWindow {
                deadline: initial_deadline,
                wake_at: None,
            },
            &call,
        )?,
    };

    // Preflight is complete: create or claim the owned tree, then admit.
    let mut pinned_base = None;
    let worktree = if owned.is_some() || command.access.worktree.is_some() {
        let backoff = provenant_core::retry::Backoff::new(
            Duration::from_millis(100),
            2,
            Duration::from_secs(2),
        );
        let mut retries = 0_u64;
        loop {
            if cancel.is_cancelled() {
                return Err(Fail::new(
                    Status::Cancelled,
                    ErrorKind::Cancelled,
                    "cancelled",
                ));
            }
            if let Some(wake) = admission_window.wake_at {
                let now = clock
                    .wall_now()
                    .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "wall clock"))?;
                let until = wake.min(admission_window.deadline);
                if now < until {
                    let micros = u64::try_from(until.as_micros() - now.as_micros()).unwrap_or(0);
                    let target = clock
                        .mono_now()
                        .checked_add(DurationMicros::from_micros(micros))
                        .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "mono clock"))?;
                    clock
                        .wait_until(target)
                        .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "wait until"))?;
                    continue;
                }
            }
            let now = clock
                .wall_now()
                .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "wall clock"))?;
            if now >= admission_window.deadline {
                return Err(Fail::new(
                    Status::Refused,
                    ErrorKind::Refused,
                    "admission deadline exceeded",
                ));
            }
            let remaining =
                u64::try_from(admission_window.deadline.as_micros() - now.as_micros()).unwrap_or(0);
            let lock_budget = provenant_core::WaitBudget::up_to(
                clock.mono_now(),
                DurationMicros::from_micros(remaining.min(5_000_000)),
            );
            let res: Result<(Value, Option<provenant_core::GitOid>), WorktreeAdmissionError> =
                if let Some(owned) = &owned {
                    match own_worktree(
                        owned,
                        command.base.as_deref(),
                        &command.operation_id,
                        edge,
                        workers,
                        |attempt| {
                            session.store.record(attempt, &call).is_ok_and(|record| {
                                matches!(record.state, run::AttemptState::Terminal { .. })
                            })
                        },
                        created,
                        &clock,
                        &cancel,
                        Some(lock_budget),
                    ) {
                        Ok((info, base)) => {
                            command.access.worktree = Some(info.path.clone());
                            match self::lane_access(
                                &command.access,
                                edge.roots.state.as_path(),
                                edge.home,
                            ) {
                                Ok(access) => {
                                    lane_access = access;
                                    Ok((
                                        json!({"path": info.path, "branch": owned.branch, "owned": true}),
                                        Some(base),
                                    ))
                                }
                                Err(err) => Err(WorktreeAdmissionError::Fail(err)),
                            }
                        }
                        Err(err) => Err(WorktreeAdmissionError::Own(err)),
                    }
                } else if let Some(path) = &command.access.worktree {
                    let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)
                        .map_err(WorktreeAdmissionError::Run)?;
                    let replay = session
                        .store
                        .operation(&command.operation_id, &call)
                        .map_err(WorktreeAdmissionError::Run)?
                        .is_some();
                    provenant_confinement::worktrees::with_context_using_workers(
                        edge.roots,
                        edge.home,
                        &writer.project,
                        &clock,
                        &cancel,
                        workers,
                        |context| {
                            use provenant_confinement::worktrees;
                            let context = &worktrees::WorktreeContext {
                                workers,
                                lock_budget,
                                ..*context
                            };
                            let info = worktrees::list(context, &cancel)?
                                .into_iter()
                                .find(|tree| tree.path == writer.worktree)
                                .ok_or(worktrees::WorktreeError::NotFound)?;
                            let branch =
                                info.branch
                                    .ok_or(worktrees::WorktreeError::DetachedWorktree {
                                        head: info.head,
                                    })?;
                            if !replay {
                                match worktrees::cache_writer_probe(
                                    context.project_state,
                                    &info.path,
                                ) {
                                    Ok(_) => {}
                                    Err(worktrees::WorkerState::Busy) => {
                                        let tree = provenant_confinement::verify_worktree(
                                            context.repo,
                                            &info.path,
                                        )?;
                                        let holder = worktrees::load_owner(&tree)?
                                            .and_then(|owner| owner.attempt_id);
                                        return Err(worktrees::WorktreeError::WriterBusy {
                                            holder,
                                        });
                                    }
                                    Err(worktrees::WorkerState::Live) => {
                                        return Err(worktrees::live_refusal(
                                            context.workers,
                                            &info.path,
                                        ));
                                    }
                                    Err(_) => return Err(worktrees::WorktreeError::UnknownWriter),
                                }
                            }
                            Ok(json!({
                                "path": info.path,
                                "branch": branch.trim_start_matches("refs/heads/"),
                                "owned": false
                            }))
                        },
                    )
                    .map_err(|error| match error {
                        provenant_confinement::worktrees::WorktreeError::NotFound => {
                            run::RunError::WorktreeUnregistered(writer.worktree.clone())
                        }
                        other => run::RunError::from(other),
                    })
                    .map_err(WorktreeAdmissionError::Run)
                    .map(|tree_res| (tree_res, None))
                } else {
                    unreachable!()
                };

            match res {
                Ok((value, base)) => {
                    pinned_base = base;
                    break Some(value);
                }
                Err(err) => {
                    if !err.is_transient() {
                        return Err(err.into());
                    }
                    let now = clock
                        .wall_now()
                        .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "wall clock"))?;
                    if now >= admission_window.deadline {
                        return Err(err.into());
                    }
                    let delay = backoff.delay(retries);
                    let left_micros = u64::try_from(
                        admission_window
                            .deadline
                            .as_micros()
                            .saturating_sub(now.as_micros()),
                    )
                    .unwrap_or(0);
                    let pause = delay.min(Duration::from_micros(left_micros));
                    let wake_micros = u64::try_from(pause.as_micros()).unwrap_or(u64::MAX);
                    let wake_at = now
                        .checked_add(DurationMicros::from_micros(wake_micros))
                        .map_or(admission_window.deadline, |w| {
                            w.min(admission_window.deadline)
                        });
                    admission_window.wake_at = Some(wake_at);
                    admission_window = session.store.save_admission_window(
                        &command.operation_id,
                        admission_window,
                        &call,
                    )?;
                    let wait_deadline = clock
                        .mono_now()
                        .checked_add(DurationMicros::from_micros(wake_micros))
                        .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "mono clock"))?;
                    clock
                        .wait_until(wait_deadline)
                        .map_err(|_| Fail::new(Status::Error, ErrorKind::Time, "wait until"))?;
                    retries = retries.saturating_add(1);
                }
            }
        }
    } else {
        None
    };
    let tasks: Vec<Submitted> = tasks
        .into_iter()
        .map(
            |(
                task_id,
                title,
                prompt,
                route,
                fallback,
                delivery_id,
                account,
                deliver,
                base,
                build_heavy,
                context,
                network,
            )| {
                let mut access = lane_access.clone();
                if network.is_some() {
                    access.network = network;
                }
                Submitted {
                    settlement_retry_limit: None,
                    context,
                    build_heavy,
                    title,
                    base,
                    pinned_base,
                    head: command.head.clone(),
                    task_id,
                    prompt,
                    route,
                    fallback,
                    account,
                    provider_timeout: command.provider_timeout,
                    delivery_id,
                    deliver,
                    worktree: command.access.worktree.clone(),
                    access,
                }
            },
        )
        .collect();
    let brief_warnings = if command.input.manifest.is_none()
        && config
            .domains
            .run
            .brief_read_root_warnings_for(&session.context.project_id.to_string())
    {
        tasks
            .first()
            .map(|task| {
                brief_read_root_warnings(
                    &task.prompt,
                    edge.cwd,
                    command.access.worktree.as_deref(),
                    &lane_access.read_roots,
                    edge.home,
                )
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let (record, started) = submit_with_host_and_wait(
        &session,
        edge,
        Submission {
            chair,
            operation_id: command.operation_id,
            tasks,
            concurrency,
            knobs: command.knobs,
        },
        &call,
        None,
        command.wait,
    )?;
    let (mut data, mut human, timed_out) =
        report(&session, &record, &started, command.wait, edge, &call)?;
    record_warnings(&mut data, brief_warnings);
    human.push_str(&warnings_text(&data));
    if let Some(worktree) = worktree {
        data["worktree"] = worktree;
    }
    Ok((data, human, timed_out))
}

/// Carry submit-time warnings as `data["warnings"]`, so JSON consumers see them.
fn record_warnings(data: &mut Value, warnings: Vec<String>) {
    if !warnings.is_empty()
        && let Some(map) = data.as_object_mut()
    {
        map.insert("warnings".into(), json!(warnings));
    }
}

/// Render `data["warnings"]` as human lines.
pub fn warnings_text(data: &Value) -> String {
    data["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|warning| format!("warning: {}\n", plain_text(warning, usize::MAX)))
        .collect()
}

/// Resolve an existing project binding without creating state during previews.
pub fn preview_project_id(roots: &Roots, cwd: &Path) -> Option<provenant_core::ProjectId> {
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

/// Resolve an existing project binding without creating state during previews.
pub fn brief_read_root_warnings_enabled(
    config: &provenant_confinement::run_config::RunConfig,
    roots: &Roots,
    cwd: &Path,
) -> bool {
    if config.projects.is_empty() {
        return config.brief_read_root_warnings;
    }
    let project = preview_project_id(roots, cwd);
    project.map_or(config.brief_read_root_warnings, |id| {
        config.brief_read_root_warnings_for(&id.to_string())
    })
}

/// Existing absolute or `~/` paths in a brief that no effective read root covers.
pub fn brief_read_root_warnings(
    prompt: &str,
    cwd: &Path,
    worktree: Option<&Path>,
    explicit_roots: &[PathBuf],
    home: &Path,
) -> Vec<String> {
    let mut roots = vec![worktree.unwrap_or(cwd).to_path_buf()];
    roots.extend(explicit_roots.iter().cloned());
    let roots: Vec<_> = roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect();
    let mut warnings = std::collections::BTreeSet::new();
    let permitted_lines = prompt.lines().filter(|line| {
        let line = line.to_lowercase();
        ![
            "never",
            "do not",
            "don't",
            "must not",
            "not allowed",
            "forbidden",
        ]
        .iter()
        .any(|phrase| {
            line.match_indices(phrase).any(|(start, matched)| {
                let before = line[..start].chars().next_back();
                let after = line[start + matched.len()..].chars().next();
                before.is_none_or(|character| !character.is_alphanumeric())
                    && after.is_none_or(|character| !character.is_alphanumeric())
            })
        })
    });
    for token in permitted_lines.flat_map(str::split_whitespace) {
        let token = token
            .trim_matches(|character: char| {
                matches!(character, '`' | '\'' | '"' | '(' | '[' | '{' | '<')
            })
            .trim_end_matches(|character: char| {
                matches!(
                    character,
                    '`' | '\'' | '"' | ')' | ']' | '}' | '>' | ',' | '.' | ';' | ':'
                )
            });
        if token.trim_end_matches('/') == "~" || token.chars().all(|character| character == '/') {
            continue;
        }
        let candidate = if let Some(relative) = token.strip_prefix("~/") {
            home.join(relative)
        } else if token.starts_with('/') {
            PathBuf::from(token)
        } else {
            continue;
        };
        let Ok(path) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if path == Path::new("/") {
            continue;
        }
        if roots
            .iter()
            .any(|root| path.starts_with(root) || root.starts_with(&path))
        {
            continue;
        }
        warnings.insert(format!(
            "brief references {} outside the lane's read roots; copy it into the worktree (for example .agent-run/) or add it to --read-root",
            path.display()
        ));
    }
    warnings.into_iter().collect()
}

/// Validate the inherited lane context and determine the next submission depth.
pub fn nested_dispatch_context(roots: &Roots) -> Result<(u64, Option<AttemptId>), Fail> {
    let parent_depth = environment_text(run::LANE_DEPTH_ENV)?;
    let parent_attempt = environment_text(run::PARENT_ATTEMPT_ENV)?;
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
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

pub(super) fn replay_owned_dispatch(
    command: &Dispatch,
    edge: &Edge<'_>,
    session: &Session,
    call: &run::Request<'_>,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<Option<(Value, String, bool)>, Fail> {
    let Some(operation) = session.store.operation(&command.operation_id, call)? else {
        return Ok(None);
    };
    let canonical = session.store.request(operation.run_id, call)?;
    let task = canonical
        .tasks
        .first()
        .filter(|_| canonical.tasks.len() == 1)
        .ok_or(run::RunError::OperationConflict)?;
    let branch = command
        .worktree_branch
        .as_deref()
        .ok_or(run::RunError::OperationConflict)?;
    let writer = task
        .writer
        .as_ref()
        .ok_or(run::RunError::OperationConflict)?;
    let prompt = match &command.input.prompt_file {
        Some(path) => read_bounded(path)?,
        None => read_text(std::io::stdin().lock())?,
    };
    let route = requested(
        command.route.model.as_ref().map(|model| model.0.as_str()),
        command.route.route.as_deref(),
        command.route.effort.clone(),
    )?;
    let delivery = match &command.deliver {
        Some(mode) => run::HostDeliveryContext::destination(mode.clone(), branch)?,
        None => command
            .deliver_remote
            .clone()
            .zip(command.deliver_branch.clone())
            .map(|(remote, branch)| run::WriterDeliveryRequest {
                extra: Default::default(),
                mode: Default::default(),
                remote,
                branch,
                open_pr: command.open_pr,
                fast_forward_only: false,
            }),
    };
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    let profile = config
        .dispatch_profile(&command.knobs)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    if task.title != command.title
        || task.build_heavy != command.build_heavy
        || task.prompt_digest != CanonicalV1::file_digest(prompt.as_bytes())
        || task.route != route
        || task.account != command.account
        || task.head != command.head
        || task.base_ref != command.base
        || task.deliver != delivery
        || task.access != lane_access(&command.access, edge.roots.state.as_path(), edge.home)?
        || task.profile != profile
        || task.provider_timeout_seconds != command.provider_timeout
        || task.delivery_id != command.delivery_id
        || command.task_id.is_some_and(|id| id != task.task_id)
    {
        return Err(run::RunError::OperationConflict.into());
    }
    let mut settlement = None;
    let owned = if writer.worktree.exists() {
        provenant_confinement::worktrees::with_context_using_workers(
            edge.roots,
            edge.home,
            &writer.project,
            call.clock,
            call.cancel,
            workers,
            |context| {
                let tree = provenant_confinement::verify_worktree(context.repo, &writer.worktree)?;
                Ok(
                    provenant_confinement::worktrees::load_owner(&tree)?.is_some_and(|owner| {
                        owner.branch == branch
                            && owner.operation_id.as_ref() == Some(&command.operation_id)
                    }),
                )
            },
        )
        .map_err(run::RunError::from)?
    } else {
        let page = session.store.lanes(
            &run::LaneQuery {
                targets: vec![run::LaneTarget {
                    run: operation.run_id,
                    task: None,
                    attempt: None,
                }],
                all: true,
                limit: 2,
                after: 0,
                newest_first: false,
                owner: None,
                unseen_after: None,
            },
            call,
        )?;
        if let Some(attempt) = page.lanes.first() {
            settlement = session
                .store
                .worktree_settlement(attempt.attempt_id, call)?;
        }
        settlement.as_ref().is_some_and(|receipt| {
            worktree_was_pruned(receipt, &writer.worktree)
                && (receipt["result"]["removed"][0]["branch"] == format!("refs/heads/{branch}")
                    || receipt["branch"] == format!("refs/heads/{branch}"))
        })
    };
    if !owned {
        return Err(run::RunError::OperationConflict.into());
    }
    let run = session.store.run(operation.run_id, call)?;
    let started = launch(session, run.run_id, edge, call)?;
    let (mut data, human, timed_out) = report(session, &run, &started, command.wait, edge, call)?;
    data["worktree"] = json!({"path": writer.worktree, "branch": branch, "owned":true});
    if let Some(settlement) = settlement {
        data["worktree_settlement"] = settlement;
    }
    Ok(Some((data, human, timed_out)))
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
                "lane environment must match its child Run store path",
            )
        };
        let relative = run_state.strip_prefix(state).map_err(|_| invalid())?;
        let mut parts = relative.components();
        let mut depth = 0;
        let mut parent = None;
        while let Some(part) = parts.next() {
            if part.as_os_str() != "children" {
                return Err(invalid());
            }
            let id = parts
                .next()
                .and_then(|part| part.as_os_str().to_str())
                .ok_or_else(invalid)?;
            parent = Some(AttemptId::parse(id).map_err(|_| invalid())?);
            depth += 1;
        }
        if depth == 0 || parent_depth != Some(depth) || parent_attempt != parent {
            return Err(invalid());
        }
        return Ok((run::next_lane_depth(depth, max_depth)?, parent));
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
    launch_on_host_with_wait(
        session,
        run,
        edge,
        call,
        None,
        None,
        ready,
        provider_lease_wait,
    )
}

pub(super) struct PreparedProvider<'a> {
    fallback: &'a Registry<'a>,
    ready: Option<&'a run::PreparedStart>,
    provider_lease_wait: Option<u64>,
}

impl run::Providers for PreparedProvider<'_> {
    fn runtime_cap(&self, provider: &str) -> Option<u64> {
        self.fallback.runtime_cap(provider)
    }
    fn priority(&self, task: &run::TaskRequest) -> Result<u8, String> {
        self.fallback.priority(task)
    }
    fn check_policy(&self, task: &run::TaskRequest) -> Result<(), String> {
        if let Some(ready) = self.ready {
            if ready.task != *task {
                return Err("prepared continuation task changed".into());
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
                return Err("prepared continuation identity changed".into());
            }
            return ready
                .plan
                .borrow_mut()
                .take()
                .ok_or_else(|| "prepared continuation consumed".into());
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

/// New lane-local associations need the same namespace proof as WorkIndex.
/// A legacy row can be replayed, but a lane cannot create or repair one.
#[derive(Debug)]
pub(super) enum ChairRunError {
    Unauthorised(Fail),
    Other(Fail),
}

impl From<ChairRunError> for Fail {
    fn from(error: ChairRunError) -> Self {
        match error {
            ChairRunError::Unauthorised(error) | ChairRunError::Other(error) => error,
        }
    }
}

pub(super) fn check_chair_run_id(
    session: &Session,
    roots: &Roots,
    run: RunId,
    request: &run::CanonicalRequest,
    call: &run::Request<'_>,
) -> Result<(), ChairRunError> {
    session
        .store
        .check_child_run_id(
            run,
            request,
            &session
                .store
                .run(run, call)
                .map_err(Fail::from)
                .map_err(ChairRunError::Other)?
                .operation_id,
        )
        .map_err(Fail::from)
        .map_err(ChairRunError::Other)?;
    if request.child_attempt_id_version.is_none() {
        let digest = request
            .digest()
            .map_err(run::RunError::from)
            .map_err(Fail::from)
            .map_err(ChairRunError::Other)?;
        let mut identity_refused = false;
        with_chair_index(roots, call, |index, c| {
            match index.read(session.context.project_id, run, c)? {
                Some(value) if value.request_digest == digest => Ok(()),
                Some(_) => Err(provenant_fabric_runtime::chairs::Error::Conflict),
                None => {
                    identity_refused = true;
                    Err(provenant_fabric_runtime::chairs::Error::Invalid(
                        "new chair associations require host-derived child run IDs",
                    ))
                }
            }
        })
        .map_err(|failure| {
            if identity_refused {
                ChairRunError::Unauthorised(failure)
            } else {
                ChairRunError::Other(failure)
            }
        })?;
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct ChairSourceError;

pub(super) fn check_chair_source(
    store: &run::RunStore,
    request: &run::CanonicalRequest,
    parent: &run::AttemptRecord,
) -> Result<(), ChairSourceError> {
    store
        .check_child_attempt_id(request, parent)
        .map_err(|_| ChairSourceError)
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

/// Called after guardian custody is released. The successor was queued in the
/// terminal transaction; a crashed launcher leaves it for ordinary recovery.
/// When the staged retry must wait, `before_wait` runs first so other work,
/// such as graph advancement, is not held behind the backoff.
pub fn launch_policy_successor(
    context: &run::HolderContext,
    before_wait: impl FnOnce(),
) -> Result<(), String> {
    let Some(host) = context.launch.as_ref() else {
        return Ok(());
    };
    if host.configuration_root != context.roots.state_root {
        return Ok(());
    }
    let mut inputs =
        provenant_core::RootInputs::new(context.roots.home.clone(), context.roots.checkout.clone());
    inputs.state_dir = Some(context.roots.state_root.clone());
    let roots = inputs
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|error| error.to_string())?;
    let (_, store) =
        run::HolderContext::open(&context.encode().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut call = request(&clock, &cancel).map_err(|error| error.message)?;
    let pending = store
        .pending_attempts(context.run_id, &call)
        .map_err(|error| error.to_string())?;
    let successor_pending = |pending: &[run::AttemptRecord]| {
        pending.iter().any(|attempt| {
            attempt.retry_of == Some(context.attempt_id)
                && matches!(
                    attempt.continuation.as_deref(),
                    Some("policy_fallback" | "policy_continuation")
                )
        })
    };
    if !successor_pending(&pending) {
        return Ok(());
    }
    // A capacity retry waits until its recorded time here, in the settled
    // attempt's guardian, and gives up the wait once the successor is no
    // longer queued. The time is sealed with the retry and every launcher
    // defers the successor until then, so a later sweep can launch it if this
    // guardian dies.
    use provenant_core::Clock;
    let backoff = store
        .record(context.attempt_id, &call)
        .ok()
        .and_then(|record| store.staged_policy_retry(&record).ok().flatten())
        .and_then(|retry| retry.not_before_unix_seconds)
        .zip(clock.wall_now().ok())
        .map_or(0, |(not_before, now)| {
            let now = now.as_micros().div_euclid(1_000_000);
            u64::try_from(not_before.saturating_sub(now)).unwrap_or(0)
        });
    if backoff > 0 {
        before_wait();
        let start = clock.mono_now();
        let deadline = provenant_core::DurationMicros::from_seconds(backoff)
            .and_then(|wait| start.checked_add(wait))
            .map_err(|error| error.to_string())?;
        let mut checked = start;
        while clock.mono_now() < deadline {
            provenant_os::sys::wait_pause(std::time::Duration::from_millis(50));
            let now = clock.mono_now();
            if now.as_micros().saturating_sub(checked.as_micros()) >= 1_000_000 {
                checked = now;
                // Each check, and the launch after the wait, gets its own budget.
                call = request(&clock, &cancel).map_err(|error| error.message)?;
                let pending = store
                    .pending_attempts(context.run_id, &call)
                    .map_err(|error| error.to_string())?;
                if !successor_pending(&pending) {
                    return Ok(());
                }
            }
        }
        call = request(&clock, &cancel).map_err(|error| error.message)?;
    }
    let session = Session {
        context: run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: context.project_id,
            actor_id: context.actor_id,
            read_prompts: true,
        },
        store,
    };
    let edge = Edge {
        roots: &roots,
        home: &context.roots.home,
        assets: &context.roots.checkout,
        cwd: &host.cwd,
    };
    launch_on_host(
        &session,
        context.run_id,
        &edge,
        &call,
        Some(host),
        None,
        None,
    )
    .map_err(|error| error.message)?;
    Ok(())
}

/// A child carries the request `validate_child` returned, which is the only
/// copy launched from. The store carries its parent's sealed requester.
pub(super) fn launch_on_host(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
    validated: Option<&run::ValidatedChild>,
    ready: Option<&run::PreparedStart>,
) -> Result<Vec<run::Started>, Fail> {
    launch_on_host_with_wait(session, run, edge, call, host, validated, ready, None)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn launch_on_host_with_wait(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
    validated: Option<&run::ValidatedChild>,
    ready: Option<&run::PreparedStart>,
    provider_lease_wait: Option<u64>,
) -> Result<Vec<run::Started>, Fail> {
    let inherited_host = host.is_some();
    let host = match host {
        Some(host) => host.clone(),
        None => run::HostLaunchContext {
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
            codex_home: std::env::var_os("CODEX_HOME").map(PathBuf::from),
            cargo_build_jobs: std::env::var("CARGO_BUILD_JOBS").ok(),
            #[cfg(feature = "fixture")]
            fixture_state_write: false,
        },
    };
    let read;
    let request = match validated {
        Some(request) => &**request,
        None => {
            read = session.store.request(run, call)?;
            &read
        }
    };
    let request_digest = request.digest().map_err(run::RunError::from)?;
    if validated.is_none() {
        if request.parent.is_some() && session.context.state_root != edge.roots.state {
            check_chair_run_id(session, edge.roots, run, request, call)?;
        }
        let operation = match request.parent {
            Some(_) => Some(session.store.run(run, call)?.operation_id),
            None => None,
        };
        with_chair_index(edge.roots, call, |index, index_call| {
            if let Some(prior) = request.parent {
                let lane_local = session.context.state_root != edge.roots.state;
                if lane_local
                    && let Some(sealed) = index.read(session.context.project_id, run, index_call)?
                {
                    return if sealed.request_digest == request_digest {
                        Ok(())
                    } else {
                        Err(provenant_fabric_runtime::chairs::Error::Conflict)
                    };
                }
                let parent = session.store.record(prior, call).map_err(|_| {
                    provenant_fabric_runtime::chairs::Error::Invalid("continuation parent")
                })?;
                let original = session.store.request(parent.run_id, call).map_err(|_| {
                    provenant_fabric_runtime::chairs::Error::Invalid("continuation request")
                })?;
                if lane_local {
                    check_chair_source(&session.store, &original, &parent).map_err(|_| {
                        provenant_fabric_runtime::chairs::Error::Invalid(
                            "continuation source identity does not match host-derived IDs",
                        )
                    })?;
                    if index
                        .read(session.context.project_id, parent.run_id, index_call)?
                        .is_none()
                    {
                        return Err(provenant_fabric_runtime::chairs::Error::NotFound);
                    }
                }
                let operation =
                    operation
                        .as_ref()
                        .ok_or(provenant_fabric_runtime::chairs::Error::Invalid(
                            "continuation operation",
                        ))?;
                index.inherit(
                    session.context.project_id,
                    (parent.run_id, original.digest()?),
                    run,
                    if lane_local {
                        provenant_fabric_runtime::chairs::Admission::Lane(operation)
                    } else {
                        provenant_fabric_runtime::chairs::Admission::HostRoot(operation)
                    },
                    request,
                    index_call,
                )
            } else {
                match index.read(session.context.project_id, run, index_call)? {
                    Some(value) if value.request_digest != request_digest => {
                        Err(provenant_fabric_runtime::chairs::Error::Conflict)
                    }
                    // Only the host root's immutable request may repair an
                    // interrupted submission, including owned-worktree replay.
                    None if session.context.state_root == edge.roots.state => {
                        index.submit(session.context.project_id, run, request, index_call)
                    }
                    None if request.chair.is_some() => {
                        Err(provenant_fabric_runtime::chairs::Error::NotFound)
                    }
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
                        owner.attempt_id = Some(attempt.attempt_id);
                        owner.operation_id = Some(attempt.operation_id.clone());
                        worktrees::save_owner(&tree, &owner)?;
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
                &session
                    .store
                    .effective_launch_task(request, task, validated),
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
            validated,
            Some(request),
            call,
        )?;
        check_launch_policy(&started)?;
        return Ok(started);
    }
    let started = session
        .service()
        .start(run, &providers, &holder, validated, request, call)?;
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
    // Preserve the full receipt whenever a sibling can still make progress
    // or needs unknown-handoff recovery, including Fabric's operation replay.
    if started
        .iter()
        .any(|started| !matches!(started, run::Started::Refused { .. }))
    {
        return Ok(());
    }
    if let Some(seconds) = started.iter().find_map(|started| match started {
        run::Started::Refused {
            kind: run::RefusalKind::SetupLockTimeout { seconds },
            ..
        } => Some(*seconds),
        _ => None,
    }) {
        return Err(run::RunError::SetupLockTimeout { seconds }.into());
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
    // Candidate preparation reports route exclusions as `route: detail` entries.
    // Promote pacing specifically; other preparation refusals remain in the receipt.
    if let Some((attempt, reason)) = started.iter().find_map(|started| match started {
        run::Started::Refused {
            attempt,
            reason,
            kind: run::RefusalKind::Policy,
        } => Some((attempt, reason)),
        run::Started::Refused {
            attempt,
            reason,
            kind: run::RefusalKind::Preparation,
        } if reason.split("; ").any(|entry| {
            entry
                .rsplit_once(": ")
                .is_some_and(|(_, detail)| detail == "PacingReserve")
        }) =>
        {
            Some((attempt, reason))
        }
        _ => None,
    }) {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::Refused,
            format!("{attempt}: {reason}"),
        ));
    }
    Ok(())
}
