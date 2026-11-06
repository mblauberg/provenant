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
            E::Invalid(_) | E::InvalidRemote => FacadeError::invalid(error.to_string()),
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
                matches!(command, "fabric graph status"),
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
/// Fetch every remote with this host's own Git authority, then require `base`
/// on a remote-tracking ref, so a peer writer never starts from a commit only
/// this clone has. Returns the pinned base commit.
pub(crate) fn fetch_remote_base(
    project: &Path,
    base: &str,
    timeout: std::time::Duration,
) -> Result<provenant_core::GitOid, FacadeError> {
    remote_base(project, base, timeout, true)
}
/// [`fetch_remote_base`], skipping the fetch when `fetch` is false so a caller
/// pinning several bases fetches once.
pub(crate) fn remote_base(
    project: &Path,
    base: &str,
    timeout: std::time::Duration,
    fetch: bool,
) -> Result<provenant_core::GitOid, FacadeError> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
    let git = provenant_os::git::git_executable().ok_or(FacadeError::OwnerUnavailable)?;
    let environment: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| !key.to_str().is_some_and(|key| GIT_REDIRECTS.contains(&key)))
        .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
        .collect();
    let executor = SystemExecutor::new(1).map_err(|_| FacadeError::io("create Git executor"))?;
    let git_output = |args: &[&str]| -> Result<Vec<u8>, FacadeError> {
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
        let mut options = ExecOptions::new(&argv, project, timeout);
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 4096 };
        options.stderr = StreamPolicy::Discard;
        let output = executor
            .execute(options)
            .map_err(|_| FacadeError::BaseUnreachable)?;
        if output.termination != Termination::Exit(0) || output.timed_out {
            return Err(FacadeError::BaseUnreachable);
        }
        Ok(output.stdout.bytes)
    };
    if fetch {
        git_output(&["fetch", "--all", "--no-tags", "--no-recurse-submodules"])?;
    }
    let contains = format!("--contains={base}");
    let refs = git_output(&[
        "for-each-ref",
        "--count=1",
        "--format=%(refname)",
        &contains,
        "refs/remotes",
    ])?;
    if refs.iter().all(u8::is_ascii_whitespace) {
        return Err(FacadeError::BaseUnreachable);
    }
    let commit = format!("{base}^{{commit}}");
    let oid = git_output(&["rev-parse", "--verify", "--end-of-options", &commit])?;
    std::str::from_utf8(&oid)
        .ok()
        .and_then(|oid| provenant_core::GitOid::parse(oid.trim()).ok())
        .ok_or(FacadeError::BaseUnreachable)
}
