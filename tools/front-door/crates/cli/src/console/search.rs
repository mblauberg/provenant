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
                    attempt.attempt_id,
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
        json!({"schema_version":1,"snapshot":snapshot,"query":query,"part":part,"matches":&matches[offset..end],"next_cursor":next_cursor,"truncated":truncated || end < matches.len(),"coverage":{"scope":"local","files_scanned":scanned,"bytes_scanned":scan_bytes,"file_limit":FILE_LIMIT,"per_artifact_bytes":ARTIFACT_BYTES,"total_bytes":TOTAL_BYTES,"artifact_errors":artifact_errors,"restart":"If the cursor is invalid or stale, restart with the same query and filters without --cursor."}}),
    )
}
