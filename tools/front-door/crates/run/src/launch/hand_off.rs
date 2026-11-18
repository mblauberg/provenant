use super::*;
use std::io::Write;

#[cfg(test)]
thread_local! {
    static TEST_UNCERTAIN_HANDOFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_HANDOFF_FAILURE: std::cell::RefCell<Option<RunError>> = const { std::cell::RefCell::new(None) };
}

fn typed_local(
    policy: &provenant_records::policy::Policy,
    phase: &str,
    error: &RunError,
) -> HandOff {
    if let RunError::Admission(error) = error
        && let Some(fact) = error.resource_fact()
    {
        return HandOff::Resource {
            message: format!("{phase}: {error}"),
            fact,
        };
    }
    if let RunError::Confinement(permission) = error
        && let Some(fact) = permission.resource_fact()
    {
        return HandOff::Resource {
            message: format!("{phase}: {error}"),
            fact,
        };
    }
    let cause = match error {
        RunError::Confinement(
            provenant_confinement::permissions::PermissionError::BackendNotAvailable(
                provenant_confinement::permissions::AvailabilityReason::UnsupportedPlatform,
            ),
        ) => Some(TerminalCause::UnsupportedPlatform),
        RunError::DeliveryPrepare {
            step: crate::DeliveryPrepareStep::CredentialHelper,
            ..
        } => Some(TerminalCause::DeliveryPrepareCredentialHelper),
        _ => None,
    };
    let message = format!("{phase}: {error}");
    let class = error.boundary_class();
    if class != provenant_records::diagnostic::BoundaryClass::None {
        let code = match class {
            provenant_records::diagnostic::BoundaryClass::Credentials => {
                "PREPARATION_PREPARE_WITH_CREDENTIALS"
            }
            provenant_records::diagnostic::BoundaryClass::LaneGit => {
                "HAND_OFF_RUNSTORE_HAND_OFF_GIT_ISOLATION"
            }
            provenant_records::diagnostic::BoundaryClass::Integrity => {
                "ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_DATA_INTEGRITY"
            }
            provenant_records::diagnostic::BoundaryClass::Authority => {
                "HAND_OFF_RUNSTORE_HAND_OFF_AUTHORITY"
            }
            provenant_records::diagnostic::BoundaryClass::None => {
                return HandOff::Local(message);
            }
        };
        let message = policy.boundary_error(
            code,
            provenant_records::policy::Facts::violation(&message),
            message,
        );
        HandOff::Boundary {
            message,
            class,
            cause,
        }
    } else if let Some(cause) = cause {
        HandOff::TypedLocal { message, cause }
    } else {
        HandOff::Local(message)
    }
}

fn boundary_handoff(
    policy: &provenant_records::policy::Policy,
    code: &str,
    message: String,
    class: provenant_records::diagnostic::BoundaryClass,
) -> HandOff {
    let message = policy.boundary_error(
        code,
        provenant_records::policy::Facts::violation(&message),
        message,
    );
    HandOff::Boundary {
        message,
        class,
        cause: None,
    }
}

fn custody_local(
    policy: &provenant_records::policy::Policy,
    phase: &str,
    error: &custody::CustodyError,
) -> HandOff {
    let message = format!("{phase}: {error}");
    match error {
        custody::CustodyError::Fenced
        | custody::CustodyError::UnknownIdentity
        | custody::CustodyError::InvalidRecord
        | custody::CustodyError::HandoffUnknown
        | custody::CustodyError::ProfileHandoffUnknown => boundary_handoff(
            policy,
            "HAND_OFF_RUNSTORE_HAND_OFF_AUTHORITY",
            message,
            provenant_records::diagnostic::BoundaryClass::Authority,
        ),
        _ => HandOff::Local(message),
    }
}

fn custody_refusal(
    policy: &provenant_records::policy::Policy,
    e: crate::writer::WriterCustodyError,
    writer_path: Option<std::path::PathBuf>,
) -> HandOff {
    match e {
        crate::writer::WriterCustodyError::Admission(custody::CustodyError::Busy) => HandOff::Busy,
        crate::writer::WriterCustodyError::Admission(
            custody::CustodyError::ReservationRetained(error),
        ) => HandOff::Protected(format!("custody reservation: {error}")),
        crate::writer::WriterCustodyError::Admission(custody::CustodyError::ImportRetained {
            fence,
        }) => HandOff::RetainedImport {
            fence,
            worktree: writer_path.unwrap_or_default(),
        },
        crate::writer::WriterCustodyError::Admission(custody::CustodyError::Admission(error)) => {
            typed_local(policy, "custody", &RunError::Admission(error))
        }
        crate::writer::WriterCustodyError::Admission(error) => {
            custody_local(policy, "custody", &error)
        }
        crate::writer::WriterCustodyError::Withdrawal { admission, release } => HandOff::Protected(
            format!("custody admission: {admission}; custody withdrawal: {release}"),
        ),
    }
}

