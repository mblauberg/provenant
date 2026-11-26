use super::*;
use provenant_records::warning::{CONFIG_KEY_IGNORED, Warning, warn};

#[cfg(any(test, feature = "fixture"))]
thread_local! {
    static TEST_DEFER_PROVIDER_LAUNCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Keep a test submission queued after admission, before provider preparation.
#[cfg(any(test, feature = "fixture"))]
pub fn with_deferred_provider_launch<T>(action: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_DEFER_PROVIDER_LAUNCH.with(|flag| flag.set(self.0));
        }
    }
    let previous = TEST_DEFER_PROVIDER_LAUNCH.with(|flag| flag.replace(true));
    let _reset = Reset(previous);
    action()
}

/// `ADAPTER/NAME` for a model-less adapter such as `command/fmt-check`.
pub(super) fn adapter_entry(route: &str) -> Option<(&str, &str)> {
    let (provider, name) = route.split_once('/')?;
    let spec = provenant_records::AdapterSpec::get(provider)?;
    (!spec.launch.model && !name.is_empty()).then_some((provider, name))
}

/// Exactly one of `ADAPTER/ID`, `ADAPTER/NAME` for a model-less adapter, or a
/// named route. `ADAPTER/ID@EFFORT`, the form route receipts print, is accepted when no separate effort is given.
pub fn requested(
    model: Option<&str>,
    route: Option<&str>,
    effort: Option<String>,
) -> Result<run::RequestedRoute, Fail> {
    let (model, effort) = match model.and_then(|m| m.rsplit_once('@')) {
        Some(_) if effort.is_some() => {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                "give the effort once: ADAPTER/ID@EFFORT or --effort",
            ));
        }
        Some((model, suffix)) => (Some(model), Some(suffix.to_owned())),
        None => (model, effort),
    };
    match (model, route) {
        (Some(model), None) => {
            let id = ModelId::parse(model).map_err(|_| {
                Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    crate::route::model_form_message(model),
                )
            })?;
            Ok(run::RequestedRoute::Model {
                required_family: None,
                extra: Default::default(),
                provider: id.as_str().split('/').next().unwrap_or_default().into(),
                model: model.into(),
                effort,
            })
        }
        (None, Some(name)) if adapter_entry(name).is_some() => {
            let (provider, entry) = adapter_entry(name).unwrap_or_default();
            if effort.is_some() {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "a model-less adapter route takes no effort",
                ));
            }
            Ok(run::RequestedRoute::Adapter {
                required_family: None,
                extra: Default::default(),
                provider: provider.into(),
                name: entry.into(),
            })
        }
        (None, Some(name)) => Ok(run::RequestedRoute::Named {
            required_family: None,
            extra: Default::default(),
            name: name.into(),
            effort,
        }),
        _ => Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "each task needs exactly one of model or route",
        )),
    }
}

/// The reach a network switch asks for. Off keeps a granted local database.
pub(super) fn network_access(switch: Option<Switch>, postgres: bool) -> Option<run::NetworkAccess> {
    match (switch, postgres) {
        (Some(Switch::On), _) => Some(run::NetworkAccess::Open),
        (Some(Switch::Off), true) => Some(run::NetworkAccess::Local),
        (Some(Switch::Off), false) => Some(run::NetworkAccess::Provider),
        (None, _) => None,
    }
}

