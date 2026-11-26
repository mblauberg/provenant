use super::*;

/// The new per-attempt scratch root for a fixture's `attempts/<id>` path.
fn scratch_root_of(attempt: &Path) -> PathBuf {
    let id = AttemptId::parse(attempt.file_name().unwrap().to_str().unwrap()).unwrap();
    crate::plan::scratch_root(attempt.parent().unwrap().parent().unwrap(), id)
}

use crate::tests::{call, clock, context, input};
use provenant_core::{CancelToken, WallTime};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn settlement_prunes_codex_caches_and_keeps_resume_state() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("attempts/{}", input.attempt_id));
    let candidate = attempt.join("provider-state/candidate-0");
    fs::create_dir_all(candidate.join("sessions"))?;
    fs::set_permissions(
        attempt.join("provider-state"),
        fs::Permissions::from_mode(0o700),
    )?;
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
    fs::write(candidate.join("provider.txt"), "codex")?;
    for name in ["cache", "plugins", "skills"] {
        fs::create_dir(candidate.join(name))?;
        fs::set_permissions(candidate.join(name), fs::Permissions::from_mode(0o755))?;
        fs::write(candidate.join(name).join("payload.lock"), "cache")?;
    }
    fs::write(candidate.join("models_cache.json"), "cache")?;
    fs::write(candidate.join("sessions/session.jsonl"), "transcript")?;
    fs::write(candidate.join("thread_history_1.sqlite"), "history")?;
    fs::write(candidate.join("auth.json"), "credential")?;
    store.withdraw(
        input.attempt_id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture",
        WallTime::from_micros(200)?,
        &call,
    )?;
    for name in ["cache", "plugins", "skills", "models_cache.json"] {
        assert!(!candidate.join(name).exists(), "{name} survived settlement");
    }
    for name in [
        "sessions/session.jsonl",
        "thread_history_1.sqlite",
        "auth.json",
    ] {
        assert!(candidate.join(name).exists(), "{name} was removed");
    }
    Ok(())
}
type Seeded = (
    tempfile::TempDir,
    ExecutionContext,
    RunStore,
    PreparedRun,
    std::path::PathBuf,
);

fn seeded() -> std::result::Result<Seeded, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("attempts/{}", input.attempt_id));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(scratch_root_of(&attempt))?;
    for path in [
        "provider-state/candidate-0/sessions",
        "provider-state/candidate-0/claude-config/projects",
        "provider-state/candidate-1",
    ] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(attempt.join(path))?;
    }
    let candidate = attempt.join("provider-state/candidate-0");
    for (name, content) in [
        ("provider.txt", "codex"),
        ("sessions/session.jsonl", "transcript"),
        ("thread_history_1.sqlite", "history"),
        ("thread_history_1.sqlite-wal", "wal"),
        ("state_5.sqlite", "state history"),
        ("state_5.sqlite-shm", "shm"),
        ("claude-config/projects/session.jsonl", "claude transcript"),
        ("auth.json", "credential"),
        ("claude-config/.credentials.json", "credential"),
        ("other", "disposable"),
    ] {
        fs::write(candidate.join(name), content)?;
    }
    fs::write(
        attempt.join("provider-state/candidate-1/provider.txt"),
        "agy",
    )?;
    fs::write(
        attempt.join("provider-state/candidate-1/unknown-session"),
        "unknown transcript",
    )?;
    fs::write(
        attempt.join("provider-state/candidate-1/auth.json"),
        "credential",
    )?;
    Ok((temp, context, store, input, attempt))
}

fn withdraw(
    store: &RunStore,
    input: &PreparedRun,
    call: &Request<'_>,
) -> crate::Result<AttemptRecord> {
    store
        .withdraw(
            input.attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(200).map_err(|_| RunError::Validation("fixture time"))?,
            call,
        )?
        .ok_or(RunError::Conflict)
}

fn sweep(
    store: &RunStore,
    context: &ExecutionContext,
    days: u64,
    age: i64,
    dry_run: bool,
    call: &Request<'_>,
) -> crate::Result<ProviderStateGc> {
    sweep_with_days(store, context, days, days, age, dry_run, call)
}

fn sweep_with_days(
    store: &RunStore,
    context: &ExecutionContext,
    provider_state_days: u64,
    scratch_days: u64,
    age: i64,
    dry_run: bool,
    call: &Request<'_>,
) -> crate::Result<ProviderStateGc> {
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let now = WallTime::from_micros(200 + age).map_err(|_| RunError::Validation("fixture time"))?;
    let mut report = store.gc_provider_state_with_logs(
        &root,
        provider_state_days,
        logs_retention_days(),
        now,
        dry_run,
        call,
        &RetentionScan::default(),
    )?;
    // These fixtures exercise age and collection mechanics for unowned work.
    let unowned = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Unowned;
    let policy = ScratchPolicy {
        owner: Some(&unowned),
        ..ScratchPolicy::new(scratch_days.saturating_mul(86_400))
    };
    let mut scratch = store.gc_temporary(
        &root,
        &policy,
        now,
        dry_run,
        call,
        &RetentionScan::default(),
    )?;
    report.removed.append(&mut scratch.removed);
    report.tightened.append(&mut scratch.tightened);
    report.attempt_tmp.append(&mut scratch.attempt_tmp);
    report.attempt_logs.append(&mut scratch.attempt_logs);
    report.expired_events += scratch.expired_events;
    report.kept.append(&mut scratch.kept);
    report.errors.append(&mut scratch.errors);
    Ok(report)
}

fn logs_retention_days() -> u64 {
    provenant_config::RetentionSettings::default().logs_days
}

#[test]
fn a_base_twelve_attempt_keeps_its_retired_policy_as_an_extra_and_drops_a_lane_cache_grant()
-> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let stored: Vec<u8> = store.db.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [input.attempt_id.to_string()],
        |row| row.get(0),
    )?;
    let rule = |boundary: &str| {
        serde_json::json!({
            "code": format!("LEGACY_{boundary}"),
            "boundary": boundary,
            "strength": "refuse",
            "knob": null,
            "justification": "pre-rename boundary",
            "site": "fixture",
            "threshold": null,
            "refusable": false,
            "source": "dispatch"
        })
    };
    let mut legacy: serde_json::Value = serde_json::from_slice(&stored)?;
    legacy["schema_version"] = 2.into();
    legacy["profile"] = serde_json::json!({
        "name": "rigorous",
        "knobs": {},
        "overrides": [],
        "launch_policy": {"rules": [rule("git_isolation"), rule("data_integrity")]}
    });
    legacy["write_grants"] = serde_json::json!([
        {
            "source": "lane_cache",
            "path": {"relative_to_root": {
                "encoding": "utf8",
                "path": "Library/Caches/provenant-lane-cargo/fixture"
            }},
            "scope": "subtree"
        },
        {
            "source": "scratch",
            "path": {"relative_to_root": {
                "encoding": "utf8",
                "path": "attempt-scratch"
            }},
            "scope": "subtree"
        }
    ]);
    // A terminal attempt is immutable to ordinary writers; the migration
    // lifts the trigger inside its own transaction.
    store
        .db
        .execute_batch("DROP TRIGGER immutable_terminal_update")?;
    store.db.execute(
        "UPDATE attempts SET record=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_vec(&legacy)?, input.attempt_id.to_string()],
    )?;
    store.db.execute_batch(
        "CREATE TRIGGER immutable_terminal_update
         BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
         WHEN OLD.terminal=1
         BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
         PRAGMA user_version=12;",
    )?;
    drop(store);

    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        provenant_sqlite::stored_version(&store.db)?,
        crate::db::SCHEMA.version
    );
    let record = store.record(input.attempt_id, &call)?;
    // The current type reads the retired snapshot as an extra field, so
    // the migration leaves it in place; the sealed request keeps the
    // authoritative copy.
    let policy = record
        .profile
        .extra
        .get("launch_policy")
        .ok_or("retained launch policy")?;
    assert_eq!(policy["rules"][0]["boundary"], "git_isolation");
    assert_eq!(policy["rules"][1]["boundary"], "data_integrity");
    assert_eq!(record.write_grants.len(), 1);
    assert!(matches!(
        record.write_grants[0].source,
        WriteSource::Scratch
    ));
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    Ok(())
}

fn seal_base_twelve_row(store: &RunStore, id: AttemptId, record: &serde_json::Value) -> TestResult {
    // A terminal attempt is immutable to ordinary writers; the migration
    // lifts the trigger inside its own transaction.
    store
        .db
        .execute_batch("DROP TRIGGER immutable_terminal_update")?;
    store.db.execute(
        "UPDATE attempts SET record=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_vec(record)?, id.to_string()],
    )?;
    store.db.execute_batch(
        "CREATE TRIGGER immutable_terminal_update
         BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
         WHEN OLD.terminal=1
         BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
         PRAGMA user_version=12;",
    )?;
    Ok(())
}

fn insert_base_twelve_record(
    store: &RunStore,
    source: AttemptId,
    id: AttemptId,
    offset: i64,
    record: &serde_json::Value,
) -> TestResult {
    // Each inserted row needs its own attempt key: (project, graph, node,
    // attempt) is unique.
    store.db.execute(
        "INSERT INTO attempts (id,project,graph,node,attempt,digest,actor,operation,number,operation_record,revision,terminal,record) SELECT ?1,project,graph,node,attempt+?4,digest,actor,operation,number+?4,operation_record,revision,1,?2 FROM attempts WHERE id=?3",
        rusqlite::params![id.to_string(), serde_json::to_vec(record)?, source.to_string(), offset],
    )?;
    Ok(())
}

fn base_twelve_record(
    store: &RunStore,
    id: AttemptId,
) -> std::result::Result<serde_json::Value, Box<dyn std::error::Error>> {
    let stored: Vec<u8> = store.db.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_slice(&stored)?)
}

fn stored_record_after_migration(
    context: &ExecutionContext,
    call: &Request<'_>,
    id: AttemptId,
) -> std::result::Result<serde_json::Value, Box<dyn std::error::Error>> {
    let store = RunStore::open(context, call)?;
    assert_eq!(
        provenant_sqlite::stored_version(&store.db)?,
        crate::db::SCHEMA.version
    );
    let stored: Vec<u8> = store.db.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_slice(&stored)?)
}

#[test]
fn base_twelve_migration_does_not_add_an_absent_write_grant_list() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let mut legacy = base_twelve_record(&store, input.attempt_id)?;
    legacy["schema_version"] = 2.into();
    legacy["profile"] = serde_json::json!({
        "name": "rigorous",
        "knobs": {},
        "overrides": [],
        "launch_policy": {"rules": []}
    });
    legacy
        .as_object_mut()
        .ok_or("attempt record")?
        .remove("write_grants");
    legacy["future_attempt_field"] = serde_json::json!({"kept":true});
    seal_base_twelve_row(&store, input.attempt_id, &legacy)?;
    drop(store);

    let migrated = stored_record_after_migration(&context, &call, input.attempt_id)?;
    assert!(migrated.get("write_grants").is_none());
    // The retired snapshot decodes as an extra field, so the rewrite leaves
    // it byte-identical rather than treating it as unreadable.
    assert_eq!(
        migrated["profile"]["launch_policy"],
        serde_json::json!({"rules": []})
    );
    assert_eq!(
        migrated["future_attempt_field"],
        serde_json::json!({"kept":true})
    );
    Ok(())
}

#[test]
fn base_twelve_migration_keeps_grants_it_did_not_retire() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let mut legacy = base_twelve_record(&store, input.attempt_id)?;
    legacy["schema_version"] = 2.into();
    legacy["profile"] = serde_json::json!({
        "name": "rigorous",
        "knobs": {},
        "overrides": [],
        "launch_policy": {"rules": []}
    });
    legacy["write_grants"] = serde_json::json!([
        {
            "path": {"relative_to_root": {
                "encoding": "utf8",
                "path": "attempt-scratch"
            }},
            "scope": "subtree"
        },
        {
            "source": "scratch",
            "path": {"relative_to_root": {
                "encoding": "utf8",
                "path": "attempt-scratch"
            }},
            "scope": "subtree"
        },
        {
            "source": "lane_cache",
            "path": {"relative_to_root": {
                "encoding": "utf8",
                "path": "Library/Caches/provenant-lane-cargo/fixture"
            }},
            "scope": "subtree"
        }
    ]);
    seal_base_twelve_row(&store, input.attempt_id, &legacy)?;
    drop(store);

    let migrated = stored_record_after_migration(&context, &call, input.attempt_id)?;
    let grants = migrated["write_grants"].as_array().ok_or("write grants")?;
    assert_eq!(grants.len(), 2);
    assert!(grants.iter().all(|grant| grant["source"] != "lane_cache"));
    assert!(grants.iter().any(|grant| grant.get("source").is_none()));
    assert!(grants.iter().any(|grant| grant["source"] == "scratch"));
    Ok(())
}

