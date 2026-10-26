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
    record_cli(&roots, "root", 0, 4, None);
    record_tool(&roots, "fabric_identity", "ok", 1, None);
    assert!(!roots.state.as_path().exists());
    Ok(())
}

#[test]
fn project_attribution_requires_the_knob_and_an_available_resolver() -> Result {
    let temp = crate::worktree_tests::support::tempdir()?;
    let roots = roots(temp.path())?;
    std::fs::create_dir(roots.state.as_path())?;
    std::fs::set_permissions(
        roots.state.as_path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let location = provenant_config::ProjectLocation::new(&project)?;
    let binding = provenant_config::bind_project(&roots, location)?;
    let expected_id = binding.project_id();
    let globals = Globals {
        project: Some(project),
        ..Globals::default()
    };
    assert!(!attribution_enabled(&roots));
    assert_eq!(project_id(&roots, &globals), None);
    std::fs::write(
        roots.state.as_path().join("config.toml"),
        "schema_version = 1\n[knobs]\n\"usage.project_attribution\" = true\n",
    )?;
    assert!(attribution_enabled(&roots));
    // No Git installation means best-effort attribution stays unavailable.
    let expected = crate::roots::git_binary().map(|_| expected_id);
    assert_eq!(project_id(&roots, &globals), expected);
    Ok(())
}

#[test]
fn unattributed_events_omit_trace_from_json() -> Result {
    let temp = crate::worktree_tests::support::tempdir()?;
    let roots = roots(temp.path())?;
    std::fs::create_dir(roots.state.as_path())?;
    record_cli(&roots, "root", 0, 4, None);
    record_tool(&roots, "fabric_identity", "ok", 1, None);
    let log = std::fs::read_to_string(roots.state.as_path().join(LOG))?;
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.get("trace").is_none()));
    Ok(())
}

#[test]
fn attributed_events_are_reported_under_their_project_id() -> Result {
    let temp = crate::worktree_tests::support::tempdir()?;
    let roots = roots(temp.path())?;
    std::fs::create_dir(roots.state.as_path())?;
    let expected_id = ProjectId::from_bytes([1; 16]);
    record_cli(&roots, "root", 0, 4, Some(expected_id));
    record_tool(&roots, "fabric_identity", "ok", 1, Some(expected_id));

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

    let stats = Stats {
        usage: true,
        since: None,
        until: None,
        run_id: None,
        detail: false,
    };
    let report = report(&roots, &stats)?;
    assert!(report.units.iter().any(|unit| {
        unit.unit == "root"
            && unit.calls == 1
            && unit.project_id == Some(expected_id)
            && unit.scope == Some("project")
    }));
    assert!(report.units.iter().any(|unit| {
        unit.unit == "fabric_identity" && unit.calls == 1 && unit.project_id == Some(expected_id)
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
    let temp = crate::worktree_tests::support::tempdir()?;
    let roots = roots(temp.path())?;
    std::fs::create_dir(roots.state.as_path())?;
    std::fs::set_permissions(
        roots.state.as_path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )?;
    record_cli(&roots, "root", 0, 4, None);
    let log = roots.state.as_path().join(LOG);
    assert!(!roots.state.as_path().join(LOCK).exists());
    let directory = Directory::open_anchor(roots.state.as_path())?;
    let lock = directory.try_lock(OsStr::new(LOCK), LockMode::Exclusive)?;
    let before = std::fs::read(&log)?;
    record_cli(&roots, "root", 0, 1, None);
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
    record_cli(&roots, "root", 0, 1, None);
    assert!(std::fs::metadata(&log)?.len() > CAP);
    assert_eq!(count(report(&roots, &stats)?), Some(2));
    drop(lock);
    // MCP logging only appends; existing workers perform rotation.
    record_tool(&roots, "fabric_identity", "ok", 1, None);
    assert!(!roots.state.as_path().join(BACKUP).exists());
    maintain_tools(&roots);
    // Rotation is permitted again; the old log is retained as the backup.
    record_cli(&roots, "root", 0, 1, None);
    assert!(std::fs::metadata(roots.state.as_path().join(BACKUP))?.len() >= CAP);
    assert!(std::fs::metadata(&log)?.len() < LINE_CAP as u64);
    assert_eq!(count(report(&roots, &stats)?), Some(3));
    // A torn line is ignored, including an event appended to that line.
    std::fs::write(&log, b"{partial")?;
    record_cli(&roots, "root", 0, 1, None);
    record_cli(&roots, "root", 0, 1, None);
    std::fs::remove_file(roots.state.as_path().join(BACKUP))?;
    assert_eq!(count(report(&roots, &stats)?), Some(1));
    let target = temp.path().join("foreign");
    std::fs::write(&target, "unchanged")?;
    std::fs::remove_file(&log)?;
    std::os::unix::fs::symlink(&target, &log)?;
    record_cli(&roots, "root", 0, 1, None);
    assert_eq!(std::fs::read_to_string(target)?, "unchanged");
    Ok(())
}
