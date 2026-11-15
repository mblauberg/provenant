//! Board envelope and opaque, filter-bound continuation tokens.
use super::{Error, PAGE_SIZE_LIMIT};
use provenant_core::{ActorId, CancelToken, ProjectId, Roots};
use provenant_fabric_runtime::chairs::{ChairIndex, Request as ChairRequest};
use provenant_os::clock::SystemClock;
use provenant_records::CanonicalV1;
use provenant_run::{AttemptState, ExecutionContext, Outcome, RunStore};
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;

const BOARD_SCHEMA_VERSION: u64 = 3;

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

pub(super) fn read(
    roots: &Roots,
    selected: Option<ProjectId>,
    all: bool,
    limit: u64,
    cursor: Option<&str>,
) -> Result<Value, Error> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let request = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    read_with_request(roots, selected, all, limit, cursor, &request)
}

#[allow(clippy::disallowed_methods)] // Explicit real-time deadline for the read-only Fabric boundary.
fn read_with_request(
    roots: &Roots,
    selected: Option<ProjectId>,
    all: bool,
    limit: u64,
    cursor: Option<&str>,
    request: &provenant_run::Request<'_>,
) -> Result<Value, Error> {
    if !(1..=PAGE_SIZE_LIMIT).contains(&limit) {
        return Err(provenant_run::RunError::Validation("board page bounds").into());
    }
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
    let store = RunStore::open_read_only(&context, request)?;
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
                request,
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
        clock: request.clock,
        cancel: request.cancel,
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
            lane.attempt.provider_silent_for_ms.0 = None;
        }
        let mut waits = Vec::new();
        let projects: std::collections::BTreeSet<_> =
            attempts.iter().map(|a| a.project_id).collect();
        for project_id in projects {
            let context = ExecutionContext {
                project_id,
                ..context.clone()
            };
            if let Ok(Some(store)) = RunStore::open_read_only(&context, request) {
                for lane in &mut page.lanes {
                    if lane.attempt.project_id == project_id
                        && let Ok(record) = store.record(lane.attempt.attempt_id, request)
                        && record.generation == lane.attempt.generation
                    {
                        lane.attempt.last_output_at = record.last_output_at;
                        lane.attempt.priority = record.priority;
                        lane.attempt.provider_silent_for_ms.0 = record.provider_silent_for_ms.0;
                        if record.process_observation_valid.0 {
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
                waits.extend(store.lane_waits(&selected, request));
            }
        }
        for lane in page.lanes {
            let attempt = &lane.attempt;
            let work_key = &attempt.key;
            let chair = match &chairs {
                Ok(Some(index)) => {
                    match index.read(attempt.project_id, work_key.graph(), &chair_call) {
                        Ok(Some(found))
                            if found.provenance == Some(lane.operation.request_digest) =>
                        {
                            found.holder.chair().cloned()
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
            let usage =
                serde_json::to_value(attempt.effective_usage()).map_err(|_| Error::Encode)?;
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
            let projected = crate::run::project_lane(
                attempt,
                &waits,
                roots.state.as_path(),
                request.clock.wall_now().ok(),
                false,
            );
            lanes.push(json!({"lane_id":{"host":host,"key":work_key,
                "attempt_id":projected.attempt_id},
                "chair":chair,"metadata":{"operation":lane.operation, "attempt":projected},"wait":waits.iter().find(|w| w.attempt_id == attempt.attempt_id),
                "attention":attention,"usage_coverage":{"known":known,"missing":missing},
                "observation_coverage":if observed.contains(&attempt.attempt_id) { "complete" } else { "unavailable" }}));
        }
    }
    Ok(
        json!({"schema_version":BOARD_SCHEMA_VERSION,"snapshot":snapshot,"high_water":high_water,"generation":generation,
        "observed_at":request.clock.wall_now().map_err(|_| Error::Encode)?,"filters":filters,
        "lanes":lanes,"next_cursor":next,"truncated":next.is_some(),
        "coverage":{"scope":"local","host":host,"runs":if store.is_some() { "complete" } else { "empty" },"chairs":chair_coverage,
            "observations":if observed.len() == lanes.len() { "complete" } else if observed.is_empty() { "unavailable" } else { "partial" },
            "nested_runs":"unavailable"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, MonoTime, OperationId, ProjectId,
        RootInputs, TestClock, WallTime,
    };
    use provenant_records::CanonicalV1;
    use provenant_run::{CanonicalRequest, PreparedRun, TerminalPublication};
    use std::collections::BTreeSet;

    const PAGE: u64 = PAGE_SIZE_LIMIT / 2;
    const ATTEMPTS: u8 = PAGE_SIZE_LIMIT as u8 + 1;

    struct BoardFixture {
        _temp: tempfile::TempDir,
        base: std::path::PathBuf,
        inputs: RootInputs,
        roots: Roots,
        clock: TestClock,
        cancel: CancelToken,
        project: ProjectId,
        other_project: ProjectId,
        request: CanonicalRequest,
        attempts: u8,
        store: RunStore,
        other: RunStore,
        baseline_changes: Value,
    }

    impl BoardFixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            Self::with_attempts(ATTEMPTS)
        }

        fn with_attempts(attempts: u8) -> Result<Self, Box<dyn std::error::Error>> {
            let temp = tempfile::tempdir()?;
            let base = temp.path().canonicalize()?;
            std::fs::create_dir(base.join("home"))?;
            std::fs::create_dir(base.join("checkout"))?;
            let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
            inputs.state_dir = Some(base.join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let baseline_changes = super::super::read(
                &roots,
                None,
                &super::super::Console::Changes { since: None },
            )?;
            let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
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
            let mut request: CanonicalRequest = serde_json::from_value(json!({"schema_version":1,
                "task":{"title":"Board fixture","prompt_digest":CanonicalV1::file_digest(b"private body"),
                "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},
                "authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null},
                "parent":null,"lane_depth":1}))?;
            request.chair = Some(provenant_records::custody::Chair {
                process: crate::fabric::chair_process(),
                extra: Default::default(),
                session: None,
                host: provenant_core::HostId::from_bytes([4; 16]),
                actor: context.actor_id,
            });
            let other_project = ProjectId::from_bytes([9; 16]);
            let other = RunStore::open(
                &ExecutionContext {
                    project_id: other_project,
                    ..context
                },
                &call,
            )?;
            for n in 1..=attempts {
                let target = if n % 2 == 0 { &other } else { &store };
                target.create(&Self::prepared(&request, &clock, n)?, &call)?;
            }
            Ok(Self {
                _temp: temp,
                base,
                inputs,
                roots,
                clock,
                cancel,
                project,
                other_project,
                request,
                attempts,
                store,
                other,
                baseline_changes,
            })
        }

        fn prepared(
            request: &CanonicalRequest,
            clock: &TestClock,
            n: u8,
        ) -> Result<PreparedRun, Box<dyn std::error::Error>> {
            let mut request = request.clone();
            request.task.context = Some(provenant_records::run::WorkContext {
                graph_id: Some(format!("lane-{n}")),
                node_id: Some("node".into()),
                attempt: Some(1),
                ..Default::default()
            });
            Ok(PreparedRun {
                attempt_id: AttemptId::from_bytes([n; 16]),
                operation_id: OperationId::parse(&format!("board-{n}"))?,
                created_at: clock.wall_now()?,
                request,
                prompt: "private body".into(),
            })
        }

        fn call(&self) -> provenant_run::Request<'_> {
            provenant_run::Request {
                clock: &self.clock,
                cancel: &self.cancel,
            }
        }

        fn chair_call(&self) -> ChairRequest<'_> {
            ChairRequest {
                clock: &self.clock,
                cancel: &self.cancel,
            }
        }

        fn page(
            &self,
            selected: Option<ProjectId>,
            all: bool,
            limit: u64,
            cursor: Option<&str>,
        ) -> Result<Value, Error> {
            let request = self.call();
            read_with_request(&self.roots, selected, all, limit, cursor, &request)
        }

        fn prepared_attempt(&self, n: u8) -> Result<PreparedRun, Box<dyn std::error::Error>> {
            Self::prepared(&self.request, &self.clock, n)
        }

        fn id(n: u8) -> AttemptId {
            AttemptId::from_bytes([n; 16])
        }

        fn last_attempt(&self) -> AttemptId {
            Self::id(self.attempts)
        }
    }

    #[test]
    fn empty_board_read_does_not_create_the_run_store() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        std::fs::create_dir(base.join("home"))?;
        std::fs::create_dir(base.join("checkout"))?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let request = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        assert_eq!(
            read_with_request(&roots, None, false, PAGE, None, &request)?["lanes"],
            json!([])
        );
        assert!(!roots.state.as_path().join("run.sqlite3").exists());
        Ok(())
    }

    #[test]
    fn board_changes_advance_after_run_creation() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::with_attempts(2)?;
        let changed = super::super::read(
            &fixture.roots,
            None,
            &super::super::Console::Changes {
                since: Some(fixture.baseline_changes["generations"].to_string()),
            },
        )?;
        assert_eq!(changed["changed"]["board"], true);
        assert_eq!(changed["changed"]["runs"], true);
        let stable = super::super::read(
            &fixture.roots,
            None,
            &super::super::Console::Changes {
                since: Some(changed["generations"].to_string()),
            },
        )?;
        assert_eq!(stable["changed"]["board"], false);
        Ok(())
    }

    #[test]
    fn board_projection_has_declared_shape_and_omits_private_prompt()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::new()?;
        let page = fixture.page(None, false, PAGE, None)?;
        assert_eq!(page["schema_version"], BOARD_SCHEMA_VERSION);
        assert_eq!(
            page["lanes"].as_array().ok_or("lanes")?.len(),
            PAGE as usize
        );
        let lane = &page["lanes"][0];
        assert!(lane["lane_id"]["key"].is_object());
        assert!(lane["lane_id"].get("run_id").is_none());
        assert!(lane["lane_id"].get("task_id").is_none());
        assert!(lane.get("estimated_cost").is_none());
        assert!(lane.get("verified_links").is_none());
        assert_eq!(page["coverage"]["chairs"], "unavailable");
        assert_eq!(
            lane["usage_coverage"]["missing"]
                .as_array()
                .ok_or("usage coverage")?
                .len(),
            provenant_telemetry::ingest::BUCKETS.len()
        );
        assert!(!serde_json::to_string(&page)?.contains("private body"));
        assert!(!fixture.roots.state.as_path().join("fabric").exists());
        Ok(())
    }

    #[test]
    fn board_detail_uses_the_declared_work_identity() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::with_attempts(1)?;
        let detail =
            super::super::detail::read(&fixture.roots, &fixture.last_attempt().to_string())?;
        assert_eq!(detail["status"], "found");
        assert!(detail["lane_id"]["key"].is_object());
        assert!(detail["lane_id"].get("run_id").is_none());
        assert!(detail["lane_id"].get("task_id").is_none());
        assert_eq!(detail["artifacts"][0]["bytes"], "private body".len());
        assert_eq!(detail["artifacts"][0]["present"], true);
        Ok(())
    }

    #[test]
    fn board_selection_returns_only_the_selected_project() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = BoardFixture::with_attempts(5)?;
        for (project, odd_attempts) in [(fixture.project, true), (fixture.other_project, false)] {
            let page = fixture.page(Some(project), false, PAGE_SIZE_LIMIT, None)?;
            let actual: BTreeSet<_> = page["lanes"]
                .as_array()
                .ok_or("lanes")?
                .iter()
                .map(|lane| {
                    lane["lane_id"]["attempt_id"]
                        .as_str()
                        .ok_or("attempt id")
                        .map(str::to_owned)
                })
                .collect::<Result<_, _>>()?;
            let expected: BTreeSet<_> = (1..=fixture.attempts)
                .filter(|n| (n % 2 == 1) == odd_attempts)
                .map(|n| BoardFixture::id(n).to_string())
                .collect();
            assert_eq!(actual, expected);
            assert!(
                page["lanes"]
                    .as_array()
                    .ok_or("lanes")?
                    .iter()
                    .all(|lane| { lane["metadata"]["attempt"]["project_id"] == json!(project) })
            );
        }
        let empty = fixture.page(
            Some(ProjectId::from_bytes([8; 16])),
            false,
            PAGE_SIZE_LIMIT,
            None,
        )?;
        assert_eq!(empty["lanes"], json!([]));
        Ok(())
    }

    #[test]
    fn board_pages_follow_cursors_without_gaps_or_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::new()?;
        let first = fixture.page(None, true, PAGE, None)?;
        let snapshot = first["snapshot"].clone();
        let high_water = first["high_water"].clone();
        let generation = first["generation"].clone();
        let mut seen = BTreeSet::new();
        let mut page = first;
        loop {
            let lanes = page["lanes"].as_array().ok_or("lanes")?;
            assert!(lanes.len() <= PAGE as usize);
            for lane in lanes {
                let id = lane["lane_id"]["attempt_id"]
                    .as_str()
                    .ok_or("attempt id")?
                    .to_owned();
                assert!(seen.insert(id));
            }
            assert_eq!(page["snapshot"], snapshot);
            assert_eq!(page["high_water"], high_water);
            assert_eq!(page["generation"], generation);
            let Some(cursor) = page["next_cursor"].as_str() else {
                break;
            };
            page = fixture.page(None, true, PAGE, Some(cursor))?;
        }
        let expected: BTreeSet<_> = (1..=fixture.attempts)
            .map(|n| BoardFixture::id(n).to_string())
            .collect();
        assert_eq!(seen, expected);
        Ok(())
    }

    #[test]
    fn board_cursors_are_bound_to_filters_and_state_root() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = BoardFixture::new()?;
        let first = fixture.page(None, false, PAGE, None)?;
        let cursor = first["next_cursor"].as_str().ok_or("cursor")?;
        for (selected, all, limit) in [
            (None, true, PAGE),
            (Some(fixture.project), false, PAGE),
            (None, false, PAGE_SIZE_LIMIT),
        ] {
            assert!(matches!(
                fixture.page(selected, all, limit, Some(cursor)),
                Err(Error::BoardCursor)
            ));
        }
        let mut other_inputs = fixture.inputs.clone();
        other_inputs.state_dir = Some(fixture.base.join("other-state"));
        let other_roots = other_inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let request = fixture.call();
        assert!(matches!(
            read_with_request(&other_roots, None, false, PAGE, Some(cursor), &request),
            Err(Error::BoardCursor)
        ));
        Ok(())
    }

    #[test]
    fn board_cursors_reject_invalid_positions() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::with_attempts(1)?;
        let first = fixture.page(None, false, PAGE, None)?;
        let snapshot = first["snapshot"].as_str().ok_or("snapshot")?;
        let high_water = first["high_water"].as_u64().ok_or("high water")?;
        let zero_position = token(snapshot, high_water, 0)?;
        let future_position = token(snapshot, high_water, high_water + 1)?;
        for invalid in ["", "not-a-cursor", &zero_position, &future_position] {
            assert!(matches!(
                fixture.page(None, false, PAGE, Some(invalid)),
                Err(Error::BoardCursor)
            ));
        }
        Ok(())
    }

    #[test]
    fn board_cursors_reject_unavailable_snapshot_high_water()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::with_attempts(1)?;
        let first = fixture.page(None, false, PAGE, None)?;
        let high_water = first["high_water"].as_u64().ok_or("high water")? + 1;
        let filters = &first["filters"];
        let snapshot = binding(high_water, filters)?;
        let unavailable = token(&snapshot, high_water, 1)?;
        assert!(matches!(
            fixture.page(None, false, PAGE, Some(&unavailable)),
            Err(Error::BoardCursor)
        ));
        Ok(())
    }

    #[test]
    fn board_page_bounds_follow_the_declared_limit() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::new()?;
        for limit in [0, PAGE_SIZE_LIMIT + 1] {
            assert!(matches!(
                fixture.page(None, false, limit, None),
                Err(Error::Run(provenant_run::RunError::Validation(_)))
            ));
        }
        let one = fixture.page(None, false, 1, None)?;
        assert_eq!(one["lanes"].as_array().ok_or("lanes")?.len(), 1);
        let maximum = fixture.page(None, false, PAGE_SIZE_LIMIT, None)?;
        assert_eq!(
            maximum["lanes"].as_array().ok_or("lanes")?.len(),
            PAGE_SIZE_LIMIT as usize
        );
        assert!(serde_json::to_vec(&maximum)?.len() < crate::MAX_RESULT_BYTES);
        Ok(())
    }

    #[test]
    fn board_cursor_remains_usable_after_generation_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::new()?;
        let first = fixture.page(None, true, PAGE, None)?;
        let cursor = first["next_cursor"].as_str().ok_or("cursor")?.to_owned();
        let snapshot = first["snapshot"].clone();
        let high_water = first["high_water"].clone();
        fixture
            .other
            .create(&fixture.prepared_attempt(ATTEMPTS + 1)?, &fixture.call())?;
        fixture.store.withdraw(
            BoardFixture::id(1),
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            fixture.clock.wall_now()?,
            &fixture.call(),
        )?;
        let resumed = fixture.page(None, true, PAGE, Some(&cursor))?;
        assert_eq!(resumed["snapshot"], snapshot);
        assert_eq!(resumed["high_water"], high_water);
        assert_ne!(resumed["generation"], first["generation"]);
        let mut seen = BTreeSet::new();
        for lane in first["lanes"].as_array().ok_or("lanes")? {
            let id = lane["lane_id"]["attempt_id"]
                .as_str()
                .ok_or("attempt id")?
                .to_owned();
            assert!(seen.insert(id));
        }
        let mut page = resumed;
        let mut terminal_parent_seen = false;
        loop {
            for lane in page["lanes"].as_array().ok_or("lanes")? {
                let id = lane["lane_id"]["attempt_id"]
                    .as_str()
                    .ok_or("attempt id")?
                    .to_owned();
                assert!(seen.insert(id.clone()));
                assert_ne!(id, BoardFixture::id(ATTEMPTS + 1).to_string());
                terminal_parent_seen |= id == BoardFixture::id(1).to_string();
            }
            let Some(next) = page["next_cursor"].as_str() else {
                break;
            };
            page = fixture.page(None, true, PAGE, Some(next))?;
            assert_eq!(page["snapshot"], snapshot);
            assert_eq!(page["high_water"], high_water);
        }
        let expected: BTreeSet<_> = (1..=fixture.attempts)
            .map(|n| BoardFixture::id(n).to_string())
            .collect();
        assert_eq!(seen, expected);
        assert!(terminal_parent_seen);
        Ok(())
    }

    #[test]
    fn board_all_filter_includes_terminal_attempts() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::with_attempts(3)?;
        fixture.store.withdraw(
            BoardFixture::id(1),
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            fixture.clock.wall_now()?,
            &fixture.call(),
        )?;
        let active = fixture.page(Some(fixture.project), false, PAGE_SIZE_LIMIT, None)?;
        let all = fixture.page(Some(fixture.project), true, PAGE_SIZE_LIMIT, None)?;
        let id = BoardFixture::id(1).to_string();
        let contains = |page: &Value| {
            page["lanes"]
                .as_array()
                .is_some_and(|lanes| lanes.iter().any(|lane| lane["lane_id"]["attempt_id"] == id))
        };
        assert!(!contains(&active));
        assert!(contains(&all));
        Ok(())
    }

    #[test]
    fn board_observation_coverage_tracks_checkpoint_validity_and_mode()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let fixture = BoardFixture::with_attempts(1)?;
        let id = fixture.last_attempt();
        let checkpoint = fixture
            .roots
            .state
            .as_path()
            .join(format!("attempts/{id}/process.json"));
        let observed_at = fixture.clock.wall_now()?;
        let valid = serde_json::to_vec(&json!({
            "schema_version": 1,
            "scope": { "attempt": id, "generation": 0 },
            "last_output_at": observed_at,
            "priority": 15
        }))?;
        let wrong_scope = serde_json::to_vec(&json!({
            "schema_version": 1,
            "scope": { "attempt": id, "generation": u64::MAX },
            "last_output_at": observed_at,
            "priority": 15
        }))?;
        let cases = [
            (valid.clone(), 0o600, "complete", true),
            (b"{".to_vec(), 0o600, "partial", false),
            (wrong_scope, 0o600, "partial", false),
            (valid, 0o644, "partial", false),
        ];
        for (bytes, mode, coverage, row_is_observed) in cases {
            std::fs::write(&checkpoint, bytes)?;
            std::fs::set_permissions(&checkpoint, std::fs::Permissions::from_mode(mode))?;
            let page = fixture.page(None, false, PAGE, None)?;
            let row = page["lanes"]
                .as_array()
                .ok_or("lanes")?
                .iter()
                .find(|lane| lane["lane_id"]["attempt_id"] == json!(id))
                .ok_or("observed row")?;
            assert_eq!(page["coverage"]["observations"], coverage);
            assert_eq!(row["observation_coverage"] == "complete", row_is_observed);
            if row_is_observed {
                assert_eq!(row["metadata"]["attempt"]["priority"], 15);
                assert_eq!(
                    row["metadata"]["attempt"]["last_output_at"],
                    json!(observed_at)
                );
            } else {
                assert!(row["metadata"]["attempt"]["priority"].is_null());
                assert!(row["metadata"]["attempt"]["last_output_at"].is_null());
                assert_eq!(
                    fixture.page(None, false, 1, None)?["coverage"]["observations"],
                    "unavailable"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn board_chair_lookup_uses_attempt_project_and_request_provenance()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = BoardFixture::with_attempts(1)?;
        let call = fixture.call();
        let chair_call = fixture.chair_call();
        let id = fixture.last_attempt();
        let row = fixture.page(Some(fixture.project), false, PAGE_SIZE_LIMIT, None)?["lanes"]
            .as_array()
            .ok_or("lanes")?
            .iter()
            .find(|lane| lane["lane_id"]["attempt_id"] == json!(id))
            .cloned()
            .ok_or("selected row")?;
        let graph = row["lane_id"]["key"]["graph"].as_str().ok_or("graph")?;
        let request = fixture.store.request(id, &call)?;
        let index = ChairIndex::open(&fixture.roots.state, &chair_call)?;
        index.submit(fixture.other_project, graph, &request, &chair_call)?;
        let unbound = fixture.page(Some(fixture.project), false, PAGE_SIZE_LIMIT, None)?;
        assert_eq!(unbound["coverage"]["chairs"], "complete");
        assert!(
            unbound["lanes"]
                .as_array()
                .ok_or("lanes")?
                .iter()
                .find(|lane| { lane["lane_id"]["attempt_id"] == json!(id) })
                .ok_or("selected row")?["chair"]
                .is_null()
        );

        index.submit(fixture.project, graph, &request, &chair_call)?;
        let bound = fixture.page(Some(fixture.project), false, PAGE_SIZE_LIMIT, None)?;
        assert_eq!(bound["coverage"]["chairs"], "complete");
        let bound_row = bound["lanes"]
            .as_array()
            .ok_or("lanes")?
            .iter()
            .find(|lane| lane["lane_id"]["attempt_id"] == json!(id))
            .ok_or("selected row")?;
        assert_eq!(bound_row["chair"], json!(fixture.request.chair));
        Ok(())
    }
}