#[test]
fn malformed_attempt_record_does_not_abort_the_base_twelve_migration() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let broken = AttemptId::from_bytes([9; 16]);
    store.db.execute(
        "INSERT INTO attempts (id,project,graph,node,attempt,digest,actor,operation,number,operation_record,revision,terminal,record) SELECT ?1,project,graph,node,attempt+1000,digest,actor,operation,number+1,operation_record,revision,1,?2 FROM attempts WHERE id=?3",
        rusqlite::params![broken.to_string(), b"{malformed JSON".as_slice(), input.attempt_id.to_string()],
    )?;
    // Valid JSON with the wrong shape must also skip the grant walk instead
    // of aborting the migration.
    let mut shaped = base_twelve_record(&store, input.attempt_id)?;
    shaped["schema_version"] = 2.into();
    let string_grants = AttemptId::from_bytes([10; 16]);
    let mut string_record = shaped.clone();
    string_record["write_grants"] = serde_json::json!("abc");
    insert_base_twelve_record(
        &store,
        input.attempt_id,
        string_grants,
        1001,
        &string_record,
    )?;
    let array_grants = AttemptId::from_bytes([11; 16]);
    let mut array_record = shaped;
    array_record["write_grants"] = serde_json::json!(["abc"]);
    insert_base_twelve_record(&store, input.attempt_id, array_grants, 1002, &array_record)?;
    store.db.execute_batch("PRAGMA user_version=12;")?;
    drop(store);

    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        provenant_sqlite::stored_version(&store.db)?,
        crate::db::SCHEMA.version
    );
    assert!(store.record(input.attempt_id, &call).is_ok());
    assert!(store.record(broken, &call).is_err());
    assert!(store.record(string_grants, &call).is_err());
    assert!(store.record(array_grants, &call).is_err());
    Ok(())
}

#[test]
fn base_fourteen_migration_preserves_local_delivery_receipts() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    crate::tests::seal_exit_fixture(&store, &input)?;
    store.publish_terminal(
        input.attempt_id,
        1,
        &crate::tests::publication(),
        &crate::tests::exit(),
        &call,
    )?;
    let mut legacy = base_twelve_record(&store, input.attempt_id)?;
    legacy["delivery"] = serde_json::json!({
        "schema_version": 1, "remote": "", "branch": "main",
        "head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "state": {"state": "delivered", "pull_request": null},
        "integration": {"remote": "", "target": "refs/heads/main",
            "previous": null, "integrated": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
    });
    let mut remote = legacy.clone();
    remote["delivery"]["remote"] = "team/upstream".into();
    remote["delivery"]["integration"]["remote"] = "team/upstream".into();
    let remote_id = AttemptId::from_bytes([21; 16]);
    insert_base_twelve_record(&store, input.attempt_id, remote_id, 1000, &remote)?;
    let remote_bytes: Vec<u8> = store.db.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [remote_id.to_string()],
        |row| row.get(0),
    )?;
    let broken_id = AttemptId::from_bytes([22; 16]);
    store.db.execute(
        "INSERT INTO attempts (id,project,graph,node,attempt,digest,actor,operation,number,operation_record,revision,terminal,record) SELECT ?1,project,graph,node,attempt+1001,digest,actor,operation,number+1001,operation_record,revision,1,?2 FROM attempts WHERE id=?3",
        rusqlite::params![broken_id.to_string(), b"{broken".as_slice(), input.attempt_id.to_string()],
    )?;
    store
        .db
        .execute_batch("DROP TRIGGER immutable_terminal_update")?;
    store.db.execute(
        "UPDATE attempts SET record=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_vec(&legacy)?, input.attempt_id.to_string()],
    )?;
    store.db.execute_batch(
        "CREATE TRIGGER immutable_terminal_update
         BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
         WHEN OLD.terminal=1 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
         PRAGMA user_version=14;",
    )?;
    drop(store);

    let store = RunStore::open(&context, &call)?;
    let receipt = store
        .record(input.attempt_id, &call)?
        .delivery
        .ok_or("receipt")?;
    assert!(receipt.remote.is_local());
    assert!(receipt.integration.ok_or("integration")?.remote.is_local());
    let retained: Vec<u8> = store.db.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [remote_id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(retained, remote_bytes);
    assert!(store.record(broken_id, &call).is_err());
    assert!(
        store
            .db
            .execute(
                "UPDATE attempts SET record=record WHERE id=?1",
                [input.attempt_id.to_string()],
            )
            .is_err()
    );
    assert_eq!(provenant_sqlite::stored_version(&store.db)?, 16);
    drop(store);
    let reopened = RunStore::open(&context, &call)?;
    assert_eq!(base_twelve_record(&reopened, remote_id)?, remote);
    assert!(
        reopened
            .record(input.attempt_id, &call)?
            .delivery
            .ok_or("receipt")?
            .remote
            .is_local()
    );
    Ok(())
}

fn seal_base_thirteen_row(
    store: &RunStore,
    id: AttemptId,
    record: &serde_json::Value,
) -> TestResult {
    // A terminal attempt is immutable to ordinary writers; the migration
    // lifts the trigger inside its own transaction.
    store
        .db
        .execute_batch("DROP TRIGGER IF EXISTS immutable_terminal_update")?;
    store.db.execute(
        "UPDATE attempts SET record=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_vec(record)?, id.to_string()],
    )?;
    store.db.execute_batch(
        "CREATE TRIGGER immutable_terminal_update
         BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
         WHEN OLD.terminal=1
         BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
         PRAGMA user_version=13;",
    )?;
    Ok(())
}

/// Build a real current route receipt, then return its v2.8.2 wire shape.
fn legacy_route(
    source: &str,
    sandbox_source: &str,
) -> std::result::Result<serde_json::Value, Box<dyn std::error::Error>> {
    let lane = LaneAccessReceipt {
        access: LaneAccess::Isolated,
        source: Layer::Route,
        key: "fixture".into(),
        extra: Default::default(),
    };
    let sandbox = SandboxModeReceipt {
        requested: true,
        effective: true,
        source: Layer::Host,
        constraint: Some(SandboxConstraint::Restriction),
        warning: None,
        extra: Default::default(),
    };
    let mut route = serde_json::to_value(RouteRecord {
        lane_access: Some(lane),
        sandbox: Some(sandbox),
        ..Default::default()
    })?;
    route["lane_access"]["source"] = source.into();
    route["sandbox"]["source"] = sandbox_source.into();
    route["sandbox"]
        .as_object_mut()
        .ok_or("sandbox object")?
        .remove("constraint");
    Ok(route)
}

#[test]
fn base_thirteen_migration_rewrites_v282_usage_and_receipts() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let mut legacy = base_twelve_record(&store, input.attempt_id)?;

    // A real v2.8.2 attempt stored flat token buckets and a free-form
    // source, including on a retained session total.
    legacy["usage"] = serde_json::json!({
        "uncached_input": 11,
        "cache_read": 22,
        "cache_write": 33,
        "cache_write_1h": 4,
        "output_excluding_reasoning": 55,
        "reasoning": 6,
        "source": "provider",
        "context_basis_points": null,
        "session_total": {
            "uncached_input": 111,
            "output_excluding_reasoning": 222,
            "source": "provider"
        }
    });
    legacy["resolved_route"] = legacy_route("route_policy", "restriction")?;
    legacy["applied_route"] = legacy_route("route_policy", "platform")?;
    legacy["observed_route"] = legacy_route("route_policy", "knob")?;
    legacy["skipped_routes"] = serde_json::json!([
        {"route":"a","kind":"unavailable","detail":"fixture",
         "guard_resource":{"kind":"disk","free":1,"floor":2,"origin":"configuration"}},
        {"route":"b","kind":"unavailable","detail":"fixture",
         "guard_resource":{"kind":"grant_limit","limit":7,"origin":"shipped_default"}},
        // These two guards use their own setting-source enums, which still
        // spell these origins `configuration` / `shipped_default`. The
        // Layer-typed rewrite must leave them alone or this terminal row
        // would stop decoding after migration.
        {"route":"c","kind":"unavailable","detail":"fixture",
         "guard_resource":{"kind":"pacing_reserve","percent":25,"setting":"account",
                           "origin":"configuration","account":"acct",
                           "window":"window","setting_path":"path"}},
        {"route":"d","kind":"unavailable","detail":"fixture",
         "guard_resource":{"kind":"lane_ports","blocks":4,"origin":"shipped_default"}},
        {"route":"e","kind":"unavailable","detail":"fixture"}
    ]);
    let command = provenant_records::PortCommand {
        argv: vec!["probe".into()],
        exit_codes: provenant_records::CommandExitCodes {
            success: vec![0],
            unknown: vec![],
            other: provenant_records::ExitDisposition::Refused,
        },
    };
    let mut binding =
        serde_json::to_value(RepositoryAdmissionBinding::new(Layer::Default, &command))?;
    binding["source"] = "builtin".into();
    legacy["repository_admission_binding"] = binding;
    legacy["future_attempt_field"] = serde_json::json!({"kept": true});
    seal_base_thirteen_row(&store, input.attempt_id, &legacy)?;
    drop(store);

    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        provenant_sqlite::stored_version(&store.db)?,
        crate::db::SCHEMA.version
    );
    // Without the migration this decode fails on the unknown source and
    // retired receipt spellings, breaking status, resume and settlement.
    let record = store.record(input.attempt_id, &call)?;
    assert_eq!(record.usage.source, provenant_records::UsageSource::Stream);
    assert_eq!(record.usage.tokens.input, Some(11));
    assert_eq!(record.usage.tokens.cache_read, Some(22));
    assert_eq!(record.usage.tokens.cache_write, Some(33));
    assert_eq!(record.usage.tokens.cache_write_1h, Some(4));
    assert_eq!(record.usage.tokens.output, Some(55));
    assert_eq!(record.usage.tokens.reasoning, Some(6));
    let total = record
        .usage
        .session_total
        .as_deref()
        .ok_or("session total")?;
    assert_eq!(total.tokens.input, Some(111));
    assert_eq!(total.tokens.output, Some(222));
    assert_eq!(total.source, provenant_records::UsageSource::Stream);

    let resolved = record.resolved_route.as_ref().ok_or("resolved route")?;
    assert_eq!(
        resolved.lane_access.as_ref().map(|lane| lane.source),
        Some(Layer::Route)
    );
    let sandbox = resolved.sandbox.as_ref().ok_or("resolved sandbox")?;
    assert_eq!(sandbox.source, Layer::Host);
    assert_eq!(sandbox.constraint, Some(SandboxConstraint::Restriction));
    let applied = record.applied_route.as_ref().ok_or("applied route")?;
    let sandbox = applied.sandbox.as_ref().ok_or("applied sandbox")?;
    assert_eq!(sandbox.source, Layer::Host);
    assert_eq!(sandbox.constraint, Some(SandboxConstraint::Platform));
    let observed = record.observed_route.as_ref().ok_or("observed route")?;
    let sandbox = observed.sandbox.as_ref().ok_or("observed sandbox")?;
    assert_eq!(sandbox.source, Layer::Host);
    assert_eq!(sandbox.constraint, None);
    let guards: Vec<_> = record
        .skipped_routes
        .iter()
        .filter_map(|route| route.guard_resource.as_ref())
        .collect();
    assert_eq!(guards.len(), 4, "every guard must still decode: {guards:?}");
    assert!(matches!(
        guards[0],
        provenant_records::diagnostic::ResourceFact::Disk {
            origin: Layer::Host,
            ..
        }
    ));
    assert!(matches!(
        guards[1],
        provenant_records::diagnostic::ResourceFact::GrantLimit {
            origin: Layer::Default,
            ..
        }
    ));
    // These origins belong to PacingSettingSource and PortSettingSource,
    // whose current spellings are still configuration / shipped_default.
    // Rewriting them would make this terminal row undecodable.
    assert!(matches!(
        guards[2],
        provenant_records::diagnostic::ResourceFact::PacingReserve {
            origin: provenant_records::diagnostic::PacingSettingSource::Configuration,
            ..
        }
    ));
    assert!(matches!(
        guards[3],
        provenant_records::diagnostic::ResourceFact::LanePorts {
            origin: provenant_records::diagnostic::PortSettingSource::ShippedDefault,
            ..
        }
    ));
    let binding = record
        .repository_admission_binding
        .as_ref()
        .ok_or("admission binding")?;
    assert_eq!(binding.source, Layer::Default);
    assert_eq!(
        record.extra.get("future_attempt_field"),
        Some(&serde_json::json!({"kept": true}))
    );
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    Ok(())
}

