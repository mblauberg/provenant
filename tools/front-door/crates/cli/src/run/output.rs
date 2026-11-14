use super::*;

/// Render a host-owned read through the same CLI projection.
pub fn render_output(command: Output, read: Result<(Value, String), Fail>) -> Reply {
    let receipt_tail = matches!(command.part, Part::Receipt)
        .then_some(command.tail)
        .flatten();
    let headed = matches!(command.part, Part::Result);
    let warn_human = headed && !command.path && command.to.is_none();
    let to = command.to.clone();
    let mut out = reply(
        "output",
        read.and_then(|(mut data, mut text)| {
            if let Some(lines) = receipt_tail {
                text =
                    text[tail_start(text.as_bytes(), lines.min(limits::TAIL_LINES))..].to_owned();
                data["tail"] = json!(lines.min(limits::TAIL_LINES));
                data["tail_text"] = json!(text);
            }
            if let Some(path) = &to {
                let bytes = chunk_bytes(&data).ok_or_else(|| {
                    Fail::new(
                        Status::Invalid,
                        ErrorKind::Invalid,
                        "--to needs a part that has bytes; use --part result, transcript, stderr or prompt",
                    )
                })?;
                std::fs::write(path, &bytes).map_err(|error| {
                    Fail::new(Status::Error, ErrorKind::Io, error.to_string())
                })?;
                let written_path = std::fs::canonicalize(path).map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
                data["to"] = json!(provenant_records::StructuredPathWire::from_path(&written_path).map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?);
                data["written"] = json!(bytes.len());
                text = format!("wrote {} bytes to {}\n", bytes.len(), path.display());
            }
            Ok((data, text))
        }),
    );
    out.read_only = true;
    if out.result.error.is_none() {
        out.summary = Some(output_summary(
            &out.result.data,
            &out.human,
            headed,
            Some(&command.target),
        ));
        if warn_human
            && out
                .result
                .data
                .get("git_import_failure")
                .is_some_and(|value| !value.is_null())
            && let Some(reason) = out.result.data["reason"].as_str()
            && out.human.trim() != reason
        {
            out.human = format!("{reason}\n\n{}", out.human);
        }
    }
    out
}

/// Read a known local attempt using the caller's cancellation token.
pub fn output_from_session(
    command: Output,
    edge: &Edge<'_>,
    session: &Session,
    attempt: AttemptId,
    call: &run::Request<'_>,
) -> Reply {
    let read = output_read(command.clone(), edge, session, attempt, call);
    render_output(command, read)
}

/// Read a peer attempt using the owning service's budget and cancellation.
pub fn output_from_host(
    command: Output,
    host: provenant_core::HostId,
    attempt: provenant_core::AttemptId,
    read: impl FnMut(
        provenant_fabric_runtime::coordination::facade::OutputInput,
    ) -> Result<provenant_fabric_runtime::coordination::facade::OutputResult, Fail>,
) -> Reply {
    let result = remote::read_output_with(&command, host, attempt, read);
    render_output(command, result)
}

/// What an agent needs from a read: how the attempt settled, what it changed,
/// and the whole final text up to a generous bound, then its head and tail.
/// Missing changed-file evidence stays null; it must never read as an empty diff.
pub(super) fn output_summary(
    data: &Value,
    text: &str,
    headed: bool,
    target: Option<&crate::Target>,
) -> crate::Summary {
    let mut lines = Vec::new();
    if headed && let Some(state) = data["state"].as_str() {
        let reason = data["reason"].as_str();
        lines.push(match reason {
            Some(reason) => format!(
                "settlement: {} ({})",
                crate::bounded_text(state, 64),
                crate::bounded_text(reason, 2048)
            ),
            None => format!("settlement: {}", crate::bounded_text(state, 64)),
        });
        if state == "failed" {
            if let Some(error) = data["provider_error"].as_str() {
                lines.push(format!(
                    "provider error: {}",
                    crate::bounded_text(error, PROVIDER_ERROR_BYTES)
                ));
            }
            if let Some(path) = structured_path_text(&data["result_path"]) {
                lines.push(format!("result: {}", crate::bounded_text(&path, 512)));
            }
        }
    }
    if headed && let Some(fallback) = data.get("planner_fallback") {
        lines.push(format!(
            "planner fallback: {} (reason: {})",
            fallback["stage"].as_str().unwrap_or("static"),
            plain_text(fallback["reason"].as_str().unwrap_or_default(), 160)
        ));
    }
    if headed && data.get("writer_base").is_some() {
        for (label, key) in [
            ("base", "writer_base"),
            ("committed head", "writer_head"),
            ("publication", "publication"),
        ] {
            lines.push(format!(
                "{label}: {}",
                data[key].as_str().unwrap_or("unavailable")
            ));
        }
        lines.push(format!(
            "residual tree state: {}",
            data["residual_tree"].as_str().unwrap_or("unavailable")
        ));
    }
    if headed && let Some(files) = data.get("changed_files") {
        lines.push(match files.as_array() {
            Some(paths) => {
                let total = data["changed_files_total"]
                    .as_u64()
                    .map_or(paths.len(), |total| total as usize);
                let more = total.saturating_sub(paths.len());
                let listed = paths
                    .iter()
                    .filter_map(structured_path_text)
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                format!("changed files ({total}): {listed}{more}")
            }
            None => "changed files: unavailable".into(),
        });
    }
    // An empty result uses its safe diagnostic as human text. The settlement
    // header already contains that detail, so render it only once.
    if !headed || data["reason"].as_str() != Some(text.trim()) {
        let body = crate::head_and_tail(text, limits::OUTPUT_BYTES);
        lines.extend(body.lines().map(str::to_owned));
    }
    // The graph handle retains placement without exposing the internal ID or
    // adding summary-only fields to the public output data schema.
    let target = target.map(ToString::to_string);
    let state = data["state"].as_str().unwrap_or_default();
    let next = target.and_then(|target| {
        if data["eof"] == json!(false) {
            let offset = data["next_offset"].as_u64()?;
            Some(format!(
                "provenant output {target} --part {} --offset {offset}",
                data["part"].as_str().unwrap_or("result")
            ))
        } else if matches!(state, "queued" | "starting" | "running" | "settling") {
            Some(format!("provenant wait {target}"))
        } else {
            None
        }
    });
    crate::Summary {
        lines,
        next,
        omitted: 0,
    }
}

