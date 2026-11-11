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

/// Strict manifest: `{schema_version:1,tasks:[...]}`.
pub(super) fn read_bounded(path: &Path) -> Result<String, Fail> {
    let invalid = |error: &dyn std::fmt::Display| {
        input_failure(
            Status::Invalid,
            ErrorKind::Invalid,
            format!("{}: {error}", path.display()),
            provenant_fabric_runtime::coordination::facade::InvalidInputDetail::InputUnreadable,
        )
    };
    // A FIFO or device would block or never end.
    if !std::fs::metadata(path).map_err(|e| invalid(&e))?.is_file() {
        return Err(invalid(&"not a regular file"));
    }
    read_text(std::fs::File::open(path).map_err(|e| invalid(&e))?)
}

fn input_failure(
    status: Status,
    kind: ErrorKind,
    message: impl Into<String>,
    detail: provenant_fabric_runtime::coordination::facade::InvalidInputDetail,
) -> Fail {
    let error = provenant_fabric_runtime::coordination::facade::FacadeError::invalid(detail);
    let mut diagnostic = error.diagnostic();
    diagnostic.code = kind.code().into();
    let mut fail = Fail::new(status, kind, message);
    fail.projection = Some(Box::new(diagnostic));
    fail.detail = Some(Box::new(error));
    fail
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn input_failures_keep_refusals_and_project_safe_reasons() {
        let oversized = vec![b'x'; (PROMPT_LIMIT + 1) as usize];
        let Err(too_large) = read_text(oversized.as_slice()) else {
            panic!("accepted oversized input")
        };
        assert_eq!(too_large.kind, ErrorKind::Invalid);
        assert_eq!(too_large.message, "input exceeds 1 MiB");
        let detail = too_large.detail.unwrap().diagnostic();
        assert!(detail.detail.unwrap().contains("1 MiB"));
        assert!(detail.next.is_some());

        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("TOKEN_SENTINEL-no-such-input");
        let Err(unreadable) = read_bounded(&missing) else {
            panic!("accepted missing input")
        };
        assert_eq!(unreadable.kind, ErrorKind::Invalid);
        let projected = unreadable.detail.unwrap().diagnostic();
        assert!(projected.detail.as_ref().unwrap().contains("unreadable"));
        assert!(
            !serde_json::to_string(&projected)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
    }

    #[test]
    fn unreadable_stream_keeps_io_refusal_and_safe_input_reason() {
        struct FailedRead;
        impl std::io::Read for FailedRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("TOKEN_SENTINEL /private/host"))
            }
        }
        let fail = read_text(FailedRead).unwrap_err();
        assert_eq!(fail.kind, ErrorKind::Io);
        assert!(fail.message.contains("TOKEN_SENTINEL"));
        let rendered = super::super::command_error(&fail);
        assert_eq!(rendered.code, "IO");
        assert!(rendered.cause.as_deref().unwrap().contains("input"));
        assert!(rendered.repair.is_some());
        assert!(!rendered.cause.unwrap().contains("TOKEN_SENTINEL"));
        let projection = fail.projection.unwrap();
        assert_eq!(projection.code, "IO");
        assert!(projection.detail.as_deref().unwrap().contains("input"));
        assert!(projection.next.is_some());
        let error = fail.detail.unwrap();
        let decoded: provenant_fabric_runtime::coordination::facade::FacadeError =
            serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
        let wire = serde_json::to_string(&decoded.diagnostic()).unwrap();
        assert!(!wire.contains("TOKEN_SENTINEL"));
        assert!(!wire.contains("/private/host"));
        assert!(wire.contains("input"));
    }
}

