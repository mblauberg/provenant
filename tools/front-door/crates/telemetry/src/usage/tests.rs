use super::*;
use provenant_core::{MonoTime, PathResolver, RootInputs, TimeError, WallTime};
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
fn tempdir() -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    Ok((temp, base))
}
fn inventory() -> Vec<(String, String)> {
    vec![
        (EVENT_KINDS[0].into(), "root".into()),
        (EVENT_KINDS[1].into(), "fabric_context".into()),
    ]
}

#[test]
fn usage_log_read_errors_include_the_os_cause() {
    let error = UsageError::Io(std::io::Error::from_raw_os_error(13));
    let rendered = error.to_string();
    assert!(rendered.starts_with("cannot read the usage log: "));
    assert!(rendered.contains("os error 13"));
}

/// A clock fixed at one wall time.
const CLOCK: &dyn Clock = &Fixed;
struct Fixed;
impl Clock for Fixed {
    fn wall_now(&self) -> std::result::Result<WallTime, TimeError> {
        WallTime::parse("2026-10-26T00:00:00.000000Z")
    }
    fn mono_now(&self) -> MonoTime {
        MonoTime::from_micros(0)
    }
    fn wait_until(&self, _: MonoTime) -> std::result::Result<(), TimeError> {
        Ok(())
    }
}

#[test]
fn recording_on_a_missing_root_creates_nothing() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    record_cli(&roots, CLOCK, "root", 0, 4, None);
    record_tool(&roots, CLOCK, "fabric_context", "ok", 1, None);
    assert!(!roots.state.as_path().exists());
    Ok(())
}

#[test]
fn unattributed_events_omit_trace_from_json() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    std::fs::create_dir(roots.state.as_path())?;
    record_cli(&roots, CLOCK, "root", 0, 4, None);
    record_tool(&roots, CLOCK, "fabric_context", "ok", 1, None);
    assert!(roots.state.as_path().join(LOG).is_file());
    let log = std::fs::read_to_string(roots.state.as_path().join(LOG))?;
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.get("project_id").is_none()));
    assert!(events.iter().all(|event| event.get("trace").is_none()));
    assert!(
        events
            .iter()
            .all(|event| event["timestamp"] == "2026-10-26T00:00:00.000000Z")
    );
    Ok(())
}

#[test]
fn attributed_events_are_reported_under_their_project_id() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    std::fs::create_dir(roots.state.as_path())?;
    let expected_id = ProjectId::from_bytes([1; 16]);
    record_cli(&roots, CLOCK, "root", 0, 4, Some(expected_id));
    record_tool(&roots, CLOCK, "fabric_context", "ok", 1, Some(expected_id));

    let log = std::fs::read_to_string(roots.state.as_path().join(LOG))?;
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["kind"], "cli.verb");
    assert_eq!(events[1]["kind"], "mcp.tool");
    assert!(
        events
            .iter()
            .all(|event| event["trace"]["project"] == expected_id.to_string())
    );

    let report = report(&roots, inventory(), None, None)?;
    assert!(report.units.iter().any(|unit| {
        unit.unit == "root"
            && unit.calls == 1
            && unit.project_id == Some(expected_id)
            && unit.scope == Some("project")
    }));
    assert!(report.units.iter().any(|unit| {
        unit.unit == "fabric_context" && unit.calls == 1 && unit.project_id == Some(expected_id)
    }));
    assert!(
        report
            .units
            .iter()
            .any(|unit| { unit.unit == "root" && unit.calls == 1 && unit.project_id.is_none() })
    );
    Ok(())
}

#[test]
fn rotation_preserves_appends_and_reports_while_the_rotation_lock_is_busy() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    std::fs::create_dir(roots.state.as_path())?;
    std::fs::set_permissions(
        roots.state.as_path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )?;
    record_cli(&roots, CLOCK, "root", 0, 4, None);
    let log = roots.state.as_path().join(LOG);
    assert!(!roots.state.as_path().join(LOCK).exists());
    let directory = Directory::open_anchor(roots.state.as_path())?;
    let lock = directory.try_lock(OsStr::new(LOCK), LockMode::Exclusive)?;
    let before = std::fs::read(&log)?;
    record_cli(&roots, CLOCK, "root", 0, 1, None);
    assert!(std::fs::read(&log)?.len() > before.len());
    let count = |data: Report| {
        data.units
            .into_iter()
            .find(|u| u.unit == "root")
            .map(|u| u.calls)
    };
    assert_eq!(count(report(&roots, inventory(), None, None)?), Some(2));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&log)?
        .set_len(CAP)?;
    record_cli(&roots, CLOCK, "root", 0, 1, None);
    assert!(std::fs::metadata(&log)?.len() > CAP);
    assert_eq!(count(report(&roots, inventory(), None, None)?), Some(2));
    drop(lock);
    // MCP logging only appends; existing workers perform rotation.
    record_tool(&roots, CLOCK, "fabric_context", "ok", 1, None);
    assert!(!roots.state.as_path().join(BACKUP).exists());
    maintain_tools(&roots);
    // Rotation is permitted again; the old log is retained as the backup.
    record_cli(&roots, CLOCK, "root", 0, 1, None);
    assert!(std::fs::metadata(roots.state.as_path().join(BACKUP))?.len() >= CAP);
    assert!(std::fs::metadata(&log)?.len() < LINE_CAP as u64);
    assert_eq!(count(report(&roots, inventory(), None, None)?), Some(3));
    // A torn line is ignored, including an event appended to that line.
    std::fs::write(&log, b"{partial")?;
    record_cli(&roots, CLOCK, "root", 0, 1, None);
    record_cli(&roots, CLOCK, "root", 0, 1, None);
    std::fs::remove_file(roots.state.as_path().join(BACKUP))?;
    assert_eq!(count(report(&roots, inventory(), None, None)?), Some(1));
    let target = base.join("foreign");
    std::fs::write(&target, "unchanged")?;
    std::fs::remove_file(&log)?;
    std::os::unix::fs::symlink(&target, &log)?;
    record_cli(&roots, CLOCK, "root", 0, 1, None);
    assert_eq!(std::fs::read_to_string(target)?, "unchanged");
    Ok(())
}
