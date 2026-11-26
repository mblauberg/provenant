#[allow(unused_imports)] // Also compiled by the harness=false worktree driver.
use super::*;

/// Counts observations instead of walking real processes. The one root that
/// stays unknown models a candidate whose custody the census cannot decide.
#[allow(dead_code)] // The harness=false worktree driver drops this #[test] helper.
#[derive(Default)]
struct CountingWorkers {
    calls: std::sync::atomic::AtomicUsize,
}

impl WorkerObserver for CountingWorkers {
    fn observe_roots(
        &self,
        roots: &[PathBuf],
    ) -> std::collections::BTreeMap<PathBuf, worktrees::Occupancy> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        roots
            .iter()
            .map(|root| {
                let state = if root.file_name().is_some_and(|name| name == "undecided") {
                    worktrees::Occupancy::census_unknown(root.clone(), "census cut short")
                } else {
                    worktrees::Occupancy::Available
                };
                (root.clone(), state)
            })
            .collect()
    }
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn one_pass_census_decides_many_candidates_and_keeps_the_undecided() {
    let roots: Vec<PathBuf> = (0..256)
        .map(|index| PathBuf::from(format!("/attempts/{index:04}")))
        .chain(std::iter::once(PathBuf::from("/attempts/undecided")))
        .collect();
    let workers = CountingWorkers::default();
    let cancel = CancelToken::new();
    let report = DwellerCensus::observe(&workers, &roots, &cancel);
    // One observation serves every settled candidate in the pass. It is
    // never re-walked per attempt, and one undecided root makes the whole
    // report incomplete: undecided custody is never read as free.
    assert_eq!(
        workers.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one census per pass, not per candidate"
    );
    assert!(report.incomplete, "an undecided candidate stays undecided");
    assert!(report.processes.is_empty());

    // A pass past its deadline spends nothing on the advisory census and
    // still reports it incomplete rather than as a clean empty result.
    let expired = CancelToken::new();
    expired.cancel();
    let skipped = DwellerCensus::observe(&workers, &roots, &expired);
    assert_eq!(
        workers.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an expired sweep does not start a new census"
    );
    assert!(skipped.incomplete);
    assert!(skipped.processes.is_empty());

    let mut state = provenant_run::ProviderStateGc::default();
    for (index, bytes) in [(0, 4096), (1, 8192)] {
        state.attempt_tmp.push(provenant_run::AttemptTmpRemoval {
            path: PathBuf::from(format!("/attempt-scratch/{index}/tmp")),
            bytes,
        });
    }
    state.partial_tmp.push(provenant_run::AttemptTmpRemoval {
        path: PathBuf::from("/attempt-scratch/2/tmp"),
        bytes: 1024,
    });
    state.scratch_deferred.extend([
        provenant_run::DeferredScratch {
            path: PathBuf::from("/attempt-scratch/2/tmp"),
            bytes: Some(512),
        },
        provenant_run::DeferredScratch {
            path: PathBuf::from("/attempt-scratch/undecided/tmp"),
            bytes: None,
        },
    ]);
    state.kept.push(provenant_run::ProviderStateIssue {
        path: PathBuf::from("/unrelated/provider-state"),
        reason: "unrelated".into(),
    });
    let progress = candidate_progress(&state);
    assert_eq!(progress.decided, 4);
    assert_eq!(progress.collected, 2);
    assert_eq!(
        progress.deferred, 2,
        "undecided candidates wait for the cursor"
    );
    assert_eq!(progress.bytes, 13_312);
    assert_eq!(progress.deferred_bytes, None);
    let _ = state.scratch_deferred.pop();
    assert_eq!(candidate_progress(&state).deferred_bytes, Some(512));
    assert!(DwellerCensus::observe(&workers, &[], &expired).incomplete);
    let mut data = GcData::default();
    assert!(candidate_progress_line(&data).contains("not measured"));
    data.report.dry_run = true;
    data.candidate_progress = Some(progress);
    let line = candidate_progress_line(&data);
    assert!(line.contains("would collect"));
    assert!(line.contains("unknown retained"));
}

