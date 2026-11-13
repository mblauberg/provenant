use super::*;

#[test]
fn disk_policy_refusal_uses_resource_projection() {
    use provenant_records::diagnostic::{ResourceFact, SettingSource};
    let refusal = crate::run::disk_policy_refusal(crate::run::DiskCheck::Low {
        path: "/tmp/example".into(),
        available: 1,
        floor: 2,
        reclaimed: 0,
        origin: SettingSource::Configuration,
    });
    let fact = refusal.resource.expect("disk fact");
    let kind = run::RefusalKind::PolicyResource(fact);
    let failure = check_launch_policy(&[run::Started::Refused {
        attempt: AttemptId::from_bytes([1; 16]),
        kind,
        reason: "local path and other source text".into(),
    }])
    .expect_err("disk floor must refuse");
    assert_eq!(failure.kind, ErrorKind::Refused);
    let diagnostic = failure.projection.expect("resource projection");
    assert_eq!(diagnostic.code, "REFUSED");
    assert!(
        diagnostic
            .detail
            .as_deref()
            .unwrap()
            .contains("gc.free_floor")
    );
    assert!(
        diagnostic
            .detail
            .as_deref()
            .unwrap()
            .contains("host configuration")
    );
    assert!(diagnostic.next.is_some());
    assert!(!diagnostic.detail.as_deref().unwrap().contains("local path"));
    assert!(
        check_launch_policy(&[
            run::Started::Refused {
                attempt: AttemptId::from_bytes([2; 16]),
                kind: run::RefusalKind::PolicyResource(ResourceFact::Disk {
                    free: 1,
                    floor: 2,
                    origin: SettingSource::Configuration,
                }),
                reason: "low disk".into(),
            },
            run::Started::Launched(AttemptId::from_bytes([3; 16])),
        ])
        .is_ok()
    );
}

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
    // All refusal kinds fail when no sibling can progress.
    for kind in [
        run::RefusalKind::PriorityUnavailable,
        run::RefusalKind::Preparation,
    ] {
        let error = check_launch_policy(&[run::Started::Refused {
            attempt: AttemptId::from_bytes([3; 16]),
            kind,
            reason: "codex/model: UnknownAvailability".into(),
        }])
        .expect_err("all-refused batch must fail");
        assert_eq!(error.status, Status::Refused);
    }
    // The typed cause promotes pacing without interpreting candidate prose.
    let pacing = || run::Started::Refused {
        attempt: AttemptId::from_bytes([4; 16]),
        kind: run::RefusalKind::PacingReserve,
        reason: "opaque candidate refusal".into(),
    };
    let error = check_launch_policy(&[pacing()])
        .err()
        .ok_or("pacing-held preparation refusal was ignored")?;
    assert_eq!(error.status, Status::Refused);
    assert_eq!(error.kind, ErrorKind::Refused);
    assert!(error.message.contains("opaque candidate refusal"));
    // Let the existing unknown-handoff handling retain the full receipt.
    for refusal in [refused(), pacing()] {
        check_launch_policy(&[
            refusal,
            run::Started::Unknown {
                attempt: AttemptId::from_bytes([2; 16]),
                error: "handoff unconfirmed".into(),
            },
        ])
        .map_err(|error| error.message)?;
    }
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
            matches!(error, DiskCheck::Low { ref path, available: 99, floor: 100, origin: provenant_records::diagnostic::SettingSource::Configuration, .. } if path == low)
        );
        assert!(observed.contains(&low.to_path_buf()));
    }
    check_writer_disk_with(&settings, state, worktree, home, |_| Ok(100))?;
    Ok(())
}

#[test]
fn writer_disk_default_floor_names_its_source() -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let settings = provenant_config::GcSettings::default();
    let error = check_writer_disk_with(
        &settings,
        directory.path(),
        directory.path(),
        directory.path(),
        |_| Ok(0),
    )
    .err()
    .ok_or("zero disk space was accepted")?;
    assert!(matches!(
        error,
        DiskCheck::Low {
            origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
            ..
        }
    ));
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

#[test]
fn writer_disk_policy_refusal_keeps_disk_facts_and_safe_recovery() -> Result<(), String> {
    use provenant_records::run::TerminalCause;
    let settings = provenant_config::GcSettings {
        free_floor: Some("100B".into()),
        ..Default::default()
    };
    let check = check_writer_disk_with(
        &settings,
        Path::new("/state"),
        Path::new("/worktree"),
        Path::new("/home"),
        |_| Ok(99),
    )
    .err()
    .ok_or("writer accepted insufficient disk")?;
    let refusal = disk_policy_refusal(check);
    assert_eq!(refusal.terminal_cause, TerminalCause::DiskFloor);
    assert_eq!(
        refusal.resource,
        Some(provenant_records::diagnostic::ResourceFact::Disk {
            free: 99,
            floor: 100,
            origin: provenant_records::diagnostic::SettingSource::Configuration,
        })
    );
    assert!(
        TerminalCause::DiskFloor
            .detail()
            .contains("provenant gc --apply")
    );
    assert!(!TerminalCause::DiskFloor.detail().contains("access grants"));
    let unobserved = disk_policy_refusal(DiskCheck::Unobserved("HOST_ERROR_SENTINEL".into()));
    assert_eq!(
        unobserved.terminal_cause,
        TerminalCause::DiskObservationUnavailable
    );
    assert!(unobserved.terminal_note.is_none());
    assert!(
        !TerminalCause::DiskObservationUnavailable
            .detail()
            .contains("HOST_ERROR_SENTINEL")
    );
    assert!(
        TerminalCause::DiskObservationUnavailable
            .detail()
            .contains("restore the host disk observation")
    );
    Ok(())
}

#[test]
fn preparation_and_handoff_resources_reach_launch_policy_projection() {
    use provenant_records::diagnostic::{
        PressureLevel, PressureThreshold, ResourceFact, SettingSource,
    };
    for kind in [
        run::RefusalKind::PreparationResource(ResourceFact::DependencyPreparation {
            configured: true,
        }),
        run::RefusalKind::HandoffResource(ResourceFact::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: SettingSource::Configuration,
        }),
    ] {
        let expected = kind.diagnostic().unwrap();
        let failure = check_launch_policy(&[run::Started::Refused {
            attempt: AttemptId::from_bytes([1; 16]),
            kind,
            reason: "UNSAFE_SOURCE_SENTINEL".into(),
        }])
        .unwrap_err();
        let diagnostic = failure.projection.expect("typed launch refusal projection");
        assert_eq!(diagnostic.code, "REFUSED");
        assert_eq!(diagnostic.detail, expected.detail);
        assert_eq!(diagnostic.next, expected.next);
        assert!(
            !serde_json::to_string(&diagnostic)
                .unwrap()
                .contains("UNSAFE_SOURCE_SENTINEL")
        );
    }
}
