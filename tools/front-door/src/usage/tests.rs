// The worktree integration target includes main.rs with its own test harness.
#![allow(dead_code)]
use super::*;
use provenant_core::{PathResolver, RootInputs};
use std::path::PathBuf;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
        Ok(path.to_owned())
    }
}
fn roots(path: &Path) -> Result<Roots> {
    let mut inputs = RootInputs::new(path.join("home"), path.join("checkout"));
    inputs.state_dir = Some(path.join("state"));
    Ok(inputs.resolve(&Resolver)?)
}
#[test]
fn append_rotates_and_never_waits_for_a_busy_writer_or_follows_log_links() -> Result {
    let temp = tempfile::tempdir_in("/tmp")?;
    let roots = roots(temp.path())?;
    record_cli(&roots, "root", 0, 4);
    let log = roots.state.as_path().join(LOG);
    let before = std::fs::read(&log)?;
    record_tool(&roots, "fabric_identity", "ok", 2);
    assert!(std::fs::read(&log)?.starts_with(&before));
    let directory = Directory::open_anchor(roots.state.as_path())?;
    let lock = directory.try_lock(OsStr::new(LOCK), LockMode::Exclusive)?;
    let before = std::fs::read(&log)?;
    let start = std::time::Instant::now();
    record_cli(&roots, "root", 0, 1);
    assert!(start.elapsed() < std::time::Duration::from_millis(100));
    assert_eq!(std::fs::read(&log)?, before);
    drop(lock);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&log)?
        .set_len(CAP)?;
    record_cli(&roots, "root", 0, 1);
    assert_eq!(
        std::fs::metadata(roots.state.as_path().join(BACKUP))?.len(),
        CAP
    );
    let fresh = std::fs::read(&log)?;
    assert!(fresh.len() < LINE_CAP);
    let target = temp.path().join("foreign");
    std::fs::write(&target, "unchanged")?;
    std::fs::remove_file(&log)?;
    std::os::unix::fs::symlink(&target, &log)?;
    record_cli(&roots, "root", 0, 1);
    assert_eq!(std::fs::read_to_string(target)?, "unchanged");
    Ok(())
}
#[test]
fn report_filters_time_reads_both_files_and_ignores_partial_lines() -> Result {
    let temp = tempfile::tempdir_in("/tmp")?;
    let roots = roots(temp.path())?;
    let dir = roots.state.as_path();
    std::fs::create_dir(dir)?;
    let line = |date: &str| {
        format!(
            "{{\"kind\":\"cli.verb\",\"unit\":\"root\",\"exit_code\":0,\"duration_ms\":1,\"timestamp\":\"{date}T00:00:00.000000Z\"}}\n"
        )
    };
    std::fs::write(dir.join(BACKUP), line("2026-01-01") + &line("2026-02-01"))?;
    std::fs::write(
        dir.join(LOG),
        line("2026-02-02") + &line("2026-02-03") + "{\"partial\":",
    )?;
    let stats = Stats {
        usage: true,
        since: Some("2026-02-01T00:00:00Z".parse()?),
        until: Some("2026-02-03T00:00:00Z".parse()?),
        detail: false,
    };
    let data = report(&roots, &stats)?;
    assert_eq!(
        data.units
            .iter()
            .find(|u| u.unit == "root")
            .ok_or("root")?
            .calls,
        2
    );
    Ok(())
}