impl RunStore {
    #[expect(
        clippy::too_many_arguments,
        reason = "the requester is resolved once per run, not per attempt"
    )]
    pub(super) fn hand_off(
        &self,
        task: &TaskRequest,
        mut plan: PreparedLaunch,
        requester: ActorId,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        record: &AttemptRecord,
        limits: &provenant_records::LaneLimits,
        call: &Request<'_>,
    ) -> std::result::Result<(), HandOff> {
        // Each failure names its phase, so the refusal says what was not ready.
        let local = |phase: &str, error: &dyn std::fmt::Display| {
            HandOff::Local(format!("{phase}: {error}"))
        };
        let id = record.attempt_id;
        let policy =
            crate::launch_policy(&self.writer_registry, &record.project_id.to_string(), task);
        let scope = Scope {
            attempt: id,
            generation: record.generation,
        };
        let directory = || {
            self.attempt_dir(id, false)
                .map_err(|e| typed_local(&policy, "attempt directory", &e))
        };
        let mut holder_context =
            context(record, requester).map_err(|e| typed_local(&policy, "launch context", &e))?;
        let reservation = PublicationReservation::prepare(
            directory()?,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )
        .map_err(|e| custody_local(&policy, "publication reservation", &e))?;
        let writer = plan.confinement.as_mut().and_then(|c| c.writer.as_mut());
        if task.writer.as_ref().map(|w| w.worktree.as_path())
            != writer.as_ref().map(|w| w.worktree.path())
        {
            return Err(boundary_handoff(
                &policy,
                "HAND_OFF_RUNSTORE_HAND_OFF_AUTHORITY",
                "writer confinement: does not match the task".into(),
                provenant_records::diagnostic::BoundaryClass::Authority,
            ));
        }
        let writer_path = writer
            .as_ref()
            .map(|writer| writer.worktree.path().to_path_buf());
        let mut lease = match writer {
            Some(writer) => {
                let (directory, binding) = writer.custody.take().ok_or_else(|| {
                    boundary_handoff(
                        &policy,
                        "HAND_OFF_RUNSTORE_HAND_OFF_AUTHORITY",
                        "writer custody: missing".into(),
                        provenant_records::diagnostic::BoundaryClass::Authority,
                    )
                })?;
                self.acquire_writer_fenced(directory, scope, binding, acquire_writer, call)
            }
            // Readers take the same injected acquisition as writers, so a
            // fixture guardian and its reservation share one boot observer.
            None => acquire_writer(directory()?, scope, custody::WriterBinding::read_only())
                .map_err(crate::writer::WriterCustodyError::Admission),
        }
        .map_err(|error| custody_refusal(&policy, error, writer_path))?;
        // Simulate a lost guardian acknowledgement after custody was reserved.
        #[cfg(test)]
        if TEST_UNCERTAIN_HANDOFF.with(|fault| fault.replace(false)) {
            return Err(HandOff::Uncertain("injected lost acknowledgement".into()));
        }
        // Once custody is acquired, every local preparation failure must
        // withdraw it before fallback can consider another candidate.
        macro_rules! local_or_withdraw {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        lease.withdraw_unlaunched().map_err(|release| {
                            let cause = match &error {
                                HandOff::Local(cause)
                                | HandOff::Boundary { message: cause, .. }
                                | HandOff::Resource { message: cause, .. }
                                | HandOff::TypedLocal { message: cause, .. } => cause.as_str(),
                                _ => "launch preparation failed",
                            };
                            HandOff::Protected(format!("{cause}; custody withdrawal: {release}"))
                        })?;
                        return Err(error);
                    }
                }
            };
        }
        #[cfg(test)]
        if let Some(error) = TEST_HANDOFF_FAILURE.with(|fault| fault.borrow_mut().take()) {
            local_or_withdraw!(Err::<(), _>(typed_local(
                &policy,
                "injected handoff",
                &error
            )));
        }
        if let Some(writer) = plan
            .confinement
            .as_ref()
            .and_then(|plan| plan.writer.as_ref())
            && let Err(error) =
                self.check_launch_writer_lineage(record, task, &writer.worktree, call)
        {
            let refusal = if matches!(error, RunError::Nonterminal) {
                boundary_handoff(
                    &policy,
                    "CONTROLS_RUNSTORE_CHECK_WRITER_LINEAGE_AUTHORITY",
                    format!("writer lineage: {error}"),
                    provenant_records::diagnostic::BoundaryClass::Authority,
                )
            } else {
                typed_local(&policy, "writer lineage", &error)
            };
            local_or_withdraw!(Err::<(), _>(refusal));
        }
        // Reconcile may have withdrawn this attempt before the lease existed.
        match self.record(id, call) {
            Ok(now)
                if now.revision == record.revision
                    && now.attempt_id == record.attempt_id
                    && now.key == record.key => {}
            Ok(_) => {
                local_or_withdraw!(Err::<(), _>(boundary_handoff(
                    &policy,
                    "HAND_OFF_RUNSTORE_HAND_OFF_AUTHORITY_2",
                    "attempt: changed before handoff".into(),
                    provenant_records::diagnostic::BoundaryClass::Authority,
                )));
            }
            Err(error) => {
                local_or_withdraw!(Err::<(), _>(typed_local(&policy, "attempt record", &error)));
            }
        }
        let setup_cancel = provenant_core::CancelToken::new();
        let _watch = CancelWatch::start(
            local_or_withdraw!(directory()),
            scope,
            &setup_cancel,
            call.cancel,
        );
        if let Some(deliver) = &task.deliver {
            let prepare = || -> Result<Vec<u8>> {
                let mut holder: HolderContext = decode(&holder_context)?;
                let writer = task
                    .writer
                    .as_ref()
                    .ok_or(RunError::Validation("delivery requires a writer"))?;
                let host = holder
                    .host_delivery
                    .take()
                    .ok_or(RunError::Validation("delivery lacks host context"))?;
                let host = host
                    .with_repo_facts(self.repo_facts())
                    .cancellable(&setup_cancel)
                    .prepare(writer, deliver)?;
                host.seal_launch_binding(&self.attempt_dir(id, false)?, scope)?;
                holder.host_delivery = Some(host);
                holder.encode()
            };
            match prepare() {
                Ok(context) => holder_context = context,
                Err(error) => {
                    local_or_withdraw!(Err::<(), _>(typed_local(&policy, "delivery", &error)));
                }
            }
        }
        // Host stop publication is an overridable default granted only for an
        // open-network lane. Bind a second destination to the lane's own branch;
        // it must not replace an explicit delivery destination.
        let publish_granted = provenant_records::run::host_publishing_requested(
            task.access.network(task.writer.is_some()),
            &task.profile.knobs,
        );
        if publish_granted
            && let (Some(writer_request), Some(writer_launch)) = (
                task.writer.as_ref(),
                plan.confinement
                    .as_ref()
                    .and_then(|plan| plan.writer.as_ref()),
            )
        {
            let prepared = (|| -> Result<Vec<u8>> {
                let branch = writer_launch
                    .branch
                    .as_str()
                    .strip_prefix("refs/heads/")
                    .ok_or(RunError::Validation("writer needs a local branch"))?;
                let mut holder: HolderContext = decode(&holder_context)?;
                let request =
                    crate::HostDeliveryContext::destination(crate::DeliveryMode::Push, branch)?
                        .ok_or(RunError::Validation("stop publication destination missing"))?;
                let host = holder.host_delivery.take().unwrap_or_else(|| {
                    crate::HostDeliveryContext::capture()
                        .with_repo_facts(self.repo_facts())
                        .with_host_state(self.context.state_root.as_path())
                });
                let host = host.prepare_stop_publish(writer_request, &request)?;
                host.seal_stop_publish_binding(&self.attempt_dir(id, false)?, scope)?;
                holder.host_delivery = Some(host);
                holder.encode()
            })();
            match prepared {
                Ok(context) => holder_context = context,
                Err(error) => {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "warning: stop publication is unavailable: {error}"
                    );
                }
            }
        }
        // A cancel that lands during preparation must reach this launcher, or
        // it keeps waiting on locks while holding the writer.
        let cancel_requested =
            || directory().map(|attempt| crate::store::cancel_requested(&attempt, scope));
        if local_or_withdraw!(cancel_requested()) {
            local_or_withdraw!(Err::<(), _>(local("launch", &CANCELLED_BEFORE_LAUNCH)));
        }
        let mut profile_file = None;
        if let Some(confinement) = &mut plan.confinement {
            let prepared = confinement.prepare_git(&setup_cancel, &policy);
            if let Err(error) = prepared {
                lease.withdraw_unlaunched().map_err(|release| {
                    HandOff::Protected(format!("Git setup: {error}; custody withdrawal: {release}"))
                })?;
                return Err(typed_local(&policy, "Git setup", &error));
            }
            // Git setup has finished; cache seeding needs no repository lock.
            let seed_attempt = local_or_withdraw!(directory());
            // A lock, not an assertion: observers stop reporting this phase if
            // the launcher dies. Failing to take it only loses the report.
            let seeding = seed_attempt
                .try_lock(
                    std::ffi::OsStr::new(crate::writer::SEED_WAIT_LOCK),
                    provenant_os::fs::LockMode::Exclusive,
                )
                .ok();
            #[cfg(feature = "fixture")]
            hold_seed_for_fixture(&setup_cancel);
            confinement.seed_cargo(&mut plan.environment, &seed_attempt, &setup_cancel);
            drop(seeding);
            if setup_cancel.is_cancelled() || local_or_withdraw!(cancel_requested()) {
                local_or_withdraw!(Err::<(), _>(local("launch", &CANCELLED_BEFORE_LAUNCH)));
            }
            let writer_attempt = local_or_withdraw!(directory());
            if let Some(writer) = &confinement.writer
                && let Err(error) = crate::objects::prepare_bound(
                    &local_or_withdraw!(
                        provenant_os::git::git_executable()
                            .ok_or_else(|| local("Git", &RunError::GitUnavailable))
                    ),
                    writer.worktree.repo(),
                    &writer_attempt,
                    &confinement.plan.permissions().scratch.path,
                    writer.branch.as_str(),
                    writer.base,
                )
                .and_then(|()| {
                    writer.snapshot(
                        self.context.state_root.as_path(),
                        &writer_attempt,
                        scope,
                        &policy,
                    )
                })
            {
                local_or_withdraw!(Err::<(), _>(typed_local(
                    &policy,
                    "lane Git objects",
                    &error
                )));
            }
            if confinement.backend.is_some() {
                match confinement.command(&plan.argv, &id) {
                    Ok(command) => {
                        plan.argv = command.argv;
                        profile_file = Some(command.profile);
                    }
                    Err(error) => {
                        local_or_withdraw!(Err::<(), _>(typed_local(
                            &policy,
                            "confinement command",
                            &error
                        )));
                    }
                }
            } else if let Err(error) = confinement.prepare_writer_directories() {
                local_or_withdraw!(Err::<(), _>(typed_local(
                    &policy,
                    "writer directories",
                    &error
                )));
            }
        }
        strip_hosted_credentials(&mut plan.environment);
        // Route records name models as `adapter/model`; prefix only a
        // provider-native one (which may itself contain `/`).
        let provider = plan.route.provider.as_deref().unwrap_or_default();
        let model = plan.route.model.as_deref().unwrap_or_default();
        let route = match model.strip_prefix(provider) {
            Some(rest) if rest.starts_with('/') => model.to_owned(),
            _ => format!("{provider}/{model}"),
        };
        let lane_ports = plan
            .confinement
            .as_ref()
            .filter(|c| c.writer.is_some())
            .and_then(|c| lane_port_plan_or_warn(&c.state_root, &policy));
        let key = record.key.clone();
        let command = ProviderCommand {
            policy: policy.clone(),
            profile_file,
            lane_ports,
            resolved_route: route,
            permission_plan_digest: task.authority_digest,
            key,
            requester,
            protected_commands: vec![],
            // OpenCode stores answering-model evidence in its shared XDG data
            // directory. The guardian otherwise starts with an empty environment
            // and no Registry home; retain the absolute path selected at launch.
            config_environment: if provider == "opencode" {
                vec!["XDG_DATA_HOME".into()]
            } else {
                vec![]
            },
            argv: plan.argv,
            environment: plan.environment,
            cwd: plan.cwd,
            stdin: plan.stdin,
            secret_input: plan.secret_input,
            holder_context,
            priority: local_or_withdraw!(
                record
                    .requested_priority
                    .ok_or_else(|| local("lane priority", &"missing"))
            ),
        };
        let capture = capture_plan(task, limits);
        if local_or_withdraw!(cancel_requested()) {
            local_or_withdraw!(Err::<(), _>(local("launch", &CANCELLED_BEFORE_LAUNCH)));
        }
        let launched = custody::launch(
            OwnedLaunch {
                command,
                lease: &mut lease,
                handshake: call.handshake_budget(),
                cancel: call.cancel,
                clock: call.clock,
            },
            capture,
            reservation,
        );
        if let Err(failure) = launched {
            match launch_failure(&failure.error) {
                uncertain @ HandOff::Uncertain(_) => return Err(uncertain),
                refused => local_or_withdraw!(Err::<(), _>(refused)),
            }
        }
        Ok(())
    }
}