pub(super) fn receipt_model(adapter: Option<&str>, model: Option<&str>) -> Option<String> {
    let adapter = adapter.filter(|value| !value.is_empty())?;
    let model = model.filter(|value| !value.is_empty())?;
    Some(if model.starts_with(&format!("{adapter}/")) {
        model.to_owned()
    } else {
        format!("{adapter}/{model}")
    })
}

pub(super) fn receipt_family(model: Option<&str>) -> String {
    provenant_routing::Catalogue::bundled()
        .ok()
        .and_then(|catalogue| {
            ModelId::parse(model?)
                .ok()
                .and_then(|id| catalogue.model(&id).and_then(|entry| entry.family.clone()))
        })
        .unwrap_or_else(|| "unknown".into())
}

pub(super) fn receipt_resolution(record: &run::AttemptRecord) -> String {
    let Some(route) = record.applied_route.as_ref() else {
        return "unknown".into();
    };
    let Some(applied) = receipt_model(route.provider.as_deref(), route.model.as_deref()) else {
        return "unknown".into();
    };
    // The recorded pick proves a pool result even when other candidates were unavailable.
    if let Some(pool) = &route.pool_selection {
        return if pool.pick == applied {
            format!("pool {}", pool.pool)
        } else {
            "unknown".into()
        };
    }
    let run::RequestedRoute::Model {
        provider, model, ..
    } = &record.requested_route
    else {
        return "unknown".into();
    };
    let Some(requested) = receipt_model(Some(provider), Some(model)) else {
        return "unknown".into();
    };
    if applied == requested {
        return "explicit".into();
    }
    "unknown".into()
}