#[test]
fn gc_known_total_includes_scratch_archives_and_cargo_reclamation() {
    let mut data = GcData::default();
    data.report.dry_run = true;
    data.report.bytes_freed = 11;
    data.discard_archives.bytes = 13;
    data.build_dirs.bytes = 17;
    data.provider_state
        .attempt_tmp
        .push(provenant_run::AttemptTmpRemoval {
            path: "/tmp/attempt".into(),
            bytes: 19,
        });
    assert_eq!(gc_known_reclaimed_bytes(&data), 60);
    let summary = serde_json::to_value(gc_result_data(&data, false)).unwrap();
    assert_eq!(summary["known_reclaimed_bytes"], 60);
    assert!(gc_data_summary(&data).contains("60 known allocated bytes reclaimable"));
}

#[test]
fn gc_reports_uncollected_candidates_with_reasons_sizes_and_floor_source() {
    let mut data = GcData {
        free_floor_bytes: 4096,
        free_floor_source: "host config.toml".into(),
        attempt_scratch_max_bytes: 2048,
        attempt_scratch_max_source: "host config.toml".into(),
        ..GcData::default()
    };
    data.report
        .kept
        .push(provenant_confinement::cache::CacheGcEntry {
            path: PathBuf::from("/cache/live"),
            bytes: 8192,
            reason: provenant_confinement::cache::CacheGcReason::Live,
        });
    data.report
        .kept
        .push(provenant_confinement::cache::CacheGcEntry {
            path: PathBuf::from("/cache/unknown"),
            bytes: 1024,
            reason: provenant_confinement::cache::CacheGcReason::Unobservable,
        });
    data.provider_state
        .kept
        .push(provenant_run::ProviderStateIssue {
            path: PathBuf::from("/attempts/1/scratch"),
            reason: "live holder".into(),
        });
    data.provider_state
        .partial_tmp
        .push(provenant_run::AttemptTmpRemoval {
            path: PathBuf::from("/attempts/1/scratch"),
            bytes: 2048,
        });
    data.retention_errors
        .push(provenant_config::RetentionIssue {
            path: PathBuf::from("/state/gc/archive"),
            operation: "archive".into(),
            cause: "retained evidence".into(),
        });
    data.stranded_work.push(crate::stranded_work::StrandedWork {
        path: PathBuf::from("/worktrees/unmerged"),
        branch: Some("fix/3950".into()),
        issue: None,
        operation: None,
        files: 2,
        lines: 9,
        age: "1d".into(),
        newest_edit_micros: None,
    });
    let rows = gc_uncollected(&data);
    assert!(rows.iter().any(|row| row.bytes == Some(8192)
        && row.class == "cargo_cache"
        && row.reason == "live holder"));
    assert!(rows.iter().any(|row| row.reason == "uncertain custody"));
    assert!(
        rows.iter()
            .any(|row| row.reason.contains("retained evidence"))
    );
    assert!(rows.iter().any(|row| row.reason.contains("unmerged work")));
    assert!(
        rows.iter().any(|row| {
            row.path == std::path::Path::new("/attempts/1/scratch") && row.bytes.is_none()
        }),
        "removed bytes must not be reported as retained: {rows:?}"
    );
    let json = serde_json::to_value(gc_result_data(&data, false)).unwrap();
    assert_eq!(json["free_floor_bytes"], 4096);
    assert_eq!(json["free_floor_source"], "host config.toml");
    assert_eq!(json["attempt_scratch_max_bytes"], 2048);
    assert_eq!(json["attempt_scratch_max_source"], "host config.toml");
    assert_eq!(json["uncollected"].as_array().unwrap().len(), rows.len());
    // --json emits the Detailed variant: its uncollected list is the one
    // the CLI actually serialises, not just the Summary variant's.
    let detailed = serde_json::to_value(gc_result_data(&data, true)).unwrap();
    let detailed_rows = detailed["uncollected"].as_array().unwrap();
    assert_eq!(detailed_rows.len(), rows.len(), "{detailed}");
    assert!(
        detailed_rows.iter().any(|row| row["path"] == "/cache/live"
            && row["bytes"] == 8192
            && row["class"] == "cargo_cache"),
        "{detailed}"
    );
    let human = gc_compact_summary(&data, false);
    let head = human.lines().take(8).collect::<Vec<_>>().join("\n");
    assert!(
        head.contains("uncollected cargo_cache: 1 item(s), 8192 bytes; kept: live holder"),
        "the aggregate must survive the compact line clip: {human}"
    );
    assert!(
        !human.contains("uncollected cargo_cache /cache/live:"),
        "the default view must not list every kept path: {human}"
    );
    let full = gc_compact_summary(&data, true);
    assert!(
        full.contains("uncollected cargo_cache /cache/live: 8192 bytes; kept: live holder"),
        "--full must keep per-candidate rows: {full}"
    );
    assert!(human.contains("free-space floor: 4096 bytes (source: host config.toml)"));
    assert!(human.contains("attempt scratch cap: 2048 bytes (source: host config.toml)"));
}