/// A launch failure is uncertain once the reservation may record a handoff.
/// A host that cannot hold provider custody refuses before that point, so the
/// attempt ends with a typed cause instead of waiting for a receipt that never
/// comes.
fn launch_failure(error: &custody::CustodyError) -> HandOff {
    match error {
        custody::CustodyError::UnsupportedPlatform => HandOff::TypedLocal {
            message: format!("provider custody: {error}"),
            cause: TerminalCause::UnsupportedPlatform,
        },
        _ => HandOff::Uncertain(error.to_string()),
    }
}

/// Integration fixtures can hold the actual handoff at the seed boundary.
#[cfg(feature = "fixture")]
#[allow(clippy::disallowed_methods)]
fn hold_seed_for_fixture(cancel: &provenant_core::CancelToken) {
    let Some(directory) = std::env::var_os("PROVENANT_FIXTURE_HOLD_SEED") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    if std::fs::write(directory.join("entered"), b"entered").is_err() {
        return;
    }
    let _ = provenant_test_support::wait_for(
        "seed preparation release",
        std::time::Duration::from_secs(30),
        || cancel.is_cancelled() || !directory.join("hold").exists(),
        |released| *released,
    );
}

/// The marker carried in a refusal when the launcher saw a cancel request.
pub(super) const CANCELLED_BEFORE_LAUNCH: &str = "cancelled before launch";

