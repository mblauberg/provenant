use std::{
    fmt::Display,
    io::Write,
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
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

/// How long a bounded wait sleeps between observations. Centralising the
/// cadence keeps tests from encoding their own fixed synchronisation sleeps:
/// the deadline is owned by the caller, the polling interval is not.
const WAIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Poll an observation until it satisfies a condition or `deadline` passes,
/// reporting the last value on timeout. Every fixed wait in a fixture goes
/// through here, so the only sleeping code lives at this explicit boundary.
#[allow(clippy::disallowed_methods)] // Test-only bounded polling boundary.
pub fn wait_until<T: std::fmt::Debug>(
    name: &str,
    deadline: std::time::Instant,
    mut observe: impl FnMut() -> T,
    mut reached: impl FnMut(&T) -> bool,
) -> Result<T, String> {
    let mut last = observe();
    loop {
        if reached(&last) {
            return Ok(last);
        }
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Err(format!(
                "timed out waiting for {name}; last observation: {last:?}"
            ));
        };
        std::thread::sleep(remaining.min(WAIT_POLL_INTERVAL));
        last = observe();
    }
}

/// Advance one bounded step of a caller-owned deadline loop without naming a
/// fixed sleep in the test. Returns `false` once the deadline has passed;
/// fixtures that need to report a specific last observation use `wait_until`.
#[allow(clippy::disallowed_methods)] // Explicit bounded polling boundary.
pub fn wait_step(deadline: std::time::Instant) -> bool {
    let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
        return false;
    };
    std::thread::sleep(remaining.min(WAIT_POLL_INTERVAL));
    true
}

/// `wait_until` with a relative timeout, for fixtures that do not share an
/// outer deadline with the steps around them.
#[allow(clippy::disallowed_methods)] // Creates the explicit real-time boundary.
pub fn wait_for<T: std::fmt::Debug>(
    name: &str,
    timeout: std::time::Duration,
    observe: impl FnMut() -> T,
    reached: impl FnMut(&T) -> bool,
) -> Result<T, String> {
    wait_until(name, std::time::Instant::now() + timeout, observe, reached)
}

/// A one-shot event with a caller-owned deadline. Producers publish a value
/// and wake every waiter; waiters block on the condition variable instead of
/// sleeping for a guessed interval, and still give up at the caller's
/// deadline. The first value wins so a repeated event cannot change an
/// asserted outcome.
#[derive(Clone, Debug)]
pub struct Event<T> {
    inner: Arc<(Mutex<Option<T>>, Condvar)>,
}

/// An `Event` used purely as a readiness signal.
pub type Signal = Event<()>;

/// Make a new, unpublished signal.
pub fn signal() -> Signal {
    Event::new()
}

impl<T> Default for Event<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Event<T> {
    /// Create an unpublished event.
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(None), Condvar::new())),
        }
    }

    /// Publish the first value and wake all waiters.
    pub fn publish(&self, value: T) {
        let (slot, wake) = &*self.inner;
        let mut slot = slot.lock().unwrap_or_else(|error| error.into_inner());
        if slot.is_none() {
            *slot = Some(value);
            wake.notify_all();
        }
    }

    /// Take the published value, if it has been sent.
    pub fn take(&self) -> Option<T> {
        let (slot, _) = &*self.inner;
        slot.lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// Wait for a published value or until `deadline` passes, whichever comes
    /// first. Returns `None` on timeout.
    #[allow(clippy::disallowed_methods)] // Explicit real-time wait boundary.
    pub fn wait_until(&self, deadline: std::time::Instant) -> Option<T> {
        let (slot, wake) = &*self.inner;
        let mut slot = slot.lock().unwrap_or_else(|error| error.into_inner());
        while slot.is_none() {
            let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
            let (next, _) = wake
                .wait_timeout(slot, remaining)
                .unwrap_or_else(|error| error.into_inner());
            slot = next;
        }
        slot.take()
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
// Test diagnostics print the skip on stderr so the harness captures it.
#[allow(clippy::disallowed_methods)]
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

/// Report a test skipped because the host filesystem cannot provide a
/// prerequisite entry, for example a non-UTF-8 name that APFS refuses to
/// create. A filesystem limitation is not a lane-policy failure: required
/// native tiers get the reported skip instead of a false failure.
// Test diagnostics print the skip on stderr so the harness captures it.
#[allow(clippy::disallowed_methods)]
pub fn skip_filesystem_dependent_test(reason: impl Display) -> Result<(), String> {
    report_filesystem_dependent_skip(reason.to_string(), &mut std::io::stderr(), &SKIP_COUNT)
}

fn report_filesystem_dependent_skip(
    reason: String,
    output: &mut impl Write,
    count: &AtomicUsize,
) -> Result<(), String> {
    report_missing_prerequisite(false, reason, output, count)
}

/// Fail a required native prerequisite, or print a numbered local skip.
// Test diagnostics print the prerequisite context on stderr for the harness.
#[allow(clippy::disallowed_methods)]
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
    // These tests own their real-time deadline boundary to prove the helpers
    // stop at it; production fixtures pass in a deadline already on hand.
    #![allow(clippy::disallowed_methods, reason = "bounded wait deadline in tests")]

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
    fn filesystem_dependent_skip_reports_a_reason_and_never_fails() {
        let mut output = Vec::new();
        let count = AtomicUsize::new(0);
        let result = super::report_filesystem_dependent_skip(
            "APFS rejects non-UTF-8 names".into(),
            &mut output,
            &count,
        );
        assert!(result.is_ok());
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "SKIP #1: APFS rejects non-UTF-8 names\n"
        );
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
    fn wait_until_returns_when_the_condition_is_reached_before_the_deadline() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut observed = 0;
        let result = super::wait_until(
            "ready",
            deadline,
            || {
                observed += 1;
                observed
            },
            |value| *value >= 3,
        );
        assert_eq!(result, Ok(3));
    }

    #[test]
    fn wait_until_stops_at_the_deadline_and_names_the_last_observation() {
        let deadline = std::time::Instant::now();
        let error = super::wait_until("fixture marker", deadline, || "still absent", |_| false)
            .unwrap_err();
        assert!(error.contains("fixture marker"), "{error}");
        assert!(error.contains("still absent"), "{error}");
    }

    #[test]
    fn wait_step_stops_at_the_callers_deadline() {
        let past = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap();
        assert!(!super::wait_step(past));
        let future = std::time::Instant::now() + std::time::Duration::from_secs(1);
        assert!(super::wait_step(future));
    }

    #[test]
    fn event_wakes_a_waiter_without_a_fixed_wait() {
        let event = super::Event::<i32>::new();
        let publisher = event.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            publisher.publish(42);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        assert_eq!(event.wait_until(deadline), Some(42));
        worker.join().unwrap();
    }

    #[test]
    fn event_keeps_the_first_value_and_times_out_when_never_published() {
        let event = super::Event::new();
        event.publish("first");
        event.publish("second");
        assert_eq!(event.take(), Some("first"));
        let deadline = std::time::Instant::now();
        assert_eq!(event.wait_until(deadline), None);
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
