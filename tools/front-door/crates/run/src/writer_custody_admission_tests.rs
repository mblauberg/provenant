use super::*;
use crate::tests::{call, clock, context};
use provenant_core::AttemptId;
use provenant_records::custody::Scope;
use provenant_supervisor::custody::{self};

#[test]
fn foreign_writer_recovery_is_scoped_but_admission_keeps_predecessor_fences()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = crate::RunStore::open(&context, &call)?;
    let source_project = ProjectId::from_bytes([99; 16]);
    let root = context.state_root.as_path();
    let refs = writer_projects_path(root)
        .join(context.project_id.to_string())
        .join(source_project.to_string());
    std::fs::create_dir_all(&refs)?;
    let slot = root
        .join("worktrees")
        .join(source_project.to_string())
        .join("shared");
    std::fs::create_dir_all(&slot)?;
    let predecessor = Scope {
        attempt: AttemptId::from_bytes([82; 16]),
        generation: 1,
    };
    let lease = custody::acquire_writer(
        Directory::open_anchor(&slot)?,
        predecessor,
        WriterBinding::read_only(),
        call.lock_budget(),
        call.cancel,
        call.clock,
    )?;
    let evidence = Directory::open_anchor(&slot.join(predecessor.attempt.to_string()))?;
    crate::artifacts::seal(
        &evidence,
        "import-retained.json",
        &crate::db::bytes(&ImportFence {
            schema_version: 1,
            scope: predecessor,
            lane: provenant_records::WorkKey::new(source_project, "retained", "writer", 1)?,
            run_attempt: temp.path().join("retained-run"),
        })?,
        crate::RECORD_LIMIT,
    )?;
    drop(lease);
    assert!(store.writer_fence_listing()?.reservations.is_empty());
    let all = store.writer_fence_listing_for_admission(true)?;
    assert!(
        all.reservations
            .iter()
            .any(|fence| fence.scope == predecessor)
    );
    // An unrelated, malformed project reference must never be scanned
    // while admitting this worktree. Its own retained import still fences it.
    std::fs::create_dir(
        writer_projects_path(root)
            .join(context.project_id.to_string())
            .join("unrelated-invalid-project"),
    )?;
    let next = Scope {
        attempt: AttemptId::from_bytes([84; 16]),
        generation: 1,
    };
    let refused = store.acquire_writer_fenced(
        Directory::open_anchor(&slot)?,
        next,
        WriterBinding::read_only(),
        &|directory, scope, binding| {
            custody::acquire_writer(
                directory,
                scope,
                binding,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )
        },
        &call,
    );
    assert!(
        matches!(refused, Err(WriterCustodyError::Admission(custody::CustodyError::ImportRetained {fence})) if fence.scope == predecessor)
    );
    assert!(custody::reservation_present(
        &Directory::open_anchor(&slot)?,
        predecessor
    )?);
    assert!(!custody::reservation_present(
        &Directory::open_anchor(&slot)?,
        next
    )?);
    let writer = WriterRequest {
        extra: Default::default(),
        custody_project: Some(source_project),
        project: temp.path().join("source"),
        worktree: temp.path().join("checkout"),
    };
    assert_eq!(
        store.writer_slot(&writer)?,
        root.join("worktrees")
            .join(source_project.to_string())
            .join(worktrees::state_dir_name(&writer.worktree))
    );
    Ok(())
}

#[test]
fn failed_admission_withdrawal_retains_a_protected_error()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let store = crate::RunStore::open(&context(temp.path())?, &call)?;
    let path = temp.path().join("slot");
    std::fs::create_dir(&path)?;
    let scope = Scope {
        attempt: AttemptId::from_bytes([4; 16]),
        generation: 1,
    };
    let result = store.acquire_writer_fenced(
        Directory::open_anchor(&path)?,
        scope,
        WriterBinding::read_only(),
        &|directory, scope, binding| {
            let lease = custody::acquire_writer(
                directory,
                scope,
                binding,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )?;
            std::fs::create_dir(path.join(scope.attempt.to_string()).join("settlement.json"))?;
            cancel.cancel();
            Ok(lease)
        },
        &call,
    );
    assert!(matches!(result, Err(WriterCustodyError::Withdrawal { .. })));
    assert!(custody::reservation_present(
        &Directory::open_anchor(&path)?,
        scope
    )?);
    Ok(())
}