/// Forwards a sealed cancel request, or the caller's own cancellation, to the
/// token that bounds launch preparation, so a lane waiting on a setup lock
/// stops as soon as `cancel` is called.
struct CancelWatch {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn forward_stop_request(
    attempt: &provenant_os::fs::Directory,
    scope: Scope,
    target: &provenant_core::CancelToken,
    caller: &provenant_core::CancelToken,
    done: &std::sync::atomic::AtomicBool,
    pause: &dyn provenant_core::Pause,
) {
    use std::sync::atomic::Ordering;
    loop {
        if done.load(Ordering::Acquire) {
            return;
        }
        if caller.is_cancelled() || crate::store::cancel_requested(attempt, scope) {
            target.cancel();
            return;
        }
        pause.wait(
            provenant_core::DurationMicros::from_micros(100_000),
            &provenant_core::CancelToken::new(),
        );
    }
}

impl CancelWatch {
    fn start(
        attempt: provenant_os::fs::Directory,
        scope: Scope,
        target: &provenant_core::CancelToken,
        caller: &provenant_core::CancelToken,
    ) -> Self {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (stop, target, caller) = (done.clone(), target.clone(), caller.clone());
        let thread = std::thread::Builder::new()
            .name("cancel-watch".into())
            .spawn(move || {
                forward_stop_request(
                    &attempt,
                    scope,
                    &target,
                    &caller,
                    &stop,
                    &provenant_core::ThreadPause,
                );
            })
            .ok();
        Self { done, thread }
    }
}

impl Drop for CancelWatch {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Hosted-cloud and third-party credentials never reach a lane, sandboxed
/// or not. Adapters already rebuild the environment from an allowlist; this
/// holds the boundary if an allowlist ever grows. `GH_TOKEN` is the one
/// credential the host hands a publishing lane on purpose.
pub(crate) fn strip_hosted_credentials(environment: &mut Vec<(OsString, OsString)>) {
    environment.retain(|(name, _)| !hosted_credential(name));
}

fn hosted_credential(name: &std::ffi::OsStr) -> bool {
    const PREFIXES: [&str; 10] = [
        "AWS_",
        "CLOUDSDK_",
        "TF_VAR_",
        "NEON_",
        "CLOUDFLARE_",
        "CF_API_",
        "AZURE_",
        "ARM_CLIENT_",
        "DIGITALOCEAN_",
        "HCLOUD_",
    ];
    const NAMES: [&str; 7] = [
        "GITHUB_TOKEN",
        "HF_TOKEN",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_OAUTH_ACCESS_TOKEN",
        "VAULT_TOKEN",
        "NPM_TOKEN",
        "PGPASSWORD",
    ];
    const SUFFIXES: [&str; 8] = [
        "_API_KEY",
        "_PASSWORD",
        "_API_TOKEN",
        "_ACCESS_KEY",
        "_ACCESS_TOKEN",
        "_AUTH_TOKEN",
        "_SECRET",
        "_SECRET_KEY",
    ];
    let Some(name) = name.to_str() else {
        return false;
    };
    let upper = name.to_ascii_uppercase();
    upper != "GH_TOKEN"
        && (PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
            || NAMES.contains(&upper.as_str())
            || SUFFIXES.iter().any(|suffix| upper.ends_with(suffix)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn hosted_credentials_are_stripped_and_the_lane_keeps_its_own_environment() {
        let cases = [
            ("AWS_ACCESS_KEY_ID", true),
            ("AWS_PROFILE", true),
            ("NEON_API_KEY", true),
            ("CLOUDFLARE_API_TOKEN", true),
            ("CF_API_TOKEN", true),
            ("ANTHROPIC_API_KEY", true),
            ("OPENAI_API_KEY", true),
            ("STRIPE_SECRET_KEY", true),
            ("GOOGLE_APPLICATION_CREDENTIALS", true),
            ("PGPASSWORD", true),
            ("REDIS_PASSWORD", true),
            ("GITHUB_TOKEN", true),
            ("HF_TOKEN", true),
            ("TF_VAR_db_password", true),
            ("CLOUDSDK_AUTH_ACCESS_TOKEN_FILE", true),
            ("aws_secret_access_key", true),
            ("GH_TOKEN", false),
            ("HOME", false),
            ("PATH", false),
            ("TMPDIR", false),
            ("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR", false),
            ("PROVENANT_SANDBOX", false),
            ("SCCACHE_DIR", false),
        ];
        for (name, expected) in cases {
            assert_eq!(
                super::hosted_credential(std::ffi::OsStr::new(name)),
                expected
            );
        }
        let mut environment: Vec<(std::ffi::OsString, std::ffi::OsString)> = cases
            .iter()
            .map(|(name, _)| ((*name).into(), format!("value:{name}").into()))
            .collect();
        let original = environment.clone();
        super::strip_hosted_credentials(&mut environment);
        for (name, value) in original {
            let retained = environment.iter().find(|(key, _)| key == &name);
            if super::hosted_credential(&name) {
                assert!(retained.is_none());
            } else {
                assert_eq!(retained.map(|(_, value)| value), Some(&value));
            }
        }
        assert!(
            environment
                .iter()
                .all(|(name, _)| !super::hosted_credential(name))
        );
    }

    use super::*;
    use crate::tests::{call, clock, context, input, route};
    use provenant_core::CancelToken;

    fn test_policy() -> provenant_records::policy::Policy {
        provenant_records::policy::Policy::default()
    }

    #[test]
    fn custody_refusals_distinguish_authority_from_operational_failures() {
        use crate::writer::WriterCustodyError;
        use provenant_records::diagnostic::BoundaryClass;
        for error in [
            custody::CustodyError::Fenced,
            custody::CustodyError::UnknownIdentity,
            custody::CustodyError::InvalidRecord,
            custody::CustodyError::HandoffUnknown,
        ] {
            assert!(matches!(
                custody_local(&test_policy(), "publication reservation", &error),
                HandOff::Boundary {
                    class: BoundaryClass::Authority,
                    ..
                }
            ));
            assert!(matches!(
                custody_refusal(&test_policy(), WriterCustodyError::Admission(error), None),
                HandOff::Boundary {
                    class: BoundaryClass::Authority,
                    ..
                }
            ));
        }
        assert!(matches!(
            custody_refusal(
                &test_policy(),
                WriterCustodyError::Admission(custody::CustodyError::Busy),
                None
            ),
            HandOff::Busy
        ));
        assert!(matches!(
            custody_refusal(
                &test_policy(),
                WriterCustodyError::Admission(custody::CustodyError::Cancelled),
                None
            ),
            HandOff::Local(_)
        ));
    }

    #[test]
    fn existing_launch_evidence_refuses_publication_with_authority_boundary()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        std::fs::write(temp.path().join("launch.json"), b"fixture")?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let directory = provenant_os::fs::Directory::open_anchor(temp.path())?;
        let error = match PublicationReservation::prepare(
            directory,
            call.lock_budget(),
            call.cancel,
            call.clock,
        ) {
            Ok(_) => panic!("existing launch evidence must fence publication"),
            Err(error) => error,
        };
        assert!(matches!(error, custody::CustodyError::Fenced));
        assert!(matches!(
            custody_local(&test_policy(), "publication reservation", &error),
            HandOff::Boundary {
                class: provenant_records::diagnostic::BoundaryClass::Authority,
                ..
            }
        ));
        Ok(())
    }

    // Spec: docs/specs/run-launch.md#admission-and-custody
    #[test]
    fn only_a_possible_handoff_makes_a_launch_failure_uncertain() {
        assert!(matches!(
            launch_failure(&custody::CustodyError::UnsupportedPlatform),
            HandOff::TypedLocal {
                cause: TerminalCause::UnsupportedPlatform,
                ..
            }
        ));
        for error in [
            custody::CustodyError::HandoffUnknown,
            custody::CustodyError::ProfileHandoffUnknown,
            custody::CustodyError::Timeout,
            custody::CustodyError::InvalidRecord,
        ] {
            assert!(
                matches!(launch_failure(&error), HandOff::Uncertain(_)),
                "{error}"
            );
        }
    }

    #[test]
    fn typed_handoff_preserves_boundary_and_terminal_cause() {
        use provenant_confinement::permissions::{AvailabilityReason, PermissionError};
        use provenant_records::diagnostic::BoundaryClass;
        let error = RunError::Confinement(PermissionError::BackendNotAvailable(
            AvailabilityReason::UnsupportedPlatform,
        ));
        assert!(matches!(
            typed_local(&test_policy(), "confinement command", &error),
            HandOff::Boundary {
                class: BoundaryClass::Authority,
                cause: Some(TerminalCause::UnsupportedPlatform),
                ..
            }
        ));
        assert!(matches!(
            typed_local(&test_policy(), "Git", &RunError::GitUnavailable),
            HandOff::Local(_)
        ));
        let credential = RunError::Confinement(PermissionError::CredentialRoot);
        assert!(matches!(
            typed_local(&test_policy(), "Git setup", &credential),
            HandOff::Boundary {
                class: BoundaryClass::Credentials,
                cause: None,
                ..
            }
        ));
        // Spec: docs/specs/routing.md#accounts-and-credentials
        for safety in [
            crate::HelperSafety::UnsupportedSshCommand,
            crate::HelperSafety::OpaqueShellCommand,
            crate::HelperSafety::NonPlainExecutable,
            crate::HelperSafety::LaneLocalPath,
            crate::HelperSafety::Unavailable,
        ] {
            let error = RunError::DeliveryPrepare {
                step: crate::DeliveryPrepareStep::CredentialHelper,
                refusal: Some(crate::DeliveryPrepareRefusal::CredentialHelper(safety)),
                reason: "fixture helper refusal".into(),
            };
            let handoff = typed_local(&test_policy(), "delivery", &error);
            if safety == crate::HelperSafety::Unavailable {
                assert!(matches!(
                    handoff,
                    HandOff::TypedLocal {
                        cause: TerminalCause::DeliveryPrepareCredentialHelper,
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    handoff,
                    HandOff::Boundary {
                        class: BoundaryClass::Credentials,
                        cause: Some(TerminalCause::DeliveryPrepareCredentialHelper),
                        ..
                    }
                ));
            }
        }
        let retained = RunError::RetainedWriterImport {
            fence: Box::new(custody::PendingImport {
                scope: provenant_records::custody::Scope {
                    attempt: provenant_core::AttemptId::from_bytes([4; 16]),
                    generation: 1,
                },
                fence: "/tmp/custody/reservation.json".into(),
                lane: provenant_records::WorkKey::new(
                    provenant_core::ProjectId::from_bytes([1; 16]),
                    "build",
                    "writer",
                    1,
                )
                .expect("key"),
            }),
            worktree: "/tmp/writer".into(),
        };
        assert!(matches!(
            typed_local(&test_policy(), "lane Git objects", &retained),
            HandOff::Boundary {
                class: BoundaryClass::LaneGit,
                cause: None,
                ..
            }
        ));
        assert!(matches!(
            typed_local(
                &test_policy(),
                "lane Git objects",
                &RunError::WriterHeadNotLaneBranch
            ),
            HandOff::Boundary {
                class: BoundaryClass::LaneGit,
                cause: None,
                ..
            }
        ));
    }

    #[test]
    fn writer_plan_mismatch_is_an_authority_refusal()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let record = store.record(request.attempt_id, &call)?;
        let mut task = request.request.task.clone();
        task.writer = Some(provenant_records::run::WriterRequest {
            extra: Default::default(),
            custody_project: Some(crate::tests::writer_project()),
            project: temp.path().into(),
            worktree: temp.path().join("writer"),
        });
        let plan = PreparedLaunch {
            limits: Default::default(),
            continuation_expectation: None,
            argv: vec!["fixture".into()],
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: temp.path().into(),
            route: route(),
            skipped_routes: Vec::new(),
            observed_head: None,
            repository: None,
            confinement: None,
        };
        let result = store.hand_off(
            &task,
            plan,
            provenant_core::ActorId::from_bytes([2; 16]),
            &|_, _| Ok(Vec::new()),
            &|_, _, _| panic!("must not acquire custody"),
            &record,
            &crate::limits::deadline_only(60),
            &call,
        );
        assert!(
            matches!(result, Err(HandOff::Boundary { message, class: provenant_records::diagnostic::BoundaryClass::Authority, .. }) if message.contains("writer confinement"))
        );
        Ok(())
    }

    #[test]
    fn cancel_watch_forwards_a_stop_request_to_setup()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let id = request.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.record(id, &call)?;
        let scope = RunStore::scope(&record);
        let directory = store.attempt_dir(id, false)?;
        let setup = CancelToken::new();
        struct RequestStopOnPause<'a> {
            directory: &'a provenant_os::fs::Directory,
            scope: Scope,
            setup: &'a CancelToken,
        }
        impl provenant_core::Pause for RequestStopOnPause<'_> {
            fn wait(&self, _: provenant_core::DurationMicros, _: &CancelToken) {
                assert!(!self.setup.is_cancelled());
                custody::request_stop(
                    self.directory.try_clone().expect("attempt directory"),
                    self.scope,
                    provenant_core::ActorId::from_bytes([2; 16]),
                    provenant_supervisor::machine::StopReason::ExplicitCancellation,
                )
                .expect("stop request");
            }
        }
        forward_stop_request(
            &directory,
            scope,
            &setup,
            &cancel,
            &std::sync::atomic::AtomicBool::new(false),
            &RequestStopOnPause {
                directory: &directory,
                scope,
                setup: &setup,
            },
        );
        assert!(setup.is_cancelled());
        assert!(
            !cancel.is_cancelled(),
            "the caller's own token is untouched"
        );
        Ok(())
    }