/// Resolve dispatch access flags into the sealed record. Roots must be
/// existing directories, stored canonically, and clear of state roots; the
/// resolved route's confinement policy validates credential overlap.
pub fn lane_access(
    access: &crate::Access,
    state_root: &Path,
) -> Result<run::LaneAccessRequest, Fail> {
    lane_access_checked(access, state_root)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AccessError {
    #[error("{flag} {} is not an existing directory", path.display())]
    MissingDirectory { flag: &'static str, path: PathBuf },
    #[error("{flag} {} overlaps the Provenant state root", path.display())]
    StateOverlap { flag: &'static str, path: PathBuf },
    #[error("--write-root {} overlaps the --worktree; worktree-write already grants the worktree", .0.display())]
    WorktreeOverlap(PathBuf),
}

pub(super) fn lane_access_checked(
    access: &crate::Access,
    state_root: &Path,
) -> Result<run::LaneAccessRequest, AccessError> {
    let mut postgres = false;
    for name in &access.capability {
        match name.as_str() {
            "postgres" => postgres = true,
            other => warn(
                Warning::new(
                    CONFIG_KEY_IGNORED,
                    format!("ignored unknown capability {}", plain_text(other, 128)),
                )
                .subject(other),
            ),
        }
    }
    let network = network_access(access.network, postgres);
    // A first dispatch can precede the state root's creation; compare lexically then.
    let state =
        provenant_os::fs::canonicalize(state_root).unwrap_or_else(|_| state_root.to_path_buf());
    let resolve = |paths: &[PathBuf], flag: &'static str| {
        paths
            .iter()
            .map(|path| {
                let canonical = provenant_os::fs::canonicalize(path)
                    .ok()
                    .filter(|canonical| canonical.is_dir())
                    .ok_or_else(|| AccessError::MissingDirectory {
                        flag,
                        path: path.clone(),
                    })?;
                if !run::access_root_allowed(&canonical, &state, flag == "--read-root") {
                    return Err(AccessError::StateOverlap {
                        flag,
                        path: path.clone(),
                    });
                }
                Ok(canonical)
            })
            .collect::<Result<Vec<_>, AccessError>>()
    };
    let write_roots = resolve(&access.write_root, "--write-root")?;
    let mut dispatch_grants: Vec<run::LaneAccessGrant> = write_roots
        .iter()
        .zip(&access.write_root)
        .map(|(path, given)| run::LaneAccessGrant {
            kind: run::LaneAccessGrantKind::WriteRoot,
            path: path.clone(),
            given: given.display().to_string(),
        })
        .collect();
    // Worktree-write protects `.git` inside the worktree, so a write root that
    // is, or contains, the worktree overlaps it and the plan is invalid. A read
    // root on the worktree is valid and stays allowed.
    if let Some(worktree) = access.worktree.as_deref() {
        let worktree =
            provenant_os::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
        if let Some(root) = write_roots.iter().find(|root| worktree.starts_with(root)) {
            return Err(AccessError::WorktreeOverlap(root.clone()));
        }
    }
    let mut attachments = Vec::new();
    for path in &access.attach {
        let canonical = match provenant_os::fs::canonicalize(path) {
            Ok(path) if path.is_file() => path,
            _ => {
                warn(
                    Warning::new(CONFIG_KEY_IGNORED, "cannot attach path")
                        .subject(path.display().to_string()),
                );
                continue;
            }
        };
        if !run::access_root_allowed(&canonical, &state, true) {
            return Err(AccessError::StateOverlap {
                flag: "--attach",
                path: path.clone(),
            });
        }
        dispatch_grants.push(run::LaneAccessGrant {
            kind: run::LaneAccessGrantKind::Attachment,
            path: canonical.clone(),
            given: path.display().to_string(),
        });
        attachments.push(canonical);
    }
    let read_roots = resolve(&access.read_root, "--read-root")?;
    dispatch_grants.extend(
        read_roots
            .iter()
            .zip(&access.read_root)
            .map(|(path, given)| run::LaneAccessGrant {
                kind: run::LaneAccessGrantKind::ReadRoot,
                path: path.clone(),
                given: given.display().to_string(),
            }),
    );
    Ok(run::LaneAccessRequest {
        extra: Default::default(),
        capability_sources: Default::default(),
        shared_worktree: None,
        network,
        database_ipc: postgres,
        read_roots,
        attachments,
        write_roots,
        dispatch_grants,
        inputs_root: None,
        source_root: None,
        source_live: false,
        source_identity: None,
    })
}

/// One task of a submission, from CLI flags, a manifest or Fabric.
pub struct Submitted {
    /// Placement, account and build weight, described as for a graph node.
    pub attempt: provenant_fabric_runtime::graph::AttemptSpec,
    /// Require the selected adapter to have its own skill location.
    pub require_skill_location: bool,
    pub settlement_retry_limit: Option<u8>,
    pub context: Option<provenant_records::run::WorkContext>,
    pub title: Option<String>,
    pub prompt: String,
    pub base: Option<String>,
    /// Portable fixture bytes staged into the lane's candidate scratch.
    pub fixture: Option<run::StagedFixture>,
    pub pinned_base: Option<provenant_core::GitOid>,
    pub head: Option<String>,
    /// Admitted subject provenance carried beside `head` onto the run request.
    pub subject: Option<provenant_records::subject::PinnedSubject>,
    /// Original source of a predecessor whose committed head is being retained.
    pub source_head: Option<String>,
    pub route: run::RequestedRoute,
    pub provider_timeout: u64,
    pub delivery_id: Option<DeliveryId>,
    pub deliver: Option<run::WriterDeliveryRequest>,
    pub worktree: Option<PathBuf>,
    pub access: run::LaneAccessRequest,
}

pub struct Submission {
    pub chair: Option<provenant_records::custody::Chair>,
    pub operation_id: OperationId,
    pub task: Submitted,
    pub knobs: Vec<String>,
}

/// Scan, record and launch work: the one path the CLI and Fabric
/// share. Replaying an operation returns its run and launches nothing new.
pub fn submit(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
) -> Result<(run::AttemptRecord, Vec<run::Started>), Fail> {
    submit_with_host(session, edge, submission, call, None)
}

/// The launch policy a submission resolves: host knobs, then project rules,
/// then dispatch overrides. The disk precheck decides under this same policy.
fn submission_policy(
    config: &provenant_config::ResolvedConfig<crate::domains::Domains>,
    project: &str,
    profile: &provenant_records::run::HarnessProfile,
    attempt_overrides: &std::collections::BTreeMap<String, provenant_records::policy::Strength>,
) -> provenant_records::policy::Policy {
    let mut dispatch = provenant_config::policy::dispatch(profile);
    dispatch.extend(attempt_overrides.clone());
    config
        .domains
        .policy
        .resolve_with_knobs(project, &config.knobs, &dispatch)
}

/// A failed writer disk precheck: either the volumes could not be observed, or
/// the measured free space is below the floor. Only the latter is `DiskFloor`.
#[derive(Debug)]
pub(super) enum DiskCheck {
    Unobserved(String),
    Low {
        path: PathBuf,
        available: u64,
        floor: u64,
        reclaimed: u64,
        gc_scheduled: bool,
        origin: provenant_records::diagnostic::Layer,
    },
}

impl From<DiskCheck> for String {
    fn from(check: DiskCheck) -> Self {
        match check {
            DiskCheck::Unobserved(message) => message,
            DiskCheck::Low {
                path,
                available,
                floor,
                gc_scheduled,
                origin,
                ..
            } => format!(
                "insufficient disk space on {}: {available} bytes available, floor {floor} bytes; gc.critical_free source: {}; {}; run `provenant gc --free-floor {floor}B` before dispatching a writer",
                path.display(),
                origin.description(),
                if gc_scheduled {
                    "background GC scheduled; reclaimed bytes are not observed here"
                } else {
                    "background GC was not scheduled; reclaimed bytes are not observed here"
                }
            ),
        }
    }
}

pub(super) fn check_writer_disk(
    settings: &provenant_config::GcSettings,
    policy: &provenant_records::policy::Policy,
    state: &Path,
    worktree: &Path,
    home: &Path,
) -> Result<(), DiskCheck> {
    check_writer_disk_with_collection(
        settings,
        policy,
        state,
        worktree,
        |mut path| {
            loop {
                match provenant_os::fs::Directory::open_anchor(path) {
                    Ok(directory) => {
                        return directory
                            .available_bytes()
                            .map_err(|error| error.to_string());
                    }
                    Err(error) if error.is_not_found() => {
                        path = path.parent().ok_or_else(|| error.to_string())?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
        },
        || schedule_low_space_gc(state, home, worktree),
    )
}

/// Start the host-wide GC worker. It acquires the shared single-flight lock
/// before sweeping, without waiting on this launch.
#[allow(
    clippy::print_stderr,
    reason = "a failed GC schedule is a launch warning"
)]
pub(super) fn schedule_low_space_gc(state: &Path, home: &Path, worktree: &Path) -> bool {
    let start = || -> Result<(), Box<dyn std::error::Error>> {
        let argv = [
            std::env::current_exe()?.into_os_string(),
            "--state-dir".into(),
            state.as_os_str().to_owned(),
            "--project".into(),
            worktree.as_os_str().to_owned(),
            "gc".into(),
            "--scheduled".into(),
            "--apply".into(),
            "--json".into(),
        ];
        provenant_os::exec::spawn_detached(&argv, home)?;
        Ok(())
    };
    if let Err(error) = start() {
        warn(Warning::new(
            provenant_records::warning::GC_SCHEDULE_FAILED,
            format!("could not schedule low-space GC: {error}"),
        ));
        false
    } else {
        true
    }
}

#[cfg(test)]
pub(super) fn check_writer_disk_with(
    settings: &provenant_config::GcSettings,
    policy: &provenant_records::policy::Policy,
    state: &Path,
    worktree: &Path,
    available: impl FnMut(&Path) -> Result<u64, String>,
) -> Result<(), DiskCheck> {
    check_writer_disk_with_collection(settings, policy, state, worktree, available, || false)
}

pub(super) fn check_writer_disk_with_collection(
    settings: &provenant_config::GcSettings,
    policy: &provenant_records::policy::Policy,
    state: &Path,
    worktree: &Path,
    mut available: impl FnMut(&Path) -> Result<u64, String>,
    mut schedule_gc: impl FnMut() -> bool,
) -> Result<(), DiskCheck> {
    let observed = (|| -> Result<(), DiskCheck> {
        let limits = settings
            .disk_limits(state)
            .map_err(|error| DiskCheck::Unobserved(error.to_string()))?;
        let (mut limiting, mut minimum) = (worktree, u64::MAX);
        for path in [worktree, state] {
            let bytes = available(path).map_err(DiskCheck::Unobserved)?;
            if bytes < minimum {
                (limiting, minimum) = (path, bytes);
            }
        }
        if minimum < limits.free_floor {
            let gc_scheduled = settings.schedule && schedule_gc();
            // Admission and new builds take this same declared decision.
            match provenant_config::DiskGate::new(limits, policy).decide(minimum) {
                provenant_records::policy::Effect::Proceed => {}
                provenant_records::policy::Effect::Warn(warning) => {
                    warn(warning.subject(limiting.display().to_string()));
                }
                provenant_records::policy::Effect::Refuse(_)
                | provenant_records::policy::Effect::Stop(_) => {
                    return Err(DiskCheck::Low {
                        path: limiting.to_path_buf(),
                        available: minimum,
                        floor: limits.critical_free,
                        reclaimed: 0,
                        gc_scheduled,
                        origin: limits.critical_free_source,
                    });
                }
            }
        }
        Ok(())
    })();
    match observed {
        Err(DiskCheck::Unobserved(message)) => {
            warn(Warning::new(
                provenant_records::warning::DISK_OBSERVATION_UNAVAILABLE,
                format!(
                    "writer disk observation unavailable; continuing without the disk floor check: {message}"
                ),
            ));
            Ok(())
        }
        result => result,
    }
}

/// Reuse the host's pinned launch inputs after its guardian has settled.
pub fn submit_with_host(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
) -> Result<(run::AttemptRecord, Vec<run::Started>), Fail> {
    submit_with_host_and_wait(session, edge, submission, call, host, None)
}

/// Supply a verified destination used only if submission is refused before
/// admission. Accepted requests retain their ordinary chair and digest.
pub fn submit_with_host_for_refusal(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
    deadline: Option<Instant>,
    refusal: run::RefusalContext,
) -> Result<(run::AttemptRecord, Vec<run::Started>), Fail> {
    submit_with_host_context(session, edge, submission, call, host, deadline, refusal)
}

/// Submit with the caller's observation deadline, including the bounded wait
/// for provider preparation. A detached launcher retains custody after expiry.
#[allow(clippy::disallowed_methods)] // The caller's wait is a real deadline.
pub fn submit_with_host_and_wait(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
    deadline: Option<Instant>,
) -> Result<(run::AttemptRecord, Vec<run::Started>), Fail> {
    submit_with_host_context(
        session,
        edge,
        submission,
        call,
        host,
        deadline,
        run::RefusalContext::default(),
    )
}

#[allow(clippy::disallowed_methods)] // The caller's wait is a real deadline.
fn submit_with_host_context(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
    deadline: Option<Instant>,
    refusal: run::RefusalContext,
) -> Result<(run::AttemptRecord, Vec<run::Started>), Fail> {
    submit_with_launcher(
        session,
        edge,
        submission,
        call,
        refusal,
        |record, parent_attempt| {
            if parent_attempt {
                Ok(Vec::new())
            } else if let Some(host) = host {
                launch_on_host_with_wait(
                    session,
                    record.attempt_id,
                    edge,
                    call,
                    Some(host),
                    None,
                    deadline.map(|end| end.saturating_duration_since(Instant::now()).as_secs()),
                )
            } else if let Some(deadline) = deadline {
                launch_with_deadline(
                    session,
                    record,
                    edge,
                    call,
                    deadline.saturating_duration_since(Instant::now()),
                )
            } else {
                launch_prepared_with_wait(session, record.attempt_id, edge, call, None, None)
            }
        },
    )
}

/// Admit and seal a submission before giving the launcher its durable run ID.
/// This boundary lets a launcher retain custody independently of the caller.
fn submit_with_launcher(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
    refusal: run::RefusalContext,
    launch: impl FnOnce(&run::AttemptRecord, bool) -> Result<Vec<run::Started>, Fail>,
) -> Result<(run::AttemptRecord, Vec<run::Started>), Fail> {
    let project_policy = provenant_config::policy::read(
        edge.roots.state.as_path(),
        &session.context.project_id.to_string(),
        &Default::default(),
    );
    let (lane_depth, parent_attempt) = nested_dispatch_context(edge.roots, &project_policy)?;
    let config = crate::config_reads::read_for(edge.roots, "run")
        .map_err(|error| Fail::config(Status::Invalid, error))?;
    let profile = match &submission.task.attempt.sealed_profile {
        Some(profile) => profile.clone(),
        None => config
            .dispatch_profile(&submission.knobs)
            .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?,
    };
    // Nested proposals are checked by the host before provider preparation.
    // A confined parent cannot open a sibling worktree to sample its volume.
    if parent_attempt.is_none() {
        let task = &submission.task;
        {
            if let Some(path) = &task.worktree {
                let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)?;
                check_writer_disk(
                    &config.gc,
                    &submission_policy(
                        &config,
                        &session.context.project_id.to_string(),
                        &profile,
                        &task.attempt.policy_overrides,
                    ),
                    edge.roots.state.as_path(),
                    &writer.worktree,
                    edge.home,
                )
                .map_err(|check| {
                    let (kind, status) = match &check {
                        DiskCheck::Low { .. } => (ErrorKind::DiskFloor, Status::Refused),
                        DiskCheck::Unobserved(_) => (ErrorKind::Io, Status::Error),
                    };
                    let projection = match &check {
                        DiskCheck::Low { path, available, floor, reclaimed, gc_scheduled, origin } => {
                            let path = path.display();
                            Some(provenant_records::diagnostic::Diagnostic::new(kind.code()).category(kind.category())
                                .detail(format!("insufficient disk space on {path}: {available} bytes available, floor {floor} bytes; gc.critical_free from {}; {}; reclaimed bytes are not observed here", origin.description(), if *gc_scheduled { "background GC scheduled" } else { "background GC was not scheduled" }))
                                .next(format!("run `provenant gc --free-floor {floor}B` before dispatching a writer"))
                                .fact(provenant_records::diagnostic::DiagnosticFact::DiskPressure { available: *available, floor: *floor, reclaimed: *reclaimed, origin: *origin }))
                        }
                        DiskCheck::Unobserved(_) => Some(provenant_records::diagnostic::Diagnostic::new(kind.code()).category(kind.category())
                            .detail("disk observation failed").next("restore the host disk observation before retrying")
                            .fact(provenant_records::diagnostic::DiagnosticFact::DiskUnobserved)),
                    };
                    let mut fail = Fail::new(status, kind, projection.as_ref().and_then(|p| p.detail.clone()).unwrap_or_default());
                    fail.projection = projection.map(Box::new);
                    fail
                })?;
            }
        }
    }
    let task = {
        let task = submission.task;
        {
            let mut task_profile = profile.clone();
            if task.attempt.sealed_profile.is_none() && task.attempt.lane_access.is_some() {
                task_profile.knob_sources.insert(
                    "lanes.access".into(),
                    task.attempt
                        .budget_sources
                        .get("lane_access")
                        .copied()
                        .unwrap_or(provenant_records::Layer::Template),
                );
            }
            task_profile.policy_project = Some(session.context.project_id);
            task_profile.require_skill_location = task.require_skill_location;
            task_profile.settlement_retry_limit = task
                .settlement_retry_limit
                .map(|limit| limit.min(provenant_routing::MAX_SETTLEMENT_RETRIES));
            let mut attempt = task.attempt;
            // Seal direct-parent admission before hashing, just as graph and
            // peer admission do. Continuations retain this immutable fact.
            if !attempt.policy_sealed {
                attempt.parent_sandboxed |= parent_attempt
                    .is_some_and(|parent| session.store.attempt_floors_children(parent, call));
            }
            seal_launch_policy(
                &mut attempt,
                task.worktree.as_deref().unwrap_or(edge.cwd),
                &task.prompt,
                &task.access,
                task.base.as_deref(),
                task.head.as_deref(),
            )?;
            task_profile.write_scope = attempt.write_scope.clone();
            task_profile.policy_overrides = attempt.policy_overrides.clone();
            task_profile.repository_policy = attempt.launch_policy.clone();
            task_profile.confidential = attempt.confidential;
            task_profile.parent_sandboxed = attempt.parent_sandboxed;
            task_profile.admitted_by = attempt.admitted_by.clone();

            if attempt.new_worktree.is_some()
                || !attempt.capabilities.is_empty()
                || !attempt.write_roots.is_empty()
            {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "resolve the attempt's new worktree, capabilities and write roots before submission",
                ));
            }
            Ok::<_, Fail>(run::DispatchTask {
                context: task.context,
                build_heavy: attempt.build_heavy,
                profile: task_profile,
                title: task.title,
                prompt: task.prompt,
                base: task.base,
                fixture: task.fixture,
                pinned_base: task.pinned_base,
                head: task.head,
                subject: task.subject,
                source_head: task.source_head,
                route: task.route,
                account: attempt.account,
                timeout_seconds: attempt.provider_timeout.unwrap_or(task.provider_timeout),
                delivery_id: task.delivery_id,
                deliver: task.deliver,
                worktree: task.worktree,
                access: task.access,
            })
        }
    }?;
    let chair = parent_attempt
        .is_none()
        .then_some(submission.chair)
        .flatten();
    let record = run::Service::new(&session.store)
        .with_refusal_context(refusal)
        .dispatch(
            run::DispatchRequest {
                chair,
                operation_id: submission.operation_id,
                task,
                lane_depth,
                parent_attempt,
            },
            edge.cwd,
            call,
        )?;
    if parent_attempt.is_none() {
        let canonical = session.store.request(record.attempt_id, call)?;
        let graph = record.key.graph().to_owned();
        with_chair_index(edge.roots, call, |index, call| {
            index.submit(session.context.project_id, &graph, &canonical, call)
        })?;
        let operation = session.store.attempt_operation(record.attempt_id, call)?;
        {
            if let Some(writer) = &canonical.task.writer {
                provenant_confinement::worktrees::with_context_using_facts(
                    edge.roots,
                    edge.home,
                    &writer.project,
                    call.clock,
                    call.cancel,
                    session.store.repo_facts(),
                    |context| {
                        use provenant_confinement::worktrees;
                        let tree =
                            provenant_confinement::verify_worktree(context.repo, &writer.worktree)?;
                        if let Some(mut owner) = worktrees::load_owner(&tree)?
                            && owner.origin == worktrees::WorktreeOrigin::Lane
                            && owner.operation_id.as_ref() == Some(&operation.operation_id)
                        {
                            let expected = owner.clone();
                            owner.attempt_id = Some(record.attempt_id);
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
        }
    }
    #[cfg(any(test, feature = "fixture"))]
    if TEST_DEFER_PROVIDER_LAUNCH.with(std::cell::Cell::get) {
        return Ok((record, Vec::new()));
    }
    let started = launch(&record, parent_attempt.is_some())?;
    Ok((record, started))
}