#[test]
fn base_thirteen_migration_leaves_current_rows_and_skips_malformed_rows() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let raw: Vec<u8> = store.db.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [input.attempt_id.to_string()],
        |row| row.get(0),
    )?;
    let broken = AttemptId::from_bytes([12; 16]);
    insert_base_twelve_record(
        &store,
        input.attempt_id,
        broken,
        1003,
        &serde_json::Value::String("{malformed JSON".into()),
    )?;
    store.db.execute_batch("PRAGMA user_version=13;")?;
    drop(store);

    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        provenant_sqlite::stored_version(&store.db)?,
        crate::db::SCHEMA.version
    );
    let migrated: Vec<u8> = store.db.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [input.attempt_id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(migrated, raw, "a current-shape row is left byte-identical");
    assert!(store.record(input.attempt_id, &call).is_ok());
    assert!(store.record(broken, &call).is_err());
    Ok(())
}

#[test]
fn an_undecodable_attempt_names_the_attempt_row_not_provider_state() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    // A terminal row is immutable, so insert a second terminal row whose
    // record cannot decode. It keeps the retention row shape GC reads.
    let broken = AttemptId::from_bytes([9; 16]);
    store.db.execute(
        "INSERT INTO attempts (id,project,graph,node,attempt,digest,actor,operation,number,operation_record,revision,terminal,record) SELECT ?1,project,graph,node,attempt+1000,digest,actor,operation,number+1,operation_record,revision,1,?2 FROM attempts WHERE id=?3",
        rusqlite::params![broken.to_string(), b"{}".as_slice(), input.attempt_id.to_string()],
    )?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("attempts/{broken}"));
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(!report.errors.is_empty(), "{report:?}");
    assert!(
        report.errors.iter().all(|issue| issue.path == attempt),
        "{report:?}"
    );
    assert!(
        report
            .errors
            .iter()
            .all(|issue| !issue.path.ends_with("provider-state")),
        "{report:?}"
    );
    Ok(())
}

fn logs_retention_age() -> i64 {
    DAY_MICROS.saturating_mul(logs_retention_days() as i64)
}

#[test]
fn gc_repairs_open_claude_config_and_expires_state() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let config = attempt.join("provider-state/candidate-0/claude-config");
    let other = attempt.join("provider-state/candidate-1/claude-config");
    fs::create_dir(&other)?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o755))?;
    fs::set_permissions(&other, fs::Permissions::from_mode(0o707))?;

    let preview = sweep(&store, &context, 7, DAY_MICROS, true, &call)?;
    assert!(preview.errors.is_empty(), "{:?}", preview.errors);
    assert!(preview.tightened.contains(&config));
    assert!(preview.tightened.contains(&other));
    assert_eq!(fs::metadata(&config)?.permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(&other)?.permissions().mode() & 0o777, 0o707);
    assert!(config.join(".credentials.json").exists());
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.tightened.contains(&config));
    assert!(report.tightened.contains(&other));
    assert_eq!(fs::metadata(&config)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(&other)?.permissions().mode() & 0o777, 0o700);
    assert!(!config.join(".credentials.json").exists());
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!attempt.join("provider-state").exists());
    Ok(())
}

#[test]
fn gc_continues_after_unsafe_credential_path() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let candidate = attempt.join("provider-state/candidate-0");
    let config = candidate.join("claude-config");
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join(".credentials.json"), "outside")?;
    fs::remove_dir_all(&config)?;
    std::os::unix::fs::symlink(outside.path(), &config)?;
    fs::create_dir(candidate.join("cache"))?;
    fs::write(candidate.join("cache/item"), "cache")?;

    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(!report.errors.is_empty());
    assert!(
        !attempt
            .join("provider-state/candidate-1/auth.json")
            .exists()
    );
    assert!(!candidate.join("cache").exists());
    assert_eq!(
        fs::read_to_string(outside.path().join(".credentials.json"))?,
        "outside"
    );
    Ok(())
}

/// Settlement seals its own result; the fixture adds exit evidence after it.
fn add_log_evidence(attempt: &std::path::Path) -> std::io::Result<()> {
    fs::write(attempt.join("process-exit.json"), "exit evidence")
}

fn logs_fixture() -> std::result::Result<(Seeded, Vec<u8>), Box<dyn std::error::Error>> {
    let seeded = seeded()?;
    let attempt = &seeded.4;
    let tail_bytes = usize::try_from(LOG_TAIL)?;
    let mut capture = vec![1; tail_bytes * 3];
    let tail_start = capture.len() - tail_bytes;
    capture[tail_start..].fill(2);
    fs::write(attempt.join("stdout.log"), &capture)?;
    fs::write(attempt.join("stderr.log"), "small capture")?;
    Ok((seeded, capture))
}

#[test]
fn gc_logs_follow_declared_age_and_preview_preserves_source() -> TestResult {
    let ((_temp, context, store, input, attempt), capture) = logs_fixture()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    add_log_evidence(&attempt)?;
    let result = fs::read(attempt.join("result.md"))?;
    let logs_age = logs_retention_age();
    assert!(
        sweep(&store, &context, 7, logs_age - 1, false, &call)?
            .attempt_logs
            .is_empty()
    );
    let preview = sweep(&store, &context, 7, logs_age, true, &call)?;
    let removal = preview
        .attempt_logs
        .iter()
        .find(|entry| entry.path.ends_with("stdout.log"))
        .ok_or("stdout log removal preview")?;
    assert_eq!(removal.bytes, capture.len() as u64 - LOG_TAIL);
    assert_eq!(fs::read(attempt.join("stdout.log"))?, capture);
    assert!(!attempt.join("stdout.log.tail").exists());
    let applied = sweep(&store, &context, 7, logs_age, false, &call)?;
    assert!(
        applied
            .attempt_logs
            .iter()
            .any(|entry| entry.path.ends_with("stdout.log"))
    );
    assert!(!attempt.join("stdout.log").exists());
    assert_eq!(
        fs::read(attempt.join("stdout.log.tail"))?,
        vec![2; LOG_TAIL as usize]
    );
    assert_eq!(fs::read(attempt.join("result.md"))?, result);
    assert_eq!(
        fs::read_to_string(attempt.join("process-exit.json"))?,
        "exit evidence"
    );
    Ok(())
}

#[test]
fn missing_release_receipt_is_reported_before_incomplete_settlement() -> TestResult {
    let (_temp, _context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut record = withdraw(&store, &input, &call)?;
    record.provider_launched = Some(true);
    let error = match store.hold_attempt(&record, &call) {
        Err(error) => error,
        Ok(_) => return Err("missing release receipt was not reported".into()),
    };
    assert!(
        error
            .to_string()
            .contains("attempt release receipt unavailable")
    );
    Ok(())
}

#[test]
fn gc_logs_wait_for_custody_release_and_keep_other_capture() -> TestResult {
    let ((_temp, context, store, input, attempt), _) = logs_fixture()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let result = fs::read(attempt.join("result.md"))?;
    let directory = Directory::open_anchor(&attempt)?;
    let lease = directory.lock(
        OsStr::new("writer.lock"),
        LockMode::Exclusive,
        call.lock_budget(),
        call.cancel,
        call.clock,
    )?;
    let logs_age = logs_retention_age();
    assert!(
        sweep(&store, &context, 7, logs_age, false, &call)?
            .attempt_logs
            .is_empty()
    );
    assert!(attempt.join("stdout.log").exists());
    drop(lease);
    sweep(&store, &context, 7, logs_age, false, &call)?;
    assert!(!attempt.join("stdout.log").exists());
    assert_eq!(
        fs::read_to_string(attempt.join("stderr.log"))?,
        "small capture"
    );
    assert_eq!(fs::read(attempt.join("result.md"))?, result);
    Ok(())
}

#[test]
fn gc_stderr_tail_remains_readable_and_reports_truncation() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let directory = Directory::open_anchor(&attempt)?;
    let capture = vec![b'x'; usize::try_from(LOG_TAIL)? * 3];
    crate::artifacts::seal(&directory, "stderr.log", &capture, LOG_TAIL * 4)?;
    crate::artifacts::seal(&directory, "stderr.log.tail", b"older tail", LOG_TAIL)?;
    let read = || {
        store.read_output(
            input.attempt_id,
            crate::ArtifactPart::Stderr,
            crate::ByteRange {
                offset: 0,
                max_bytes: LOG_TAIL,
            },
            &context,
            &call,
        )
    };
    let original = read()?;
    assert!(original.complete);
    assert_eq!(original.len, capture.len() as u64);
    assert_eq!(original.bytes, vec![b'x'; LOG_TAIL as usize]);
    let report = sweep(&store, &context, 7, logs_retention_age() + 1, false, &call)?;
    assert_eq!(report.attempt_logs.len(), 1, "{report:?}");
    let tail = read()?;
    assert!(!tail.complete);
    assert!(tail.eof);
    assert_eq!(tail.len, LOG_TAIL);
    assert_eq!(tail.bytes, vec![b'x'; LOG_TAIL as usize]);
    assert_eq!(
        tail.digest,
        provenant_records::CanonicalV1::file_digest(&tail.bytes)
    );
    Ok(())
}

#[test]
fn gc_expires_closed_run_events_after_log_retention() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let id = input.attempt_id;
    withdraw(&store, &input, &call)?;
    let logs_age = logs_retention_age();

    let events_before = store.events(0, 64, Some(id), &call)?;
    assert!(
        events_before.len() > 1,
        "seeded terminal attempt must record multiple events"
    );

    let young = sweep(&store, &context, 7, logs_age - 1, false, &call)?;
    assert_eq!(young.expired_events, 0);
    assert_eq!(
        store.events(0, 64, Some(id), &call)?.len(),
        events_before.len()
    );

    let preview = sweep(&store, &context, 7, logs_age, true, &call)?;
    assert_eq!(preview.expired_events, (events_before.len() - 1) as u64);
    assert_eq!(
        store.events(0, 64, Some(id), &call)?.len(),
        events_before.len()
    );

    // Applied sweep expires intermediate events, retaining the newest row.
    let applied = sweep(&store, &context, 7, logs_age, false, &call)?;
    assert_eq!(applied.expired_events, preview.expired_events);
    let retained = store.events(0, 64, Some(id), &call)?;
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].sequence,
        events_before.last().ok_or("last event")?.sequence
    );
    for dry_run in [true, false] {
        assert_eq!(
            sweep(&store, &context, 7, logs_age + 1, dry_run, &call)?.expired_events,
            0
        );
    }
    Ok(())
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn event_retention_stops_between_committed_batches_and_keeps_newest() -> TestResult {
    use provenant_core::{Clock, DurationMicros};
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel, observer_cancel) = (clock(), CancelToken::new(), CancelToken::new());
    let operation = call(&clock, &cancel);
    let observer = call(&clock, &observer_cancel);
    withdraw(&store, &input, &operation)?;
    let record = store.record(input.attempt_id, &operation)?;
    store.write(&operation, |db| {
        let mut record = record.clone();
        for _ in 0..256 {
            record.revision = record.revision.checked_add(1).ok_or(RunError::Conflict)?;
            crate::store::insert_event(db, &record)?;
        }
        Ok(())
    })?;
    let read_events = || -> Result<Vec<crate::RunEvent>> {
        let mut result = Vec::new();
        loop {
            let after = result
                .last()
                .map_or(0, |event: &crate::RunEvent| event.sequence);
            let page = store.events(after, 64, Some(input.attempt_id), &observer)?;
            if page.is_empty() {
                return Ok(result);
            }
            result.extend(page);
        }
    };
    let original = read_events()?;
    let newest = original.last().ok_or("latest event")?.sequence;
    let budget_clock = std::sync::Mutex::new(clock.clone());
    let deadline = clock
        .mono_now()
        .checked_add(DurationMicros::from_micros(1))?;
    let keep_going = || {
        let Ok(events) = read_events() else {
            return false;
        };
        let mut clock = budget_clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if events.len() < original.len() {
            let _ = clock.advance(DurationMicros::from_micros(1));
        }
        clock.mono_now() < deadline
    };
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let now = WallTime::from_micros(200 + 31 * DAY_MICROS)?;
    let partial = store.gc_provider_state_with_logs(
        &root,
        3650,
        30,
        now,
        false,
        &operation,
        &RetentionScan {
            keep_going: Some(&keep_going),
            ..Default::default()
        },
    )?;
    assert!(partial.interrupted_at.is_some());
    let remaining = read_events()?;
    assert!(remaining.len() > 1 && remaining.len() < original.len());
    let resumed = store.gc_provider_state_with_logs(
        &root,
        3650,
        30,
        now,
        false,
        &observer,
        &RetentionScan {
            start_at: Some(input.attempt_id),
            ..Default::default()
        },
    )?;
    assert!(resumed.interrupted_at.is_none());
    let retained = read_events()?;
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].sequence, newest);
    assert_eq!(
        partial.expired_events + resumed.expired_events,
        (original.len() - 1) as u64
    );
    Ok(())
}

