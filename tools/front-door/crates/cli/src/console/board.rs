//! Board envelope and opaque, filter-bound continuation tokens.
use super::Error;
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, Roots};
use provenant_fabric_runtime::chairs::{ChairIndex, Request as ChairRequest};
use provenant_os::clock::SystemClock;
use provenant_records::CanonicalV1;
use provenant_run::{AttemptState, ExecutionContext, Outcome, RunStore};
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;

fn binding(high_water: u64, filters: &Value) -> Result<String, Error> {
    Ok(
        CanonicalV1::record_digest("console-board", 2, &(high_water, filters))
            .map_err(|_| Error::Encode)?
            .to_string(),
    )
}

fn token(snapshot: &str, high_water: u64, before: u64) -> Result<String, Error> {
    let checksum = CanonicalV1::record_digest("console-board-position", 2, &(snapshot, before))
        .map_err(|_| Error::Encode)?;
    Ok(format!("2.{high_water}.{before}.{checksum}"))
}

fn cursor_position(cursor: &str, filters: &Value) -> Result<(u64, u64), Error> {
    let parts: Vec<_> = cursor.split('.').collect();
    let ["2", high_water, before, _] = parts.as_slice() else {
        return Err(Error::BoardCursor);
    };
    let high_water = high_water.parse::<u64>().map_err(|_| Error::BoardCursor)?;
    let before = before.parse::<u64>().map_err(|_| Error::BoardCursor)?;
    if before == 0
        || before > high_water
        || high_water > i64::MAX as u64
        || cursor != token(&binding(high_water, filters)?, high_water, before)?
    {
        return Err(Error::BoardCursor);
    }
    Ok((high_water, before))
}

