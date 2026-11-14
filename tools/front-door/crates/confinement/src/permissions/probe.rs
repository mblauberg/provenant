//! Sandbox backend selection and OS probing.

use super::*;
use provenant_core::CancelToken;
use provenant_os::exec::Executor;
#[cfg(target_os = "macos")]
use provenant_os::exec::{ExecOptions, ExecOutcome, StreamPolicy, Termination};
#[cfg(target_os = "macos")]
use provenant_os::fs::FileKind;
use provenant_records::policy::{Effect, Facts, Policy};
#[cfg(target_os = "macos")]
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
pub(super) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[cfg(target_os = "macos")]
pub(super) const PROBE_PROCESS_RULES: &str = "(allow process-exec (literal \"/usr/bin/true\"))\n(allow process-exec (literal \"/bin/cat\"))\n";

#[cfg(target_os = "macos")]
pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
pub(super) const PROBE_CAPTURE_LIMIT: usize = 1024;
#[cfg(target_os = "macos")]
pub(super) const CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);

/// Host-configured warning thresholds for profile compilation.
#[derive(Clone, Copy, Debug)]
pub struct ProfileLimits {
    pub grant_limit: usize,
    pub profile_limit_bytes: usize,
    pub grant_origin: provenant_records::diagnostic::SettingSource,
    pub profile_origin: provenant_records::diagnostic::SettingSource,
}
impl Default for ProfileLimits {
    fn default() -> Self {
        let config = crate::run_config::RunConfig::default();
        Self {
            grant_limit: config.grant_limit,
            profile_limit_bytes: config.profile_limit_bytes,
            grant_origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
            profile_origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
        }
    }
}

pub struct BackendContext<'a> {
    pub executor: &'a dyn Executor,
    pub cancel: &'a CancelToken,
    #[cfg(target_os = "macos")]
    pub env: &'a [(OsString, OsString)],
}

