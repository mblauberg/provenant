// The worktree integration target includes main.rs with its own test harness.
#![allow(dead_code)]
use super::*;
use provenant_core::{PathResolver, RootInputs};
use std::path::{Path, PathBuf};
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
fn recording_on_a_missing_root_creates_nothing() -> Result {
    let temp = crate::worktree_tests::support::tempdir()?;
    let roots = roots(temp.path())?;
    record_cli(&roots, "root", 0, 4);
    record_tool(&roots, "fabric_identity", "ok", 1);
    assert!(!roots.state.as_path().exists());
    Ok(())
}
#[test]
fn rotation_preserves_appends_and_reports_while_the_rotation_lock_is_busy() -> Result {
    let temp = crate::worktree_tests::support::tempdir()?;
    let roots = roots(temp.path())?;
    std::fs::create_dir(roots.state.as_path())?;
    std::fs::set_permissions(
        roots.state.as_path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )?;
    record_cli(&roots, "root", 0, 4);
    let log = roots.state.as_path().join(LOG);
    assert!(!roots.state.as_path().join(LOCK).exists());
    let directory = Directory::open_anchor(roots.state.as_path())?;
    let lock = directory.try_lock(OsStr::new(LOCK), LockMode::Exclusive)?;
    let before = std::fs::read(&log)?;
    record_cli(&roots, "root", 0, 1);
    assert!(std::fs::read(&log)?.len() > before.len());
    let stats = Stats {
        usage: true,
        since: None,
        until: None,
        run_id: None,
        detail: false,
    };
    let count = |data: Report| {
        data.units
            .into_iter()
            .find(|u| u.unit == "root")
            .map(|u| u.calls)
    };
    assert_eq!(count(report(&roots, &stats)?), Some(2));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&log)?
        .set_len(CAP)?;
    record_cli(&roots, "root", 0, 1);
    assert!(std::fs::metadata(&log)?.len() > CAP);
    assert_eq!(count(report(&roots, &stats)?), Some(2));
    drop(lock);
    // MCP logging only appends; existing workers perform rotation.
    record_tool(&roots, "fabric_identity", "ok", 1);
    assert!(!roots.state.as_path().join(BACKUP).exists());
    maintain_tools(&roots);
    // Rotation is permitted again; the old log is retained as the backup.
    record_cli(&roots, "root", 0, 1);
    assert!(std::fs::metadata(roots.state.as_path().join(BACKUP))?.len() >= CAP);
    assert!(std::fs::metadata(&log)?.len() < LINE_CAP as u64);
    assert_eq!(count(report(&roots, &stats)?), Some(3));
    // A torn line is ignored, including an event appended to that line.
    std::fs::write(&log, b"{partial")?;
    record_cli(&roots, "root", 0, 1);
    record_cli(&roots, "root", 0, 1);
    std::fs::remove_file(roots.state.as_path().join(BACKUP))?;
    assert_eq!(count(report(&roots, &stats)?), Some(1));
    let target = temp.path().join("foreign");
    std::fs::write(&target, "unchanged")?;
    std::fs::remove_file(&log)?;
    std::os::unix::fs::symlink(&target, &log)?;
    record_cli(&roots, "root", 0, 1);
    assert_eq!(std::fs::read_to_string(target)?, "unchanged");
    Ok(())
}