#[test]
fn gc_summary_reports_build_output_reclaimed_and_remaining() {
    let mut data = GcData::default();
    data.build_dirs
        .removed
        .push(provenant_run::build_dir_gc::TargetRemoval {
            path: "/cache/a/target".into(),
            worktree: Some("/repo/.worktrees/gone".into()),
            reason: provenant_run::build_dir_gc::TargetReason::Idle,
            last_use: Some(provenant_run::build_dir_gc::LastUse::Stamp),
            bytes: 4096,
        });
    data.build_dirs.bytes = 4096;
    data.build_dirs.cache_bytes = 8192;
    data.build_dirs.budget_bytes = 16384;
    data.build_dirs.budget_setting = "16K".into();
    data.build_dirs.budget_source = "host".into();
    data.build_dirs.target_idle_setting = "30d".into();
    data.build_dirs.target_idle_source = "compiled default".into();
    data.build_dirs.byte_method = provenant_run::build_dir_gc::ByteMethod::AllocatedFallback;
    data.build_dirs.busy.push("/cache/b/target".into());
    data.build_dirs.recent.push("/cache/c/target".into());
    for (dry_run, expected, verb) in [
        (
            true,
            "1 Cargo target(s), 4096 allocated bytes reclaimable; 8192 / 16384 allocated bytes remain (budget setting 16K; source: host; idle after 30d (source: compiled default)), 1 busy, 1 recent\n",
            "would remove",
        ),
        (
            false,
            "1 Cargo target(s), 4096 allocated bytes reclaimed; 8192 / 16384 allocated bytes remain (budget setting 16K; source: host; idle after 30d (source: compiled default)), 1 busy, 1 recent\n",
            "removed",
        ),
    ] {
        data.report.dry_run = dry_run;
        let summary = gc_data_summary(&data);
        assert!(summary.contains(expected), "{summary}");
        assert!(
            summary.contains(&format!(
                "{verb} Cargo target /cache/a/target (idle; last use from last-use stamp; 4096 allocated bytes; worktree /repo/.worktrees/gone)\n"
            )),
            "{summary}"
        );
    }
    let json = serde_json::to_value(gc_result_data(&data, false)).unwrap_or_default();
    assert_eq!(json["build_dirs_removed"], 1);
    assert_eq!(json["build_dir_bytes_reclaimed"], 4096);
    assert_eq!(json["build_dir_bytes"], 8192);
    assert_eq!(json["build_dir_budget_source"], "host");
    assert_eq!(json["build_dir_target_idle_setting"], "30d");
    assert_eq!(json["build_dir_target_idle_source"], "compiled default");
    let rows = gc_uncollected(&data);
    assert!(
        rows.iter().any(|row| {
            row.class == "cargo_target"
                && row.bytes == Some(8192)
                && row.reason.contains("class total")
        }),
        "kept Cargo targets must carry the measured class total: {rows:?}"
    );
    let json = serde_json::to_value(gc_result_data(&data, true)).unwrap_or_default();
    assert_eq!(json["build_dirs"]["cache_bytes"], 8192);
    assert_eq!(json["build_dirs"]["budget_source"], "host");
    assert_eq!(json["build_dirs"]["recent"][0], "/cache/c/target");
    assert_eq!(json["build_dirs"]["removed"][0]["path"], "/cache/a/target");
    assert_eq!(json["build_dirs"]["removed"][0]["reason"], "idle");
    assert_eq!(json["build_dirs"]["removed"][0]["last_use"], "stamp");
    assert_eq!(
        json["build_dirs"]["removed"][0]["worktree"],
        "/repo/.worktrees/gone"
    );
}
#[test]
fn gc_summary_names_the_cargo_target_byte_figure() {
    let mut data = GcData::default();
    data.report.dry_run = true;
    data.build_dirs.bytes = 4096;
    data.build_dirs.cache_bytes = 8192;
    data.build_dirs.budget_bytes = 16384;
    for (method, label, warns, json_name) in [
        (
            provenant_run::build_dir_gc::ByteMethod::AllocatedFallback,
            "allocated bytes",
            true,
            "allocated_fallback",
        ),
        (
            provenant_run::build_dir_gc::ByteMethod::Private,
            "private bytes",
            false,
            "private",
        ),
        (
            provenant_run::build_dir_gc::ByteMethod::Unmeasured,
            "unmeasured bytes",
            false,
            "unmeasured",
        ),
    ] {
        data.build_dirs.byte_method = method;
        let summary = gc_data_summary(&data);
        assert!(
            summary.contains(&format!("4096 {label} reclaimable")),
            "{summary}"
        );
        assert_eq!(
            summary.contains("private bytes unavailable; reporting allocated bytes"),
            warns,
            "{summary}"
        );
        let json = serde_json::to_value(gc_result_data(&data, false)).unwrap_or_default();
        assert_eq!(json["build_dir_byte_method"], json_name);
    }
}