pub trait ConfinementBackend: Send + Sync {
    fn probe(&self, context: &BackendContext<'_>) -> Result<Availability, PermissionError>;

    /// Compile immediately before launch while holding writer exclusion over
    /// every writable root through the child handoff. The pathname sandbox
    /// denies child-created hard links; compile inspects only the Git control
    /// files for other names, and cannot stop an external writer from adding
    /// an alias to one after that check.
    fn compile(
        &self,
        plan: &PlanPermissions,
        profile: &ProviderProfile,
        anchors: &Anchors<'_>,
    ) -> Result<ConfinedLaunch, PermissionError>;
}

/// Returns a backend bound to a successful OS probe. On macOS the returned
/// backend's private readiness value is immutable, so `compile` cannot be
/// called through an unprobed backend instance.
pub fn system_backend(
    context: &BackendContext<'_>,
) -> Result<Box<dyn ConfinementBackend>, PermissionError> {
    system_backend_with_limits(context, ProfileLimits::default())
}

pub fn system_backend_with_limits(
    context: &BackendContext<'_>,
    limits: ProfileLimits,
) -> Result<Box<dyn ConfinementBackend>, PermissionError> {
    system_backend_with_limits_and_policy(context, limits, &Policy::default())
}

/// Bind the resolved launch policy to the backend that compiles this launch.
/// Boundary checks remain immutable; warning thresholds use the sealed source
/// and strength selected before dispatch.
pub fn system_backend_with_limits_and_policy(
    context: &BackendContext<'_>,
    limits: ProfileLimits,
    policy: &Policy,
) -> Result<Box<dyn ConfinementBackend>, PermissionError> {
    #[cfg(target_os = "macos")]
    {
        match probe_sandbox(context)
            .map_err(|error| backend_boundary_error(error, "sandbox probe failed before launch"))?
        {
            Availability::Available {
                backend: BackendKind::MacOsSbpl,
            } => Ok(Box::new(MacOsSandboxBackend {
                ready: true,
                limits,
                policy: policy.clone(),
            })),
            Availability::Unavailable { reason } => Err(backend_boundary_error(
                PermissionError::BackendNotAvailable(reason),
                "sandbox probe did not establish an enforcing backend",
            )),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (context, limits, policy);
        Err(backend_boundary_error(
            PermissionError::BackendNotAvailable(AvailabilityReason::UnsupportedPlatform),
            "this platform has no enforcing sandbox backend",
        ))
    }
}

fn backend_boundary_error(error: PermissionError, detail: &str) -> PermissionError {
    let policy = Policy::default();
    let error = policy.boundary_error(
        "PROBE_SYSTEM_BACKEND_CREDENTIALS",
        Facts::violation(detail),
        error,
    );
    policy.boundary_error(
        "PROBE_SYSTEM_BACKEND_GIT_ISOLATION",
        Facts::violation(detail),
        error,
    )
}

#[cfg(target_os = "macos")]
pub(super) struct MacOsSandboxBackend {
    pub(super) ready: bool,
    pub(super) limits: ProfileLimits,
    pub(super) policy: Policy,
}

#[cfg(target_os = "macos")]
impl ConfinementBackend for MacOsSandboxBackend {
    fn probe(&self, context: &BackendContext<'_>) -> Result<Availability, PermissionError> {
        if !self.ready {
            return Err(PermissionError::BackendUnavailable);
        }
        probe_sandbox(context)
    }

    fn compile(
        &self,
        plan: &PlanPermissions,
        profile: &ProviderProfile,
        anchors: &Anchors<'_>,
    ) -> Result<ConfinedLaunch, PermissionError> {
        if !self.ready {
            return Err(PermissionError::BackendUnavailable);
        }
        compile_sbpl_with_diagnostic(plan, profile, anchors, self.limits, &self.policy)
    }
}

#[cfg(target_os = "macos")]
pub(super) fn probe_sandbox(context: &BackendContext<'_>) -> Result<Availability, PermissionError> {
    probe_sandbox_with_diagnostic(context).map(|(availability, _)| availability)
}

#[cfg(target_os = "macos")]
pub(super) fn describe_sandbox_probe(phase: &str, outcome: &ExecOutcome) -> String {
    format!(
        "{phase}: termination={:?}, timed_out={}, drain_truncated={}, stdout_read_failed={}, stderr_read_failed={}, stdout_bytes={}, stderr={:?}",
        outcome.termination,
        outcome.timed_out,
        outcome.drain_truncated,
        outcome.stdout.read_failed,
        outcome.stderr.read_failed,
        outcome.stdout.bytes.len(),
        String::from_utf8_lossy(&outcome.stderr.bytes),
    )
}

#[cfg(target_os = "macos")]
pub(super) fn probe_exec_options<'a>(
    argv: &'a [OsString],
    context: &'a BackendContext<'_>,
) -> ExecOptions<'a> {
    let mut options = ExecOptions::new(argv, Path::new("/"), PROBE_TIMEOUT);
    options.env = Some(context.env);
    options.cancel = Some(context.cancel);
    options.stdout = StreamPolicy::Capture {
        limit: PROBE_CAPTURE_LIMIT,
    };
    options.stderr = StreamPolicy::Capture {
        limit: PROBE_CAPTURE_LIMIT,
    };
    options.terminate_grace = CLEANUP_TIMEOUT;
    options.reap_timeout = CLEANUP_TIMEOUT;
    options
}