pub(super) fn reviewer_receipt(
    record: &provenant_records::run::AttemptRecord,
    writer: bool,
) -> Result<(Value, String), Fail> {
    use provenant_records::run::AttemptState;
    if !matches!(record.state, AttemptState::Terminal { .. }) {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::Nonterminal,
            "receipt requires a settled attempt",
        ));
    }
    let route = record.applied_route.as_ref();
    let adapter = route
        .and_then(|r| r.provider.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let model = route
        .and_then(|r| r.model.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let qualified_model =
        route.and_then(|r| receipt_model(r.provider.as_deref(), r.model.as_deref()));
    let effort = route
        .and_then(|r| r.effort.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let family = receipt_family(qualified_model.as_deref());
    let resolved_from = receipt_resolution(record);
    let reviewer = run::fact(record.clone())?.reviewer;
    let reviewer_model = record
        .observed_route
        .as_ref()
        .and_then(|route| receipt_model(route.provider.as_deref(), route.model.as_deref()));
    let reviewer_family = receipt_family(reviewer_model.as_deref());
    let provenance = reviewer
        .as_ref()
        .map_or("unknown", |reviewer| match reviewer.provenance {
            provenant_records::ReviewerProvenance::Observed => "observed",
            provenant_records::ReviewerProvenance::ChairAttested => "chair_attested",
            provenant_records::ReviewerProvenance::Unknown => "unknown",
        });
    let actor = reviewer
        .as_ref()
        .map_or_else(|| "unknown".into(), |r| r.actor_id.to_string());
    let reviewer_model = reviewer
        .as_ref()
        .and_then(|r| r.model.as_deref())
        .unwrap_or("unknown");
    let reviewer_family = if reviewer.is_some() {
        reviewer_family
    } else {
        "unknown".into()
    };
    let role = if writer { "writer" } else { "reviewer" };
    let label = if writer { "Writer" } else { "Reviewer" };
    let qualified_model = qualified_model.as_deref().unwrap_or("unknown");
    let line = format!(
        "{label}: {qualified_model}@{effort} ({family}; resolved from {resolved_from}), attempt {} {}",
        record.key, record.attempt_id
    );
    let attribution = format!(
        "Reviewer provenance: {provenance}, actor {actor}, model {reviewer_model}, family {reviewer_family}"
    );
    Ok((
        json!({"key": record.key, "attempt_id": record.attempt_id, "adapter": adapter, "model": model, "effort": effort, "family": family, "role": role, "resolved_from": resolved_from, "receipt": line, "reviewer": {"provenance": provenance, "actor_id": actor, "model": reviewer_model, "family": reviewer_family}, "reviewer_provenance": attribution}),
        format!("{line}\n{attribution}\n"),
    ))
}

/// The most lines `output --tail` returns; a larger request is clamped.
pub(super) const TAIL_MAX: u64 = limits::TAIL_LINES;

/// Read the selected part and attach the shared lane projection.
fn output_read(
    command: Output,
    edge: &Edge<'_>,
    session: &Session,
    attempt: AttemptId,
    call: &run::Request<'_>,
) -> Result<(Value, String), Fail> {
    let (mut data, human) = output_read_part(command, edge, session, attempt, call)?;
    let id = data["attempt_id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| {
            Fail::new(
                Status::Error,
                ErrorKind::StateCorrupt,
                "output has no attempt identity",
            )
        })?;
    let record = session.service().status(id, call)?;
    let waits = session
        .store
        .lane_waits(std::slice::from_ref(&record), call);

    let projected = project_lane(
        &record,
        &waits,
        session.context.state_root.as_path(),
        call.clock.wall_now().ok(),
        true,
    );
    if let (Some(data), Some(row)) = (data.as_object_mut(), json!(projected).as_object()) {
        data.remove("settlement_reason");
        if let Some(grants) = data.get_mut("grants").and_then(Value::as_object_mut) {
            for field in ["write_grants", "read_only_writable"] {
                if let Some(value) = row.get(field) {
                    grants.insert(field.into(), value.clone());
                }
            }
            if let Some(route) = row.get("applied_route") {
                grants.insert(
                    "user_skills".into(),
                    route.get("user_skills").cloned().unwrap_or(Value::Null),
                );
            }
        }
        data.extend(row.clone());
    }
    Ok((data, human))
}

/// Read the selected part of an attempt in an opened local store.
fn output_read_part(
    command: Output,
    edge: &Edge<'_>,
    session: &Session,
    id: AttemptId,
    call: &run::Request<'_>,
) -> Result<(Value, String), Fail> {
    let record = session.service().status(id, call)?;
    if command.path {
        let file = match command.part {
            Part::Result if record.result.is_none() => {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::NotFound,
                    "attempt has no result to point at",
                ));
            }
            Part::Result => provenant_records::AttemptFile::Result,
            Part::Prompt => provenant_records::AttemptFile::Prompt,
            Part::Stderr => provenant_records::AttemptFile::Stderr,
            _ => {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "--path needs --part result, prompt or stderr",
                ));
            }
        };
        let path = artifact(session.context.state_root.as_path(), &record, file.name());
        return Ok((
            json!({ "attempt_id": id, "part": format!("{:?}", command.part).to_lowercase(), "path": provenant_records::StructuredPathWire::from_path(&path).ok() }),
            format!("{}\n", path.display()),
        ));
    }
    let part = match command.part {
        Part::Receipt => {
            let task = session.store.request(record.attempt_id, call)?.task;
            return reviewer_receipt(&record, task.writer.is_some());
        }
        Part::Result => run::ArtifactPart::Result,
        Part::Prompt => run::ArtifactPart::Prompt,
        Part::Stderr => run::ArtifactPart::Stderr,
        Part::Transcript => run::ArtifactPart::Transcript,
        Part::Events => {
            // An attempt has a handful of revisions; offset is an event cursor.
            let mut events = session
                .service()
                .events(command.offset, 64, Some(id), call)?;
            if let Some(lines) = command.tail {
                let mut cursor = events.last().map_or(command.offset, |event| event.sequence);
                let mut more = events.len() == 64;
                while more {
                    let page = session.service().events(cursor, 64, Some(id), call)?;
                    more = page.len() == 64;
                    let next = page.last().map_or(cursor, |event| event.sequence);
                    if more && next <= cursor {
                        return Err(Fail::new(
                            Status::Error,
                            ErrorKind::StateCorrupt,
                            "event cursor did not advance",
                        ));
                    }
                    cursor = next;
                    events.extend(page);
                    let excess = events.len().saturating_sub(lines.min(TAIL_MAX) as usize);
                    events.drain(..excess);
                }
                let excess = events.len().saturating_sub(lines.min(TAIL_MAX) as usize);
                events.drain(..excess);
                while !events.is_empty()
                    && serde_json::to_vec(&events)
                        .map_err(|error| {
                            Fail::new(Status::Error, ErrorKind::Io, error.to_string())
                        })?
                        .len() as u64
                        > command.max_bytes
                {
                    events.remove(0);
                }
            }
            let human = events
                .iter()
                .map(|e| format!("{} {}\n", e.revision, state_and_reason(&e.change).0))
                .collect();
            return Ok((json!({ "attempt_id": id, "events": events }), human));
        }
    };
    let mut range = run::ByteRange {
        offset: command.offset,
        max_bytes: command.max_bytes,
    };
    let waits = session
        .store
        .lane_waits(std::slice::from_ref(&record), call);
    let detail = status::lane_reason(&record, &waits);
    if matches!(part, run::ArtifactPart::Result)
        && record.result.is_none()
        && command.offset == 0
        && command.tail.is_none()
        && command.to.is_none()
        && let Some(reason) = &detail
    {
        // There may be useful admission evidence before a result exists. Do
        // not invent an artefact length, digest or completeness for it.
        return Ok((
            json!({
                "attempt_id": id, "state": state_and_reason(&record.state).0,
                "reason": reason, "part": "result", "offset": 0, "next_offset": 0,
                "length": null, "eof": true, "clipped": null, "complete": null,
                "digest": null, "bytes": json!({"encoding":"utf8", "data":""}),
            }),
            format!("{reason}\n"),
        ));
    }
    let evicted = status::artifact(
        session.context.state_root.as_path(),
        &record,
        provenant_records::AttemptFile::PayloadEvicted.name(),
    )
    .exists();
    if evicted && matches!(part, run::ArtifactPart::Transcript) {
        return Ok((
            json!({
                "attempt_id": id, "state": state_and_reason(&record.state).0,
                "payload": "evicted", "part": "transcript", "offset": 0, "next_offset": 0,
                "length": null, "eof": true, "clipped": null, "complete": null,
                "digest": null, "bytes": json!({"encoding":"utf8", "data":""}),
            }),
            "payload evicted by retention; the transcript is no longer retained\n".into(),
        ));
    }
    let mut chunk = session.service().output(id, part, range, call)?;
    let tail = command.tail.map(|lines| lines.min(TAIL_MAX));
    if let Some(lines) = tail {
        range.offset = chunk.len.saturating_sub(command.max_bytes);
        // A short transcript's first page already includes the requested tail.
        if !matches!(part, run::ArtifactPart::Transcript) || range.offset != chunk.offset {
            chunk = session.service().output(id, part, range, call)?;
        }
        let start = tail_start(&chunk.bytes, lines);
        chunk.offset += start as u64;
        chunk.bytes.drain(..start);
    }
    align_utf8_tail(&mut chunk);
    let mut text = String::from_utf8_lossy(&chunk.bytes).into_owned();
    if matches!(command.part, Part::Stderr) && !chunk.complete {
        text.insert_str(0, "[stderr truncated; retained tail]\n");
    }
    let fallback_reason = if matches!(command.part, Part::Result) {
        planner_fallback_reason(&edge.roots.state, session.context.project_id, &record)
    } else {
        None
    };
    // The text alone cannot tell a clean success from one that needs the chair.
    let state = state_and_reason(&record.state).0;
    let reason = detail;
    if matches!(command.part, Part::Result)
        && text.is_empty()
        && command.offset == 0
        && command.tail.is_none()
        && command.to.is_none()
        && let Some(reason) = &reason
    {
        text = format!("{reason}\n");
    }
    let mut data = json!({
            "attempt_id": id,
            "state": state,
            "reason": reason,
            "settlement_reason": record.settlement_reason,
            "part": format!("{:?}", command.part).to_lowercase(),
            "offset": chunk.offset,
            "next_offset": chunk.next_offset,
            "length": chunk.len,
            "eof": chunk.eof,
            "clipped": chunk.clipped,
            "complete": chunk.complete,
            "digest": chunk.digest,
            "bytes": provenant_fabric_runtime::coordination::facade::OutputBytes::from_bytes(chunk.bytes),
    });
    if evicted {
        // Retention removed the bulk payload; the record, result and capture
        // tails remain.
        data["payload"] = json!("evicted");
        if matches!(command.part, Part::Stderr) && text.is_empty() {
            text = "payload evicted by retention; the retained capture tail is empty\n".into();
        }
    }
    if let Some(lines) = tail {
        data["tail"] = json!(lines);
        data["tail_clamped"] = json!(command.tail != tail);
    }
    if matches!(command.part, Part::Result) && command.tail.is_none() && command.offset == 0 {
        let base = run::writer_base(session.context.state_root.as_path(), &record)
            .ok()
            .flatten();
        data["writer_base"] = json!(base);
        data["writer_head"] = json!(record.head);
        data["residual_tree"] = residual_tree_evidence(session, &record, call);
        data["publication"] = match &record.delivery {
            Some(delivery)
                if matches!(delivery.state, run::WriterDeliveryState::Delivered { .. })
                    && delivery
                        .integration
                        .as_ref()
                        .is_some_and(|integration| !integration.remote.is_empty()) =>
            {
                json!("pushed")
            }
            _ => Value::Null,
        };
        let (paths, total) = changed_file_evidence(session, &record, call);
        data["changed_files"] = paths;
        data["changed_files_total"] = total;
    }
    if let Some(reason) = fallback_reason {
        add_planner_fallback(&mut data, &mut text, reason);
    }
    if matches!(command.part, Part::Result)
        && matches!(
            record.state,
            run::AttemptState::Terminal {
                outcome: run::Outcome::Failed,
                ..
            }
        )
    {
        data["provider_error"] = json!(provider_error_line(session, id, call));
    }
    Ok((data, text))
}