#[test]
fn gc_keeps_expired_unacknowledged_settlements_visible() -> TestResult {
    let (_temp, context, store, input, _attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let id = input.attempt_id;
    let selection = "retention-unseen";
    // Save a cursor before settlement, then leave the settlement unacknowledged.
    store.ack_seen(selection, store.last_event_sequence(id, &call)?, &call)?;
    withdraw(&store, &input, &call)?;
    let newest = store.last_event_sequence(id, &call)?;
    let cursor = store.seen_cursor(selection, false, &call)?;
    assert!(newest > cursor);
    sweep(&store, &context, 7, 30 * DAY_MICROS, false, &call)?;
    let mut query = LaneQuery {
        targets: vec![],
        all: true,
        limit: 10,
        after: 0,
        newest_first: false,
        owner: Some(context.actor_id),
        unseen_after: Some(cursor),
    };
    // Exercise both the owned and explicit-target unseen filters.
    for targets in [vec![], vec![id]] {
        query.targets = targets;
        let page = store.lanes(&query, &call)?;
        assert_eq!(page.lanes.len(), 1, "expired settlement must remain unseen");
        assert_eq!(page.lanes[0].attempt_id, id);
        assert_eq!(store.last_event_sequence(id, &call)?, newest);
        store.ack_seen(selection, newest, &call)?;
        query.unseen_after = Some(store.seen_cursor(selection, false, &call)?);
        assert!(store.lanes(&query, &call)?.lanes.is_empty());
        query.unseen_after = Some(cursor);
    }
    Ok(())
}

#[test]
fn pressure_only_collects_temporary_and_keeps_captures_and_provider_state() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt).join("tmp"))?;
    fs::write(scratch_root_of(&attempt).join("tmp/payload"), vec![1; 8192])?;
    fs::write(attempt.join("stdout.log"), vec![1; 200_000])?;
    let other_project = [provenant_core::ProjectId::from_bytes([99; 16])];
    let mut scoped_policy = ScratchPolicy::new(7 * 86_400);
    scoped_policy.projects = Some(&other_project);
    let scoped = store.gc_temporary(
        &Directory::open_anchor(context.state_root.as_path())?,
        &scoped_policy,
        WallTime::from_micros(200 + 31 * DAY_MICROS)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(scoped.attempt_tmp.is_empty());
    assert!(scratch_root_of(&attempt).join("tmp/payload").is_file());
    let unowned = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Unowned;
    let policy = ScratchPolicy {
        owner: Some(&unowned),
        ..ScratchPolicy::new(7 * 86_400)
    };
    let report = store.gc_temporary(
        &Directory::open_anchor(context.state_root.as_path())?,
        &policy,
        WallTime::from_micros(200 + 31 * DAY_MICROS)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.attempt_tmp.len(), 1);
    assert!(report.attempt_logs.is_empty());
    assert!(
        attempt
            .join("provider-state/candidate-0/auth.json")
            .exists()
    );
    assert_eq!(fs::metadata(attempt.join("stdout.log"))?.len(), 200_000);
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert!(
        !scratch_root_of(&attempt).exists(),
        "an emptied per-attempt scratch root must not accumulate"
    );
    Ok(())
}

#[test]
fn over_cap_scratch_is_collected_before_idle_once_settled() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let scratch = scratch_root_of(&attempt).join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&scratch)?;
    fs::write(scratch.join("payload"), vec![1; 8192])?;
    let unowned = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Unowned;
    let root = Directory::open_anchor(context.state_root.as_path())?;
    // Three days into a seven-day idle window: age alone keeps the scratch.
    let now = WallTime::from_micros(200 + 3 * DAY_MICROS)?;
    let under_cap = ScratchPolicy {
        scratch_cap_bytes: 16_384,
        owner: Some(&unowned),
        ..ScratchPolicy::new(7 * 86_400)
    };
    let kept = store.gc_temporary(
        &root,
        &under_cap,
        now,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(kept.errors.is_empty(), "{kept:?}");
    assert!(
        kept.attempt_tmp.is_empty(),
        "under-cap scratch must wait out gc.idle"
    );
    assert!(scratch.join("payload").is_file());
    // The same settled attempt is over a smaller cap, so its scratch goes now.
    let over_cap = ScratchPolicy {
        scratch_cap_bytes: 4096,
        ..under_cap
    };
    let collected = store.gc_temporary(
        &root,
        &over_cap,
        now,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(collected.errors.is_empty(), "{collected:?}");
    assert_eq!(collected.attempt_tmp.len(), 1);
    assert!(
        !scratch.exists(),
        "over-cap scratch must be reclaimed once settled"
    );
    Ok(())
}

#[test]
fn collecting_tmp_prunes_only_empty_attempt_scratch_roots() -> TestResult {
    let (_temp, _context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let scratch_root = scratch_root_of(&attempt);
    let temporary = scratch_root.join("tmp");
    // The run store refuses non-private scratch, as production creates it.
    let private = |path: &std::path::Path| {
        std::os::unix::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700)
            .create(path)
    };
    private(&temporary.join("candidate-0"))?;
    fs::write(temporary.join("candidate-0/payload"), "disposable")?;
    let mut report = ProviderStateGc::default();
    let mut sizes = provenant_os::disk::DiskReport::with_walk_budget(
        temporary.clone(),
        0,
        100,
        std::time::Duration::from_secs(1),
    );
    collect_temporary(
        &Directory::open_anchor(&scratch_root)?,
        &temporary,
        false,
        &mut report,
        &mut sizes,
        &|| true,
    )?;
    assert_eq!(report.attempt_tmp.len(), 1);
    assert!(!temporary.exists());
    assert!(!scratch_root.exists(), "empty scratch root was not pruned");

    // A root still holding another entry belongs to its owner; keep it.
    private(&scratch_root)?;
    fs::write(scratch_root.join("sentinel"), "keep")?;
    private(&temporary)?;
    fs::write(temporary.join("payload"), "disposable")?;
    collect_temporary(
        &Directory::open_anchor(&scratch_root)?,
        &temporary,
        false,
        &mut report,
        &mut sizes,
        &|| true,
    )?;
    assert!(scratch_root.join("sentinel").is_file());
    assert!(
        scratch_root.exists(),
        "a non-empty scratch root must be kept"
    );
    Ok(())
}

#[test]
fn gc_logs_use_configured_age() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let input = input();
    store.create(&input, &call)?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("attempts/{}", input.attempt_id));
    withdraw(&store, &input, &call)?;
    fs::write(attempt.join("stdout.log"), vec![1; 200_000])?;
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let now = WallTime::from_micros(200 + 2 * DAY_MICROS)?;
    assert!(
        store
            .gc_provider_state_with_logs(&root, 7, 3, now, false, &call, &RetentionScan::default())?
            .attempt_logs
            .is_empty(),
        "logs younger than the configured age stay"
    );
    assert_eq!(
        store
            .gc_provider_state_with_logs(&root, 7, 1, now, false, &call, &RetentionScan::default())?
            .attempt_logs
            .len(),
        1
    );
    Ok(())
}