#[test]
fn gc_summary_reports_event_expiry_in_preview_and_applied_mode() {
    let mut data = GcData::default();
    data.provider_state.expired_events = 3;
    for (dry_run, expected) in [
        (true, "would expire 3 event rows\n"),
        (false, "expired 3 event rows\n"),
    ] {
        data.report.dry_run = dry_run;
        assert!(gc_data_summary(&data).contains(expected));
        let json = serde_json::to_value(&data).unwrap_or_default();
        assert_eq!(json["provider_state"]["expired_events"], 3);
    }
}
#[test]
fn gc_preview_names_idle_output_and_why_it_qualifies() {
    let mut data = GcData::default();
    let mut project = worktrees::WorktreeGcReport::default();
    project.idle_output.push(worktrees::OutputEntry {
        bytes: 0,
        path: "/checkout/target".into(),
        removed: false,
        reason: "would_remove".into(),
    });
    data.worktree_cleanup.push(project);
    let summary = gc_data_summary(&data);
    assert!(summary.contains("would reclaim idle output /checkout/target"));
    assert!(summary.contains("idle output and custody checks passed"));
    let json = serde_json::to_value(&data).unwrap();
    assert_eq!(
        json["worktree_cleanup"][0]["idle_output"][0]["reason"],
        "would_remove"
    );
}
#[test]
fn retention_registry_settings_resolve_and_adapters_cover_every_class() {
    let temp = tempfile::tempdir().unwrap();
    let mut input =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("assets"));
    input.state_dir = Some(temp.path().join("state"));
    let roots = input
        .resolve(&provenant_os::fs::SystemPathResolver)
        .unwrap();
    let sections = provenant_cli::config_reads::sections_for("config").unwrap();
    let config = provenant_cli::config_reads::read_for(&roots, "config").unwrap();
    let schema = provenant_config::schema(&config, &sections, String::new()).unwrap();
    for class in provenant_config::RETENTION_CLASSES {
        assert_eq!(class.adapter.kind(), class.class);
        for key in class.settings {
            assert!(
                schema.fields.iter().any(|field| field.path == *key),
                "{:?} declares missing setting {key}",
                class.class
            );
        }
    }
}

