//! Shared test-only process helpers for the top-level crate.
use std::{error::Error, io::Write, time::Duration};

pub(crate) fn reexecute_exact_test(
    module_path: &str,
    test_name: &str,
    child_marker: &str,
    extra_env: &[(std::ffi::OsString, std::ffi::OsString)],
    timeout: Duration,
) -> Result<bool, Box<dyn Error>> {
    if std::env::var_os(child_marker).is_some() {
        return Ok(false);
    }
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let module = module_path
        .split_once("::")
        .map_or(module_path, |(_, path)| path);
    let name = format!("{module}::{test_name}");
    let argv = vec![
        std::env::current_exe()?.into_os_string(),
        "--exact".into(),
        name.into(),
        "--nocapture".into(),
    ];
    const ENVIRONMENT: &[&str] = &[
        "PATH",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LC_MESSAGES",
        "LC_COLLATE",
        "LC_MONETARY",
        "LC_NUMERIC",
        "LC_TIME",
        "RUST_BACKTRACE",
        "RUST_TEST_THREADS",
        "RUST_TEST_NOCAPTURE",
        "PROVENANT_CONFINEMENT_REQUIRED",
        "PROVENANT_POSTGRES_REQUIRED",
        "PROVENANT_LANE_TOOLS_REQUIRED",
        "PROVENANT_TEST_POSTGRES_BIN",
    ];
    let fixture_home = tempfile::tempdir()?;
    let fixture_tmp =
        std::env::var_os("TMPDIR").unwrap_or_else(|| fixture_home.path().as_os_str().to_owned());
    let mut environment: Vec<_> = ENVIRONMENT
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| ((*key).into(), value)))
        .collect();
    environment.push(("HOME".into(), fixture_home.path().as_os_str().to_owned()));
    for key in ["TMPDIR", "TMP", "TEMP"] {
        environment.push((key.into(), fixture_tmp.clone()));
    }
    for (key, value) in extra_env {
        environment.retain(|(existing, _)| existing != key);
        environment.push((key.clone(), value.clone()));
    }
    environment.retain(|(key, _)| key != child_marker);
    environment.push((child_marker.into(), "1".into()));
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, timeout);
    options.env = Some(&environment);
    options.process_group = true;
    options.cleanup_process_group = true;
    let output = SystemExecutor::new(1)?.execute(options)?;
    std::io::stderr().lock().write_all(&output.stderr.bytes)?;
    if output.termination != Termination::Exit(0) {
        return Err(format!(
            "re-executed test failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout.bytes);
    let started = stdout.lines().any(|line| line.trim() == "running 1 test");
    let passed_once = stdout
        .lines()
        .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;"));
    if !started || !passed_once {
        return Err(format!("exact re-exec did not run one test: {stdout}").into());
    }
    Ok(true)
}