#[test]
fn gc_logs_refuse_symlinks() -> TestResult {
    {
        let (temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let outside = temp.path().join("outside");
        fs::write(&outside, vec![3; 200_000])?;
        std::os::unix::fs::symlink(&outside, attempt.join("stdout.log"))?;
        let report = sweep(&store, &context, 7, 31 * DAY_MICROS, false, &call)?;
        assert!(report.attempt_logs.is_empty());
        assert!(!report.errors.is_empty());
        assert!(
            !attempt.join("provider-state").exists(),
            "log refusal must not block provider expiry"
        );
        assert_eq!(fs::read(outside)?, vec![3; 200_000]);
    }
    Ok(())
}

#[test]
fn gc_preserves_tmp_for_lost_attempt_without_exit_evidence() -> TestResult {
    use provenant_core::Clock;
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let record = store.record(input.attempt_id, &call)?;
    store.commit_lost(RunStore::scope(&record), "fixture", &call)?;
    let temporary = scratch_root_of(&attempt).join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), b"scratch")?;
    let report = store.gc_provider_state(
        &Directory::open_anchor(context.state_root.as_path())?,
        7,
        WallTime::from_micros(clock.wall_now()?.as_micros() + DAY_MICROS)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(report.attempt_tmp.is_empty());
    let scratch = store.gc_temporary(
        &Directory::open_anchor(context.state_root.as_path())?,
        &ScratchPolicy::new(86_400),
        WallTime::from_micros(clock.wall_now()?.as_micros() + DAY_MICROS)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(scratch.attempt_tmp.is_empty());
    assert!(temporary.exists());
    Ok(())
}

// Spec: docs/specs/disk.md#scratch-and-cache-custody
#[test]
fn released_read_only_scratch_uses_idle_age_independent_of_graph_and_checkout() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    let source = temp.path().join("source-checkout");
    fs::create_dir(&source)?;
    input.request.task.access.source_root = Some(source.clone());
    input
        .request
        .task
        .context
        .as_mut()
        .ok_or("graph context missing")?
        .pr = Some(3518);
    store.create(&input, &call)?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("attempts/{}", input.attempt_id));
    let temporary = scratch_root_of(&attempt).join("tmp/candidate-0/target/debug");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&temporary)?;
    fs::write(temporary.join("artifact"), b"scratch target")?;
    let durable = [
        (
            attempt.join("provider-state/handoff.json"),
            b"handoff".as_slice(),
        ),
        (attempt.join("stdout.log"), b"log".as_slice()),
    ];

    let root = Directory::open_anchor(context.state_root.as_path())?;
    let idle = 7 * 86_400;
    let retained_by_writer_owner = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Owned;
    let policy = ScratchPolicy {
        idle_seconds: idle,
        owner: Some(&retained_by_writer_owner),
        ..ScratchPolicy::new(idle)
    };
    let before_release = store.gc_temporary(
        &root,
        &policy,
        WallTime::from_micros(200 + i64::try_from(idle)? * 1_000_000)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(before_release.attempt_tmp.is_empty());
    assert!(temporary.exists(), "a live attempt retains scratch");

    withdraw(&store, &input, &call)?;
    let result = fs::read(attempt.join("result.md"))?;
    for (path, contents) in &durable {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    let before_idle = store.gc_temporary(
        &root,
        &policy,
        WallTime::from_micros(200 + i64::try_from(idle)? * 1_000_000 - 1)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(before_idle.attempt_tmp.is_empty());
    assert!(temporary.exists(), "scratch remains until gc.idle");

    let collected = store.gc_temporary(
        &root,
        &policy,
        WallTime::from_micros(200 + i64::try_from(idle)? * 1_000_000)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert_eq!(collected.attempt_tmp.len(), 1, "{collected:?}");
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert_eq!(fs::read(attempt.join("result.md"))?, result);
    for (path, contents) in &durable {
        assert_eq!(
            fs::read(path)?,
            *contents,
            "{} remains durable",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn settled_writer_attempt_tmp_collected_after_idle_while_writer_worktree_present() -> TestResult {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut input = input();
    let worktree = temp.path().join("writer-worktree");
    fs::create_dir(&worktree)?;
    input.request.task.writer = Some(provenant_records::run::WriterRequest {
        extra: Default::default(),
        custody_project: Some(context.project_id),
        project: temp.path().join("project"),
        worktree: worktree.clone(),
    });
    store.create(&input, &call)?;
    let attempt = context
        .state_root
        .as_path()
        .join(format!("attempts/{}", input.attempt_id));
    let temporary = scratch_root_of(&attempt).join("tmp/candidate-0/warm-target");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&temporary)?;
    fs::write(temporary.join("artifact"), b"warm target build output")?;

    let root = Directory::open_anchor(context.state_root.as_path())?;
    let idle = 6 * 3600;
    let policy = ScratchPolicy::new(idle);

    let live_sweep = store.gc_temporary(
        &root,
        &policy,
        WallTime::from_micros(200 + i64::try_from(idle)? * 1_000_000)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(live_sweep.attempt_tmp.is_empty());
    assert!(temporary.exists(), "live writer attempt retains scratch");

    withdraw(&store, &input, &call)?;

    let before_idle = store.gc_temporary(
        &root,
        &policy,
        WallTime::from_micros(200 + i64::try_from(idle)? * 1_000_000 - 1)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(before_idle.attempt_tmp.is_empty());
    assert!(temporary.exists(), "scratch remains until gc.idle");

    let after_idle = store.gc_temporary(
        &root,
        &policy,
        WallTime::from_micros(200 + i64::try_from(idle)? * 1_000_000)?,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert_eq!(after_idle.attempt_tmp.len(), 1, "{after_idle:?}");
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert!(worktree.exists(), "writer worktree is preserved");
    Ok(())
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn scratch_budget_stops_inside_an_attempt_and_resumes_remaining_files() -> TestResult {
    use provenant_core::{Clock, DurationMicros};
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let temporary = scratch_root_of(&attempt).join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    for n in 0..256 {
        fs::write(temporary.join(format!("payload-{n}")), b"regenerable")?;
    }
    let proof = fs::read(attempt.join("terminal-pending.json"))?;
    let budget_clock = std::sync::Mutex::new(clock.clone());
    let deadline = clock
        .mono_now()
        .checked_add(DurationMicros::from_micros(420))?;
    let keep_going = || {
        let mut clock = budget_clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clock.advance(DurationMicros::from_micros(1)).is_ok() && clock.mono_now() < deadline
    };
    let scan = RetentionScan {
        keep_going: Some(&keep_going),
        ..Default::default()
    };
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let now = WallTime::from_micros(200 + 31 * DAY_MICROS)?;
    let partial = store.gc_temporary(&root, &ScratchPolicy::new(0), now, false, &call, &scan)?;
    assert_eq!(
        partial.interrupted_at.as_ref(),
        Some(&attempt),
        "{partial:?}"
    );
    assert!(partial.errors.is_empty());
    assert!(
        partial.attempt_tmp.is_empty(),
        "a partial directory is not reported removed"
    );
    assert!(
        partial.temporary_bytes() > 0,
        "partial reclaimed bytes remain observable"
    );
    let remaining = fs::read_dir(&temporary)?.count();
    assert!(
        remaining > 0 && remaining < 256,
        "bounded pass makes durable progress"
    );
    assert_eq!(fs::read(attempt.join("terminal-pending.json"))?, proof);
    let resume_cancel = CancelToken::new();
    let resume_call = crate::tests::call(&clock, &resume_cancel);
    assert!(store.temporary_retained(input.attempt_id, &resume_call)?);
    let custody = Directory::open_anchor(&attempt)?;
    drop(custody.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?);
    drop(custody.try_lock(OsStr::new("publication.lock"), LockMode::Exclusive)?);
    let resumed = store.gc_temporary(
        &root,
        &ScratchPolicy::new(0),
        now,
        false,
        &resume_call,
        &RetentionScan {
            start_at: Some(input.attempt_id),
            ..Default::default()
        },
    )?;
    assert!(resumed.interrupted_at.is_none());
    assert!(!temporary.exists());
    assert!(!store.temporary_retained(input.attempt_id, &resume_call)?);
    assert_eq!(fs::read(attempt.join("terminal-pending.json"))?, proof);
    Ok(())
}

// Spec: docs/specs/disk.md#scratch-and-cache-custody
#[test]
fn scratch_gc_requires_release_and_free_attempt_custody() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let temporary = scratch_root_of(&attempt).join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), b"must survive until release")?;
    let finished = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Finished;
    let policy = ScratchPolicy {
        owner: Some(&finished),
        ..ScratchPolicy::new(0)
    };
    let root = Directory::open_anchor(context.state_root.as_path())?;

    // A queued attempt is never eligible, regardless of the owner callback.
    let collection_time = WallTime::from_micros(200 + 31 * DAY_MICROS)?;
    let before_release = store.gc_temporary(
        &root,
        &policy,
        collection_time,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(before_release.attempt_tmp.is_empty());
    assert_eq!(
        fs::read(temporary.join("payload"))?,
        b"must survive until release"
    );

    withdraw(&store, &input, &call)?;
    let attempt_dir = Directory::open_anchor(&attempt)?;
    let busy = attempt_dir.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
    let while_busy = store.gc_temporary(
        &root,
        &policy,
        collection_time,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(while_busy.attempt_tmp.is_empty());
    assert_eq!(
        fs::read(temporary.join("payload"))?,
        b"must survive until release"
    );
    drop(busy);

    let terminal = fs::read(attempt.join("terminal-pending.json"))?;
    let after_release = store.gc_temporary(
        &root,
        &policy,
        collection_time,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert_eq!(after_release.attempt_tmp.len(), 1);
    assert!(!temporary.exists());
    assert_eq!(fs::read(attempt.join("terminal-pending.json"))?, terminal);
    Ok(())
}

#[test]
fn gc_spares_private_objects_while_a_settlement_driver_is_importing() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let temporary = scratch_root_of(&attempt).join("tmp");
    let objects = temporary.join("candidate-0/git-objects");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&objects)?;
    let private = b"the lane's private commit";
    fs::write(objects.join("private"), private)?;
    withdraw(&store, &input, &call)?;
    let finished = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Finished;
    let policy = ScratchPolicy {
        owner: Some(&finished),
        ..ScratchPolicy::new(0)
    };
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let collection_time = WallTime::from_micros(200 + 31 * DAY_MICROS)?;
    let dir = Directory::open_anchor(&attempt)?;
    let record = store.record(input.attempt_id, &call)?;
    // GC's custody excludes a new claimant until collection has ended.
    let held = store.hold_attempt(&record, &call)?.ok_or("GC custody")?;
    assert!(crate::settlement::Guard::claim(&dir, RunStore::scope(&record), &clock)?.is_none());
    drop(held);

    // Pause the pass after observing a free lease. A driver claims before
    // GC continues; collection must re-establish custody, not trust the probe.
    std::thread::scope(|threads| -> TestResult {
        let (probed_tx, probed_rx) = std::sync::mpsc::channel();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let path = attempt.clone();
        let scope = RunStore::scope(&record);
        let worker = threads.spawn(move || {
            let dir = Directory::open_anchor(&path).expect("attempt directory");
            let clock = crate::tests::clock();
            probed_rx.recv().expect("free lease probe");
            let driver = crate::settlement::Guard::claim(&dir, scope, &clock)
                .expect("claim result")
                .expect("settlement claim");
            claimed_tx.send(()).expect("signal claim");
            release_rx.recv().expect("release driver");
            drop(driver);
        });
        struct ClearHook;
        impl Drop for ClearHook {
            fn drop(&mut self) {
                BEFORE_SETTLEMENT_LEASE.with(|hook| {
                    hook.borrow_mut().take();
                });
            }
        }
        let clear_hook = ClearHook;
        let path = attempt.clone();
        BEFORE_SETTLEMENT_LEASE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let dir = Directory::open_anchor(&path).expect("attempt directory");
                let clock = crate::tests::clock();
                let free = crate::settlement::collection_lease(&dir, scope, &clock, false)
                    .expect("free settlement lease");
                drop(free);
                probed_tx.send(()).expect("signal free lease");
                claimed_rx.recv().expect("driver claim");
            }));
        });
        let busy = store.gc_temporary(
            &root,
            &policy,
            collection_time,
            false,
            &call,
            &RetentionScan::default(),
        )?;
        assert!(busy.attempt_tmp.is_empty(), "{busy:?}");
        assert_eq!(fs::read(objects.join("private"))?, private);
        drop(clear_hook);
        release_tx.send(())?;
        worker.join().map_err(|_| "settlement worker panicked")?;
        Ok(())
    })?;

    let after = store.gc_temporary(
        &root,
        &policy,
        collection_time,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert_eq!(after.attempt_tmp.len(), 1, "{after:?}");
    assert!(!temporary.exists());
    Ok(())
}

#[test]
fn gc_preserves_scratch_when_settlement_ownership_is_unknown() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let temporary = scratch_root_of(&attempt).join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), b"settlement scratch")?;
    withdraw(&store, &input, &call)?;
    let journal = attempt.join("settlement-progress.json");
    fs::write(&journal, b"not a journal")?;
    let finished = |_: &AttemptRecord, _: &CanonicalRequest| ScratchOwner::Finished;
    let policy = ScratchPolicy {
        owner: Some(&finished),
        ..ScratchPolicy::new(0)
    };
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let collection_time = WallTime::from_micros(200 + 31 * DAY_MICROS)?;
    let report = store.gc_temporary(
        &root,
        &policy,
        collection_time,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert!(report.attempt_tmp.is_empty(), "{report:?}");
    assert_eq!(fs::read(temporary.join("payload"))?, b"settlement scratch");

    fs::remove_file(journal)?;
    let report = store.gc_temporary(
        &root,
        &policy,
        collection_time,
        false,
        &call,
        &RetentionScan::default(),
    )?;
    assert_eq!(report.attempt_tmp.len(), 1, "{report:?}");
    assert!(!temporary.exists());
    Ok(())
}

#[test]
fn gc_reclaims_only_old_terminal_tmp_and_previews_paths_and_bytes() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let temporary = scratch_root_of(&attempt).join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), vec![1; 8192])?;
    assert!(
        sweep(&store, &context, 7, 100 * DAY_MICROS, false, &call)?
            .attempt_tmp
            .is_empty()
    );
    assert!(temporary.exists());
    withdraw(&store, &input, &call)?;
    // Terminal release preserves scratch while the work has no completion proof.
    assert!(temporary.join("payload").is_file());
    fs::create_dir(temporary.join("candidate-0"))?;
    fs::write(temporary.join("candidate-0/payload.lock"), "payload")?;
    assert!(
        sweep(&store, &context, 7, 7 * DAY_MICROS - 1, false, &call)?
            .attempt_tmp
            .is_empty()
    );
    assert!(temporary.exists());
    let preview = sweep(&store, &context, 7, 7 * DAY_MICROS, true, &call)?;
    assert!(preview.errors.is_empty(), "{:?}", preview.errors);
    assert_eq!(preview.attempt_tmp.len(), 1);
    assert_eq!(preview.attempt_tmp[0].path, temporary);
    assert!(preview.attempt_tmp[0].bytes >= 8192);
    assert!(temporary.join("candidate-0/payload.lock").exists());
    let applied = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(applied.errors.is_empty(), "{:?}", applied.errors);
    assert_eq!(applied.attempt_tmp.len(), 1);
    assert_eq!(applied.attempt_tmp[0].path, temporary);
    assert_eq!(applied.attempt_tmp[0].bytes, preview.attempt_tmp[0].bytes);
    assert!(!temporary.exists());
    assert!(attempt.join("terminal-pending.json").exists());
    assert!(
        sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?
            .attempt_tmp
            .is_empty()
    );
    Ok(())
}

