//! Doctor diagnoses the same custody boundary as admission without repairing it.
use super::lifecycle_tests::TestContext;
use super::*;
use crate::test_support::Fixture;
use std::{error::Error, fs};

#[test]
fn doctor_reports_five_worktree_states_and_one_repair_without_mutation()
-> Result<(), Box<dyn Error>> {
    for scenario in ["dirty", "stale", "held", "unregistered", "detached"] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let cancel = CancelToken::new();
        let path = fixture.root.join(format!("doctor-{scenario}"));
        if scenario != "unregistered" {
            fixture.run(&["worktree", "add", "--detach", path.to_str().ok_or("path")?])?;
            if scenario != "detached" {
                fixture.run(&[
                    "-C",
                    path.to_str().ok_or("path")?,
                    "switch",
                    "-c",
                    "doctor-fixture",
                ])?;
            }
        }
        let mut lease = None;
        let holder = AttemptId::from_bytes([42; 16]);
        match scenario {
            "dirty" => fs::write(path.join("uncommitted"), "preserve this work")?,
            "stale" => fs::remove_dir_all(&path)?,
            "held" => {
                let slot = writer_directory(context.project_state, &path)?;
                lease = Some(provenant_supervisor::fixture::acquire_writer(
                    slot,
                    provenant_supervisor::custody::Scope {
                        run: provenant_core::RunId::from_bytes([41; 16]),
                        attempt: holder,
                        generation: 1,
                    },
                    WaitBudget::nonblocking(context.clock.mono_now()),
                    &cancel,
                    context.clock,
                )?);
            }
            _ => {}
        }
        let before = context.facts.writer_inventory(
            context.repo,
            context.executor,
            context.utility,
            context.utility_budget,
            &cancel,
        )?;
        let report = doctor(&context, &path, &cancel)?;
        let (expected, repair_suffix) = match scenario {
            "dirty" => (
                WorktreeDiagnosisState::Dirty,
                "stash push --include-untracked",
            ),
            "stale" => (WorktreeDiagnosisState::StaleRegistration, "worktree prune"),
            "held" => (
                WorktreeDiagnosisState::LiveWriter,
                "provenant status --wait",
            ),
            "unregistered" => (
                WorktreeDiagnosisState::Unregistered,
                "provenant worktree create BRANCH",
            ),
            "detached" => (WorktreeDiagnosisState::Detached, "switch -c BRANCH"),
            _ => unreachable!(),
        };
        assert_eq!(report.state, expected, "{scenario}: {report:?}");
        assert!(!report.reason.is_empty());
        assert!(report.repair.ends_with(repair_suffix), "{report:?}");
        assert!(!report.repair.contains('\n'), "one repair command");
        assert_eq!(
            context.facts.writer_inventory(
                context.repo,
                context.executor,
                context.utility,
                context.utility_budget,
                &cancel,
            )?,
            before,
            "doctor must not prune, attach, clean or register the worktree"
        );
        match scenario {
            "dirty" => assert_eq!(
                fs::read_to_string(path.join("uncommitted"))?,
                "preserve this work"
            ),
            "held" => {
                assert_eq!(report.holder, Some(holder));
                assert!(matches!(
                    cache_writer_probe(context.project_state, &path),
                    Err(Occupancy::Held)
                ));
            }
            "stale" | "unregistered" => assert!(!path.exists()),
            _ => {}
        }
        drop(lease);
    }
    Ok(())
}

#[test]
fn doctor_unknown_preserves_census_and_custody_scope_and_evidence() -> Result<(), Box<dyn Error>> {
    struct Incomplete;
    impl WorkerObserver for Incomplete {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .map(|root| {
                    (
                        root.clone(),
                        Occupancy::census_unknown(
                            root.clone(),
                            "fixture descriptors unreadable for pid 24",
                        ),
                    )
                })
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = WorktreeContext {
        workers: &Incomplete,
        ..test.context()
    };
    let cancel = CancelToken::new();
    let path = fixture.root.join("doctor-unknown");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "doctor-unknown",
        path.to_str().ok_or("path")?,
    ])?;
    let census = doctor(&context, &path, &cancel)?;
    assert_eq!(census.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(census.source, Some("census"));
    assert_eq!(
        serde_json::to_value(&census.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert_eq!(
        census.evidence.as_deref(),
        Some("fixture descriptors unreadable for pid 24")
    );
    assert!(census.repair.starts_with("provenant worktree doctor "));

    struct CensusHeld;
    impl WorkerObserver for CensusHeld {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Held))
                .collect()
        }
    }
    let census_held = doctor(
        &WorktreeContext {
            workers: &CensusHeld,
            ..test.context()
        },
        &path,
        &cancel,
    )?;
    assert_eq!(census_held.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(census_held.source, Some("census"));
    assert_eq!(census_held.holder, None);
    assert_eq!(
        serde_json::to_value(&census_held.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert!(
        census_held
            .evidence
            .as_deref()
            .is_some_and(|evidence| evidence.contains("without a held writer lease"))
    );

    let verified = verify_worktree(context.repo, &path)?;
    let owner_path = verified
        .repo()
        .private_git_dir()
        .join("provenant-owner.json");
    fs::write(&owner_path, "malformed owner record")?;
    let malformed = doctor(&test.context(), &path, &cancel)?;
    assert_eq!(malformed.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(malformed.source, Some("custody"));
    assert_eq!(
        serde_json::to_value(&malformed.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert!(
        malformed
            .evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.is_empty())
    );
    assert!(malformed.repair.starts_with("provenant worktree doctor "));
    assert_eq!(fs::read_to_string(&owner_path)?, "malformed owner record");
    fs::remove_file(owner_path)?;

    // Custody uncertainty takes priority over an incomplete advisory census.
    let slot = writer_directory(context.project_state, &path)?;
    let scope = provenant_supervisor::custody::Scope {
        run: provenant_core::RunId::from_bytes([51; 16]),
        attempt: AttemptId::from_bytes([52; 16]),
        generation: 1,
    };
    let lease = provenant_supervisor::fixture::acquire_writer(
        slot.try_clone()?,
        scope,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &cancel,
        context.clock,
    )?;
    let retained = slot.current_path()?.join(scope.attempt.to_string());
    fs::write(retained.join("import-retained.json"), "retained")?;
    let reservation = fs::read(retained.join("reservation.json"))?;
    drop(lease);
    let custody = doctor(&context, &path, &cancel)?;
    assert_eq!(custody.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(custody.source, Some("custody"));
    assert_eq!(
        serde_json::to_value(&custody.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert!(
        custody
            .evidence
            .as_deref()
            .is_some_and(|evidence| evidence.contains("retained import fence"))
    );
    assert!(custody.repair.starts_with("provenant worktree doctor "));
    assert_eq!(fs::read(retained.join("reservation.json"))?, reservation);
    assert_eq!(
        fs::read(retained.join("import-retained.json"))?,
        b"retained"
    );
    Ok(())
}
