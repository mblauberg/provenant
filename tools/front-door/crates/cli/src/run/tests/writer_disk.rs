use super::*;

#[test]
fn retained_import_refusal_names_worktree_attempt_and_recovery() {
    let run_id = provenant_core::RunId::from_bytes([3; 16]);
    let attempt = AttemptId::from_bytes([4; 16]);
    let failure = check_launch_policy(&[run::Started::Refused {
        attempt: AttemptId::from_bytes([1; 16]),
        kind: run::RefusalKind::RetainedImport {
            run: run_id,
            attempt,
            worktree: "/tmp/writer".into(),
        },
        reason: "retained".into(),
    }])
    .expect_err("retained import must refuse");
    assert_eq!(failure.kind.code(), "WRITER_IMPORT_RETAINED");
    assert!(failure.message.contains("/tmp/writer"));
    assert!(failure.message.contains(&attempt.to_string()));
    let diagnostic = failure.projection.expect("diagnostic");
    assert!(
        diagnostic
            .next
            .iter()
            .any(|next| next.contains(&format!("reconcile --abandon {run_id}/{attempt}")))
    );
}

#[test]
fn setup_timeout_is_promoted_only_without_a_progressing_sibling() {
    let refused = || run::Started::Refused {
        attempt: AttemptId::from_bytes([1; 16]),
        kind: run::RefusalKind::SetupLockTimeout { seconds: 42 },
        reason: "setup timed out".into(),
    };
    let failure = check_launch_policy(&[refused()]).expect_err("timeout must be reported");
    assert_eq!(failure.kind.code(), "SETUP_LOCK_TIMEOUT");
    assert!(failure.message.contains("42s"));
    assert!(
        check_launch_policy(&[
            refused(),
            run::Started::Launched(AttemptId::from_bytes([2; 16]))
        ])
        .is_ok()
    );
}

#[test]
fn dispatch_policy_refusal_preserves_launched_sibling() -> Result<(), String> {
    check_launch_policy(&[
        run::Started::Refused {
            attempt: AttemptId::from_bytes([1; 16]),
            kind: run::RefusalKind::Policy,
            reason: "insufficient disk space".into(),
        },
        run::Started::Launched(AttemptId::from_bytes([2; 16])),
    ])
    .map_err(|error| error.message)
}

#[test]
fn policy_refusal_preserves_uncertain_sibling_handoff() -> Result<(), String> {
    let refused = || run::Started::Refused {
        attempt: AttemptId::from_bytes([1; 16]),
        kind: run::RefusalKind::Policy,
        reason: "insufficient disk space".into(),
    };
    let error = check_launch_policy(&[refused()])
        .err()
        .ok_or("policy refusal was ignored")?;
    assert_eq!(error.status, Status::Refused);
    assert_eq!(error.kind, ErrorKind::Refused);
    // Unavailable routes still return their normal per-attempt receipts.
    for kind in [
        run::RefusalKind::PriorityUnavailable,
        run::RefusalKind::Preparation,
    ] {
        check_launch_policy(&[run::Started::Refused {
            attempt: AttemptId::from_bytes([3; 16]),
            kind,
            reason: "codex/model: UnknownAvailability".into(),
        }])
        .map_err(|error| error.message)?;
    }
    // Production classifies a pacing-held route as a preparation refusal whose
    // reason is the routing exclusion detail.
    let held = provenant_routing::exclusion_detail(
        &provenant_records::RouteExclusionReason::PacingReserve,
    );
    let error = check_launch_policy(&[run::Started::Refused {
        attempt: AttemptId::from_bytes([4; 16]),
        kind: run::RefusalKind::Preparation,
        reason: format!("codex/model: {held}"),
    }])
    .err()
    .ok_or("pacing-held preparation refusal was ignored")?;
    assert_eq!(error.status, Status::Refused);
    assert!(error.message.contains("PacingReserve"));
    // Let the existing unknown-handoff handling retain the full receipt.
    check_launch_policy(&[
        refused(),
        run::Started::Unknown {
            attempt: AttemptId::from_bytes([2; 16]),
            error: "handoff unconfirmed".into(),
        },
    ])
    .map_err(|error| error.message)?;
    Ok(())
}