#[test]
fn gc_reports_settled_dwellers_and_warns_when_census_is_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("settled-lane");
    let data = GcData {
        held_processes: vec![worktrees::HeldProcess {
            root: provenant_records::StructuredPathWire::from_path(&root).unwrap(),
            pid: 4242,
            command: Some("zsh -c burn".into()),
            evidence: worktrees::ProcessEvidenceKind::WorkingDirectory,
            path: None,
            report: String::new(),
        }],
        held_incomplete: true,
        ..GcData::default()
    };
    let compact = serde_json::to_value(gc_result_data(&data, false)).unwrap();
    let detailed = serde_json::to_value(gc_result_data(&data, true)).unwrap();
    assert_eq!(compact["held_incomplete"], true);
    assert_eq!(detailed["held_incomplete"], true);
    let empty = GcData {
        held_incomplete: true,
        ..GcData::default()
    };
    let compact_empty = serde_json::to_value(gc_result_data(&empty, false)).unwrap();
    assert_eq!(compact_empty["held_incomplete"], true);

    let human = gc_data_summary(&data);
    assert!(human.contains("4242"), "{human}");
    assert!(human.contains("zsh -c burn"), "{human}");
    assert!(human.contains("report only"), "{human}");
    assert!(
        human.contains(worktrees::HELD_CENSUS_INCOMPLETE_WARNING),
        "{human}"
    );
}

/// A sweep that stops before a class never reports that class's empty
/// report as a measured zero with a blank budget and source.
#[test]
fn a_class_the_sweep_never_reached_reports_not_run() {
    use provenant_config::{ByteObservation, RetentionKind};
    let mut data = GcData {
        classes_run: vec![RetentionKind::LaneCargoCaches],
        ..GcData::default()
    };
    data.observe_retention(provenant_config::RetentionTrigger::FullGc);
    assert!(
        data.classes_not_run
            .contains(&RetentionKind::BuildDirectories)
    );
    assert!(
        !data
            .classes_not_run
            .contains(&RetentionKind::LaneCargoCaches)
    );
    let rows = gc_retention_bytes(&data);
    let class = RetentionKind::BuildDirectories;
    assert_eq!(
        rows.iter()
            .find(|row| row.class == class)
            .unwrap()
            .observation,
        ByteObservation::Unknown,
        "{class:?}"
    );
    let summary = gc_data_summary(&data);
    assert!(
        summary.contains("Cargo targets: not run in this pass\n"),
        "{summary}"
    );
    assert!(!summary.contains("Cargo target(s)"), "{summary}");
    assert!(!summary.contains("budget setting ;"), "{summary}");
    let json = serde_json::to_value(gc_result_data(&data, false)).unwrap();
    assert!(
        json["classes_not_run"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("build_directories"))
    );
}