/// Wire key under which a serving peer carries the fallback reason in its
/// output chunk, so `output --host` reports what the peer's graph recorded.
pub const PLANNER_FALLBACK_KEY: &str = "planner_fallback";

/// Name the planner fallback and its reason in an output reply; one rendering
/// for local and peer output.
pub(super) fn add_planner_fallback(data: &mut Value, text: &mut String, reason: String) {
    text.push_str(&format!(
        "\nPlanner fallback: static stage\nReason: {}\n",
        plain_text(&reason, 1024)
    ));
    data["planner_fallback"] = json!({"stage":"static", "reason":reason});
}

/// The reason the attempt's graph planner fell back to its static stage, read
/// from the graph checkpoint in `state_root`; the serving side computes it.
pub fn planner_fallback_reason(
    state_root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    record: &run::AttemptRecord,
) -> Option<String> {
    use provenant_fabric_runtime::graph::FileJournal;

    let context = record.context.as_ref()?;
    let graph_id = context.graph_id.as_deref()?;
    FileJournal::read_checkpoint(state_root, project, graph_id)
        .ok()?
        .and_then(|checkpoint| fallback_reason_for_graph(&checkpoint))
}

/// The fallback reason of the graph's planner, when it fell back. Every node of
/// that graph reports it: the static stage, its dependants (such as a
/// `synthesis` node outside the expansion) and the planner itself.
fn fallback_reason_for_graph(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Option<String> {
    checkpoint.nodes.iter().find_map(|node| {
        node.outputs
            .as_ref()
            .and_then(|outputs| outputs.plan.as_ref())
            .map(|plan| plan.fallback_reason.clone())
    })
}

/// The last non-empty stderr line of a failed attempt, redacted as reasons are.
/// The tail is read so a long capture still shows its final error.
pub(super) fn provider_error_line(
    session: &Session,
    id: AttemptId,
    call: &run::Request<'_>,
) -> Option<String> {
    const TAIL_BYTES: u64 = 8192;
    // A structured provider error (the result's own message) outranks the
    // last stderr line, which may be only the tail of a longer explanation.
    if let Ok(chunk) = session.service().output(
        id,
        run::ArtifactPart::Result,
        run::ByteRange {
            offset: 0,
            max_bytes: PROVIDER_ERROR_BYTES as u64,
        },
        call,
    ) && let Some(message) = provider_error_from_result(&chunk.bytes)
    {
        return Some(message);
    }
    let length = session
        .service()
        .output(
            id,
            run::ArtifactPart::Stderr,
            run::ByteRange {
                offset: 0,
                max_bytes: 1,
            },
            call,
        )
        .ok()?
        .len;
    let tail = session
        .service()
        .output(
            id,
            run::ArtifactPart::Stderr,
            run::ByteRange {
                offset: length.saturating_sub(TAIL_BYTES),
                max_bytes: TAIL_BYTES,
            },
            call,
        )
        .ok()?;
    provider_error_from_stderr(&tail.bytes)
}

const PROVIDER_ERROR_BYTES: usize = 1024;

/// The provider's whole error message from a failed attempt's result, with its
/// lines joined and the text redacted and bounded.
fn provider_error_from_result(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if joined.is_empty() {
        return None;
    }
    let redacted = provenant_confinement::redact::known_shapes(joined.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())?;
    Some(crate::bounded_text(&redacted, PROVIDER_ERROR_BYTES))
}

/// Select a cause from captured stderr without promoting known Codex skill
/// discovery and other warning-level diagnostics to the failure headline.
fn provider_error_from_stderr(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty() && !warning_level_stderr_line(line))?;
    let redacted = provenant_confinement::redact::known_shapes(line.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())?;
    Some(crate::bounded_text(&redacted, 160))
}

