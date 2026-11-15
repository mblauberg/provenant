use super::*;
use provenant_core::{MonoTime, TimeError, WallTime};
use provenant_test_support::{IdentityPathResolver, test_roots};
use std::path::{Path, PathBuf};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn roots(path: &Path) -> Result<Roots> {
    Ok(test_roots(path)?.resolve(&IdentityPathResolver)?)
}
fn tempdir() -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    Ok((temp, base))
}
fn inventory() -> Vec<(String, String)> {
    vec![(EVENT_KINDS[0].into(), "root".into())]
}

fn event_line(
    unit: &str,
    exit_code: u8,
    timestamp: &str,
    session: Option<&str>,
    project: Option<ProjectId>,
    wait: Option<bool>,
) -> Result<String> {
    let event = Event {
        kind: EVENT_KINDS[0].into(),
        unit: unit.into(),
        exit_code: Some(exit_code),
        outcome: None,
        duration_ms: 1,
        timestamp: timestamp.into(),
        trace: project_trace(project),
        account_identity: AccountIdentity::no_provider_account(),
        session: session.map(str::to_owned),
        config: None,
        wait,
        extra: Default::default(),
    };
    Ok(format!("{}\n", serde_json::to_string(&event)?))
}

#[test]
fn usage_log_read_errors_preserve_os_errno() {
    let error = UsageError::Io(std::io::Error::from_raw_os_error(13));
    assert_eq!(error.code(), "io");
    let errno = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .and_then(std::io::Error::raw_os_error);
    assert_eq!(errno, Some(13));
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
fn every_account_identity_case_round_trips() -> Result {
    let cases = [
        AccountIdentity {
            identity: Identity::Known {
                provider: "claude".into(),
                account_key: "account-key".into(),
            },
            epoch: IdentityEpoch::Known { id: "epoch".into() },
            evidence_source: IdentityEvidenceSource::Provider,
        },
        AccountIdentity::unknown(),
        AccountIdentity::no_provider_account(),
    ];
    for identity in cases {
        let bytes = serde_json::to_vec(&identity)?;
        let decoded: AccountIdentity = serde_json::from_slice(&bytes)?;
        assert_eq!(decoded, identity);
    }
    Ok(())
}

#[test]
fn usage_event_retains_additive_fields_through_bounded_decode() -> Result {
    let mut event: serde_json::Value = serde_json::from_str(&event_line(
        "root",
        0,
        "2026-10-26T00:00:00Z",
        Some("session"),
        Some(ProjectId::from_bytes([1; 16])),
        Some(true),
    )?)?;
    event["future_event"] = serde_json::json!(1);
    event["trace"]["future_trace"] = serde_json::json!(2);
    event["account_identity"]["future_identity"] = serde_json::json!(3);
    crate::assert_record_round_trip::<super::Event>(event);
    Ok(())
}

#[test]
fn recording_on_a_missing_root_creates_nothing() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    record_cli(&roots, CLOCK, "root", 0, 4, None);
    assert!(!roots.state.as_path().exists());
    Ok(())
}

#[test]
fn chair_observations_survive_rotation_and_respect_window_bounds() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    let project = ProjectId::from_bytes([1; 16]);
    record_chair(
        &roots,
        CLOCK,
        AccountIdentity {
            identity: Identity::Known {
                provider: "claude".into(),
                account_key: "account-work".into(),
            },
            epoch: IdentityEpoch::Known {
                id: "epoch-1".into(),
            },
            evidence_source: IdentityEvidenceSource::Provider,
        },
        "session",
        Some(project),
        Some(".claude-work"),
    );
    std::fs::rename(
        roots.state.as_path().join(LOG),
        roots.state.as_path().join(BACKUP),
    )?;
    record_chair(
        &roots,
        CLOCK,
        AccountIdentity::unknown(),
        "another-session",
        Some(project),
        None,
    );
    let since = CLOCK.wall_now()?.as_timestamp();
    let until = "2026-10-27T00:00:00Z".parse()?;
    let chairs = chair_sessions(&roots, since, until)?;
    assert_eq!(chairs.len(), 2);
    assert_eq!(
        chairs[0].account_identity.account_key(),
        "claude/account-work"
    );
    assert_eq!(chairs[0].config.as_deref(), Some(".claude-work"));
    assert_eq!(chairs[1].account_identity, AccountIdentity::unknown());
    assert_eq!(chairs[1].config, None);
    assert!(
        chairs
            .iter()
            .all(|c| c.project == Some(project) && c.adapter == "claude")
    );
    assert!(chair_sessions(&roots, "2026-10-25T00:00:00Z".parse()?, since)?.is_empty());
    assert!(chair_sessions(&roots, until, "2026-10-28T00:00:00Z".parse()?)?.is_empty());
    // Chair observations do not become command-demand counts.
    assert!(report(&roots, [], None, None)?.units.is_empty());
    Ok(())
}

