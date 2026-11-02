use std::{
    fmt::Display,
    io::Write,
    sync::atomic::{AtomicUsize, Ordering},
};

static SKIP_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Reports an unavailable sandbox-dependent test locally, or fails when CI
/// requires native confinement. The ordinal makes skips visible and countable
/// in captured test output.
pub fn skip_sandbox_dependent_test(reason: impl Display) -> Result<(), String> {
    let reason = reason.to_string();
    report_sandbox_skip(
        std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some(),
        reason,
    )
}

fn report_sandbox_skip(required: bool, reason: String) -> Result<(), String> {
    if required {
        return Err(reason);
    }

    let count = SKIP_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    writeln!(std::io::stderr(), "SKIP #{count}: {reason}")
        .map_err(|error| format!("failed to report sandbox-dependent test skip: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::report_sandbox_skip;

    #[test]
    fn optional_sandbox_failure_is_a_visible_skip() {
        assert_eq!(report_sandbox_skip(false, "unavailable".into()), Ok(()));
    }

    #[test]
    fn required_sandbox_failure_fails_closed() {
        assert_eq!(
            report_sandbox_skip(true, "unavailable".into()),
            Err("unavailable".into())
        );
    }
}
