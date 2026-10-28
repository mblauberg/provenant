//! Board envelope and opaque, filter-bound continuation tokens.
use super::Error;
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, Roots, WaitBudget};
use provenant_fabric_runtime::chairs::{ChairIndex, Request as ChairRequest, ThreadWait};
use provenant_os::clock::SystemClock;
use provenant_records::CanonicalV1;
use provenant_run::{AttemptState, ExecutionContext, Outcome, RunStore};
use serde_json::{Value, json};

fn binding(generation: &str, filters: &Value) -> Result<String, Error> {
    Ok(
        CanonicalV1::record_digest("console-board", 1, &(generation, filters))
            .map_err(|_| Error::Encode)?
            .to_string(),
    )
}

fn cursor_offset(cursor: Option<&str>) -> Result<u64, Error> {
    let Some(cursor) = cursor else { return Ok(0) };
    let (_, offset) = cursor.rsplit_once(':').ok_or(Error::BoardCursor)?;
    let offset = offset.parse::<u64>().map_err(|_| Error::BoardCursor)?;
    if offset == 0 || offset > i64::MAX as u64 {
        return Err(Error::BoardCursor);
    }
    Ok(offset)
}

#[allow(clippy::disallowed_methods)] // Explicit real-time deadline for the read-only Fabric boundary.
pub(super) fn read(
    roots: &Roots,
    selected: Option<ProjectId>,
    all: bool,
    limit: u64,
    cursor: Option<&str>,
) -> Result<Value, Error> {
    let after = cursor_offset(cursor)?;
    if !(1..=100).contains(&limit) {
        return Err(provenant_run::RunError::Validation("board page bounds").into());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Encode)?;
    let call = provenant_run::Request {
        clock: &clock,
        budget,
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
    let filters = json!({"host":host,"project_id":selected,"all":all,"limit":limit});
    let store = RunStore::open_read_only(&context, &call)?;
    let page = store
        .as_ref()
        .map(|store| store.board_page(selected, all, limit, after, &call))
        .transpose()?;
    let generation = page
        .as_ref()
        .map_or("empty", |p| p.generation.as_str())
        .to_owned();
    let snapshot = binding(&generation, &filters)?;
    if let Some(cursor) = cursor
        && (cursor != format!("{snapshot}:{after}")
            || page.as_ref().is_none_or(|p| p.lanes.is_empty()))
    {
        return Err(Error::BoardCursor);
    }
    let chair_call = ChairRequest {
        clock: &clock,
        budget,
        cancel: &cancel,
        backoff: &ThreadWait,
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
    let next = page
        .as_ref()
        .and_then(|page| page.next)
        .map(|offset| format!("{snapshot}:{offset}"));
    if let Some(mut page) = page {
        let attempts: Vec<_> = page.lanes.iter().map(|lane| lane.attempt.clone()).collect();
        let mut waits = Vec::new();
        let projects: std::collections::BTreeSet<_> =
            attempts.iter().map(|a| a.project_id).collect();
        for project_id in projects {
            let context = ExecutionContext {
                project_id,
                ..context.clone()
            };
            if let Some(store) = RunStore::open_read_only(&context, &call)? {
                for lane in &mut page.lanes {
                    if lane.attempt.project_id == project_id {
                        let observed = store.record(lane.attempt.attempt_id, &call)?;
                        if observed.generation == lane.attempt.generation {
                            lane.attempt.last_output_at = observed.last_output_at;
                            lane.attempt.priority = observed.priority;
                        }
                    }
                }
                let selected: Vec<_> = attempts
                    .iter()
                    .filter(|a| a.project_id == project_id)
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
                "estimated_cost":null,"verified_links":null}));
        }
    }
    Ok(
        json!({"schema_version":1,"snapshot":snapshot,"generation":generation,
        "observed_at":clock.wall_now().map_err(|_| Error::Encode)?,"filters":filters,
        "lanes":lanes,"next_cursor":next,"truncated":next.is_some(),
        "coverage":{"scope":"local","host":host,"runs":"complete","chairs":chair_coverage,
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
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
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
        let request: CanonicalRequest = serde_json::from_value(json!({"schema_version":1,
            "tasks":[{"task_id":task,"title":"Board fixture","prompt_digest":CanonicalV1::file_digest(b"private body"),
            "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},
            "fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}],
            "concurrency":1,"parent":null,"lane_depth":1}))?;
        for n in 1..=101u8 {
            store.create(
                &PreparedRun {
                    run_id: RunId::from_bytes([n; 16]),
                    operation_id: OperationId::parse(&format!("board-{n}"))?,
                    created_at: clock.wall_now()?,
                    request: request.clone(),
                    attempts: vec![NewAttempt {
                        task_id: task,
                        attempt_id: AttemptId::from_bytes([n; 16]),
                        prompt: "private body".into(),
                    }],
                },
                &call,
            )?;
        }
        let first = read(&roots, None, false, 50, None)?;
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
        std::fs::set_permissions(checkpoint, std::fs::Permissions::from_mode(0o600))?;
        let observed = read(&roots, None, false, 50, None)?;
        assert_eq!(observed["lanes"][0]["metadata"]["attempt"]["priority"], 15);
        assert_eq!(
            observed["lanes"][0]["metadata"]["attempt"]["last_output_at"],
            json!(observed_at)
        );
        assert_eq!(observed["snapshot"], first["snapshot"]);
        let chair_call = ChairRequest {
            clock: &clock,
            budget: call.budget,
            cancel: &cancel,
            backoff: &ThreadWait,
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
        let cursor = first["next_cursor"].as_str().ok_or("missing cursor")?;
        for malformed in ["", "junk", "g:0", "g:-1", "g:18446744073709551615"] {
            let error = read(&roots, None, false, 50, Some(malformed)).unwrap_err();
            assert!(matches!(error, Error::BoardCursor));
            assert!(error.to_string().contains("without --cursor"));
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
        assert!(samples[18] <= std::time::Duration::from_secs(1));
        store.withdraw(
            AttemptId::from_bytes([1; 16]),
            AttemptState::Queued,
            Outcome::Cancelled,
            "test",
            clock.wall_now()?,
            &call,
        )?;
        assert!(matches!(
            read(&roots, None, false, 50, Some(cursor)),
            Err(Error::BoardCursor)
        ));
        Ok(())
    }
}
