use super::Error;
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
    if query.is_empty() || query.len() > 1_024 || !(1..=100).contains(&limit) {
        return Err(provenant_run::RunError::Validation("search bounds").into());
    }
    let artifact_part = match part {
        "prompt" => ArtifactPart::Prompt,
        "result" => ArtifactPart::Result,
        _ => return Err(provenant_run::RunError::Validation("search part").into()),
    };
    let query_folded = query.to_lowercase();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
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
        let Some(store) = RunStore::open_read_only(&context, &call)? else {
            continue;
        };
        let mut after = 0;
        let mut through = None;
        loop {
            if scanned >= FILE_LIMIT || scan_bytes >= TOTAL_BYTES {
                truncated = true;
                break 'projects;
            }
            let remaining = (FILE_LIMIT - scanned).min(100);
            let page = store.board_page(
                Some(project.project_id()),
                true,
                remaining as u64,
                after,
                through,
                &call,
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
                let bytes = match store.console_text_artifact(
                    attempt,
                    artifact_part,
                    ARTIFACT_BYTES,
                    &call,
                ) {
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
                matches.push(json!({"lane_id":{"host":null,"project_id":attempt.project_id,"run_id":attempt.run_id,"task_id":attempt.task_id,"attempt_id":attempt.attempt_id},"part":part,"snippet":snippet,"artifact_bytes":expected_bytes}));
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
    use provenant_core::{AttemptId, Clock, Digest, OperationId, RootInputs, RunId, TaskId};
    use provenant_run::{CanonicalRequest, NewAttempt, PreparedRun};
    use std::time::Instant;

    #[test]
    #[allow(clippy::disallowed_methods, clippy::print_stderr)] // This test measures real elapsed time.
    fn search_bounds_and_detail_performance() -> Result<(), Box<dyn std::error::Error>> {
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
        let clock = SystemClock::new();
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
        for batch in 0..16u16 {
            let mut tasks = Vec::new();
            let mut attempts = Vec::new();
            for index in 0..64u16 {
                let n = batch * 64 + index;
                if n == 1001 {
                    break;
                }
                let mut bytes = [0u8; 16];
                bytes[..2].copy_from_slice(&n.to_be_bytes());
                let task = TaskId::from_bytes(bytes);
                let attempt = AttemptId::from_bytes(bytes);
                let prompt = "searchable fixture";
                tasks.push(json!({"task_id":task,"prompt_digest":CanonicalV1::file_digest(prompt.as_bytes()),"route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},"fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null}));
                attempts.push(NewAttempt {
                    task_id: task,
                    attempt_id: attempt,
                    prompt: prompt.into(),
                });
                ids.push(attempt);
            }
            let request: CanonicalRequest = serde_json::from_value(
                json!({"schema_version":1,"tasks":tasks,"concurrency":1,"parent":null,"lane_depth":1}),
            )?;
            let mut run_bytes = [0u8; 16];
            run_bytes[..2].copy_from_slice(&batch.to_be_bytes());
            store.create(
                &PreparedRun {
                    run_id: RunId::from_bytes(run_bytes),
                    operation_id: OperationId::parse(&format!("search-{batch}"))?,
                    created_at: clock.wall_now()?,
                    request,
                    attempts,
                },
                &call,
            )?;
            // This fixture retains attempts but does not model scheduler admission.
            rusqlite::Connection::open(state.join("run.sqlite3"))?
                .execute("DELETE FROM admission", [])?;
        }
        let page = read(&roots, "searchable", "prompt", None, 100, None)?;
        assert_eq!(page["matches"].as_array().ok_or("matches")?.len(), 100);
        assert_eq!(page["coverage"]["files_scanned"], 1000);
        assert_eq!(page["coverage"]["peers"], "not_searched");
        assert_eq!(page["truncated"], true);
        let cursor = page["next_cursor"].as_str().ok_or("cursor")?;
        assert!(read(&roots, "different", "prompt", None, 100, Some(cursor)).is_err());
        let second = read(&roots, "searchable", "prompt", None, 100, Some(cursor))?;
        assert_ne!(
            page["matches"][0]["lane_id"],
            second["matches"][0]["lane_id"]
        );
        assert!(read(&roots, "searchable", "prompt", None, 101, None).is_err());
        assert!(read(&roots, "searchable", "transcript", None, 50, None).is_err());
        let mut details = Vec::new();
        let mut searches = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            for id in ids.iter().take(100) {
                assert_eq!(
                    super::super::detail::read(&roots, &id.to_string())?["status"],
                    "found"
                );
            }
            details.push(start.elapsed());
            let start = Instant::now();
            let result = read(&roots, "searchable", "prompt", None, 100, None)?;
            assert_eq!(result["coverage"]["files_scanned"], 1000);
            searches.push(start.elapsed());
        }
        details.sort();
        searches.sort();
        eprintln!(
            "100-detail p95: {:?}; 1000-attempt search p95: {:?}",
            details[18], searches[18]
        );
        assert!(
            details[18] <= std::time::Duration::from_secs(2),
            "100-detail p95: {:?}",
            details[18]
        );
        assert!(
            searches[18] <= std::time::Duration::from_millis(6500),
            "1000-attempt search p95: {:?}",
            searches[18]
        );
        let credential = base.join("credential");
        std::fs::write(&credential, "searchable fixture")?;
        let prompt = state
            .join("runs")
            .join(RunId::from_bytes([0, 15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).to_string())
            .join("attempts")
            .join(ids[1000].to_string())
            .join("prompt.txt");
        std::fs::remove_file(&prompt)?;
        let absent = super::super::detail::read(&roots, &ids[1000].to_string())?;
        assert_eq!(absent["artifacts"][0]["present"], false);
        std::os::unix::fs::symlink(&credential, &prompt)?;
        let guarded = read(&roots, "searchable", "prompt", None, 100, None)?;
        assert!(
            guarded["coverage"]["artifact_errors"]
                .as_array()
                .ok_or("errors")?
                .iter()
                .any(|error| error["attempt_id"] == json!(ids[1000]))
        );
        assert!(
            !guarded["matches"]
                .as_array()
                .ok_or("matches")?
                .iter()
                .any(|item| item["lane_id"]["attempt_id"] == json!(ids[1000]))
        );
        std::fs::remove_file(&prompt)?;
        std::fs::write(&prompt, "searchable fixture")?;
        std::fs::set_permissions(&prompt, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let db = rusqlite::Connection::open(state.join("run.sqlite3"))?;
        for (index, id) in ids.iter().enumerate().rev().take(130) {
            let mut record = serde_json::to_value(store.record(*id, &call)?)?;
            let content = if index == 1000 {
                format!("oversizedmarker{}", "x".repeat(ARTIFACT_BYTES as usize))
            } else {
                "x".repeat(ARTIFACT_BYTES as usize)
            };
            let mut run_bytes = [0u8; 16];
            run_bytes[..2].copy_from_slice(&((index / 64) as u16).to_be_bytes());
            let path = state
                .join("runs")
                .join(RunId::from_bytes(run_bytes).to_string())
                .join("attempts")
                .join(id.to_string())
                .join("prompt.txt");
            std::fs::write(path, &content)?;
            record["prompt"]["bytes"] = json!(content.len());
            record["prompt"]["digest"] = json!(CanonicalV1::file_digest(content.as_bytes()));
            db.execute(
                "UPDATE attempts SET record=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_vec(&record)?, id.to_string()],
            )?;
        }
        let byte_limited = read(&roots, "oversizedmarker", "prompt", None, 100, None)?;
        assert_eq!(byte_limited["matches"], json!([]));
        assert_eq!(byte_limited["truncated"], true);
        assert_eq!(byte_limited["coverage"]["bytes_scanned"], TOTAL_BYTES);
        Ok(())
    }
}