    #[test]
    fn missing_priority_withdraws_acquired_custody()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let id = request.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("claim")?;
        assert!(record.requested_priority.is_none());
        let plan = || PreparedLaunch {
            limits: Default::default(),
            continuation_expectation: None,
            argv: vec!["fixture".into()],
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: temp.path().into(),
            route: route(),
            skipped_routes: Vec::new(),
            observed_head: None,
            repository: None,
            confinement: None,
        };
        let acquire = |directory, scope, binding| {
            custody::acquire_writer(
                directory,
                scope,
                binding,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )
        };
        let result = store.hand_off(
            &request.request.task,
            plan(),
            provenant_core::ActorId::from_bytes([2; 16]),
            &|_, _| Ok(Vec::new()),
            &acquire,
            &record,
            &crate::limits::deadline_only(60),
            &call,
        );
        match result {
            Err(HandOff::Local(reason)) => assert!(reason.contains("lane priority"), "{reason}"),
            Err(HandOff::Boundary { message, class, .. }) => {
                panic!("unexpected {class:?} boundary: {message}")
            }
            Err(HandOff::Resource { message, fact }) => {
                panic!("unexpected resource refusal {fact:?}: {message}")
            }
            Err(HandOff::TypedLocal { message, cause }) => {
                panic!("unexpected typed refusal {cause:?}: {message}")
            }
            Err(HandOff::Busy) => panic!("custody busy"),
            Err(HandOff::RetainedImport { .. }) => {
                panic!("unexpected refusal")
            }
            Err(HandOff::Uncertain(reason)) => panic!("uncertain: {reason}"),
            Err(HandOff::Protected(reason)) => panic!("protected: {reason}"),
            Ok(()) => panic!("unexpected launch"),
        }
        let directory = store.attempt_dir(record.attempt_id, false)?;
        let lease = custody::acquire_reader(
            directory,
            RunStore::scope(&record),
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        lease.withdraw_unlaunched()?;
        // Each handoff reserves a fresh publication file; these calls exercise
        // separate failure paths against the same attempt fixture.
        store
            .attempt_dir(id, false)?
            .unlink(std::ffi::OsStr::new("reserve"))?;
        let result = store.hand_off(
            &request.request.task,
            plan(),
            provenant_core::ActorId::from_bytes([2; 16]),
            &|_, _| Ok(Vec::new()),
            &|_, _, _| Err(custody::CustodyError::ReservationRetained("fixture".into())),
            &record,
            &crate::limits::deadline_only(60),
            &call,
        );
        match result {
            Err(HandOff::Protected(reason)) => assert!(reason.contains("fixture"), "{reason}"),
            Err(HandOff::Uncertain(reason)) => panic!("uncertain: {reason}"),
            Err(HandOff::Local(reason)) => panic!("local: {reason}"),
            Err(HandOff::Boundary { message, class, .. }) => {
                panic!("unexpected {class:?} boundary: {message}")
            }
            Err(HandOff::Resource { message, fact }) => {
                panic!("unexpected resource refusal {fact:?}: {message}")
            }
            Err(HandOff::TypedLocal { message, cause }) => {
                panic!("unexpected typed refusal {cause:?}: {message}")
            }
            Err(HandOff::Busy) => panic!("busy"),
            Err(HandOff::RetainedImport { .. }) => {
                panic!("unexpected refusal")
            }
            Ok(()) => panic!("unexpected launch"),
        }
        store
            .attempt_dir(id, false)?
            .unlink(std::ffi::OsStr::new("reserve"))?;
        let result = store.hand_off(
            &request.request.task,
            plan(),
            provenant_core::ActorId::from_bytes([2; 16]),
            &|_, _| Ok(Vec::new()),
            &|directory, scope, binding| {
                let slot = directory.try_clone()?;
                let lease = acquire(directory, scope, binding)?;
                let evidence =
                    slot.open_directory(std::ffi::OsStr::new(&scope.attempt.to_string()))?;
                evidence.unlink(std::ffi::OsStr::new("settlement.json"))?;
                evidence.mkdir(std::ffi::OsStr::new("settlement.json"), 0o700)?;
                Ok(lease)
            },
            &record,
            &crate::limits::deadline_only(60),
            &call,
        );
        match result {
            Err(HandOff::Protected(reason)) => {
                assert!(reason.contains("lane priority") && reason.contains("custody withdrawal"))
            }
            Err(HandOff::Uncertain(reason)) => panic!("uncertain: {reason}"),
            Err(HandOff::Local(reason)) => panic!("local: {reason}"),
            Err(HandOff::Boundary { message, class, .. }) => {
                panic!("unexpected {class:?} boundary: {message}")
            }
            Err(HandOff::Resource { message, fact }) => {
                panic!("unexpected resource refusal {fact:?}: {message}")
            }
            Err(HandOff::TypedLocal { message, cause }) => {
                panic!("unexpected typed refusal {cause:?}: {message}")
            }
            Err(HandOff::Busy) => panic!("busy"),
            Err(HandOff::RetainedImport { .. }) => {
                panic!("unexpected refusal")
            }
            Ok(()) => panic!("unexpected launch"),
        }
        assert!(custody::reservation_present(
            &store.attempt_dir(id, false)?,
            RunStore::scope(&record),
        )?);
        Ok(())
    }