#[test]
fn blocked_custody_is_reported_without_hiding_a_running_sibling() -> Result<(), String> {
    let blocked = || run::Started::Blocked {
        attempt: AttemptId::from_bytes([1; 16]),
        error: "custody withdrawal failed".into(),
    };
    let error = check_launch_policy(&[blocked()])
        .err()
        .ok_or("block was ignored")?;
    assert_eq!(error.status, Status::Refused);
    assert!(error.message.contains("custody withdrawal failed"));
    assert!(
        matches!(error.detail.as_deref(), Some(provenant_fabric_runtime::coordination::facade::FacadeError::DispatchRefused { reason, .. }) if reason.contains("custody withdrawal failed"))
    );
    check_launch_policy(&[
        blocked(),
        run::Started::Launched(AttemptId::from_bytes([2; 16])),
    ])
    .map_err(|error| error.message)
}

#[test]
fn writer_gc_settings_use_defaults_when_configuration_is_unreadable()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    std::os::unix::fs::symlink(temp.path().join("missing"), temp.path().join("config.toml"))?;
    assert_eq!(
        writer_gc_settings(temp.path()),
        provenant_config::GcSettings::default()
    );
    Ok(())
}

#[test]
fn writer_requires_space_on_every_output_volume() -> Result<(), String> {
    let settings = provenant_config::GcSettings {
        free_floor: Some("100B".into()),
        ..Default::default()
    };
    let state = Path::new("/state");
    let worktree = Path::new("/worktree");
    let home = Path::new("/home");
    let cache = home.join("Library/Caches/provenant-lane-cargo");
    for low in [worktree, state, cache.as_path()] {
        let mut observed = Vec::new();
        let result = check_writer_disk_with(&settings, state, worktree, home, |path| {
            observed.push(path.to_path_buf());
            Ok(if path == low { 99 } else { 1000 })
        });
        let error = result
            .err()
            .ok_or_else(|| format!("writer accepted low volume {}", low.display()))?;
        assert!(
            matches!(error, DiskCheck::Low { ref path, available: 99, floor: 100, .. } if path == low)
        );
        assert!(observed.contains(&low.to_path_buf()));
    }
    check_writer_disk_with(&settings, state, worktree, home, |_| Ok(100))?;
    Ok(())
}

#[test]
fn writer_disk_observation_failure_warns_and_continues() -> Result<(), String> {
    let settings = provenant_config::GcSettings {
        free_floor: Some("100B".into()),
        ..Default::default()
    };
    let result = check_writer_disk_with(
        &settings,
        Path::new("/state"),
        Path::new("/worktree"),
        Path::new("/home"),
        |_| Err("Not a directory".into()),
    );
    result.map_err(String::from)
}

#[test]
fn writer_rechecks_once_after_collection_and_reports_reclaimed_bytes() -> Result<(), String> {
    use std::cell::Cell;
    let settings = provenant_config::GcSettings {
        free_floor: Some("100B".into()),
        ..Default::default()
    };
    for remaining in [99, 100] {
        let collected = Cell::new(false);
        let samples = Cell::new(0);
        let result = check_writer_disk_with_collection(
            &settings,
            Path::new("/state"),
            Path::new("/tree"),
            Path::new("/home"),
            |_| {
                samples.set(samples.get() + 1);
                Ok(if collected.get() { remaining } else { 50 })
            },
            || {
                assert!(!collected.replace(true));
                49
            },
        );
        assert_eq!(samples.get(), 6);
        if remaining == 100 {
            result?;
        } else {
            let error = result.err().ok_or("writer accepted low disk")?;
            assert!(matches!(
                error,
                DiskCheck::Low {
                    available: 99,
                    floor: 100,
                    reclaimed: 49,
                    ..
                }
            ));
        }
    }
    let calls = Cell::new(0);
    check_writer_disk_with_collection(
        &settings,
        Path::new("/state"),
        Path::new("/tree"),
        Path::new("/home"),
        |_| Ok(100),
        || {
            calls.set(calls.get() + 1);
            0
        },
    )?;
    assert_eq!(calls.get(), 0);
    let disabled = provenant_config::GcSettings {
        schedule: false,
        ..settings
    };
    assert!(
        check_writer_disk_with_collection(
            &disabled,
            Path::new("/state"),
            Path::new("/tree"),
            Path::new("/home"),
            |_| Ok(1),
            || {
                calls.set(calls.get() + 1);
                99
            }
        )
        .is_err()
    );
    assert_eq!(calls.get(), 0);

    Ok(())
}

#[test]
fn writer_samples_missing_roots_without_creating_them() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let settings = provenant_config::GcSettings {
        free_floor: Some("1B".into()),
        ..Default::default()
    };
    let state = base.join("missing/state");
    let home = base.join("missing/home");
    check_writer_disk(&settings, &state, &base, &base, &home).map_err(String::from)?;
    assert!(!base.join("missing").exists());
    Ok(())
}
