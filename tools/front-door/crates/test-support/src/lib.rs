use std::{
    fmt::Display,
    io::Write,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use provenant_core::{PathResolver, RootInputs};

/// Run native Git in an isolated fixture, without inherited lane bindings.
#[allow(clippy::disallowed_methods)]
pub fn git_command() -> std::process::Command {
    let program = [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/opt/local/bin/git",
        "/Library/Developer/CommandLineTools/usr/bin/git",
        "/usr/bin/git",
        "/bin/git",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .unwrap_or("/usr/bin/git");
    let mut command = std::process::Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_")
            || key == provenant_core::consumer_env::GIT
            || key == provenant_core::consumer_env::WORKTREE_GITDIR
        {
            command.env_remove(key);
        }
    }
    command.envs([
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "core.hooksPath"),
        ("GIT_CONFIG_VALUE_1", "/dev/null"),
    ]);
    command
}

static SKIP_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Keep the fixture's state outside both its home and its checkout. A state
/// root enclosing home would also enclose protected credentials and old state.
/// The caller owns the temporary base; each root is an existing private directory.
pub fn test_roots(base: &Path) -> std::io::Result<RootInputs> {
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    let state = base.join("state");
    for path in [&inputs.home, &inputs.checkout, &state] {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
    }
    inputs.state_dir = Some(state);
    Ok(inputs)
}

/// Existing callers can continue to use the established fixture constructor
/// while slices adopt `test_roots`.
pub fn root_inputs(base: &Path) -> std::io::Result<RootInputs> {
    test_roots(base)
}

/// Test-only resolver for fixture paths whose directories have already been
/// created under an isolated temporary root.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdentityPathResolver;

impl PathResolver for IdentityPathResolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<std::path::PathBuf> {
        Ok(path.to_owned())
    }
}

/// Poll an observation until it satisfies a condition, reporting the last value
/// when the deadline expires.
#[allow(clippy::disallowed_methods)] // Test-only polling has a bounded deadline.
pub fn wait_for<T: std::fmt::Debug>(
    name: &str,
    timeout: std::time::Duration,
    mut observe: impl FnMut() -> T,
    mut reached: impl FnMut(&T) -> bool,
) -> Result<T, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let observation = observe();
        if reached(&observation) {
            return Ok(observation);
        }
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Err(format!(
                "timed out waiting for {name}; last observation: {observation:?}"
            ));
        };
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(25)));
    }
}

/// True when the process no longer exists or is a zombie awaiting its parent's
/// reap: a zombie has already run its last instruction, but `kill -0` still
/// succeeds on it until init collects it, so liveness is read from `ps` state.
#[allow(clippy::disallowed_methods)] // Observe a process without signalling it.
pub fn process_is_gone(pid: &str) -> std::io::Result<bool> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", pid])
        .stderr(std::process::Stdio::null())
        .output()?;
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    Ok(state.is_empty() || state.starts_with('Z'))
}

/// Wait up to two seconds for a process to be gone (see `process_is_gone`),
/// reporting the survivor when the deadline expires. A live process always
/// fails.
pub fn assert_process_gone(pid: &str) -> Result<(), String> {
    let mut failure = None;
    wait_for(
        &format!("process {pid} to be gone"),
        std::time::Duration::from_secs(2),
        || match process_is_gone(pid) {
            Ok(gone) => gone,
            Err(error) => {
                failure = Some(error.to_string());
                true
            }
        },
        |gone| *gone,
    )?;
    failure.map_or(Ok(()), |error| Err(format!("probe process {pid}: {error}")))
}

/// Reports an unavailable sandbox-dependent test locally, or fails when CI
/// requires native confinement. The ordinal makes skips visible and countable
/// in captured test output.
pub fn skip_sandbox_dependent_test(reason: impl Display) -> Result<(), String> {
    report_unavailable_sandbox(
        reason.to_string(),
        &|name| std::env::var_os(name),
        &mut std::io::stderr(),
        &SKIP_COUNT,
    )
}

fn report_unavailable_sandbox(
    reason: String,
    environment: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    output: &mut impl Write,
    count: &AtomicUsize,
) -> Result<(), String> {
    report_missing_prerequisite(
        environment("PROVENANT_CONFINEMENT_REQUIRED").is_some(),
        reason,
        output,
        count,
    )
}

/// Fail a required native prerequisite, or print a numbered local skip.
pub fn missing_prerequisite(required: bool, reason: impl Display) -> Result<(), String> {
    report_missing_prerequisite(
        required,
        reason.to_string(),
        &mut std::io::stderr(),
        &SKIP_COUNT,
    )
}

fn report_missing_prerequisite(
    required: bool,
    reason: String,
    output: &mut impl Write,
    count: &AtomicUsize,
) -> Result<(), String> {
    if required {
        return Err(reason);
    }

    let count = count.fetch_add(1, Ordering::Relaxed) + 1;
    writeln!(output, "SKIP #{count}: {reason}")
        .map_err(|error| format!("failed to report native prerequisite skip: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::report_missing_prerequisite;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn unavailable_sandbox_uses_injected_requirement_and_reports_local_skip() {
        for control in [None, Some("1")] {
            let mut output = Vec::new();
            let count = AtomicUsize::new(0);
            let result = super::report_unavailable_sandbox(
                "sandbox executable unavailable".into(),
                &|name| {
                    assert_eq!(name, "PROVENANT_CONFINEMENT_REQUIRED");
                    control.map(Into::into)
                },
                &mut output,
                &count,
            );
            assert_eq!(result.is_err(), control.is_some());
            if control.is_none() {
                assert!(String::from_utf8(output).unwrap().contains("SKIP #1:"));
            } else {
                assert!(output.is_empty());
            }
        }
    }

    #[test]
    fn bounded_wait_returns_the_reached_observation() {
        let mut value = 0;
        let result = super::wait_for(
            "count",
            std::time::Duration::from_secs(1),
            || {
                value += 1;
                value
            },
            |value| *value == 2,
        );
        assert_eq!(result, Ok(2));
    }

    #[test]
    fn bounded_wait_reports_condition_and_last_observation() {
        let error = super::wait_for(
            "fixture ready",
            std::time::Duration::ZERO,
            || "still starting",
            |_| false,
        )
        .unwrap_err();
        assert!(error.contains("fixture ready"));
        assert!(error.contains("still starting"));
    }

    #[test]
    fn native_prerequisites_fail_when_required_and_name_optional_skips() {
        for capability in ["sandbox backend", "rg", "PostgreSQL 18", "node/pnpm"] {
            let count = AtomicUsize::new(0);
            let mut output = Vec::new();
            assert_eq!(
                report_missing_prerequisite(true, capability.into(), &mut output, &count),
                Err(capability.into())
            );
            assert!(output.is_empty());
            assert_eq!(
                report_missing_prerequisite(false, capability.into(), &mut output, &count),
                Ok(())
            );
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("SKIP #1: {capability}\n")
            );
        }
    }
}
