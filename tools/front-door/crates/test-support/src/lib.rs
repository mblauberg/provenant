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
