use std::{
    fmt::Display,
    io::Write,
    sync::atomic::{AtomicUsize, Ordering},
};

static SKIP_COUNT: AtomicUsize = AtomicUsize::new(0);

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

/// Reports an unavailable sandbox-dependent test locally, or fails when CI
/// requires native confinement. The ordinal makes skips visible and countable
/// in captured test output.
pub fn skip_sandbox_dependent_test(reason: impl Display) -> Result<(), String> {
    let reason = reason.to_string();
    report_sandbox_skip(
        std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some(),
        reason,
        &mut std::io::stderr(),
        &SKIP_COUNT,
    )
}

fn report_sandbox_skip(
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
        .map_err(|error| format!("failed to report sandbox-dependent test skip: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::report_sandbox_skip;
    use std::sync::atomic::AtomicUsize;

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
    fn optional_sandbox_failure_is_a_visible_skip() {
        let mut output = Vec::new();
        let count = AtomicUsize::new(0);
        assert_eq!(
            report_sandbox_skip(false, "sandbox backend".into(), &mut output, &count),
            Ok(())
        );
        assert_eq!(output, b"SKIP #1: sandbox backend\n");
    }

    #[test]
    fn required_sandbox_failure_fails_closed() {
        let mut output = Vec::new();
        let count = AtomicUsize::new(0);
        assert_eq!(
            report_sandbox_skip(true, "sandbox backend".into(), &mut output, &count),
            Err("sandbox backend".into())
        );
        assert!(output.is_empty());
    }
}