/// Warning-level lines start with `warn`/`warning` (optionally bracketed or
/// followed by a colon), or are tracing-formatted: an RFC3339 timestamp, then
/// exactly `WARN`.
fn warning_level_stderr_line(line: &str) -> bool {
    let mut tokens = line.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    let level = first.trim_matches(['[', ']', ':']).to_ascii_lowercase();
    if matches!(level.as_str(), "warn" | "warning") {
        return true;
    }
    let bytes = first.as_bytes();
    let timestamp = bytes.len() > 4
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && first.contains('T');
    timestamp && tokens.next() == Some("WARN")
}

/// The exact bytes of a read, from the lossless encoding `output` reports.
pub(super) fn chunk_bytes(data: &Value) -> Option<Vec<u8>> {
    let bytes = &data["bytes"];
    let text = bytes["data"].as_str()?;
    match bytes["encoding"].as_str()? {
        "utf8" => Some(text.as_bytes().to_vec()),
        "base64" => provenant_fabric_runtime::handoff::decode_base64(text),
        _ => None,
    }
}

/// Observe the current worktree only while it still names the recorded head.
fn residual_tree_evidence(
    session: &Session,
    record: &run::AttemptRecord,
    call: &run::Request<'_>,
) -> Value {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let read = || -> Option<&'static str> {
        let task = session.store.request(record.attempt_id, call).ok()?.task;
        let worktree = task.writer?.worktree;
        let git = provenant_os::git::git_executable()?;
        let executor = SystemExecutor::new(1).ok()?;
        let run_git = |args: &[&str]| {
            let argv: Vec<std::ffi::OsString> = std::iter::once(git.as_os_str().to_owned())
                .chain(args.iter().map(|arg| std::ffi::OsString::from(*arg)))
                .collect();
            let mut options = ExecOptions::new(&argv, &worktree, Duration::from_secs(5));
            options.stdout = StreamPolicy::Capture { limit: 65536 };
            options.stderr = StreamPolicy::Discard;
            options.stop_on_output_limit = true;
            let output = executor.execute(options).ok()?;
            if output.termination != Termination::Exit(0)
                || output.timed_out
                || output.cancelled
                || output.stdout.truncated
                || output.stdout.read_failed
                || output.drain_truncated
            {
                return None;
            }
            Some(output.stdout.bytes)
        };
        let head = run_git(&["rev-parse", "--verify", "HEAD^{commit}"])?;
        if std::str::from_utf8(&head).ok()?.trim() != record.head?.to_string() {
            return None;
        }
        let status = run_git(&["status", "--porcelain", "-z", "--untracked-files=normal"])?;
        Some(if status.is_empty() {
            "clean (current observation)"
        } else {
            "uncommitted changes (current observation)"
        })
    };
    json!(read())
}