#[test]
fn temporary_preview_preserves_directory_permissions() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir()?;
    let temporary = fixture.path().join("tmp");
    std::fs::create_dir(&temporary)?;
    std::fs::write(temporary.join("keep"), b"diagnostics")?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o500))?;
    let attempt = Directory::open_anchor(fixture.path())?;
    let mut report = ProviderStateGc::default();
    let mut sizes = provenant_os::disk::DiskReport::with_walk_budget(
        fixture.path().into(),
        0,
        100,
        std::time::Duration::from_secs(1),
    );
    collect_temporary(&attempt, &temporary, true, &mut report, &mut sizes, &|| {
        true
    })?;
    assert_eq!(
        std::fs::metadata(&temporary)?.permissions().mode() & 0o777,
        0o500
    );
    assert_eq!(std::fs::read(temporary.join("keep"))?, b"diagnostics");
    assert_eq!(report.attempt_tmp.len(), 1);
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[test]
fn scratch_waits_for_declared_terminal_effects() -> TestResult {
    use provenant_records::run::{EffectDisposition, SettlementEffect};
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let dir = Directory::open_anchor(&attempt)?;
    let record = store.record(input.attempt_id, &call)?;
    crate::artifacts::seal(
        &dir,
        "terminal-effects.json",
        &crate::db::bytes(&crate::store::TerminalEffects {
            schema_version: 1,
            scope: RunStore::scope(&record),
            effects: vec![(SettlementEffect::Delivery, EffectDisposition::Pending)],
        })?,
        crate::RECORD_LIMIT,
    )?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt).join("tmp"))?;
    fs::write(scratch_root_of(&attempt).join("tmp/payload"), b"scratch")?;
    withdraw(&store, &input, &call)?;
    let before = sweep(&store, &context, 7, 7 * DAY_MICROS, true, &call)?;
    assert!(before.attempt_tmp.is_empty());
    assert!(scratch_root_of(&attempt).join("tmp/payload").exists());
    let mut receipts = store
        .record(input.attempt_id, &call)?
        .settlement_receipts
        .ok_or("missing receipts")?;
    receipts.delivery = EffectDisposition::Done;
    store.db.execute(
        "UPDATE settlement_receipts SET record=?1 WHERE attempt=?2",
        rusqlite::params![crate::db::bytes(&receipts)?, input.attempt_id.to_string()],
    )?;
    let after = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert_eq!(after.attempt_tmp.len(), 1, "{after:?}");
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    Ok(())
}

#[test]
fn collect_temporary_spares_tmp_when_cargo_lock_is_held() -> TestResult {
    let fixture = tempfile::tempdir()?;
    let temporary = fixture.path().join("tmp");
    std::fs::create_dir(&temporary)?;
    let cargo_dir = temporary.join("candidate-0/target");
    std::fs::create_dir_all(&cargo_dir)?;
    let lock_path = cargo_dir.join(".cargo-lock");
    std::fs::write(&lock_path, b"")?;

    let attempt = Directory::open_anchor(fixture.path())?;
    let mut report = ProviderStateGc::default();
    let mut sizes = provenant_os::disk::DiskReport::with_walk_budget(
        fixture.path().into(),
        0,
        100,
        std::time::Duration::from_secs(1),
    );

    let target_dir = Directory::open_anchor(&cargo_dir)?;
    let _guard = match target_dir
        .try_flock_foreign(std::ffi::OsStr::new(".cargo-lock"), LockMode::Exclusive)?
    {
        provenant_os::fs::ForeignLock::Acquired(g) => g,
        other => panic!("expected acquired, got {other:?}"),
    };

    collect_temporary(&attempt, &temporary, true, &mut report, &mut sizes, &|| {
        true
    })?;
    assert!(report.attempt_tmp.is_empty());
    assert_eq!(report.kept.len(), 1);
    report.kept.clear();
    collect_temporary(
        &attempt,
        &temporary,
        false,
        &mut report,
        &mut sizes,
        &|| true,
    )?;
    assert_eq!(report.kept.len(), 1);
    assert!(report.kept[0].reason.contains("Cargo lock"));
    assert!(temporary.exists());
    assert!(report.attempt_tmp.is_empty());
    Ok(())
}

#[test]
fn gc_unprobeable_tmp_is_kept_without_blocking_provider_retention() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let temporary = scratch_root_of(&attempt).join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), vec![1; 8192])?;
    // Exceed the measurement depth budget without a million-entry fixture.
    let mut deepest = temporary.clone();
    for _ in 0..258 {
        deepest.push("d");
        fs::create_dir(&deepest)?;
    }
    let candidate = attempt.join("provider-state/candidate-0");
    fs::create_dir(candidate.join("cache"))?;
    fs::write(candidate.join("cache/payload"), "regenerable")?;
    let preview = sweep_with_days(&store, &context, 30, 7, 7 * DAY_MICROS, true, &call)?;
    assert!(preview.attempt_tmp.is_empty());
    assert!(!preview.kept.is_empty());
    assert!(temporary.exists());
    let applied = sweep_with_days(&store, &context, 30, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(applied.attempt_tmp.is_empty());
    assert!(!applied.kept.is_empty());
    assert!(temporary.exists());
    assert!(!candidate.join("auth.json").exists());
    assert!(!candidate.join("cache").exists());
    assert_eq!(
        fs::read(candidate.join("sessions/session.jsonl"))?,
        b"transcript"
    );
    Ok(())
}

#[test]
fn gc_refreshes_the_hint_update_budget_after_collection() -> TestResult {
    use provenant_core::{Clock, MonoTime, TimeError};
    // Each DB observation completes within its own five-second budget,
    // while time between observations exceeds the original deadline.
    struct AdvancingClock(std::sync::Mutex<u64>);
    impl Clock for AdvancingClock {
        fn mono_now(&self) -> MonoTime {
            let mut value = self.0.lock().unwrap();
            let now = *value;
            *value += 2_000_000;
            MonoTime::from_micros(now)
        }
        fn wall_now(&self) -> std::result::Result<WallTime, TimeError> {
            WallTime::from_micros(200 + DAY_MICROS)
        }
        fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), TimeError> {
            let mut value = self.0.lock().unwrap();
            *value = (*value).max(deadline.as_micros());
            Ok(())
        }
    }
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let id = input.attempt_id;
    withdraw(&store, &input, &call)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt).join("tmp"))?;
    store.set_temporary_retained(id, true, &call)?;
    let slow = AdvancingClock(std::sync::Mutex::new(0));
    let call = Request {
        clock: &slow,
        cancel: &cancel,
    };
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.kept.is_empty(), "{:?}", report.kept);
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert!(!store.temporary_retained(id, &call)?);
    Ok(())
}

#[test]
fn gc_tmp_removal_refusal_does_not_block_credentials_caches_or_expiry() -> TestResult {
    for days in [1, 7] {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let id = input.attempt_id;
        store.set_temporary_retained(id, true, &call)?;
        // Payload disposal refuses a root accessible to other users.
        let temporary = scratch_root_of(&attempt).join("tmp");
        fs::DirBuilder::new().mode(0o755).create(&temporary)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
        let candidate = attempt.join("provider-state/candidate-0");
        fs::create_dir(candidate.join("cache"))?;
        fs::write(candidate.join("cache/payload"), "regenerable")?;
        let report = sweep(
            &store,
            &context,
            days,
            DAY_MICROS.saturating_mul(days as i64),
            false,
            &call,
        )?;
        assert!(
            report
                .errors
                .iter()
                .chain(&report.kept)
                .any(|issue| issue.path == temporary)
        );
        assert!(temporary.exists());
        assert!(!candidate.join("auth.json").exists());
        assert!(!candidate.join("cache").exists());
        assert!(attempt.join(EXPIRED).exists());
        assert!(store.temporary_retained(id, &call)?);
    }
    Ok(())
}

#[test]
fn gc_clears_temporary_retained_only_after_applied_removal_across_projects() -> TestResult {
    let (_temp, mut context, store, input, attempt) = seeded()?;
    let original_context = context.clone();
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let id = input.attempt_id;
    let terminal = withdraw(&store, &input, &call)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt).join("tmp"))?;
    store.set_temporary_retained(id, true, &call)?;
    drop(store);
    // Host GC covers all indexed projects, not just the opening project.
    context.project_id = provenant_core::ProjectId::from_bytes([99; 16]);
    let store = RunStore::open(&context, &call)?;
    let original_store = RunStore::open(&original_context, &call)?;
    sweep(&store, &context, 7, 7 * DAY_MICROS, true, &call)?;
    assert!(original_store.temporary_retained(id, &call)?);
    sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert!(!original_store.temporary_retained(id, &call)?);
    // The public record API projects receipts from the journal; GC leaves
    // every other field of the terminal record as settlement wrote it.
    let mut restored = original_store.record(id, &call)?;
    assert!(restored.settlement_receipts.is_some());
    restored.settlement_receipts = None;
    let mut stored = terminal;
    stored.settlement_receipts = None;
    assert_eq!(restored, stored);
    Ok(())
}

#[test]
fn gc_expires_credentials_at_one_day_and_archives_history_at_configured_age() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let terminal = withdraw(&store, &input, &call)?;
    let candidate = attempt.join("provider-state/candidate-0");
    let before = fs::read(attempt.join("terminal-pending.json"))?;
    fs::write(attempt.join("process-exit.json"), "exit")?;
    fs::write(attempt.join("stdout.log"), "capture")?;
    let report = sweep(&store, &context, 7, DAY_MICROS - 1, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(candidate.join("auth.json").exists());
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, true, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.removed.contains(&candidate.join("auth.json")));
    assert!(candidate.join("auth.json").exists());
    assert!(!attempt.join(EXPIRED).exists());
    assert!(!attempt.join("provider-history").exists());
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!candidate.join("auth.json").exists());
    assert!(!candidate.join("claude-config/.credentials.json").exists());
    assert!(candidate.join("sessions/session.jsonl").exists());
    assert!(
        sweep(&store, &context, 7, 7 * DAY_MICROS - 1, false, &call)?
            .removed
            .is_empty()
    );
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!attempt.join("provider-state").exists());
    let history = attempt.join("provider-history/candidate-0");
    for name in [
        "sessions/session.jsonl",
        "thread_history_1.sqlite",
        "thread_history_1.sqlite-wal",
        "state_5.sqlite",
        "state_5.sqlite-shm",
        "claude-config/projects/session.jsonl",
    ] {
        assert!(history.join(name).exists(), "{name} lost");
    }
    assert!(!history.join("other").exists());
    assert!(
        attempt
            .join("provider-history/candidate-1/unknown-session")
            .exists()
    );
    assert!(
        !attempt
            .join("provider-history/candidate-1/auth.json")
            .exists()
    );
    assert_eq!(fs::read(attempt.join("terminal-pending.json"))?, before);
    assert_eq!(
        fs::read_to_string(attempt.join("process-exit.json"))?,
        "exit"
    );
    assert_eq!(fs::read_to_string(attempt.join("stdout.log"))?, "capture");
    assert_eq!(store.record(terminal.attempt_id, &call)?, terminal);
    assert!(matches!(
        store.retain_provider_state(
            terminal.attempt_id,
            &call,
            &provenant_records::policy::Policy::default()
        ),
        Err(RunError::ProviderStateExpired)
    ));
    assert!(
        sweep(&store, &context, 7, 8 * DAY_MICROS, false, &call)?
            .removed
            .is_empty()
    );
    Ok(())
}

#[test]
fn gc_archives_provider_history_before_disposing_private_state() -> TestResult {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "cursor",
            ".cursor/chats",
            &["project/session/store.db", "project/session/store.db-wal"],
        ),
        (
            "opencode",
            "data/opencode",
            &[
                "opencode.db",
                "opencode.db-wal",
                "log/session.log",
                "storage/message/session.json",
                "snapshot/project/HEAD",
                "shell/session.sh",
            ],
        ),
    ];
    for (provider, layout, artifacts) in cases {
        relocated_history_lifecycle(provider, layout, artifacts)?;
    }
    Ok(())
}

