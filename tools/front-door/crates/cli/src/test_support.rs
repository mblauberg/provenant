//! Small, shared test seams for subprocess boundaries.

use provenant_os::exec::{ExecOptions, ExecOutcome, Executor, SystemExecutor};
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
    time::Duration,
};

/// Re-run a unit test in a child process with a path derived from its module.
/// The child receives only this safe process context and caller-supplied fixture
/// values; host credentials and HOME are never copied into the child.
#[cfg(test)]
pub(crate) fn rerun_test(
    module: &str,
    test_name: &str,
    additions: &[(OsString, OsString)],
    remove_env: &[&str],
    timeout: Duration,
) -> Result<ExecOutcome, Box<dyn std::error::Error>> {
    const SAFE_TEST_ENV: &[&str] = &[
        "PATH",
        "PROVENANT_CONFINEMENT_REQUIRED",
        "PROVENANT_LANE_TOOLS_REQUIRED",
        "TMPDIR",
        "TMP",
        "TEMP",
        "RUST_BACKTRACE",
        "RUST_TEST_THREADS",
        "RUST_TEST_NOCAPTURE",
        "LANG",
        "LC_ALL",
    ];
    let removed = |key: &OsStr| remove_env.iter().any(|name| key == OsStr::new(name));
    let mut environment: Vec<_> = SAFE_TEST_ENV
        .iter()
        .filter_map(|name| {
            let key = OsStr::new(name);
            (!removed(key))
                .then(|| std::env::var_os(key).map(|value| (key.to_owned(), value)))
                .flatten()
        })
        .collect();
    for (key, value) in additions {
        environment.retain(|(existing, _)| existing != key);
        environment.push((key.clone(), value.clone()));
    }
    let fixture_home = tempfile::tempdir()?;
    if !removed(OsStr::new("HOME")) && !environment.iter().any(|(key, _)| key == "HOME") {
        environment.push(("HOME".into(), fixture_home.path().as_os_str().to_owned()));
    }

    let module = module.split_once("::").map_or(module, |(_, path)| path);
    let argv = [
        std::env::current_exe()?.into_os_string(),
        "--exact".into(),
        format!("{module}::{test_name}").into(),
        "--nocapture".into(),
    ];
    let cwd: PathBuf = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, timeout);
    options.env = Some(&environment);
    options.process_group = true;
    options.cleanup_process_group = true;
    let outcome = SystemExecutor::new(1)?.execute(options)?;
    let stdout = String::from_utf8_lossy(&outcome.stdout.bytes);
    if !stdout.lines().any(|line| line.trim() == "running 1 test")
        || !stdout
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;"))
    {
        return Err("re-exec filter did not run exactly one test".into());
    }
    Ok(outcome)
}