#[test]
fn unattributed_events_omit_trace_from_json() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    record_cli(&roots, CLOCK, "root", 0, 4, None);
    assert!(roots.state.as_path().join(LOG).is_file());
    let log = std::fs::read_to_string(roots.state.as_path().join(LOG))?;
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(events.len(), 1);
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
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    let expected_id = ProjectId::from_bytes([1; 16]);
    record_cli(&roots, CLOCK, "root", 0, 4, Some(expected_id));

    let log = std::fs::read_to_string(roots.state.as_path().join(LOG))?;
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "cli.verb");
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
    assert!(
        report
            .units
            .iter()
            .any(|unit| { unit.unit == "root" && unit.calls == 1 && unit.project_id.is_none() })
    );
    Ok(())
}

#[test]
fn rotation_preserves_the_tail_and_reports_without_a_lock() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    record_cli(&roots, CLOCK, "root", 0, 4, None);
    let log = roots.state.as_path().join(LOG);
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
    assert_eq!(
        std::fs::metadata(roots.state.as_path().join(BACKUP))?.len(),
        CAP
    );
    assert!(std::fs::metadata(&log)?.len() < LINE_CAP as u64);
    assert_eq!(count(report(&roots, inventory(), None, None)?), Some(3));
    assert!(!roots.state.as_path().join("usage.lock").exists());
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

#[test]
fn oversized_log_reads_recent_complete_events() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    let mut bytes = vec![b'x'; CAP as usize];
    bytes.push(b'\n');
    bytes.extend_from_slice(
        event_line("root", 0, "2026-10-26T00:00:00.000000Z", None, None, None)?.as_bytes(),
    );
    std::fs::write(roots.state.as_path().join(LOG), bytes)?;
    let report = report(&roots, inventory(), None, None)?;
    assert_eq!(
        report
            .units
            .iter()
            .find(|unit| unit.unit == "root")
            .unwrap()
            .calls,
        1
    );
    Ok(())
}

#[test]
fn window_starting_on_a_record_start_keeps_that_record() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    // The newline before the event puts the window's start exactly on the event.
    let mut bytes = b"x\n".to_vec();
    bytes.extend_from_slice(
        event_line("root", 0, "2026-10-26T00:00:00.000000Z", None, None, None)?.as_bytes(),
    );
    bytes.resize(2 + CAP as usize, b'x');
    std::fs::write(roots.state.as_path().join(LOG), bytes)?;
    let report = report(&roots, inventory(), None, None)?;
    assert_eq!(
        report
            .units
            .iter()
            .find(|unit| unit.unit == "root")
            .unwrap()
            .calls,
        1
    );
    Ok(())
}

#[test]
fn failed_verb_retry_help_and_wait_poll_are_counted() -> Result {
    let (_temp, base) = tempdir()?;
    let roots = roots(&base)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    // Native usage.jsonl records are built from the current typed event shape.
    let project = ProjectId::from_bytes([1; 16]);
    let mut log = String::new();
    for (unit, exit_code, timestamp, session, project, wait) in [
        (
            "run",
            2,
            "2026-10-01T00:00:00Z",
            Some("fixture-session"),
            Some(project),
            None,
        ),
        (
            "help",
            0,
            "2026-10-01T00:00:01Z",
            Some("fixture-session"),
            Some(project),
            None,
        ),
        (
            "run",
            0,
            "2026-10-01T00:00:02Z",
            Some("fixture-session"),
            Some(project),
            None,
        ),
        (
            "status",
            0,
            "2026-10-01T00:00:03Z",
            Some("fixture-session"),
            Some(project),
            Some(true),
        ),
        (
            "status",
            0,
            "2026-10-01T00:00:04Z",
            Some("fixture-session"),
            Some(project),
            Some(false),
        ),
        ("status", 0, "2026-10-01T00:00:05Z", None, None, None),
        (
            "run",
            2,
            "2026-10-01T00:00:06Z",
            Some("fixture-unscoped"),
            None,
            None,
        ),
        (
            "run",
            0,
            "2026-10-01T00:00:07Z",
            Some("fixture-unscoped"),
            None,
            None,
        ),
    ] {
        log.push_str(&event_line(
            unit, exit_code, timestamp, session, project, wait,
        )?);
    }
    std::fs::write(roots.state.as_path().join(LOG), log)?;
    let report = ergonomics(&roots, None, None, None)?;
    assert_eq!(report.help_calls, 1);
    assert_eq!(report.failed_verb_then_retry, 1);
    assert_eq!(report.first_attempts, 3);
    assert_eq!(report.first_attempt_success, 2);
    assert_eq!(report.sequence_unknown, 3);
    assert_eq!(report.wait_calls, 1);
    assert_eq!(report.poll_calls, 1);
    assert_eq!(report.wait_mode_unknown, 1);
    Ok(())
}