/// Use the recorded base and final head, never the caller's current checkout
/// diff. Pruned/private objects and read-only attempts may have no evidence.
pub(super) fn changed_file_evidence(
    session: &Session,
    record: &run::AttemptRecord,
    call: &run::Request<'_>,
) -> (Value, Value) {
    let read = || -> Option<(Vec<Option<provenant_records::StructuredPathWire>>, usize)> {
        let task = session.store.request(record.attempt_id, call).ok()?.task;
        let writer = task.writer?;
        let project = writer.project;
        let base = run::writer_base(session.context.state_root.as_path(), record).ok()??;
        let head = record.head?;
        let all = changed_paths(&project, base, head)?;
        Some((
            all.iter()
                .take(limits::OUTPUT_PATHS)
                .map(|path| {
                    (path.len() <= limits::PATH_BYTES)
                        .then(|| writer.worktree.join(path))
                        .and_then(|path| {
                            provenant_records::StructuredPathWire::from_path(&path).ok()
                        })
                })
                .collect(),
            all.len(),
        ))
    };
    read().map_or((Value::Null, Value::Null), |(paths, total)| {
        (json!(paths), json!(total))
    })
}

fn changed_paths(
    project: &std::path::Path,
    base: provenant_core::GitOid,
    head: provenant_core::GitOid,
) -> Option<Vec<String>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let git = provenant_os::git::git_executable()?;
    let argv = [
        git.into_os_string(),
        "diff".into(),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--name-only".into(),
        "-z".into(),
        base.to_string().into(),
        head.to_string().into(),
        "--".into(),
    ];
    let mut options = ExecOptions::new(&argv, project, Duration::from_secs(5));
    options.stdout = StreamPolicy::Capture { limit: 65536 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let output = SystemExecutor::new(1).ok()?.execute(options).ok()?;
    if output.termination != Termination::Exit(0)
        || output.timed_out
        || output.cancelled
        || output.stdout.truncated
        || output.stdout.read_failed
        || output.drain_truncated
    {
        return None;
    }
    let text = String::from_utf8(output.stdout.bytes).ok()?;
    let all: Vec<_> = text.split('\0').filter(|path| !path.is_empty()).collect();
    Some(all.into_iter().map(str::to_owned).collect())
}