#[allow(clippy::disallowed_methods)] // Explicit real-time deadline for the read-only Fabric boundary.
pub(super) fn read(
    roots: &Roots,
    selected: Option<ProjectId>,
    all: bool,
    limit: u64,
    cursor: Option<&str>,
) -> Result<Value, Error> {
    if !(1..=100).contains(&limit) {
        return Err(provenant_run::RunError::Validation("board page bounds").into());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: selected.unwrap_or(ProjectId::from_bytes([0; 16])),
        actor_id: ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let host = match provenant_config::lookup_host(roots) {
        Err(provenant_config::ConfigError::ProjectBindingFilesystem(error))
            if error.is_not_found() =>
        {
            None
        }
        found => found?,
    };
    let root = CanonicalV1::file_digest(roots.state.as_path().as_os_str().as_bytes());
    let filters =
        json!({"state_root":root,"host":host,"project_id":selected,"all":all,"limit":limit});
    let position = cursor
        .map(|cursor| cursor_position(cursor, &filters))
        .transpose()?;
    let store = RunStore::open_read_only(&context, &call)?;
    if position.is_some() && store.is_none() {
        return Err(Error::BoardCursor);
    }
    let page = store
        .as_ref()
        .map(|store| {
            store.board_page(
                selected,
                all,
                limit,
                position.map_or(0, |p| p.1),
                position.map(|p| p.0),
                &call,
            )
        })
        .transpose()
        .map_err(|error| match error {
            provenant_run::RunError::Validation("board high-water mark") => Error::BoardCursor,
            other => other.into(),
        })?;
    let generation = page
        .as_ref()
        .map_or("empty", |p| p.generation.as_str())
        .to_owned();
    let high_water = page.as_ref().map_or(0, |page| page.high_water);
    let snapshot = binding(high_water, &filters)?;
    let chair_call = ChairRequest {
        clock: &clock,
        cancel: &cancel,
    };
    let chairs = ChairIndex::open_read_only(
        &roots.state,
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    );
    let mut chair_coverage = if matches!(chairs, Ok(Some(_))) {
        "complete"
    } else {
        "unavailable"
    };
    let mut lanes = Vec::new();
    let mut observed = std::collections::BTreeSet::new();
    let next = page
        .as_ref()
        .and_then(|page| page.next)
        .map(|offset| token(&snapshot, high_water, offset))
        .transpose()?;
    if let Some(mut page) = page {
        let attempts: Vec<_> = page.lanes.iter().map(|lane| lane.attempt.clone()).collect();
        for lane in &mut page.lanes {
            lane.attempt.last_output_at = None;
            lane.attempt.priority = None;
            lane.attempt.provider_silent_for_ms = None;
        }
        let mut waits = Vec::new();
        let projects: std::collections::BTreeSet<_> =
            attempts.iter().map(|a| a.project_id).collect();
        for project_id in projects {
            let context = ExecutionContext {
                project_id,
                ..context.clone()
            };
            if let Ok(Some(store)) = RunStore::open_read_only(&context, &call) {
                for lane in &mut page.lanes {
                    if lane.attempt.project_id == project_id
                        && let Ok(record) = store.record(lane.attempt.attempt_id, &call)
                        && record.generation == lane.attempt.generation
                    {
                        lane.attempt.last_output_at = record.last_output_at;
                        lane.attempt.priority = record.priority;
                        lane.attempt.provider_silent_for_ms = record.provider_silent_for_ms;
                        if record.process_observation_valid {
                            observed.insert(record.attempt_id);
                        }
                    }
                }
                let selected: Vec<_> = page
                    .lanes
                    .iter()
                    .map(|lane| &lane.attempt)
                    .filter(|attempt| attempt.project_id == project_id)
                    .cloned()
                    .collect();
                waits.extend(store.lane_waits(&selected, &call));
            }
        }
        for lane in page.lanes {
            let attempt = &lane.attempt;
            let chair = match &chairs {
                Ok(Some(index)) => {
                    match index.read(attempt.project_id, attempt.run_id, &chair_call) {
                        Ok(Some(found)) if found.request_digest == lane.run.request_digest => {
                            found.chair
                        }
                        Ok(None) => None,
                        _ => {
                            chair_coverage = "partial";
                            None
                        }
                    }
                }
                _ => None,
            };
            let usage = serde_json::to_value(&attempt.usage).map_err(|_| Error::Encode)?;
            let mut known = Vec::new();
            let mut missing = Vec::new();
            for key in [
                "uncached_input",
                "cache_read",
                "cache_write",
                "output_excluding_reasoning",
                "reasoning",
            ] {
                if usage[key].is_null() {
                    missing.push(key)
                } else {
                    known.push(key)
                }
            }
            let attention = matches!(attempt.state, AttemptState::Terminal { outcome, .. }
                if !matches!(outcome, Outcome::Succeeded | Outcome::Cancelled));
            lanes.push(json!({"lane_id":{"host":host,"run_id":attempt.run_id,
                "task_id":attempt.task_id,"attempt_id":attempt.attempt_id},
                "chair":chair,"metadata":lane,"wait":waits.iter().find(|w| w.attempt_id == attempt.attempt_id),
                "attention":attention,"usage_coverage":{"known":known,"missing":missing},
                "observation_coverage":if observed.contains(&attempt.attempt_id) { "complete" } else { "unavailable" },
                "estimated_cost":null,"verified_links":null}));
        }
    }
    Ok(
        json!({"schema_version":2,"snapshot":snapshot,"high_water":high_water,"generation":generation,
        "observed_at":clock.wall_now().map_err(|_| Error::Encode)?,"filters":filters,
        "lanes":lanes,"next_cursor":next,"truncated":next.is_some(),
        "coverage":{"scope":"local","host":host,"runs":if store.is_some() { "complete" } else { "empty" },"chairs":chair_coverage,
            "observations":if observed.len() == lanes.len() { "complete" } else if observed.is_empty() { "unavailable" } else { "partial" },
            "nested_runs":"unavailable","estimated_cost":"unavailable","verified_links":"unavailable"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods, clippy::print_stderr)] // Measured read boundary and benchmark report.
    fn board_paging_end_to_end_and_100_row_p95() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{AttemptId, Digest, OperationId, RootInputs, RunId, TaskId};
        use provenant_run::{CanonicalRequest, NewAttempt, PreparedRun};
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        std::fs::create_dir(base.join("home"))?;
        std::fs::create_dir(base.join("checkout"))?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        assert_eq!(read(&roots, None, false, 50, None)?["lanes"], json!([]));
        assert!(!roots.state.as_path().join("run.sqlite3").exists());
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let project = ProjectId::from_bytes([1; 16]);
        let context = ExecutionContext {
            state_root: roots.state.clone(),
            project_id: project,
            actor_id: ActorId::from_bytes([2; 16]),
            read_prompts: false,
        };
        let store = RunStore::open(&context, &call)?;
        let task = TaskId::from_bytes([3; 16]);
        let mut request: CanonicalRequest = serde_json::from_value(json!({"schema_version":1,
            "tasks":[{"task_id":task,"title":"Board fixture","prompt_digest":CanonicalV1::file_digest(b"private body"),
            "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},
            "fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}],
            "concurrency":1,"parent":null,"lane_depth":1}))?;
        request.chair = Some(provenant_run::ChairBinding {
            extra: Default::default(),
            session: None,
            host: provenant_core::HostId::from_bytes([4; 16]),
            actor: context.actor_id,
        });
        let other_project = ProjectId::from_bytes([9; 16]);
        let other = RunStore::open(
            &ExecutionContext {
                project_id: other_project,
                ..context.clone()
            },
            &call,
        )?;
        let prepared = |n: u8| -> Result<PreparedRun, Box<dyn std::error::Error>> {
            Ok(PreparedRun {
                run_id: RunId::from_bytes([n; 16]),
                operation_id: OperationId::parse(&format!("board-{n}"))?,
                created_at: clock.wall_now()?,
                request: request.clone(),
                attempts: vec![NewAttempt {
                    task_id: task,
                    attempt_id: AttemptId::from_bytes([n; 16]),
                    prompt: "private body".into(),
                }],
            })
        };
        for n in 1..=101u8 {
            if n % 2 == 0 { &other } else { &store }.create(&prepared(n)?, &call)?;
        }
        let first = read(&roots, None, false, 50, None)?;
        assert_eq!(first["schema_version"], 2);
        assert_eq!(first["coverage"]["chairs"], "unavailable");
        assert_eq!(first["lanes"].as_array().unwrap().len(), 50);
        assert_eq!(
            first["lanes"][0]["wait"]["waiting_for"],
            "launch: not yet handed to a guardian"
        );
        assert_eq!(
            first["lanes"][0]["metadata"]["attempt"]["usage"]["uncached_input"],
            Value::Null
        );
        assert_eq!(
            first["lanes"][0]["usage_coverage"]["missing"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert!(!serde_json::to_string(&first)?.contains("private body"));
        assert!(!roots.state.as_path().join("fabric").exists());
        assert_eq!(
            first["lanes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["metadata"]["attempt"]["project_id"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            2
        );
        assert!(
            first["lanes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["wait"]["waiting_for"].is_string())
        );
        let selected = read(&roots, Some(project), false, 100, None)?;
        let selected_page = read(&roots, Some(project), false, 50, None)?;
        let selected_cursor = selected_page["next_cursor"]
            .as_str()
            .ok_or("selected cursor")?;
        assert_eq!(selected["lanes"].as_array().unwrap().len(), 51);
        assert!(
            selected["lanes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["metadata"]["attempt"]["project_id"] == json!(project))
        );
        use std::os::unix::fs::PermissionsExt;
        let id = AttemptId::from_bytes([101; 16]);
        let checkpoint = roots.state.as_path().join(format!(
            "runs/{}/attempts/{id}/process.json",
            RunId::from_bytes([101; 16])
        ));
        let observed_at = clock.wall_now()?;
        std::fs::write(
            &checkpoint,
            serde_json::to_vec(
                &json!({"schema_version":1,"scope":{"run":RunId::from_bytes([101;16]),"attempt":id,"generation":0},"last_output_at":observed_at,"priority":15}),
            )?,
        )?;
        std::fs::set_permissions(&checkpoint, std::fs::Permissions::from_mode(0o600))?;
        let observed = read(&roots, None, false, 50, None)?;
        assert_eq!(observed["lanes"][0]["metadata"]["attempt"]["priority"], 15);
        assert_eq!(
            observed["lanes"][0]["metadata"]["attempt"]["last_output_at"],
            json!(observed_at)
        );
        assert_eq!(observed["snapshot"], first["snapshot"]);
        let checkpoint_bytes = std::fs::read(&checkpoint)?;
        for bytes in [
            b"{".to_vec(),
            serde_json::to_vec(
                &json!({"schema_version":1,"scope":{"run":RunId::from_bytes([101;16]),"attempt":id,"generation":999}}),
            )?,
        ] {
            std::fs::write(&checkpoint, bytes)?;
            let partial = read(&roots, None, false, 50, None)?;
            assert_eq!(partial["lanes"].as_array().unwrap().len(), 50);
            assert_eq!(partial["coverage"]["observations"], "partial");
            assert_eq!(partial["lanes"][0]["observation_coverage"], "unavailable");
            assert_eq!(
                read(&roots, None, false, 1, None)?["coverage"]["observations"],
                "unavailable"
            );
            assert!(partial["lanes"][0]["metadata"]["attempt"]["priority"].is_null());
            assert!(partial["lanes"][0]["metadata"]["attempt"]["last_output_at"].is_null());
        }
        std::fs::write(&checkpoint, checkpoint_bytes)?;
        std::fs::set_permissions(&checkpoint, std::fs::Permissions::from_mode(0o644))?;
        assert_eq!(
            read(&roots, None, false, 50, None)?["coverage"]["observations"],
            "partial"
        );
        std::fs::set_permissions(&checkpoint, std::fs::Permissions::from_mode(0o600))?;
        let chair_call = ChairRequest {
            clock: &clock,
            cancel: &cancel,
        };
        ChairIndex::open(&roots.state, &chair_call)?.submit(
            project,
            RunId::from_bytes([101; 16]),
            &request,
            &chair_call,
        )?;
        assert_eq!(
            read(&roots, None, false, 50, None)?["coverage"]["chairs"],
            "complete"
        );
        assert_eq!(
            read(&roots, None, false, 50, None)?["lanes"][0]["chair"],
            json!(request.chair)
        );
        let mut forged = request.clone();
        forged.chair = None;
        ChairIndex::open(&roots.state, &chair_call)?.submit(
            other_project,
            RunId::from_bytes([100; 16]),
            &forged,
            &chair_call,
        )?;
        let mismatch = read(&roots, None, false, 50, None)?;
        assert!(mismatch["lanes"][1]["chair"].is_null());
        assert_eq!(mismatch["coverage"]["chairs"], "partial");
        let cursor = first["next_cursor"].as_str().ok_or("missing cursor")?;
        for malformed in ["", "junk", "g:0", "g:-1", "g:18446744073709551615"] {
            let error = read(&roots, None, false, 50, Some(malformed)).unwrap_err();
            assert!(matches!(error, Error::BoardCursor));
        }
        let second = read(&roots, None, false, 50, Some(cursor))?;
        assert_ne!(first["lanes"][0]["lane_id"], second["lanes"][0]["lane_id"]);
        for (selected, all, limit) in [
            (None, true, 50),
            (Some(project), false, 50),
            (None, false, 100),
        ] {
            assert!(matches!(
                read(&roots, selected, all, limit, Some(cursor)),
                Err(Error::BoardCursor)
            ));
        }
        let edited = cursor.replacen(".52.", ".51.", 1);
        assert_ne!(edited, cursor);
        assert!(matches!(
            read(&roots, None, false, 50, Some(&edited)),
            Err(Error::BoardCursor)
        ));
        let mut other_inputs = inputs.clone();
        other_inputs.state_dir = Some(base.join("other-state"));
        let other_roots = other_inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let filters = json!({"state_root":CanonicalV1::file_digest(other_roots.state.as_path().as_os_str().as_bytes()),"host":null,"project_id":null,"all":false,"limit":50});
        assert!(matches!(
            cursor_position(cursor, &filters),
            Err(Error::BoardCursor)
        ));
        let mut samples = Vec::new();
        for _ in 0..20 {
            let start = std::time::Instant::now();
            let page = read(&roots, None, false, 100, None)?;
            assert_eq!(page["lanes"].as_array().unwrap().len(), 100);
            assert!(serde_json::to_vec(&page)?.len() < crate::MAX_RESULT_BYTES);
            samples.push(start.elapsed());
        }
        samples.sort();
        eprintln!(
            "board_paging 100-row p95: {:?} (20 samples; read + JSON encoding)",
            samples[18]
        );
        let walk = read(&roots, None, true, 50, None)?;
        other.create(&prepared(102)?, &call)?;
        store.withdraw(
            AttemptId::from_bytes([1; 16]),
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "test",
            clock.wall_now()?,
            &call,
        )?;
        let resumed = read(&roots, None, false, 50, Some(cursor))?;
        assert_ne!(resumed["generation"], first["generation"]);
        assert_eq!(resumed["snapshot"], first["snapshot"]);
        let empty = read(&roots, Some(project), false, 50, Some(selected_cursor))?;
        assert_eq!(empty["lanes"], json!([]));
        assert!(empty["next_cursor"].is_null());
        assert_eq!(empty["coverage"]["observations"], "complete");
        other.withdraw(
            AttemptId::from_bytes([2; 16]),
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Rejected,
            "review",
            clock.wall_now()?,
            &call,
        )?;
        let mut seen = std::collections::BTreeSet::new();
        let mut page = walk.clone();
        loop {
            for row in page["lanes"].as_array().unwrap() {
                let id = row["lane_id"]["attempt_id"].as_str().unwrap().to_owned();
                assert!(seen.insert(id.clone()), "duplicate {id}");
                if id == AttemptId::from_bytes([2; 16]).to_string() {
                    assert_eq!(row["attention"], true);
                }
                if id == AttemptId::from_bytes([1; 16]).to_string() {
                    assert_eq!(row["attention"], false);
                }
            }
            let Some(cursor) = page["next_cursor"].as_str() else {
                break;
            };
            let next = read(&roots, None, true, 50, Some(cursor))?;
            assert_eq!(next["snapshot"], walk["snapshot"]);
            assert_eq!(next["high_water"], walk["high_water"]);
            assert_ne!(next["generation"], walk["generation"]);
            page = next;
        }
        assert_eq!(
            seen,
            (1..=101)
                .map(|n| AttemptId::from_bytes([n; 16]).to_string())
                .collect()
        );
        let active = read(&roots, None, false, 100, None)?;
        assert_eq!(active["lanes"].as_array().unwrap().len(), 100);
        assert!(
            active["lanes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["metadata"]["attempt"]["state"] == json!({"state":"queued"}))
        );
        Ok(())
    }
}