#[cfg(target_os = "macos")]
pub(super) fn probe_sandbox_with_diagnostic(
    context: &BackendContext<'_>,
) -> Result<(Availability, Option<String>), PermissionError> {
    if context.cancel.is_cancelled() {
        return Err(PermissionError::Cancelled);
    }
    let executable = match provenant_os::fs::FileHandle::open_source(Path::new(SANDBOX_EXEC)) {
        Ok(executable) => executable,
        Err(error) if error.is_not_found() => {
            return Ok((
                Availability::Unavailable {
                    reason: AvailabilityReason::SandboxExecutableMissing,
                },
                None,
            ));
        }
        Err(error) => return Err(PermissionError::Filesystem(error)),
    };
    let metadata = executable.metadata().map_err(PermissionError::Filesystem)?;
    if metadata.kind != FileKind::Regular
        || metadata.uid != 0
        || metadata.mode & 0o111 == 0
        || metadata.mode & 0o022 != 0
    {
        return Ok((
            Availability::Unavailable {
                reason: AvailabilityReason::SandboxExecutableUntrusted,
            },
            None,
        ));
    }

    let mut probe_profile = String::from("(version 1)\n(deny default)\n");
    probe_profile.push_str(DYLD_BOOTSTRAP_RULES);
    probe_profile.push_str(PROBE_PROCESS_RULES);
    let argv = [
        OsString::from(SANDBOX_EXEC),
        OsString::from("-p"),
        OsString::from(probe_profile.as_str()),
        OsString::from("/usr/bin/true"),
    ];
    let options = probe_exec_options(&argv, context);
    match context.executor.execute(options) {
        Ok(outcome) if successful_probe(&outcome) => (),
        Err(provenant_os::exec::ExecError::Cancelled) => {
            return Err(PermissionError::Cancelled);
        }
        Err(error) => return Err(PermissionError::Execution(error)),
        Ok(outcome) => {
            return Ok((
                Availability::Unavailable {
                    reason: AvailabilityReason::ProbeFailed,
                },
                Some(describe_sandbox_probe(
                    "sandbox-exec allowed-command probe",
                    &outcome,
                )),
            ));
        }
    };

    let denied_argv = [
        OsString::from(SANDBOX_EXEC),
        OsString::from("-p"),
        OsString::from(probe_profile.as_str()),
        OsString::from("/bin/cat"),
        OsString::from("/etc/passwd"),
    ];
    let denied_options = probe_exec_options(&denied_argv, context);
    match context.executor.execute(denied_options) {
        Ok(outcome) if denied_probe(&outcome) => Ok((
            Availability::Available {
                backend: BackendKind::MacOsSbpl,
            },
            None,
        )),
        Err(provenant_os::exec::ExecError::Cancelled) => Err(PermissionError::Cancelled),
        Err(error) => Err(PermissionError::Execution(error)),
        Ok(outcome) => Ok((
            Availability::Unavailable {
                reason: AvailabilityReason::ProbeFailed,
            },
            Some(describe_sandbox_probe(
                "sandbox-exec denied-command probe",
                &outcome,
            )),
        )),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn successful_probe(outcome: &ExecOutcome) -> bool {
    matches!(outcome.termination, Termination::Exit(0))
        && !outcome.timed_out
        && !outcome.drain_truncated
        && !outcome.stdout.read_failed
        && !outcome.stderr.read_failed
}

#[cfg(target_os = "macos")]
pub(super) fn denied_probe(outcome: &ExecOutcome) -> bool {
    matches!(outcome.termination, Termination::Exit(code) if code != 0)
        && String::from_utf8_lossy(&outcome.stderr.bytes).contains("Operation not permitted")
        && !outcome.timed_out
        && !outcome.cancelled
        && !outcome.overflow_stopped
        && !outcome.drain_truncated
        && !outcome.stdout.read_failed
        && !outcome.stderr.read_failed
}

#[cfg(any(target_os = "macos", test, feature = "snapshot"))]
pub(super) fn report_policy_message(message: String) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}

/// Apply one advisory launch threshold. The table controls whether an
/// observation warns, is silent, or returns its existing typed strict error.
/// Queue has no admission point in profile compilation and is rejected if a
/// malformed table ever enables it.
#[cfg(any(target_os = "macos", test, feature = "snapshot"))]
#[allow(clippy::panic)]
pub(super) fn check_capacity_policy(
    policy: &Policy,
    code: &str,
    setting: &str,
    actual: usize,
    threshold: usize,
    origin: provenant_records::diagnostic::SettingSource,
    strict_error: PermissionError,
) -> Result<Option<String>, PermissionError> {
    let facts = if actual > threshold {
        Facts::violation(format!(
            "{setting} exceeded: effective value={actual}, threshold={threshold} (setting source: {})",
            origin.description()
        ))
    } else {
        Facts {
            violated: false,
            detail: String::new(),
        }
    };
    match policy.check(code, facts) {
        Effect::Proceed => Ok(None),
        Effect::Warn(message) => Ok(Some(message)),
        Effect::Queue(_) => panic!("capacity rules do not support queue effects"),
        Effect::Refuse(diagnostic) => {
            if let Some(detail) = diagnostic.detail {
                report_policy_message(detail);
            }
            Err(strict_error)
        }
    }
}