pub(super) fn read_text(source: impl Read) -> Result<String, Fail> {
    let mut bytes = Vec::new();
    source
        .take(PROMPT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            let fact = provenant_records::diagnostic::IoFact::InputUnreadable;
            let detail = provenant_fabric_runtime::coordination::facade::FacadeError::Io {
                operation: "read input".into(),
                path: None,
                delivery_limit: None,
                delivery_stage: None,
                resource: None,
                input_failure: Some(fact),
                cause: None,
            };
            let mut projection = detail.diagnostic();
            projection.code = ErrorKind::Io.code().into();
            projection.category = Some(ErrorKind::Io.category().into());
            let mut fail = Fail::new(Status::Error, ErrorKind::Io, error.to_string());
            fail.projection = Some(Box::new(projection));
            fail.detail = Some(Box::new(detail));
            fail
        })?;
    if bytes.len() as u64 > PROMPT_LIMIT {
        return Err(input_failure(
            Status::Invalid,
            ErrorKind::Invalid,
            "input exceeds 1 MiB",
            provenant_fabric_runtime::coordination::facade::InvalidInputDetail::InputTooLarge,
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        input_failure(
            Status::Invalid,
            ErrorKind::Invalid,
            "input is not UTF-8",
            provenant_fabric_runtime::coordination::facade::InvalidInputDetail::InputInvalidUtf8,
        )
    })
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

/// Task IDs default to a value derived from the operation, so replaying the
/// same operation reproduces the same canonical request.
pub fn derived_task(operation: &OperationId, index: usize) -> Result<TaskId, Fail> {
    let digest = CanonicalV1::record_digest("run-task-id", 1, &(operation.as_str(), index))
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(TaskId::from_bytes(bytes))
}

/// Refuse input that matches a secret pattern before any record exists. The
/// message never repeats the input.
pub(super) fn scan(prompt: &str) -> Result<(), Fail> {
    run::Service::scan(prompt).map_err(Fail::from)
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
    pub task_id: TaskId,
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
    pub chair: Option<run::ChairBinding>,
    pub operation_id: OperationId,
    pub tasks: Vec<Submitted>,
    pub concurrency: Option<u64>,
    pub knobs: Vec<String>,
}

/// Scan, record and launch work: the one path the CLI and Fabric
/// share. Replaying an operation returns its run and launches nothing new.
pub fn submit(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
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
                "insufficient disk space on {}: {available} bytes available, floor {floor} bytes; gc.free_floor source: {}; bounded GC reported {reclaimed} bytes reclaimed; run `provenant gc --free-floor {floor}B` before dispatching a writer",
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
        let cache = home.join("Library/Caches/provenant-lane-cargo");
        let (mut limiting, mut minimum) = (worktree, u64::MAX);
        for path in [worktree, state, cache.as_path()] {
            let bytes = available(path).map_err(DiskCheck::Unobserved)?;
            if bytes < minimum {
                (limiting, minimum) = (path, bytes);
            }
        }
        if minimum < floor {
            let reclaimed = if settings.schedule { collect() } else { 0 };
            minimum = u64::MAX;
            for path in [worktree, state, cache.as_path()] {
                let bytes = available(path).map_err(DiskCheck::Unobserved)?;
                if bytes < minimum {
                    (limiting, minimum) = (path, bytes);
                }
            }
            if minimum < floor {
                return Err(DiskCheck::Low {
                    path: limiting.to_path_buf(),
                    available: minimum,
                    floor,
                    reclaimed,
                    origin: if settings.free_floor.is_some() {
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
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
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
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
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
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
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
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
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
                    record.run_id,
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
                launch_prepared_with_wait(session, record.run_id, edge, call, None, None)
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
    launch: impl FnOnce(&run::RunRecord, bool) -> Result<Vec<run::Started>, Fail>,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    let (lane_depth, parent_attempt) = nested_dispatch_context(edge.roots)?;
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::config(Status::Invalid, error))?;
    // Nested proposals are checked by the host before provider preparation.
    // A confined parent cannot open a sibling worktree to sample its volume.
    if parent_attempt.is_none() {
        for task in &submission.tasks {
            if let Some(path) = &task.worktree {
                let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)?;
                check_writer_disk(
                    &config.gc,
                    edge.roots.state.as_path(),
                    &writer.worktree,
                    &writer.project,
                    edge.home,
                )
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
    let profile = config
        .dispatch_profile(&submission.knobs)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    let read_only = submission.tasks.iter().all(|task| task.worktree.is_none());
    let bound = (read_only
        && config.knob_is_set("lanes.max_concurrency", &submission.knobs)
        && !config.knob_is_set("lanes.max_concurrency_read_only", &submission.knobs))
    .then(|| profile.knob_u64("lanes.max_concurrency"))
    .flatten();
    let concurrency = run::concurrency(submission.concurrency, &profile, read_only, bound);
    let tasks = submission
        .tasks
        .into_iter()
        .map(|task| {
            let mut task_profile = profile.clone();
            task_profile.require_skill_location = task.require_skill_location;
            task_profile.settlement_retry_limit = task
                .settlement_retry_limit
                .map(|limit| limit.min(provenant_routing::MAX_SETTLEMENT_RETRIES));
            let attempt = task.attempt;
            if attempt.confidential { task_profile.extra.insert("confidential".into(), serde_json::json!(true)); }
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
            Ok(run::DispatchTask {
                context: task.context,
                build_heavy: attempt.build_heavy,
                profile: task_profile,
                title: task.title,
                task_id: task.task_id,
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
        })
        .collect::<Result<Vec<_>, Fail>>()?;
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
                tasks,
                concurrency,
                lane_depth,
                parent_attempt,
            },
            edge.cwd,
            call,
        )?;
    if parent_attempt.is_none() {
        let canonical = session.store.request(record.run_id, call)?;
        with_chair_index(edge.roots, call, |index, call| {
            index.submit(session.context.project_id, record.run_id, &canonical, call)
        })?;
        let pending = session.store.pending_attempts(record.run_id, call)?;
        for task in &canonical.tasks {
            if let Some(writer) = &task.writer {
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
                            && owner.operation_id.as_ref() == Some(&record.operation_id)
                            && let Some(attempt) = pending
                                .iter()
                                .find(|attempt| attempt.task_id == task.task_id)
                        {
                            let expected = owner.clone();
                            owner.attempt_id = Some(attempt.attempt_id);
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