#[test]
fn measurement_failure_is_scoped_to_its_retention_class() {
    use provenant_config::{ByteObservation, RetentionIssue, RetentionKind};
    for failed_class in [RetentionKind::GitWorktrees, RetentionKind::LaneCargoCaches] {
        let mut report = worktrees::WorktreeGcReport::default();
        let issues = if failed_class == RetentionKind::GitWorktrees {
            &mut report.byte_errors
        } else {
            &mut report.cache_errors
        };
        issues.push(RetentionIssue {
            path: "/fixture".into(),
            operation: "measure".into(),
            cause: "unavailable".into(),
        });
        let mut data = GcData::default();
        data.worktree_cleanup.push(report);
        data.classes_run = provenant_config::RETENTION_CLASSES
            .iter()
            .map(|class| class.class)
            .collect();
        data.observe_retention(provenant_config::RetentionTrigger::FullGc);
        for row in gc_retention_bytes(&data).into_iter().filter(|r| {
            matches!(
                r.class,
                RetentionKind::GitWorktrees | RetentionKind::LaneCargoCaches
            )
        }) {
            assert_eq!(
                matches!(row.observation, ByteObservation::Partial { .. }),
                row.class == failed_class
            );
        }
    }
}

#[test]
fn gc_reports_every_retention_class_and_only_existing_byte_measurements() {
    use provenant_config::{ByteObservation, RETENTION_CLASSES, RetentionKind};
    let mut data = GcData::default();
    data.report.bytes_freed = 19;
    data.build_dirs.bytes = 7;
    data.build_dirs.cache_bytes = 29;
    data.provider_state.payload.reclaimed_bytes = 31;
    data.provider_state
        .attempt_logs
        .push(provenant_run::AttemptLogRemoval {
            path: "/state/attempt/a/log".into(),
            bytes: 43,
            tail_bytes: 7,
        });
    data.provider_state.attempt_tmp.extend([
        provenant_run::AttemptTmpRemoval {
            path: "/state/attempt/a".into(),
            bytes: 37,
        },
        provenant_run::AttemptTmpRemoval {
            path: "/state/attempt/b".into(),
            bytes: 41,
        },
    ]);

    assert!(
        gc_retention_bytes(&data)
            .iter()
            .all(|row| matches!(row.observation, ByteObservation::Unknown))
    );
    data.classes_run = RETENTION_CLASSES.iter().map(|class| class.class).collect();
    data.observe_retention(provenant_config::RetentionTrigger::FullGc);
    let rows = gc_retention_bytes(&data);
    assert_eq!(rows.len(), RETENTION_CLASSES.len());
    assert!(RETENTION_CLASSES.iter().all(|declared| {
        rows.iter()
            .filter(|row| row.class == declared.class)
            .count()
            == 1
    }));
    let observation = |kind| {
        rows.iter()
            .find(|row| row.class == kind)
            .map(|row| row.observation)
            .unwrap()
    };
    assert_eq!(
        observation(RetentionKind::LaneCargoCaches),
        ByteObservation::Measured { bytes: 19 }
    );
    assert_eq!(
        observation(RetentionKind::BuildDirectories),
        ByteObservation::Measured { bytes: 29 }
    );
    assert_eq!(
        observation(RetentionKind::PayloadCap),
        ByteObservation::Measured { bytes: 31 }
    );
    assert_eq!(
        observation(RetentionKind::AttemptScratch),
        ByteObservation::Partial { bytes: 78 }
    );
    assert_eq!(
        observation(RetentionKind::ProviderState),
        ByteObservation::Partial { bytes: 43 }
    );
    assert_eq!(
        observation(RetentionKind::GraphCheckpoints),
        ByteObservation::Unknown
    );

    data.report.dry_run = true;
    let preview = gc_retention_bytes(&data);
    for class in [RetentionKind::BuildDirectories, RetentionKind::GitWorktrees] {
        assert_eq!(
            preview
                .iter()
                .find(|row| row.class == class)
                .unwrap()
                .observation,
            rows.iter()
                .find(|row| row.class == class)
                .unwrap()
                .observation
        );
    }
    data.report.dry_run = false;
    let summary = serde_json::to_value(gc_result_data(&data, false)).unwrap();
    let detailed = serde_json::to_value(gc_result_data(&data, true)).unwrap();
    assert_eq!(
        summary["retention_bytes"].as_array().unwrap().len(),
        rows.len()
    );
    assert_eq!(
        detailed["retention_bytes"].as_array().unwrap().len(),
        rows.len()
    );
}

