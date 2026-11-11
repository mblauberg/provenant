use super::*;

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
