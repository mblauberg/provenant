use super::*;

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
                extra: Default::default(),
                provider: provider.into(),
                name: entry.into(),
            })
        }
        (None, Some(name)) => Ok(run::RequestedRoute::Named {
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
/// existing directories, stored canonically, and clear of credential and state
/// roots; the confinement compiler also checks broad and Git roots.
pub fn lane_access(
    access: &crate::Access,
    state_root: &Path,
    home: &Path,
) -> Result<run::LaneAccess, Fail> {
    lane_access_checked(access, state_root, home)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))
}

/// Emit an advisory for an unsupported capability without granting it.
pub fn warn_ignored_capability(origin: &str, name: &str) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: ignored unknown {origin}capability {}",
        plain_text(name, 128)
    );
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AccessError {
    #[error("{flag} {} is not an existing directory", path.display())]
    MissingDirectory { flag: &'static str, path: PathBuf },
    #[error("{flag} {} overlaps the Provenant state root", path.display())]
    StateOverlap { flag: &'static str, path: PathBuf },
    #[error("{flag} {} is or encloses a credential root", path.display())]
    CredentialRoot { flag: &'static str, path: PathBuf },
    #[error("--write-root {} overlaps the --worktree; worktree-write already grants the worktree", .0.display())]
    WorktreeOverlap(PathBuf),
}

/// Refuse a resolved root by the same rule the confinement compiler applies,
/// naming the flag and the path in the message.
pub(super) fn refused_root(
    flag: &'static str,
    given: &Path,
    canonical: &Path,
    home: &Path,
) -> Option<AccessError> {
    provenant_confinement::refuses_lane_root(canonical, flag == "--read-root", Some(home)).then(
        || AccessError::CredentialRoot {
            flag,
            path: given.to_path_buf(),
        },
    )
}

pub(super) fn lane_access_checked(
    access: &crate::Access,
    state_root: &Path,
    home: &Path,
) -> Result<run::LaneAccess, AccessError> {
    let mut postgres = false;
    for name in &access.capability {
        match name.as_str() {
            "postgres" => postgres = true,
            other => warn_ignored_capability("", other),
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
                if let Some(error) = refused_root(flag, path, &canonical, home) {
                    return Err(error);
                }
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
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: cannot attach {}",
                    path.display()
                );
                continue;
            }
        };
        if let Some(error) = refused_root("--read-root", path, &canonical, home) {
            return Err(error);
        }
        if !run::access_root_allowed(&canonical, &state, true) {
            return Err(AccessError::StateOverlap {
                flag: "--attach",
                path: path.clone(),
            });
        }
        attachments.push(canonical);
    }
    Ok(run::LaneAccess {
        extra: Default::default(),
        network,
        database_ipc: postgres,
        read_roots: resolve(&access.read_root, "--read-root")?,
        attachments,
        write_roots,
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
    pub pinned_base: Option<provenant_core::GitOid>,
    pub head: Option<String>,
    /// Original source of a predecessor whose committed head is being retained.
    pub source_head: Option<String>,
    pub route: run::RequestedRoute,
    pub provider_timeout: u64,
    pub delivery_id: Option<DeliveryId>,
    pub deliver: Option<run::WriterDeliveryRequest>,
    pub worktree: Option<PathBuf>,
    pub access: run::LaneAccess,
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
        origin: provenant_records::diagnostic::SettingSource,
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
                reclaimed,
                origin,
            } => format!(
                "insufficient disk space on {}: {available} bytes available, floor {floor} bytes; gc.critical_free source: {}; bounded GC reported {reclaimed} bytes reclaimed; run `provenant gc --free-floor {floor}B` before dispatching a writer",
                path.display(),
                origin.description()
            ),
        }
    }
}

pub(super) fn check_writer_disk(
    settings: &provenant_config::GcSettings,
    state: &Path,
    worktree: &Path,
    project: &Path,
    home: &Path,
) -> Result<(), DiskCheck> {
    check_writer_disk_with_collection(
        settings,
        state,
        worktree,
        home,
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
        || pressure_collection(state, home, project),
    )
}