#[test]
fn gc_reports_keep_retirement_projections_and_never_infer_them_from_removed_rows() {
    use provenant_records::custody::{LandingPredicate, LandingSource, RemovalOutcome};
    let project = provenant_core::ProjectId::from_bytes([9; 16]);
    let tip = GitOid::parse(&"a1".repeat(20)).unwrap();
    let lane = worktrees::WorktreeInfo {
        warnings: Vec::new(),
        path: PathBuf::from("/repo/.worktrees/topic"),
        branch: Some("refs/heads/topic".into()),
        head: tip,
        dirty: worktrees::WorktreeCondition::Clean,
        occupancy: worktrees::Occupancy::Available,
        owner: Ok(None),
        primary: false,
        lane: None,
        attempt: None,
        base: None,
        reused: false,
    };
    let mut report = worktrees::WorktreeGcReport::default();
    report.worktrees.pruned_refs.push("topic".into());
    append_gc_removal_records(&mut report);
    assert!(report.removals.is_empty());
    let projection = RemovalRecord {
        project,
        subject: Subject::worktree("/repo/.worktrees/topic", "fixture-1"),
        tip,
        outcome: RemovalOutcome::Removed {
            evidence: provenant_records::custody::RemovalEvidence::Landing {
                evidence: LandedEvidence {
                    predicate: LandingPredicate::Landed,
                    source: LandingSource::IntegrationTarget {
                        reference: "refs/remotes/origin/main".into(),
                        commit: GitOid::parse(&"b2".repeat(20)).unwrap(),
                    },
                },
            },
            retirement_id: "retirement-1".into(),
        },
    };
    report.refs.push(
        provenant_confinement::worktrees::ArtefactOutcome::new(
            provenant_confinement::worktrees::Artefact::LocalBranch,
            "refs/heads/topic",
            provenant_confinement::worktrees::ArtefactState::Removed,
        )
        .with_removal(projection.clone()),
    );
    append_gc_removal_records(&mut report);
    assert_eq!(report.removals, vec![projection.clone()]);

    let mut preview = worktrees::WorktreeGcReport {
        dry_run: true,
        ..Default::default()
    };
    preview.worktrees.removed.push(lane);
    preview.worktrees.pruned_refs.push("topic".into());
    append_gc_removal_records(&mut preview);
    assert!(preview.removals.is_empty());

    let mut output = GcData::default();
    output.worktree_cleanup.push(report);
    let expected = serde_json::to_value(&projection).unwrap();
    let records = serde_json::to_value(gc_result_data(&output, false)).unwrap();
    assert!(
        records["removal_records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record == &expected),
    );
}

#[test]
// This fixture measures its own stderr byte count as the reported contract.
#[allow(clippy::disallowed_methods)]
fn gc_default_output_aggregates_routine_rows_and_keeps_exceptions() {
    use provenant_confinement::cache::{CacheGcEntry, CacheGcReason};
    let mut data = GcData::default();
    for index in 0..10 {
        data.report.removed.push(CacheGcEntry {
            path: format!("/home/Library/Caches/provenant-lane-cargo/{index:064}").into(),
            bytes: 1024,
            reason: CacheGcReason::Orphaned,
        });
        data.report.kept.push(CacheGcEntry {
            path: format!(
                "/home/Library/Caches/provenant-lane-cargo/{:064}",
                index + 10
            )
            .into(),
            bytes: 2048,
            reason: CacheGcReason::Live,
        });
    }
    data.report.bytes_freed = 10240;
    data.provider_state
        .errors
        .push(provenant_run::ProviderStateIssue {
            path: "/state/broken".into(),
            reason: "permission denied".into(),
        });
    let mut project = worktrees::WorktreeGcReport::default();
    project.worktrees.retained.push(worktrees::RetainedBranch {
        branch: "broken".into(),
        reason: RetainReason::GitFailure,
        detail: None,
    });
    project.worktrees.retained.push(worktrees::RetainedBranch {
        branch: "census-blocked".into(),
        reason: RetainReason::UnknownWriter,
        detail: Some("process 968: cwd unreadable; configure confinement.census_ignore_unreadable; path=/Library/Developer/simdiskimaged, team_id=TEAM123".into()),
    });
    data.worktree_cleanup.push(project);
    let human = gc_data_summary(&data);
    let after = human.len();
    let detailed_json = serde_json::to_vec(&data).unwrap_or_default().len();
    let after_json = serde_json::to_vec(&gc_result_data(&data, false))
        .unwrap_or_default()
        .len();
    std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!(
            "gc fixture bytes: human {after}, detailed data {detailed_json}, default data {after_json}\n"
        ),
    )
    .unwrap_or_default();
    assert!(after_json < detailed_json);
    assert!(human.contains("10 cache(s), 10240 bytes freed"));
    assert!(human.contains("/state/broken: permission denied"));
    assert!(human.contains("kept worktree broken (GitFailure)"));
    assert!(human.contains("kept worktree census-blocked (UnknownWriter): process 968"));
    assert!(human.contains("confinement.census_ignore_unreadable"));
    assert!(human.contains("/Library/Developer/simdiskimaged"));
    assert!(human.contains("team_id=TEAM123"));
    assert!(!human.contains("/home/Library/Caches"));
    assert!(serde_json::to_value(&data).unwrap_or_default()["removed"].is_array());
}
#[test]
fn gc_failure_human_output_names_failed_paths_and_next_step()
-> Result<(), Box<dyn std::error::Error>> {
    let mut data = GcData::default();
    data.provider_state
        .errors
        .push(provenant_run::ProviderStateIssue {
            path: "/state/failed-cache".into(),
            reason: "permission denied".into(),
        });
    let result = gc_failure(&data, "provider-state retention incomplete", false);
    let mut output = Vec::new();
    provenant_cli::render(
        &result,
        provenant_cli::RenderMode::Human {
            text: "",
            read_only: false,
            summary: None,
        },
        &mut output,
    )?;
    let human = String::from_utf8(output)?;
    assert!(
        human.contains("/state/failed-cache: permission denied"),
        "{human}"
    );
    assert!(human.contains("provenant gc --apply --json"), "{human}");
    Ok(())
}
#[test]
fn read_only_context_accepts_repository_without_mutating_it()
-> Result<(), Box<dyn std::error::Error>> {
    let rig = crate::worktree_tests::repository()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let facts = worktrees::RepoFacts::default();
    let workers = worktrees::SystemWorkerObserver;
    // Establish the project binding and state directories before preview.
    with_context_using_facts(
        &roots,
        &rig.home,
        &rig.project,
        MUTATION_TIMEOUT,
        &workers,
        &facts,
        |_, _| Ok(()),
    )?;
    let control_files = || -> std::io::Result<std::collections::BTreeSet<std::ffi::OsString>> {
        std::fs::read_dir(rig.project.join(".git"))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    };
    let before = control_files()?;
    let observed = with_context_mode(
        &roots,
        &rig.home,
        &rig.project,
        MUTATION_TIMEOUT,
        &workers,
        &facts,
        true,
        |_, _| Ok(true),
    )?;
    assert!(observed);
    assert_eq!(
        control_files()?,
        before,
        "read-only setup must not create Git control files"
    );
    Ok(())
}

#[test]
fn gc_preview_does_not_initialise_absent_state() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let assets = base.join("assets");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&assets)?;
    let state = base.join("absent");
    let roots = crate::roots::child(&home, &assets, state.clone())?;
    let result = with_context_mode(
        &roots,
        &home,
        &base,
        MUTATION_TIMEOUT,
        &worktrees::SystemWorkerObserver,
        &worktrees::RepoFacts::default(),
        true,
        |_, _| Ok(()),
    );
    assert!(result.is_err());
    assert!(!state.exists());
    Ok(())
}
