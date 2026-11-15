use super::{Error, PAGE_SIZE_LIMIT};
use provenant_core::{ActorId, CancelToken, Roots};
use provenant_os::clock::SystemClock;
use provenant_records::CanonicalV1;
use provenant_run::{ArtifactPart, ExecutionContext, RunStore};
use serde_json::{Value, json};

const FILE_LIMIT: usize = 1_000;
const ARTIFACT_BYTES: u64 = 65_536;
const TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const SNIPPET_CHARS: usize = 240;

fn digest(value: &impl serde::Serialize) -> Result<String, Error> {
    CanonicalV1::record_digest("console-search", 1, value)
        .map(|digest| digest.to_string())
        .map_err(|_| Error::Encode)
}

pub(super) fn read(
    roots: &Roots,
    query: &str,
    part: &str,
    session: Option<&str>,
    limit: u64,
    cursor: Option<&str>,
) -> Result<Value, Error> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let request = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    read_with_request(roots, query, part, session, limit, cursor, &request)
}

fn read_with_request(
    roots: &Roots,
    query: &str,
    part: &str,
    session: Option<&str>,
    limit: u64,
    cursor: Option<&str>,
    call: &provenant_run::Request<'_>,
) -> Result<Value, Error> {
    if query.is_empty() || query.len() > 1_024 || !(1..=PAGE_SIZE_LIMIT).contains(&limit) {
        return Err(provenant_run::RunError::Validation("search bounds").into());
    }
    let artifact_part = match part {
        "prompt" => ArtifactPart::Prompt,
        "result" => ArtifactPart::Result,
        _ => return Err(provenant_run::RunError::Validation("search part").into()),
    };
    let query_folded = query.to_lowercase();
    let projects = provenant_config::registered_projects(roots)?;
    let mut matches = Vec::new();
    let mut scan_bytes = 0u64;
    let mut scanned = 0usize;
    let mut truncated = false;
    let mut snapshot_rows = Vec::new();
    let mut artifact_errors = Vec::new();

    'projects: for project in projects {
        let context = ExecutionContext {
            state_root: roots.state.clone(),
            project_id: project.project_id(),
            actor_id: ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        let Some(store) = RunStore::open_read_only(&context, call)? else {
            continue;
        };
        let mut after = 0;
        let mut through = None;
        loop {
            if scanned >= FILE_LIMIT || scan_bytes >= TOTAL_BYTES {
                truncated = true;
                break 'projects;
            }
            let remaining = (FILE_LIMIT - scanned).min(PAGE_SIZE_LIMIT as usize);
            let page = store.board_page(
                Some(project.project_id()),
                true,
                remaining as u64,
                after,
                through,
                call,
            )?;
            through = Some(page.high_water);
            after = page.next.unwrap_or(0);
            snapshot_rows.push((
                project.project_id(),
                page.generation.clone(),
                page.high_water,
            ));
            for lane in &page.lanes {
                if scanned == FILE_LIMIT || scan_bytes >= TOTAL_BYTES {
                    truncated = true;
                    break 'projects;
                }
                scanned += 1;
                let attempt = &lane.attempt;
                if session.is_some_and(|session| attempt.session.as_deref() != Some(session)) {
                    continue;
                }
                let expected_bytes = if part == "prompt" {
                    attempt.prompt.bytes
                } else {
                    attempt.result.as_ref().map_or(0, |artifact| artifact.bytes)
                };
                if expected_bytes > ARTIFACT_BYTES
                    || expected_bytes > TOTAL_BYTES.saturating_sub(scan_bytes)
                {
                    truncated = true;
                    continue;
                }
                scan_bytes += expected_bytes;
                let bytes =
                    match store.console_text_artifact(attempt, artifact_part, ARTIFACT_BYTES, call)
                    {
                        Ok(Some(bytes)) => bytes,
                        Ok(None) => continue,
                        Err(error) => {
                            artifact_errors.push(
                                json!({"attempt_id":attempt.attempt_id,"reason":error.to_string()}),
                            );
                            continue;
                        }
                    };
                let text = String::from_utf8_lossy(&bytes);
                let chars: Vec<char> = text.chars().collect();
                let mut folded = Vec::new();
                let mut original_positions = Vec::new();
                for (index, character) in chars.iter().enumerate() {
                    for lower in character.to_lowercase() {
                        folded.push(lower);
                        original_positions.push(index);
                    }
                }
                let query_chars: Vec<char> = query_folded.chars().collect();
                let Some(folded_position) = folded
                    .windows(query_chars.len())
                    .position(|part| part == query_chars)
                else {
                    continue;
                };
                let match_position = original_positions[folded_position];
                let start = match_position.saturating_sub(SNIPPET_CHARS / 3);
                let snippet: String = chars.iter().skip(start).take(SNIPPET_CHARS).collect();
                let key = &attempt.key;
                matches.push(json!({"lane_id":{"host":null,"project_id":attempt.project_id,"key":key,"attempt_id":attempt.attempt_id},"part":part,"snippet":snippet,"artifact_bytes":expected_bytes}));
            }
            if page.next.is_none() {
                break;
            }
        }
    }
    let snapshot = digest(&(
        query,
        part,
        session,
        &snapshot_rows,
        scanned,
        scan_bytes,
        truncated,
    ))?;
    let offset = if let Some(cursor) = cursor {
        let (found_snapshot, found_offset, checksum) = cursor
            .split_once('.')
            .and_then(|(snapshot, tail)| {
                tail.split_once('.')
                    .map(|(offset, checksum)| (snapshot, offset, checksum))
            })
            .ok_or(Error::SearchCursor)?;
        let found_offset: usize = found_offset.parse().map_err(|_| Error::SearchCursor)?;
        let expected = digest(&(query, part, session, &snapshot, found_offset))?;
        if found_snapshot != snapshot
            || checksum != expected
            || found_offset == 0
            || found_offset >= matches.len()
        {
            return Err(Error::SearchCursor);
        }
        found_offset
    } else {
        0
    };
    let end = offset.saturating_add(limit as usize).min(matches.len());
    let next_cursor = if end < matches.len() {
        let checksum = digest(&(query, part, session, &snapshot, end))?;
        Some(format!("{snapshot}.{end}.{checksum}"))
    } else {
        None
    };
    Ok(
        json!({"schema_version":1,"snapshot":snapshot,"query":query,"part":part,"matches":&matches[offset..end],"next_cursor":next_cursor,"truncated":truncated || end < matches.len(),"coverage":{"scope":"local","peers":"not_searched","files_scanned":scanned,"bytes_scanned":scan_bytes,"file_limit":FILE_LIMIT,"per_artifact_bytes":ARTIFACT_BYTES,"total_bytes":TOTAL_BYTES,"artifact_errors":artifact_errors,"restart":"If the cursor is invalid or stale, restart with the same query and filters without --cursor."}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{
        AttemptId, Clock, Digest, MonoTime, OperationId, RootInputs, TestClock, WallTime,
    };
    use provenant_run::{CanonicalRequest, PreparedRun};

    struct SearchFixture {
        _temp: tempfile::TempDir,
        base: std::path::PathBuf,
        roots: Roots,
        ids: Vec<AttemptId>,
        clock: TestClock,
        cancel: CancelToken,
    }

    fn fixture(oversized: bool) -> Result<SearchFixture, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let checkout = base.join("checkout");
        let state = base.join("state");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&checkout)?;
        std::fs::create_dir(checkout.join(".git"))?;
        std::fs::create_dir(&state)?;
        std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let mut inputs = RootInputs::new(home, checkout.clone());
        inputs.state_dir = Some(state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(checkout.join(".git"))?,
        )?;
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = RunStore::open(
            &ExecutionContext {
                state_root: roots.state.clone(),
                project_id: binding.project_id(),
                actor_id: ActorId::from_bytes([2; 16]),
                read_prompts: false,
            },
            &call,
        )?;
        let mut ids = Vec::new();
        let prompt = "searchable fixture";
        let request: CanonicalRequest = serde_json::from_value(json!({"schema_version":1,
            "task":{"prompt_digest":CanonicalV1::file_digest(prompt.as_bytes()),"route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null},
            "parent":null,"lane_depth":1}))?;
        for n in 0..2u16 {
            let mut bytes = [0u8; 16];
            bytes[..2].copy_from_slice(&n.to_be_bytes());
            let attempt = AttemptId::from_bytes(bytes);
            let mut request = request.clone();
            request.task.context = Some(provenant_records::run::WorkContext {
                graph_id: Some(format!("lane-{attempt}")),
                node_id: Some("node".into()),
                attempt: Some(1),
                ..Default::default()
            });
            store.create(
                &PreparedRun {
                    attempt_id: attempt,
                    operation_id: OperationId::parse(&format!("search-{n}"))?,
                    created_at: clock.wall_now()?,
                    request,
                    prompt: prompt.into(),
                },
                &call,
            )?;
            ids.push(attempt);
        }
        if oversized {
            let content = format!("oversizedmarker{}", "x".repeat(ARTIFACT_BYTES as usize));
            let oversized = AttemptId::from_bytes([2; 16]);
            let mut oversized_request = request.clone();
            oversized_request.task.prompt_digest = CanonicalV1::file_digest(content.as_bytes());
            oversized_request.task.context = Some(provenant_records::run::WorkContext {
                graph_id: Some(format!("lane-{oversized}")),
                node_id: Some("node".into()),
                attempt: Some(1),
                ..Default::default()
            });
            store.create(
                &PreparedRun {
                    attempt_id: oversized,
                    operation_id: OperationId::parse("search-oversized")?,
                    created_at: clock.wall_now()?,
                    request: oversized_request,
                    prompt: content,
                },
                &call,
            )?;
        }
        Ok(SearchFixture {
            _temp: temp,
            base,
            roots,
            ids,
            clock,
            cancel,
        })
    }

    impl SearchFixture {
        fn read(
            &self,
            query: &str,
            part: &str,
            session: Option<&str>,
            limit: u64,
            cursor: Option<&str>,
        ) -> Result<Value, Error> {
            let request = provenant_run::Request {
                clock: &self.clock,
                cancel: &self.cancel,
            };
            read_with_request(&self.roots, query, part, session, limit, cursor, &request)
        }
    }

    #[test]
    fn search_pages_bind_the_query_and_respect_declared_bounds()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture(false)?;
        let page = fixture.read("searchable", "prompt", None, 1, None)?;
        let matches = page["matches"].as_array().ok_or("matches")?;
        assert_eq!(matches.len(), 1);
        assert!(page["coverage"]["files_scanned"].as_u64().unwrap() > 0);
        assert_eq!(page["coverage"]["peers"], "not_searched");
        assert_eq!(page["truncated"], true);
        let cursor = page["next_cursor"].as_str().ok_or("cursor")?;
        assert!(
            fixture
                .read("different", "prompt", None, 1, Some(cursor))
                .is_err()
        );
        let second = fixture.read("searchable", "prompt", None, 1, Some(cursor))?;
        assert_ne!(
            page["matches"][0]["lane_id"]["attempt_id"],
            second["matches"][0]["lane_id"]["attempt_id"]
        );
        assert!(
            fixture
                .read("searchable", "prompt", None, PAGE_SIZE_LIMIT + 1, None)
                .is_err()
        );
        assert!(
            fixture
                .read("searchable", "transcript", None, PAGE_SIZE_LIMIT, None)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn search_reads_owned_artifacts_and_refuses_symlink_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture(false)?;
        let id = fixture.ids.first().ok_or("missing search fixture")?;
        assert_eq!(
            super::super::detail::read(&fixture.roots, &id.to_string())?["status"],
            "found"
        );
        let result = fixture.read("searchable", "prompt", None, PAGE_SIZE_LIMIT, None)?;
        assert!(result["coverage"]["files_scanned"].as_u64().unwrap() > 0);
        assert!(result["matches"][0]["lane_id"]["key"].is_object());
        assert!(result["matches"][0]["lane_id"].get("run_id").is_none());
        assert!(result["matches"][0]["lane_id"].get("task_id").is_none());
        let credential = fixture.base.join("credential");
        std::fs::write(&credential, "searchable fixture")?;
        let last_attempt = fixture.ids.last().ok_or("missing last search fixture")?;
        let prompt = fixture
            .roots
            .state
            .as_path()
            .join("attempts")
            .join(last_attempt.to_string())
            .join("prompt.txt");
        std::fs::remove_file(&prompt)?;
        let absent = super::super::detail::read(&fixture.roots, &last_attempt.to_string())?;
        assert_eq!(absent["artifacts"][0]["present"], false);
        std::os::unix::fs::symlink(&credential, &prompt)?;
        let guarded = fixture.read("searchable", "prompt", None, PAGE_SIZE_LIMIT, None)?;
        assert!(
            guarded["coverage"]["artifact_errors"]
                .as_array()
                .ok_or("errors")?
                .iter()
                .any(|error| error["attempt_id"] == json!(last_attempt))
        );
        assert!(
            !guarded["matches"]
                .as_array()
                .ok_or("matches")?
                .iter()
                .any(|item| item["lane_id"]["attempt_id"] == json!(last_attempt))
        );
        Ok(())
    }

    #[test]
    fn search_byte_budget_truncates_an_oversized_artifact() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture(true)?;
        let byte_limited =
            fixture.read("oversizedmarker", "prompt", None, PAGE_SIZE_LIMIT, None)?;
        assert!(
            byte_limited["matches"]
                .as_array()
                .is_some_and(|items| items.is_empty())
        );
        assert_eq!(byte_limited["truncated"], true);
        assert!(byte_limited["coverage"]["bytes_scanned"].as_u64().unwrap() <= TOTAL_BYTES);
        Ok(())
    }
}