fn relocated_history_lifecycle(provider: &str, layout: &str, artifacts: &[&str]) -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let candidate = attempt.join("provider-state/candidate-0");
    fs::write(candidate.join("provider.txt"), provider)?;
    for artifact in artifacts {
        let path = candidate.join(layout).join(artifact);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().ok_or("parent")?)?;
        fs::write(path, format!("retained {artifact}"))?;
    }
    let host_auth = attempt.join("synthetic-host-credential");
    if provider == "opencode" {
        fs::write(&host_auth, "synthetic credential")?;
        std::os::unix::fs::symlink(&host_auth, candidate.join(layout).join("auth.json"))?;
    }
    let disposable = if provider == "cursor" {
        ".cursor/settings.json"
    } else {
        "data/other"
    };
    fs::write(candidate.join(disposable), "disposable")?;
    let preview = sweep(&store, &context, 7, 7 * DAY_MICROS, true, &call)?;
    assert!(preview.errors.is_empty(), "{:?}", preview.errors);
    assert!(candidate.join(layout).exists());
    assert!(!attempt.join(HISTORY).exists());
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!attempt.join(STATE).exists());
    let archive = attempt.join("provider-history/candidate-0");
    for artifact in artifacts {
        assert_eq!(
            fs::read_to_string(archive.join(layout).join(artifact))?,
            format!("retained {artifact}")
        );
    }
    assert!(!archive.join(disposable).exists());
    if provider == "opencode" {
        assert!(fs::symlink_metadata(archive.join(layout).join("auth.json")).is_err());
        assert_eq!(fs::read_to_string(&host_auth)?, "synthetic credential");
    }
    let repeated = sweep(&store, &context, 7, 8 * DAY_MICROS, false, &call)?;
    assert!(repeated.errors.is_empty(), "{:?}", repeated.errors);
    assert!(repeated.removed.is_empty());
    Ok(())
}

#[test]
fn gc_spares_nonterminal_and_live_custody_but_collects_released_read_only_reservation() -> TestResult
{
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    assert!(
        sweep(&store, &context, 1, 100 * DAY_MICROS, false, &call)?
            .removed
            .is_empty()
    );
    assert!(
        attempt
            .join("provider-state/candidate-0/auth.json")
            .exists()
    );
    withdraw(&store, &input, &call)?;
    let directory = Directory::open_anchor(&attempt)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt).join("tmp"))?;
    fs::write(scratch_root_of(&attempt).join("tmp/keep"), "locked payload")?;
    let run = store.attempt_dir(input.attempt_id, false)?;
    let publication = run.try_lock_existing(OsStr::new("publication.lock"), LockMode::Shared)?;
    assert!(
        !sweep(&store, &context, 1, DAY_MICROS, false, &call)?
            .kept
            .is_empty()
    );
    assert!(scratch_root_of(&attempt).join("tmp/keep").exists());
    drop(publication);
    let writer = directory.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
    assert!(
        !sweep(&store, &context, 1, DAY_MICROS, false, &call)?
            .kept
            .is_empty()
    );
    assert!(scratch_root_of(&attempt).join("tmp/keep").exists());
    drop(writer);
    let evidence = attempt.join(input.attempt_id.to_string());
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&evidence)?;
    fs::write(evidence.join("reservation.json"), "{}")?;
    let custody = directory.try_lock(OsStr::new("custody.lock"), LockMode::Exclusive)?;
    let candidate = attempt.join("provider-state/candidate-0");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(candidate.join("cache"))?;
    fs::write(candidate.join("cache/entry"), "cache")?;
    let record = store.record(input.attempt_id, &call)?;
    store.settle_cleanup(&record, None, &call);
    assert!(candidate.join("cache/entry").exists());
    assert!(
        !sweep(&store, &context, 1, DAY_MICROS, false, &call)?
            .kept
            .is_empty()
    );
    assert!(scratch_root_of(&attempt).join("tmp/keep").exists());
    assert!(candidate.join("auth.json").exists());
    drop(custody);
    let (_, warnings) = provenant_records::warning::capture(|| {
        store.settle_cleanup(&record, None, &call);
    });
    assert!(!candidate.join("cache").exists());
    assert!(warnings.iter().any(|warning| {
        warning.code.as_ref() == provenant_records::warning::READ_ONLY_RESERVATION_RETAINED.as_str()
    }));
    let (report, warnings) = provenant_records::warning::capture(|| {
        sweep(&store, &context, 1, DAY_MICROS, false, &call)
    });
    let report = report?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(warnings.iter().any(|warning| {
        warning.code.as_ref() == provenant_records::warning::READ_ONLY_RESERVATION_RETAINED.as_str()
    }));
    assert!(!attempt.join("provider-state").exists());
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert!(evidence.join("reservation.json").exists());
    Ok(())
}

#[test]
fn gc_temporary_never_follows_links_outside_the_attempt() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("keep"), "outside")?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt).join("tmp"))?;
    std::os::unix::fs::symlink(outside.path(), scratch_root_of(&attempt).join("tmp/escape"))?;
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.attempt_tmp.len(), 1);
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert_eq!(fs::read_to_string(outside.path().join("keep"))?, "outside");
    // Collection prunes the emptied per-attempt root; recreate it for the
    // second case, a tmp that is itself a link.
    assert!(!scratch_root_of(&attempt).exists());
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt))?;
    std::os::unix::fs::symlink(outside.path(), scratch_root_of(&attempt).join("tmp"))?;
    store.set_temporary_retained(input.attempt_id, true, &call)?;
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.attempt_tmp.len(), 1);
    assert!(!scratch_root_of(&attempt).join("tmp").exists());
    assert_eq!(fs::read_to_string(outside.path().join("keep"))?, "outside");
    Ok(())
}

#[test]
fn gc_keeps_unprobeable_tmp_without_following_links() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;

    let temporary = scratch_root_of(&attempt).join("tmp");
    let nested = temporary.join("candidate-0/.tmpX/child");
    fs::create_dir_all(&nested)?;
    fs::write(nested.join("payload"), b"recoverable scratch")?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o000))?;

    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("keep"), "outside")?;
    symlink(outside.path(), temporary.join("candidate-0/.tmpX/escape"))?;

    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.attempt_tmp.is_empty());
    assert!(!report.kept.is_empty());
    assert!(temporary.exists());
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o700))?;
    assert_eq!(fs::read_to_string(outside.path().join("keep"))?, "outside");
    Ok(())
}

#[test]
fn candidate_provider_uses_only_its_current_marker() -> TestResult {
    let temp = tempfile::tempdir()?;
    let candidate = Directory::open_anchor(temp.path())?;
    fs::write(temp.path().join("provider.txt"), "codex")?;
    assert_eq!(candidate_provider(&candidate)?, "codex");
    fs::write(temp.path().join("provider.txt"), [0xff])?;
    assert!(candidate_provider(&candidate).is_err());
    fs::remove_file(temp.path().join("provider.txt"))?;
    assert!(candidate_provider(&candidate)?.is_empty());
    Ok(())
}

#[test]
fn gc_ignores_indexed_attempt_with_missing_attempt_directory() -> TestResult {
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let terminal = withdraw(&store, &input, &call)?;
    let attempts = attempt.parent().ok_or("attempt collection path")?;
    fs::remove_dir_all(&attempt)?;

    let report = sweep(&store, &context, 7, 0, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.kept.is_empty(), "{:?}", report.kept);
    assert!(store.record(terminal.attempt_id, &call).is_ok());

    fs::remove_dir_all(attempts)?;
    let report = sweep(&store, &context, 7, 0, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    Ok(())
}

#[test]
fn gc_retries_only_authorised_tombstones_and_never_follows_links() -> TestResult {
    use std::os::unix::fs::{MetadataExt, symlink};
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let candidate = attempt.join("provider-state/candidate-0");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(candidate.join("cache"))?;
    fs::write(candidate.join("cache/payload.lock"), "cache")?;
    let metadata = fs::metadata(candidate.join("cache"))?;
    let cache_tombstone = candidate.join(tombstone_name(
        metadata.dev(),
        metadata.ino(),
        OsStr::new("cache"),
    ));
    fs::rename(candidate.join("cache"), &cache_tombstone)?;
    let metadata = fs::metadata(candidate.join("sessions"))?;
    let unapproved = candidate.join(tombstone_name(
        metadata.dev(),
        metadata.ino(),
        OsStr::new("sessions"),
    ));
    fs::rename(candidate.join("sessions"), &unapproved)?;
    let host = context
        .state_root
        .as_path()
        .parent()
        .ok_or("state parent")?
        .join("host-auth");
    fs::write(&host, "host credential")?;
    fs::remove_file(candidate.join("auth.json"))?;
    symlink(&host, candidate.join("auth.json"))?;
    symlink(&host, candidate.join("plugins"))?;
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!cache_tombstone.exists());
    assert!(unapproved.join("session.jsonl").exists());
    assert!(fs::symlink_metadata(candidate.join("auth.json")).is_err());
    assert!(fs::symlink_metadata(candidate.join("plugins")).is_err());
    assert_eq!(fs::read_to_string(host)?, "host credential");
    // A forged identity never grants disposal authority.
    let forged = candidate.join(format!("{TOMBSTONE}{:032x}-cache", 0));
    fs::DirBuilder::new().mode(0o700).create(&forged)?;
    fs::write(forged.join("keep"), "keep")?;
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    // The refusal is reported against the provider state it was found in,
    // naming the forged entry.
    let forged_name = forged.file_name().and_then(OsStr::to_str).ok_or("name")?;
    assert!(
        report
            .errors
            .iter()
            .any(|issue| issue.reason.contains(forged_name)),
        "{:?}",
        report.errors
    );
    assert!(forged.join("keep").exists());
    Ok(())
}

#[test]
fn a_pending_continuation_keeps_its_source_and_expired_resume_spends_nothing() -> TestResult {
    use provenant_core::{AttemptId, OperationId};
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let resume = Resume {
        extend: Vec::new(),
        prior: input.attempt_id,
        expected_generation: 0,
        operation_id: OperationId::parse("continue-fixture")?,
        attempt_id: AttemptId::from_bytes([9; 16]),
        prompt: "continue".into(),
        created_at: WallTime::from_micros(300)?,
    };
    store.resume(&resume, &call)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch_root_of(&attempt).join("tmp"))?;
    fs::write(scratch_root_of(&attempt).join("tmp/payload"), "disposable")?;
    let report = sweep(&store, &context, 1, DAY_MICROS, false, &call)?;
    assert_eq!(report.kept.len(), 1);
    assert!(report.attempt_tmp.is_empty());
    assert!(
        scratch_root_of(&attempt).join("tmp/payload").exists(),
        "pending continuation retains scratch"
    );
    assert!(
        attempt
            .join("provider-state/candidate-0/auth.json")
            .exists()
    );
    store.withdraw(
        resume.attempt_id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture",
        WallTime::from_micros(400)?,
        &call,
    )?;
    let report = sweep(&store, &context, 1, DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!attempt.join("provider-state").exists());
    assert!(
        !scratch_root_of(&attempt).join("tmp").exists(),
        "scratch is collectable after continuation settles"
    );
    assert_eq!(store.resume(&resume, &call)?.attempt_id, resume.attempt_id);
    let fresh = Resume {
        extend: Vec::new(),
        operation_id: OperationId::parse("expired-fixture")?,
        ..resume
    };
    assert!(matches!(
        store.resume(&fresh, &call),
        Err(RunError::ProviderStateExpired)
    ));
    assert!(store.operation(&fresh.operation_id, &call)?.is_none());
    Ok(())
}
#[test]
fn interrupted_expiry_keeps_markerless_provider_history_and_retries_credential_tombstones()
-> TestResult {
    use std::os::unix::fs::MetadataExt;
    let (_temp, context, store, input, attempt) = seeded()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    withdraw(&store, &input, &call)?;
    let state = attempt.join("provider-state");
    let candidate = state.join("candidate-0");
    // An interrupted directory-shaped credential removal is retried at
    // the credential deadline, rather than waiting for state expiry.
    fs::remove_file(candidate.join("auth.json"))?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(candidate.join("auth.json"))?;
    fs::write(candidate.join("auth.json/token"), "credential")?;
    let metadata = fs::metadata(candidate.join("auth.json"))?;
    let tombstone = candidate.join(tombstone_name(
        metadata.dev(),
        metadata.ino(),
        OsStr::new("auth.json"),
    ));
    fs::rename(candidate.join("auth.json"), &tombstone)?;
    let malformed = candidate.join(format!(
        "{TOMBSTONE}{}é{}-cache",
        "a".repeat(15),
        "a".repeat(15)
    ));
    fs::DirBuilder::new().mode(0o700).create(&malformed)?;
    let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!tombstone.exists());
    assert!(
        malformed.exists(),
        "malformed names grant no disposal authority"
    );
    // A prior sweep already moved one markerless candidate. The remaining
    // candidate must not change provider classification on retry.
    fs::remove_file(candidate.join("provider.txt"))?;
    fs::remove_file(state.join("candidate-1/provider.txt"))?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(attempt.join("provider-history"))?;
    fs::rename(&candidate, attempt.join("provider-history/candidate-0"))?;
    seal(&Directory::open_anchor(&attempt)?, EXPIRED, b"{}\n", 64)?;
    let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!state.exists());
    assert_eq!(
        fs::read_to_string(attempt.join("provider-history/candidate-1/unknown-session"))?,
        "unknown transcript"
    );
    Ok(())
}

