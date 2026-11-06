use super::*;

pub(crate) fn landing_error(error: &(dyn std::error::Error + 'static)) -> FacadeError {
    use provenant_fabric_runtime::git_transport::GitTransportError as E;
    if let Some(error) = error.downcast_ref::<E>() {
        return match error {
            E::PushUnknown { operation } | E::PushUnresolved { operation } => {
                FacadeError::LaunchUnknown {
                    operation_id: operation.clone(),
                }
            }
            E::Store(error) => FacadeError::from(error),
            E::PushNotFound { .. } => FacadeError::NotFound,
            E::Invalid(_) | E::InvalidRemote => FacadeError::InvalidInput { detail: None },
            E::DirtyWorktree
            | E::IncompleteRepository
            | E::OidMismatch
            | E::NonFastForward
            | E::TransportConflict
            | E::RemoteMoved => FacadeError::Conflict,
            _ => FacadeError::io("landing"),
        };
    }
    error
        .downcast_ref::<provenant_fabric_store::Error>()
        .map_or(FacadeError::io("landing"), FacadeError::from)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn landing_local(
    context: &Context,
    executor: &SystemExecutor,
    proof: LandingProof,
    operation_id: &provenant_core::OperationId,
    reconcile: bool,
    actor: provenant_core::ActorId,
    clock: &SystemClock,
    cancel: &CancelToken,
) -> Result<provenant_fabric_store::PushOperation, Box<dyn std::error::Error>> {
    let request = provenant_fabric_store::Request { clock, cancel };
    let store = provenant_fabric_store::Store::open(&context.roots.state, &request)?;
    let guard = store.lock_landing(context.identity.project, &proof.reference, &request)?;
    let fence = provenant_fabric_store::ClaimFence {
        extra: Default::default(),
        id: proof.id,
        actor,
        generation: proof.generation,
        token: proof.token,
    };
    let git = provenant_fabric_runtime::git_transport::GitTransport::discover_with_policy(
        executor,
        std::env::var_os("SSH_AUTH_SOCK").map(Into::into),
        context.config.ssh_executable.as_deref(),
    )?;
    let cwd = context.identity.cwd.to_path_buf()?;
    if reconcile {
        Ok(git.reconcile_landing(
            &store,
            &guard,
            &fence,
            context.identity.project,
            operation_id,
            &request,
            &cwd,
            cancel,
        )?)
    } else {
        Ok(git.push_landing(&store, &guard, &fence, &cwd, operation_id, &request, cancel)?)
    }
}
pub fn landing(
    input: provenant_cli::fabric::Landing,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    use provenant_cli::fabric::Landing;
    let (fence, operation, reconcile) = match input {
        Landing::Push { fence, operation } => (fence, operation, false),
        Landing::Reconcile { fence, operation } => (fence, operation, true),
        _ => return 2,
    };
    let recovery_id = operation.operation_id.clone();
    let command = if reconcile {
        "fabric landing reconcile"
    } else {
        "fabric landing push"
    };
    let result =
        (|| -> Result<provenant_fabric_store::PushOperation, Box<dyn std::error::Error>> {
            let proof = fence.proof()?;
            let factory = factory(globals, workers)?;
            let ctx = &factory.context;
            if ctx.in_lane {
                return Err(CoordinationError::LaneMutation.into());
            }
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(factory.clock.mono_now(), 300)?;
            if let Some(project) = &ctx.identity.federation
                && let Some(host) = ctx
                    .config
                    .home_hosts
                    .get(project.key.as_str())
                    .filter(|h| **h != ctx.identity.host)
            {
                let peer = ctx
                    .config
                    .peers
                    .values()
                    .find(|p| p.host == *host)
                    .ok_or(PeerError::Unavailable)?;
                let executable = provenant_fabric_runtime::hosts::discover_ssh_executable(
                    ctx.config.ssh_executable.as_deref(),
                )?;
                let request = PeerRequest::new(
                    *host,
                    ctx.identity.host,
                    ctx.identity.inbox.agent.clone(),
                    ProjectLocator::new(&ctx.identity, &factory.home)?,
                    PeerAction::LandingPush {
                        extra: Default::default(),
                        proof,
                        operation_id: operation.operation_id,
                        reconcile,
                    },
                );
                let transport = SshTransport {
                    executor: &factory.executor,
                    clock: &factory.clock,
                    home: &factory.home,
                    cwd: &factory.cwd,
                    executable: executable.clone(),
                    seconds: ctx.config.limits.peer_seconds,
                };
                return match transport.exchange(peer, &request, budget, &cancel)?.result {
                    PeerResult::Push { data: value, .. } if value.id == recovery_id => Ok(value),
                    PeerResult::Error {
                        data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
                        ..
                    } => {
                        if let FacadeError::LaunchUnknown { operation_id } = &error
                            && operation_id != &recovery_id
                        {
                            return Err(PeerError::Frame.into());
                        }
                        if matches!(error, FacadeError::LaunchUnknown { .. }) {
                            Err(PeerError::Unknown {
                                operation_id: recovery_id.clone(),
                            }
                            .into())
                        } else {
                            Err(PeerError::Refused { error }.into())
                        }
                    }
                    _ => Err(PeerError::Frame.into()),
                };
            }
            landing_local(
                ctx,
                &factory.executor,
                proof,
                &operation.operation_id,
                reconcile,
                ctx.identity.inbox.actor,
                &factory.clock,
                &cancel,
            )
        })();
    match result {
        Ok(data) => {
            let value = serde_json::to_value(&data).unwrap_or_default();
            let human = provenant_cli::terse(&value);
            let summary = graph_summary(&value);
            crate::emit_summarised(
                &provenant_records::CommandResult::new(
                    command,
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                matches!(command, "status"),
                Some(&summary),
            )
        }
        Err(error)
            if matches!(
                error.downcast_ref::<PeerError>(),
                Some(PeerError::Unknown { .. })
            ) || error
                .downcast_ref::<provenant_fabric_runtime::git_transport::GitTransportError>()
                .is_some_and(|error| error.is_unknown()) =>
        {
            let mut result = provenant_records::CommandResult::new(
                command,
                provenant_records::Outcome::Error,
                serde_json::json!({"operation_id":recovery_id,"state":"unknown"}),
            );
            result.error = Some(provenant_records::CommandError {
                diagnostic: None,
                code: "LAUNCH_UNKNOWN".into(),
                message: "push outcome requires remote reconciliation".into(),
                cause: None,
                repair: Some(
                    "repeat fabric landing reconcile with the same fence and operation id".into(),
                ),
            });
            crate::emit(
                &result,
                globals,
                "push outcome requires remote reconciliation",
                false,
            )
        }
        Err(error)
            if matches!(
                error.downcast_ref::<PeerError>(),
                Some(PeerError::Refused {
                    error: FacadeError::Busy
                })
            ) =>
        {
            let message = error.to_string();
            let mut result = provenant_records::CommandResult::new(
                command,
                provenant_records::Outcome::Refused,
                serde_json::json!({"operation_id":recovery_id}),
            );
            result.error = Some(provenant_records::CommandError {
                diagnostic: None,
                code: "QUEUE_FULL".into(),
                message: message.clone(),
                cause: None,
                repair: Some(format!(
                    "repeat {command} with the same fence and operation id"
                )),
            });
            crate::emit(&result, globals, &message, false)
        }
        Err(error) => crate::fail(
            command,
            provenant_records::Outcome::Refused,
            if matches!(
                error.downcast_ref::<CoordinationError>(),
                Some(CoordinationError::LaneMutation)
            ) {
                "lane_coordination_refused"
            } else if matches!(error.downcast_ref::<PeerError>(), Some(PeerError::Frame)) {
                "state_corrupt"
            } else {
                "landing_refused"
            },
            &error.to_string(),
            globals,
        ),
    }
}

/// Git variables that would redirect a fetch away from the peer's clone or
/// let it prompt.
pub(crate) const GIT_REDIRECTS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_TERMINAL_PROMPT",
];
/// Project bounded repository facts without exposing worktree paths.
pub(crate) fn repository_fact_error(
    error: provenant_confinement::worktrees::WorktreeError,
) -> FacadeError {
    use provenant_confinement::{utility::UtilityError, worktrees::WorktreeError};
    match error {
        WorktreeError::Utility(UtilityError::Truncated { .. }) => FacadeError::io(
            provenant_fabric_runtime::coordination::facade::REPOSITORY_FACT_LIMIT_OPERATION,
        ),
        WorktreeError::Cancelled | WorktreeError::Utility(UtilityError::Cancelled) => {
            FacadeError::Cancelled
        }
        WorktreeError::Utility(UtilityError::TimedOut) => FacadeError::Timeout,
        _ => FacadeError::BaseUnreachable,
    }
}

/// Fetch with host Git authority, resolve through the command facts session,
/// and require the pinned commit on a remote-tracking ref. A caller pinning
/// several bases may skip later fetches.
pub(crate) fn remote_base(
    context: &provenant_confinement::worktrees::WorktreeContext<'_>,
    base: &str,
    timeout: std::time::Duration,
    fetch: bool,
    cancel: &CancelToken,
) -> Result<provenant_core::GitOid, FacadeError> {
    let project = context.repo.root_path();
    let revision = provenant_confinement::repo::GitRevision::new(base)
        .map_err(|_| FacadeError::BaseUnreachable)?;
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
    let git = provenant_os::git::git_executable().ok_or(FacadeError::OwnerUnavailable)?;
    let environment: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            key != "GIT_NO_REPLACE_OBJECTS"
                && key != "GIT_GRAFT_FILE"
                && !key.to_str().is_some_and(|key| GIT_REDIRECTS.contains(&key))
        })
        .chain([
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
            ("GIT_GRAFT_FILE".into(), "/dev/null".into()),
        ])
        .collect();
    let executor = SystemExecutor::new(1).map_err(|_| FacadeError::io("create Git executor"))?;
    let clock = SystemClock::new();
    let started = clock.mono_now().as_micros();
    let remaining = || {
        timeout.saturating_sub(std::time::Duration::from_micros(
            clock.mono_now().as_micros().saturating_sub(started),
        ))
    };
    let git_output = |args: &[&str]| -> Result<Vec<u8>, FacadeError> {
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled);
        }
        let remaining = remaining();
        if remaining.is_zero() {
            return Err(FacadeError::Timeout);
        }
        context
            .repo
            .revalidate()
            .map_err(|_| FacadeError::BaseUnreachable)?;
        let argv: Vec<std::ffi::OsString> = std::iter::once(git.as_os_str().to_owned())
            .chain(
                [
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                ]
                .iter()
                .chain(args)
                .map(std::ffi::OsString::from),
            )
            .collect();
        let mut options = ExecOptions::new(&argv, project, remaining);
        options.env = Some(&environment);
        options.cancel = Some(cancel);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture {
            limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
        };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let output = executor
            .execute(options)
            .map_err(|_| FacadeError::BaseUnreachable)?;
        if output.stdout.truncated || output.overflow_stopped {
            return Err(FacadeError::io(
                provenant_fabric_runtime::coordination::facade::REPOSITORY_FACT_LIMIT_OPERATION,
            ));
        }
        if cancel.is_cancelled() {
            return Err(FacadeError::Cancelled);
        }
        if output.timed_out {
            return Err(FacadeError::Timeout);
        }
        if output.termination != Termination::Exit(0) {
            return Err(FacadeError::BaseUnreachable);
        }
        Ok(output.stdout.bytes)
    };
    if fetch {
        context.facts.invalidate_inventory(context);
        git_output(&["fetch", "--all", "--no-tags", "--no-recurse-submodules"])?;
    }
    let remaining = remaining();
    if remaining.is_zero() {
        return Err(FacadeError::Timeout);
    }
    let fact_context = provenant_confinement::worktrees::WorktreeContext {
        utility_budget: context
            .utility_budget
            .with_timeout_cap(remaining)
            .map_err(|_| FacadeError::Timeout)?,
        ..*context
    };
    let oid = context
        .facts
        .resolve_revision(&fact_context, revision, cancel)
        .map_err(repository_fact_error)?;
    let contains = format!("--contains={oid}");
    let refs = git_output(&[
        "for-each-ref",
        "--format=%(objectname) %(symref)",
        &contains,
        "refs/remotes",
    ])?;
    let refs = std::str::from_utf8(&refs).map_err(|_| FacadeError::BaseUnreachable)?;
    let tips: std::collections::HashSet<_> = refs
        .lines()
        .filter_map(|line| {
            let (tip, symbolic) = line.split_once(' ')?;
            if symbolic.is_empty() {
                provenant_core::GitOid::parse(tip).ok()
            } else {
                None
            }
        })
        .collect();
    if tips.is_empty() {
        return Err(FacadeError::BaseUnreachable);
    }
    // A tracking ref can outlive its remote branch. An advertised branch with
    // the same tip proves it is still remote, regardless of fetch refspecs.
    let remotes = git_output(&["remote"])?;
    let remotes = std::str::from_utf8(&remotes).map_err(|_| FacadeError::BaseUnreachable)?;
    for remote in remotes.lines() {
        let advertised = match git_output(&["ls-remote", "--heads", "--", remote]) {
            Ok(advertised) => advertised,
            Err(FacadeError::BaseUnreachable) => continue,
            Err(error) => return Err(error),
        };
        let advertised =
            std::str::from_utf8(&advertised).map_err(|_| FacadeError::BaseUnreachable)?;
        for line in advertised.lines() {
            let Some((tip, name)) = line.split_once('\t') else {
                continue;
            };
            if name.starts_with("refs/heads/")
                && provenant_core::GitOid::parse(tip).is_ok_and(|tip| tips.contains(&tip))
            {
                return Ok(oid);
            }
        }
    }
    Err(FacadeError::BaseUnreachable)
}