/// Run the same pressure classes as GC in a separate bounded process. A killed
/// collector leaves identity-bearing tombstones for the next sweep.
pub(super) fn pressure_collection(state: &Path, home: &Path, project: &Path) -> u64 {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
    let collect = || -> Result<u64, Box<dyn std::error::Error>> {
        let program = std::env::current_exe()?;
        let argv = vec![
            program.into_os_string(),
            "--state-dir".into(),
            state.as_os_str().to_owned(),
            "--project".into(),
            project.as_os_str().to_owned(),
            "gc".into(),
            "--pressure".into(),
            "--apply".into(),
            "--json".into(),
        ];
        let mut options = ExecOptions::new(&argv, home, Duration::from_secs(5));
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let output = SystemExecutor::new(1)?.execute(options)?;
        if output.stdout.truncated || output.drain_truncated || output.stdout.sink_lost_bytes != 0 {
            return Ok(0);
        }
        let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
        let data = &value["data"];
        let temporary = data["provider_state"]["attempt_tmp"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["bytes"].as_u64())
            .fold(0_u64, u64::saturating_add);
        Ok(data["bytes_freed"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(temporary))
    };
    collect().unwrap_or(0)
}

#[cfg(test)]
pub(super) fn check_writer_disk_with(
    settings: &provenant_config::GcSettings,
    state: &Path,
    worktree: &Path,
    home: &Path,
    available: impl FnMut(&Path) -> Result<u64, String>,
) -> Result<(), DiskCheck> {
    check_writer_disk_with_collection(settings, state, worktree, home, available, || 0)
}

#[allow(clippy::print_stderr)]
pub(super) fn check_writer_disk_with_collection(
    settings: &provenant_config::GcSettings,
    state: &Path,
    worktree: &Path,
    home: &Path,
    mut available: impl FnMut(&Path) -> Result<u64, String>,
    mut collect: impl FnMut() -> u64,
) -> Result<(), DiskCheck> {
    let observed = (|| -> Result<(), DiskCheck> {
        let floor = settings
            .free_floor_bytes(state)
            .map_err(|error| DiskCheck::Unobserved(error.to_string()))?;
        let critical = settings
            .critical_free_bytes()
            .map_err(|error| DiskCheck::Unobserved(error.to_string()))?;
        let cache = home.join("Library/Caches/provenant-lane-cargo");
        let (mut limiting, mut minimum) = (worktree, u64::MAX);
        for path in [worktree, state, cache.as_path()] {
            let bytes = available(path).map_err(DiskCheck::Unobserved)?;
            if bytes < minimum {
                (limiting, minimum) = (path, bytes);
            }
        }
        if minimum < floor || minimum < critical {
            let reclaimed = if settings.schedule { collect() } else { 0 };
            minimum = u64::MAX;
            for path in [worktree, state, cache.as_path()] {
                let bytes = available(path).map_err(DiskCheck::Unobserved)?;
                if bytes < minimum {
                    (limiting, minimum) = (path, bytes);
                }
            }
            if minimum < floor {
                eprintln!(
                    "warning: {minimum} bytes free; gc.free_floor={floor} (source: {}); builds continue above gc.critical_free={critical} (source: {})",
                    if settings.free_floor.is_some() {
                        "configuration"
                    } else {
                        "compiled default"
                    },
                    if settings.critical_free.is_some() {
                        "configuration"
                    } else {
                        "compiled default"
                    }
                );
            }
            if minimum < critical {
                return Err(DiskCheck::Low {
                    path: limiting.to_path_buf(),
                    available: minimum,
                    floor: critical,
                    reclaimed,
                    origin: if settings.critical_free.is_some() {
                        provenant_records::diagnostic::SettingSource::Configuration
                    } else {
                        provenant_records::diagnostic::SettingSource::ShippedDefault
                    },
                });
            }
        }
        Ok(())
    })();
    match observed {
        Err(DiskCheck::Unobserved(message)) => {
            eprintln!(
                "provenant: writer disk observation unavailable; continuing without the disk floor check: {message}"
            );
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
    let (lane_depth, parent_attempt) = nested_dispatch_context(edge.roots)?;
    let config = crate::config_reads::read_for(edge.roots, "run")
        .map_err(|error| Fail::config(Status::Invalid, error))?;
    // Nested proposals are checked by the host before provider preparation.
    // A confined parent cannot open a sibling worktree to sample its volume.
    if parent_attempt.is_none() {
        let task = &submission.task;
        {
            if let Some(path) = &task.worktree {
                let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)?;
                check_writer_disk(
                    &config.gc,
                    edge.roots.state.as_path(),
                    &writer.worktree,
                    &writer.project,
                    edge.home,
                )
                .or_else(|check| {
                    let profile = config.dispatch_profile(&submission.knobs);
                    let dispatch = profile.as_ref().map(provenant_config::policy::dispatch).unwrap_or_default();
                    let policy = config.domains.policy.resolve_with_knobs(&session.context.project_id.to_string(), &config.knobs, &dispatch);
                    let (code, detail) = match &check {
                        DiskCheck::Low { .. } => (
                            "LAUNCH_DISK",
                            "writer disk observation is below its configured floor",
                        ),
                        DiskCheck::Unobserved(_) => (
                            "SUBMIT_CHECK_WRITER_DISK_WITH_COLLECTION_NONE",
                            "writer disk space could not be observed",
                        ),
                    };
                    let effect = policy.check(code, provenant_records::policy::Facts::violation(detail));
                    if effect.blocks() { Err(check) } else {
                        use std::io::Write;
                        if let Some(message) = effect.message() { let _ = writeln!(std::io::stderr().lock(), "warning: {message}"); }
                        Ok(())
                    }
                })
                .map_err(|check| {
                    let (kind, status) = match &check {
                        DiskCheck::Low { .. } => (ErrorKind::DiskFloor, Status::Refused),
                        DiskCheck::Unobserved(_) => (ErrorKind::Io, Status::Error),
                    };
                    let projection = match &check {
                        DiskCheck::Low { path, available, floor, reclaimed, origin } => {
                            let path = provenant_records::diagnostic::safe_path(path, provenant_records::diagnostic::Destination::Local)
                                .unwrap_or_else(|| "the limiting volume".into());
                            Some(provenant_records::diagnostic::Diagnostic::new(kind.code()).category(kind.category())
                                .detail(format!("insufficient disk space on {path}: {available} bytes available, floor {floor} bytes; gc.free_floor from {}; bounded GC reported {reclaimed} bytes reclaimed", origin.description()))
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
    let profile = match &submission.task.attempt.sealed_profile {
        Some(profile) => profile.clone(),
        None => config
            .dispatch_profile(&submission.knobs)
            .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?,
    };
    let task = {
        let task = submission.task;
        {
            let mut task_profile = profile.clone();
            task_profile.extra.insert(
                "policy_project".into(),
                serde_json::json!(session.context.project_id.to_string()),
            );
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
            if let Some(policy) = &attempt.launch_policy {
                task_profile.extra.insert(
                    provenant_records::run::LAUNCH_POLICY_FIELD.into(),
                    serde_json::to_value(policy).map_err(|error| {
                        Fail::new(Status::Invalid, ErrorKind::Config, error.to_string())
                    })?,
                );
            }
            if let Some(admitted_by) = &attempt.admitted_by {
                task_profile
                    .extra
                    .insert("admitted_by".into(), serde_json::json!(admitted_by));
            }
            for key in ["write_scope", "policy_overrides"] {
                if let Some(value) = attempt.extra.get(key) {
                    task_profile.extra.insert(key.into(), value.clone());
                }
            }
            let mut dispatch_policy = provenant_config::policy::dispatch(&task_profile);
            if let Some(value) = attempt.extra.get("policy_overrides")
                && let Ok(values) = serde_json::from_value::<
                    std::collections::BTreeMap<String, provenant_records::policy::Strength>,
                >(value.clone())
            {
                dispatch_policy.extend(values);
            }
            let policy = config.domains.policy.resolve_with_knobs(
                &session.context.project_id.to_string(),
                &config.knobs,
                &dispatch_policy,
            );
            task_profile
                .extra
                .insert("launch_policy".into(), serde_json::json!(policy));
            if attempt.confidential {
                task_profile
                    .extra
                    .insert("confidential".into(), serde_json::json!(true));
            }
            if attempt.parent_sandboxed {
                task_profile.extra.insert(
                    provenant_records::run::PARENT_SANDBOXED_FIELD.into(),
                    serde_json::json!(true),
                );
            }
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
                pinned_base: task.pinned_base,
                head: task.head,
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