struct Pair {
    _temp: tempfile::TempDir,
    context: ExecutionContext,
    store: RunStore,
    older: std::path::PathBuf,
    newer: std::path::PathBuf,
    newer_input: PreparedRun,
}

/// Two terminal attempts, each holding 256 KiB of provider state; the
/// first finished earlier.
fn pair() -> std::result::Result<Pair, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let first = input();
    let mut second = input();
    second.attempt_id = provenant_core::AttemptId::from_bytes([14; 16]);
    second.operation_id = provenant_core::OperationId::parse("run-test-b")?;
    second.attempt_id = AttemptId::from_bytes([16; 16]);
    crate::tests::rekey(&mut second);
    let mut paths = vec![];
    for (index, prepared) in [&first, &second].into_iter().enumerate() {
        store.create(prepared, &call)?;
        let attempt = context
            .state_root
            .as_path()
            .join(format!("attempts/{}", prepared.attempt_id));
        let candidate = attempt.join("provider-state/candidate-0");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&candidate)?;
        fs::write(candidate.join("provider.txt"), "codex")?;
        fs::write(candidate.join("blob"), vec![7_u8; 262_144])?;
        store.withdraw(
            prepared.attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(200 + index as i64 * 1_000)?,
            &call,
        )?;
        paths.push(attempt);
    }
    let newer = paths.pop().ok_or("attempt")?;
    let older = paths.pop().ok_or("attempt")?;
    Ok(Pair {
        _temp: temp,
        context,
        store,
        older,
        newer,
        newer_input: second,
    })
}

fn payload(
    pair: &Pair,
    cap: u64,
    age: i64,
    dry_run: bool,
    measure: bool,
) -> crate::Result<PayloadReport> {
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    pair.store.gc_payloads(
        &Directory::open_anchor(pair.context.state_root.as_path())?,
        cap,
        WallTime::from_micros(200 + age).map_err(|_| RunError::Validation("fixture time"))?,
        dry_run,
        measure,
        None,
        &call,
        &RetentionScan::default(),
    )
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn incomplete_payload_census_is_uncertain_and_never_grants_eviction() -> TestResult {
    let pair = pair()?;
    let mut path = pair.older.join(STATE).join("deep");
    for _ in 0..270 {
        fs::create_dir_all(&path)?;
        path.push("d");
    }
    fs::write(path.with_file_name("payload"), b"unmeasured")?;
    let report = payload(&pair, 0, 8 * DAY_MICROS, false, true)?;
    assert!(!report.errors.is_empty(), "partial census is visible");
    assert!(
        report.evicted.is_empty(),
        "an incomplete lower bound cannot authorise cap eviction"
    );
    let pressure = payload(&pair, 0, 8 * DAY_MICROS, false, false)?;
    assert!(
        !pressure.errors.is_empty(),
        "ledger-only pressure retains uncertainty"
    );
    assert!(pressure.evicted.is_empty());
    assert!(pair.older.join(STATE).exists() && pair.newer.join(STATE).exists());
    Ok(())
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn interrupted_payload_measurement_preserves_observed_totals_and_resumes() -> TestResult {
    let pair = pair()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let baseline_call = call(&clock, &cancel);
    let root = Directory::open_anchor(pair.context.state_root.as_path())?;
    let now = WallTime::from_micros(200 + 8 * DAY_MICROS)?;
    let checks = std::cell::Cell::new(0_u64);
    let count = || {
        checks.set(checks.get() + 1);
        true
    };
    let baseline = pair.store.gc_payloads(
        &root,
        u64::MAX,
        now,
        false,
        true,
        None,
        &baseline_call,
        &RetentionScan {
            keep_going: Some(&count),
            ..Default::default()
        },
    )?;
    let cutoff = checks.get() * 3 / 4;
    checks.set(0);
    let limited = || {
        checks.set(checks.get() + 1);
        checks.get() <= cutoff
    };
    let partial = pair.store.gc_payloads(
        &root,
        u64::MAX,
        now,
        false,
        true,
        None,
        &baseline_call,
        &RetentionScan {
            keep_going: Some(&limited),
            ..Default::default()
        },
    )?;
    let path = partial
        .interrupted_at
        .as_ref()
        .ok_or("budget did not interrupt")?;
    assert_eq!(partial.interrupted_phase, Some("measure"));
    assert!(partial.accounted_bytes > 0 && partial.accounted_bytes < baseline.accounted_bytes);
    let id = AttemptId::parse(
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or("attempt cursor")?,
    )?;
    let resume_cancel = CancelToken::new();
    let resumed_call = call(&clock, &resume_cancel);
    let complete = pair.store.gc_payloads(
        &root,
        u64::MAX,
        now,
        false,
        true,
        None,
        &resumed_call,
        &RetentionScan {
            start_at: Some(id),
            detail: Some("measure"),
            ..Default::default()
        },
    )?;
    assert!(complete.interrupted_at.is_none());
    assert_eq!(complete.accounted_bytes, baseline.accounted_bytes);
    assert!(pair.older.join(STATE).exists() && pair.newer.join(STATE).exists());
    Ok(())
}

#[test]
fn payload_pressure_evicts_oldest_first_down_to_low_water() -> TestResult {
    let pair = pair()?;
    let cap_bytes = 400_000;
    let (numerator, denominator) = PAYLOAD_LOW_WATER_RATIO;
    let expected_low_water =
        (u128::from(cap_bytes) * u128::from(numerator) / u128::from(denominator)) as u64;
    let report = payload(&pair, cap_bytes, 8 * DAY_MICROS, false, true)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.low_water_bytes, expected_low_water);
    assert_eq!(report.evicted.len(), 1, "{report:?}");
    assert_eq!(report.evicted[0].path, pair.older);
    assert!(report.reclaimed_bytes >= 262_144);
    assert!(!pair.older.join("provider-state").exists());
    assert!(pair.older.join("payload-evicted.json").exists());
    assert!(pair.older.join("provider-state-expired.json").exists());
    assert!(pair.newer.join("provider-state/candidate-0/blob").exists());
    // Under the low-water mark nothing more goes, and a rerun is a no-op.
    let again = payload(&pair, 400_000, 8 * DAY_MICROS, false, true)?;
    assert!(again.evicted.is_empty());
    assert_eq!(again.total_reclaimed_bytes, report.reclaimed_bytes);
    assert_eq!(again.pinned_bytes, 0);
    Ok(())
}

#[test]
fn payload_pressure_spares_recent_and_reports_them_pinned() -> TestResult {
    let pair = pair()?;
    let report = payload(&pair, 1_000, DAY_MICROS / 2, false, true)?;
    assert!(report.evicted.is_empty());
    assert!(report.accounted_bytes >= 524_288);
    assert_eq!(report.pinned_bytes, report.accounted_bytes);
    assert!(pair.older.join("provider-state/candidate-0/blob").exists());
    assert!(pair.newer.join("provider-state/candidate-0/blob").exists());
    Ok(())
}

#[test]
fn age_collection_keeps_unimported_git_until_custody_is_proven() -> TestResult {
    let pair = pair()?;
    let temporary = scratch_root_of(&pair.older).join("tmp");
    // Run-store scratch is private in production; match that mode here.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&temporary)?;
    fs::write(temporary.join("objects"), "unlanded work")?;
    fs::write(pair.older.join("writer-objects.json"), "{}")?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let report = sweep(&pair.store, &pair.context, 1, 8 * DAY_MICROS, false, &call)?;
    assert!(
        report
            .kept
            .iter()
            .any(|issue| issue.path.starts_with(&pair.older))
    );
    assert_eq!(
        fs::read_to_string(temporary.join("objects"))?,
        "unlanded work"
    );
    assert!(pair.older.join("provider-state/candidate-0/blob").exists());
    Ok(())
}

#[test]
fn payload_pressure_never_evicts_private_git_or_dry_runs() -> TestResult {
    let pair = pair()?;
    // Run-store scratch is private in production; a non-private root makes
    // the measurement report an unsafe path and blocks cap eviction.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(scratch_root_of(&pair.older).join("tmp"))?;
    fs::write(pair.older.join("writer-objects.json"), "{}")?;
    let preview = payload(&pair, 1_000, 8 * DAY_MICROS, true, true)?;
    assert_eq!(preview.evicted.len(), 1, "{preview:?}");
    assert_eq!(preview.evicted[0].path, pair.newer);
    assert!(preview.pinned_bytes >= 262_144);
    assert!(
        pair.newer.join("provider-state").exists(),
        "dry run removes nothing"
    );
    let report = payload(&pair, 1_000, 8 * DAY_MICROS, false, true)?;
    assert_eq!(report.evicted.len(), 1);
    assert!(pair.older.join("provider-state/candidate-0/blob").exists());
    assert!(!pair.newer.join("provider-state").exists());
    Ok(())
}

#[test]
fn payload_pressure_reads_the_ledger_without_measuring() -> TestResult {
    let pair = pair()?;
    let measured = payload(&pair, u64::MAX / 2, 8 * DAY_MICROS, false, true)?;
    assert!(measured.evicted.is_empty());
    // Growth after accounting is invisible to the ledger-only pass.
    fs::write(
        pair.older.join("provider-state/candidate-0/late"),
        vec![1_u8; 1 << 20],
    )?;
    let report = payload(&pair, 400_000, 8 * DAY_MICROS, false, false)?;
    assert_eq!(report.evicted.len(), 1, "{report:?}");
    assert!(report.evicted[0].bytes < 600_000, "{report:?}");
    Ok(())
}

#[test]
fn evicted_payload_keeps_records_tails_and_refuses_resume() -> TestResult {
    let pair = pair()?;
    let directory = Directory::open_anchor(&pair.older)?;
    crate::artifacts::seal(&directory, "stdout.log", &vec![b'x'; 200_000], 300_000)?;
    let result = fs::read(pair.older.join("result.md"))?;
    let report = payload(&pair, 400_000, 8 * DAY_MICROS, false, true)?;
    assert_eq!(report.evicted.len(), 1, "{report:?}");
    assert_eq!(fs::read(pair.older.join("stdout.log.tail"))?.len(), 65_536);
    assert!(!pair.older.join("stdout.log").exists());
    assert_eq!(fs::read(pair.older.join("result.md"))?, result);
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    assert!(matches!(
        pair.store.retain_provider_state(
            AttemptId::from_bytes([6; 16]),
            &call,
            &provenant_records::policy::Policy::default()
        ),
        Err(RunError::ProviderStateExpired)
    ));
    assert!(
        pair.store
            .record(pair.newer_input.attempt_id, &call)
            .is_ok()
    );
    assert!(
        pair.store
            .record(AttemptId::from_bytes([6; 16]), &call)
            .is_ok()
    );
    let totals = payload_totals(&Directory::open_anchor(pair.context.state_root.as_path())?)
        .ok_or("totals")?;
    assert_eq!(totals.1, report.reclaimed_bytes);
    Ok(())
}