    #[test]
    fn uncertain_handoff_retains_attempt_route_and_does_not_launch_again()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let acquisitions = std::cell::Cell::new(0);
        let acquire = |directory, scope, binding| {
            acquisitions.set(acquisitions.get() + 1);
            custody::acquire_writer(
                directory,
                scope,
                binding,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )
        };
        TEST_UNCERTAIN_HANDOFF.with(|fault| fault.set(true));
        let start = || {
            store.start_inner(
                request.attempt_id,
                &crate::dispatch_tests::Ready,
                &|_, _| Ok(Vec::new()),
                &acquire,
                None,
                &call,
            )
        };
        assert!(
            matches!(start()?.as_slice(), [Started::Unknown { attempt, error }] if *attempt == request.attempt_id && error.contains("lost acknowledgement"))
        );
        let operation = store
            .operation(&request.operation_id, &call)?
            .ok_or("operation")?;
        assert_eq!(operation.state, OperationState::LaunchUnknown);
        let record = store.record(request.attempt_id, &call)?;
        assert!(matches!(record.state, AttemptState::Starting { .. }));
        let recorded_route = record.resolved_route.as_ref().ok_or("resolved route")?;
        assert_eq!(recorded_route.provider, route().provider);
        assert_eq!(recorded_route.model, route().model);
        assert_eq!(recorded_route.effort, route().effort);
        assert!(custody::reservation_present(
            &store.attempt_dir(record.attempt_id, false)?,
            RunStore::scope(&record)
        )?);
        assert!(start()?.is_empty());
        assert_eq!(acquisitions.get(), 1);
        assert_eq!(store.record(record.attempt_id, &call)?, record);
        Ok(())
    }

    // Spec: docs/specs/retry-owner.md#admission-is-distinct-from-observation
    #[test]
    fn local_handoff_failure_retains_preparation_fallback_evidence()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for safety in [
            None,
            Some(crate::HelperSafety::Unavailable),
            Some(crate::HelperSafety::OpaqueShellCommand),
        ] {
            for cancelled in [false, true] {
                let temp = tempfile::tempdir()?;
                let (clock, cancel) = (clock(), CancelToken::new());
                let call = call(&clock, &cancel);
                let context = context(temp.path())?;
                let store = RunStore::open(&context, &call)?;
                let request = input();
                store.create(&request, &call)?;
                let acquire = |directory, scope: Scope, binding| {
                    if cancelled {
                        let attempt = store
                            .attempt_dir(scope.attempt, false)
                            .expect("attempt directory");
                        let cancellation = crate::db::bytes(&serde_json::json!({
                        "schema_version": 1,
                        "scope": scope,
                        "requester": context.actor_id,
                        "request_id": "fixture-cancel",
                        "reason": provenant_supervisor::machine::StopReason::ExplicitCancellation
                    }))
                    .expect("cancel bytes");
                        artifacts::seal(&attempt, "cancel.json", &cancellation, RECORD_LIMIT)
                            .expect("seal cancellation");
                    }
                    custody::acquire_writer(
                        directory,
                        scope,
                        binding,
                        call.lock_budget(),
                        call.cancel,
                        call.clock,
                    )
                };
                let error = match safety {
                    None => RunError::GitUnavailable,
                    Some(safety) => RunError::DeliveryPrepare {
                        step: crate::DeliveryPrepareStep::CredentialHelper,
                        refusal: Some(crate::DeliveryPrepareRefusal::CredentialHelper(safety)),
                        reason: "fixture helper refusal".into(),
                    },
                };
                TEST_HANDOFF_FAILURE.with(|fault| *fault.borrow_mut() = Some(error));
                let result = store.start_inner(
                    request.attempt_id,
                    &crate::dispatch_tests::Ready,
                    &|_, _| Ok(Vec::new()),
                    &acquire,
                    None,
                    &call,
                )?;
                assert!(matches!(result.as_slice(), [Started::Refused { .. }]));
                let record = store.record(request.attempt_id, &call)?;
                assert_eq!(record.provider_launched, Some(false));
                assert_eq!(
                    record.preparation_failure,
                    (!cancelled && safety != Some(crate::HelperSafety::OpaqueShellCommand))
                        .then_some(provenant_records::run::PreparationFailure::AdapterUnavailable)
                );
                assert!(
                    matches!(record.state, AttemptState::Terminal { outcome, .. }
            if outcome == if cancelled { Outcome::Cancelled } else { Outcome::Rejected })
                );
                assert!(store.custody_released(&record, &call)?);
                let reopened = RunStore::open(&context, &call)?;
                assert_eq!(
                    reopened
                        .record(request.attempt_id, &call)?
                        .preparation_failure,
                    record.preparation_failure
                );
            }
        }
        Ok(())
    }

    #[test]
    fn local_handoff_write_failure_returns_error_without_reporting_withdrawal()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let before = std::cell::RefCell::new(None);
        let acquire = |directory, scope: Scope, binding| {
            let lease = custody::acquire_writer(
                directory,
                scope,
                binding,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )?;
            *before.borrow_mut() =
                Some(store.record(scope.attempt, &call).expect("starting record"));
            store.db.execute_batch("CREATE TEMP TRIGGER fail_handoff_withdraw BEFORE UPDATE ON attempts BEGIN SELECT RAISE(ABORT, 'injected withdrawal write'); END;").expect("inject write failure");
            Ok(lease)
        };
        TEST_HANDOFF_FAILURE.with(|fault| *fault.borrow_mut() = Some(RunError::GitUnavailable));
        let result = store.start_inner(
            request.attempt_id,
            &crate::dispatch_tests::Ready,
            &|_, _| Ok(Vec::new()),
            &acquire,
            None,
            &call,
        );
        assert!(matches!(result, Err(RunError::Sqlite(_))));
        let before = before.into_inner().ok_or("starting record")?;
        assert!(matches!(before.state, AttemptState::Starting { .. }));
        assert_eq!(store.record(before.attempt_id, &call)?, before);
        let reopened = RunStore::open(&context(temp.path())?, &call)?;
        assert_eq!(reopened.record(before.attempt_id, &call)?, before);
        assert_eq!(
            reopened
                .operation(&request.operation_id, &call)?
                .ok_or("operation")?
                .state,
            OperationState::Accepted
        );
        Ok(())
    }

    #[test]
    fn failed_local_handoff_withdrawal_keeps_starting_record_and_reservation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let id = request.attempt_id;
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let directory = store.attempt_dir(id, false)?;
        let scope = RunStore::scope(&record);
        let lease = custody::acquire_writer(
            directory.try_clone()?,
            scope,
            custody::WriterBinding::read_only(),
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let before = store.record(id, &call)?;
        store.db.execute_batch("CREATE TEMP TRIGGER fail_withdraw BEFORE UPDATE ON attempts BEGIN SELECT RAISE(ABORT, 'fixture withdrawal write'); END;")?;
        let result = store.withdraw(
            id,
            before.state.clone(),
            Outcome::Rejected,
            "local handoff failed",
            call.clock.wall_now()?,
            &call,
        );
        assert!(matches!(result, Err(RunError::Sqlite(_))));
        assert_eq!(store.record(id, &call)?, before);
        assert!(custody::reservation_present(&directory, scope)?);
        drop(lease);
        Ok(())
    }

    #[test]
    fn failed_unknown_disposition_write_keeps_custody_and_prior_operation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let request = input();
        store.create(&request, &call)?;
        let id = request.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.claim(id, &clock, &call)?.ok_or("claim")?;
        let scope = RunStore::scope(&record);
        let directory = store.attempt_dir(record.attempt_id, false)?;
        let lease = custody::acquire_writer(
            directory.try_clone()?,
            scope,
            custody::WriterBinding::read_only(),
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let prior = store
            .operation(&request.operation_id, &call)?
            .ok_or("operation")?;
        store.db.execute_batch("CREATE TEMP TRIGGER fail_unknown BEFORE UPDATE OF operation_record ON attempts BEGIN SELECT RAISE(ABORT, 'fixture unknown write'); END;")?;
        assert!(matches!(
            store.launch_unknown(request.attempt_id, &call),
            Err(RunError::Sqlite(_))
        ));
        assert_eq!(store.operation(&request.operation_id, &call)?, Some(prior));
        assert!(custody::reservation_present(&directory, scope)?);
        store.db.execute_batch("DROP TRIGGER fail_unknown")?;
        store.launch_unknown(request.attempt_id, &call)?;
        assert_eq!(
            store
                .operation(&request.operation_id, &call)?
                .ok_or("operation")?
                .state,
            OperationState::LaunchUnknown
        );
        assert!(custody::reservation_present(&directory, scope)?);
        drop(lease);
        Ok(())
    }

    #[test]
    fn failed_withdrawal_blocks_the_operation_without_relaunch()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for fail_save in [false, true] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let request = input();
            store.create(&request, &call)?;
            let acquisitions = std::cell::Cell::new(0);
            let acquire = |directory: provenant_os::fs::Directory, scope: Scope, binding| {
                acquisitions.set(acquisitions.get() + 1);
                let slot = directory.try_clone()?;
                let lease = custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                )?;
                slot.open_directory(std::ffi::OsStr::new(&scope.attempt.to_string()))?
                    .mkdir(std::ffi::OsStr::new("settlement.json"), 0o700)?;
                let mut changed = store.record(scope.attempt, &call).expect("attempt record");
                changed.revision += 1;
                store
                    .write(&call, |tx| {
                        tx.execute(
                            "UPDATE attempts SET revision=?1, record=?2 WHERE id=?3",
                            params![
                                i64::try_from(changed.revision).expect("fixture revision"),
                                crate::db::bytes(&changed)?,
                                scope.attempt.to_string()
                            ],
                        )?;
                        Ok(())
                    })
                    .expect("change attempt before handoff");
                if fail_save {
                    store.db.execute_batch("CREATE TEMP TRIGGER fail_blocked BEFORE UPDATE OF operation_record ON attempts BEGIN SELECT RAISE(ABORT, 'fixture blocked write'); END;").expect("inject write failure");
                }
                Ok(lease)
            };
            let launch = || {
                store.start_inner(
                    request.attempt_id,
                    &crate::dispatch_tests::Ready,
                    &|_, _| Ok(Vec::new()),
                    &acquire,
                    None,
                    &call,
                )
            };
            if fail_save {
                assert!(matches!(launch(), Err(RunError::Sqlite(_))));
                store.db.execute_batch("DROP TRIGGER fail_blocked")?;
            } else {
                assert!(
                    matches!(launch()?.as_slice(), [Started::Blocked { error, .. }] if error.contains("changed before handoff") && error.contains("custody withdrawal"))
                );
            }
            let operation = store
                .operation(&request.operation_id, &call)?
                .ok_or("operation")?;
            if fail_save {
                assert_eq!(operation.state, OperationState::Accepted);
                assert!(operation.blocked_reason.is_none());
            } else {
                assert_eq!(operation.state, OperationState::Blocked);
                assert!(
                    operation
                        .blocked_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("custody withdrawal"))
                );
            }
            let reopened = RunStore::open(&context(temp.path())?, &call)?;
            assert_eq!(
                reopened.operation(&request.operation_id, &call)?,
                Some(operation)
            );
            let record = store.record(request.attempt_id, &call)?;
            assert!(custody::reservation_present(
                &store.attempt_dir(record.attempt_id, false)?,
                RunStore::scope(&record)
            )?);
            assert!(launch()?.is_empty());
            assert_eq!(acquisitions.get(), 1);
        }
        Ok(())
    }
}