pub(super) fn tail_start(bytes: &[u8], lines: u64) -> usize {
    bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| **b == b'\n')
        .nth(usize::try_from(lines.saturating_sub(1)).unwrap_or(usize::MAX))
        .map_or(0, |(i, _)| i + 1)
}

/// A clipped page that ends inside a UTF-8 character stops before it, so the
/// next offset starts the character whole and paged text concatenates exactly.
pub fn align_utf8_tail(chunk: &mut run::OutputChunk) {
    if !chunk.clipped {
        return;
    }
    let Err(error) = std::str::from_utf8(&chunk.bytes) else {
        return;
    };
    let valid = error.valid_up_to();
    if error.error_len().is_none() && valid > 0 {
        chunk.next_offset -= (chunk.bytes.len() - valid) as u64;
        chunk.bytes.truncate(valid);
        chunk.encoding = "utf8";
    }
}

/// The adapter recorded for a local attempt, read from its run record alone:
/// no transcript, output or provider is touched.
pub fn attempt_provider(
    edge: &Edge<'_>,
    attempt: provenant_core::AttemptId,
) -> Result<Option<String>, Fail> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let (_, store) = open_read_only(edge, &call)?;
    let record = store
        .ok_or(run::RunError::NotFound)?
        .record(attempt, &call)?;
    Ok([&record.applied_route, &record.resolved_route]
        .into_iter()
        .flatten()
        .find_map(|route| route.provider.clone()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn committed_paths_follow_git_across_edits_and_push() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let remote = temp.path().join("remote.git");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| -> String {
            String::from_utf8(provenant_os::fixture::run_git(&repo, args).unwrap())
                .unwrap()
                .trim()
                .to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["commit", "--allow-empty", "-qm", "base"]);
        let base: provenant_core::GitOid = git(&["rev-parse", "HEAD"]).parse().unwrap();
        git(&["init", "--bare", "-q", remote.to_str().unwrap()]);
        for step in 0..4 {
            if step == 1 || step == 3 {
                std::fs::write(repo.join(format!("file{step}")), step.to_string()).unwrap();
                git(&["add", "."]);
                git(&["commit", "-qm", "change"]);
            }
            if step == 2 {
                std::fs::write(repo.join("uncommitted"), "edit").unwrap();
                git(&[
                    "push",
                    "-q",
                    remote.to_str().unwrap(),
                    "HEAD:refs/heads/lane",
                ]);
            }
            let head: provenant_core::GitOid = git(&["rev-parse", "HEAD"]).parse().unwrap();
            let expected = git(&["diff", "--name-only", &base.to_string(), &head.to_string()]);
            let paths = super::changed_paths(&repo, base, head).unwrap();
            assert_eq!(
                paths,
                expected.lines().map(str::to_owned).collect::<Vec<_>>()
            );
        }
        let missing: provenant_core::GitOid =
            "1111111111111111111111111111111111111111".parse().unwrap();
        assert!(super::changed_paths(&repo, base, missing).is_none());
    }

    use super::{
        add_planner_fallback, fallback_reason_for_graph, output_summary,
        provider_error_from_result, provider_error_from_stderr,
    };
    use serde_json::json;

    #[test]
    fn output_followups_use_the_public_handle_for_local_and_peer_placements() {
        for host in [
            serde_json::Value::Null,
            json!("11111111111111111111111111111111"),
        ] {
            let mut data = json!({
                "attempt_id":"22222222222222222222222222222222", "host":host,
                "state":"running", "eof":false,
                "part":"result", "next_offset":42
            });
            let command = crate::Output {
                target: "repair/writer#2".parse().unwrap(),
                part: crate::Part::Result,
                to: None,
                path: false,
                offset: 0,
                tail: None,
                max_bytes: 65536,
            };
            let rendered = super::render_output(command.clone(), Ok((data.clone(), String::new())));
            let next = rendered.summary.unwrap().next.unwrap();
            assert_eq!(
                next,
                "provenant output repair/writer#2 --part result --offset 42"
            );
            data["eof"] = json!(true);
            let rendered = super::render_output(command, Ok((data, String::new())));
            assert_eq!(
                rendered.summary.unwrap().next.as_deref(),
                Some("provenant wait repair/writer#2")
            );
        }
    }

    #[test]
    fn output_summary_names_planner_fallback_and_reason() {
        let data = json!({
            "attempt_id":"attempt",
            "state":"succeeded",
            "planner_fallback":{"stage":"static", "reason":"planner route failed"}
        });
        let summary = output_summary(&data, "fallback completed", true, None);
        let text = summary.lines.join("\n");
        assert!(text.contains("planner fallback: static (reason: planner route failed)"));
    }

    #[test]
    fn output_summary_sanitises_planner_fallback_reason() {
        let data = json!({
            "planner_fallback":{"stage":"static", "reason":"fragment \u{1b}[31minvalid"}
        });
        let summary = output_summary(&data, "fallback completed", true, None);
        let text = summary.lines.join("\n");
        assert!(text.contains("planner fallback: static (reason: fragment [31minvalid)"));
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn planner_fallback_text_sanitises_reason_and_keeps_json_value() {
        let reason = "invalid fragment \u{1b}[31mpattern";
        let mut data = serde_json::Value::Null;
        let mut text = String::new();
        add_planner_fallback(&mut data, &mut text, reason.into());
        assert!(text.contains("Reason: invalid fragment [31mpattern"));
        assert!(!text.contains('\u{1b}'));
        assert_eq!(data["planner_fallback"]["reason"], reason);
    }

    #[test]
    fn output_finds_fallback_reason_for_every_node_of_the_graph() {
        // `synthesis` depends on the expansion but is not part of it.
        let checkpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"graph","plan_digest":"digest","nodes":[
                {"id":"planner", "state":"succeeded", "attempts":[],
                 "outputs":{"schema_version":2,"result":null,"verdict":null,"findings":[],"findings_available":null,"branch":null,"commit":null,"warnings":[],
                    "plan":{"fallback_reason":"planner fragment invalid: invalid fragment"}}},
                {"id":"planner--fallback--static", "state":"succeeded", "attempts":[]},
                {"id":"synthesis", "state":"succeeded", "attempts":[]}
            ],
            "expansions":[{"source":"planner","nodes":[{
                "id":"planner--fallback--static","access":"read_only","prompt":"Static stage","route":{"alias":"junior"}
            }]}]
        }))
        .unwrap();
        assert_eq!(
            fallback_reason_for_graph(&checkpoint).as_deref(),
            Some("planner fragment invalid: invalid fragment")
        );
    }

    #[test]
    fn output_reports_no_fallback_when_no_planner_fell_back() {
        let checkpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"graph","plan_digest":"digest","nodes":[
                {"id":"synthesis", "state":"succeeded", "attempts":[]}
            ]
        }))
        .unwrap();
        assert_eq!(fallback_reason_for_graph(&checkpoint), None);
    }

    #[test]
    fn provider_error_from_result_keeps_the_whole_multiline_message() {
        let message = provider_error_from_result(
            b"Model not recognized: gemini-3.8-pro\nAvailable models:\nGemini 2.5 Flash\nGPT-OSS 120B (Medium)\n",
        )
        .unwrap();
        assert!(message.starts_with("Model not recognized: gemini-3.8-pro; "));
        assert!(message.ends_with("GPT-OSS 120B (Medium)"));
        assert_eq!(provider_error_from_result(b"\n  \n"), None);
    }

    #[test]
    fn provider_error_uses_failure_before_trailing_tracing_warning() {
        let stderr = b"provider process failed: exit code 1\n\
2026-11-02T00:00:00.123456Z  WARN codex_core: skill scan failed\n";
        assert_eq!(
            provider_error_from_stderr(stderr).as_deref(),
            Some("provider process failed: exit code 1")
        );
    }

    #[test]
    fn provider_error_keeps_non_warning_lines_that_mention_warn() {
        assert_eq!(
            provider_error_from_stderr(b"test-failed WARN: assertion failed\n").as_deref(),
            Some("test-failed WARN: assertion failed")
        );
    }

    #[test]
    fn provider_error_skips_warning_level_lines() {
        assert_eq!(
            provider_error_from_stderr(
                b"warning: retrying request\nreal failure\n2026-11-02T00:00:00Z WARN cleanup failed\n"
            )
            .as_deref(),
            Some("real failure")
        );
    }

    #[test]
    fn provider_error_does_not_headline_a_warning_without_a_failure_line() {
        assert_eq!(
            provider_error_from_stderr(
                b"WARN: skill scan failed\n2026-11-02T00:00:00Z WARN cleanup failed\n"
            ),
            None
        );
    }
}
