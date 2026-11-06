use super::*;

pub fn output(command: Output, edge: &Edge<'_>) -> Reply {
    output_with(command, edge, None)
}

/// `output` for a local attempt whose store the caller already opened, so a
/// run that tails many lanes resolves the project and opens the store once
/// instead of once per lane.
pub(super) fn output_with(
    command: Output,
    edge: &Edge<'_>,
    local: Option<(&Session, &Target)>,
) -> Reply {
    let receipt_tail = matches!(command.part, Part::Receipt)
        .then_some(command.tail)
        .flatten();
    let headed = matches!(command.part, Part::Result) && !command.trace;
    let to = command.to.clone();
    let mut out = reply(
        "output",
        output_inner(command, edge, local).and_then(|(mut data, mut text)| {
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
        out.summary = Some(output_summary(&out.result.data, &out.human, headed));
    }
    out
}

/// What an agent needs from a read: how the attempt settled, what it changed,
/// and the whole final text up to a generous bound, then its head and tail.
/// Missing changed-file evidence stays null; it must never read as an empty diff.
pub(super) fn output_summary(data: &Value, text: &str, headed: bool) -> crate::Summary {
    let mut lines = Vec::new();
    if let Some(selected) = data.get("selected") {
        lines.push(format!(
            "attempt {} chosen: {} of {} attempts",
            data["attempt_id"].as_str().unwrap_or_default(),
            selected["rule"].as_str().unwrap_or("newest"),
            selected["attempts"]
        ));
    }
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
                    crate::bounded_text(error, 160)
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
    let target = data["attempt_id"].as_str().map(|id| {
        data["host"]
            .as_str()
            .map_or_else(|| id.to_owned(), |host| format!("{host}/{id}"))
    });
    let state = data["state"].as_str().unwrap_or_default();
    let next = target.and_then(|target| {
        if data["eof"] == json!(false) {
            let offset = data["next_offset"].as_u64()?;
            Some(format!(
                "provenant output {target} --part {} --offset {offset}",
                data["part"].as_str().unwrap_or("result")
            ))
        } else if matches!(state, "queued" | "starting" | "running" | "settling") {
            Some(format!("provenant status {target} --wait=4m"))
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
        "{label}: {qualified_model}@{effort} ({family}; resolved from {resolved_from}), run {}/{}/{}",
        record.run_id, record.task_id, record.attempt_id
    );
    let attribution = format!(
        "Reviewer provenance: {provenance}, actor {actor}, model {reviewer_model}, family {reviewer_family}"
    );
    Ok((
        json!({"run_id": record.run_id, "task_id": record.task_id, "attempt_id": record.attempt_id, "adapter": adapter, "model": model, "effort": effort, "family": family, "role": role, "resolved_from": resolved_from, "receipt": line, "reviewer": {"provenance": provenance, "actor_id": actor, "model": reviewer_model, "family": reviewer_family}, "reviewer_provenance": attribution}),
        format!("{line}\n{attribution}\n"),
    ))
}

/// The most lines `output --tail` returns; a larger request is clamped.
pub(super) const TAIL_MAX: u64 = limits::TAIL_LINES;

pub(super) fn output_inner(
    mut command: Output,
    edge: &Edge<'_>,
    local: Option<(&Session, &Target)>,
) -> Result<(Value, String), Fail> {
    if let Some((session, target)) = local {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel)?;
        return output_read(command, edge, session, target.clone(), &call);
    }
    let site = match command.host.as_deref() {
        Some(host) => Some(remote::site(edge, host)?),
        None => command
            .attempt
            .0
            .run
            .or_else(|| {
                command
                    .attempt
                    .0
                    .selection
                    .as_deref()?
                    .split_once('/')?
                    .0
                    .parse()
                    .ok()
            })
            .and_then(|host| remote::qualifier_site(edge, host)),
    };
    if let Some(remote::Site::Peer(host)) = site {
        if command.attempt.0.selection.is_some() {
            command.attempt.0 = remote::resolve_prefix(edge, host, &command.attempt.0)?;
        }
        return remote::output_on_peer(&command, host, edge);
    }
    // `HOST/ATTEMPT` is the form a graph `attempt_ref` has; a host qualifier
    // is not a run, so it never has to match the record.
    if command.attempt.is_qualified_pair()
        && let Some(qualifier) = command.attempt.0.run
    {
        match remote::qualifier_site(edge, qualifier) {
            Some(remote::Site::Peer(host)) if site.is_none() => {
                return remote::output_on_peer(&command, host, edge);
            }
            Some(remote::Site::Local) => command.attempt.0.run = None,
            _ => {}
        }
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    // A RUN or RUN/TASK names attempts, so every part resolves runs as receipts do.
    let candidate = targets::resolve(edge, &command.attempt.0, true)?;
    command.attempt.0 = candidate.target.clone();
    if site.is_none()
        && let Some(placement) = targets::placement(edge, &candidate.target, false)?
    {
        return remote::output(&command, &placement, edge);
    }
    let (session, target) = targets::read_resolved(edge, candidate, &call)?;
    output_read(command, edge, &session, target, &call)
}

/// Read the selected part and attach the shared lane projection.
fn output_read(
    command: Output,
    edge: &Edge<'_>,
    session: &Session,
    target: Target,
    call: &run::Request<'_>,
) -> Result<(Value, String), Fail> {
    let (mut data, human) = output_read_part(command, edge, session, target, call)?;
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
    let mut record = session.service().status(id, call)?;
    cloud_owner::hydrate(&session.store, std::slice::from_mut(&mut record), call)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
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
    target: Target,
    call: &run::Request<'_>,
) -> Result<(Value, String), Fail> {
    let receipt = matches!(command.part, Part::Receipt);
    let mut selected = None;
    let id = match target.attempt {
        Some(id) => id,
        None if receipt => {
            let page = session.service().lanes(
                &lane_query(session, std::slice::from_ref(&target), true, 2, 0, call)?,
                call,
            )?;
            if page.lanes.len() != 1 || page.next.is_some() {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Conflict,
                    "receipt target must identify exactly one attempt; specify RUN/TASK/ATTEMPT",
                ));
            }
            page.lanes[0].attempt_id
        }
        None => {
            let (id, attempts, rule) = newest_attempt(session, &target, call)?;
            selected = Some(json!({ "attempts": attempts, "rule": rule }));
            id
        }
    };
    let record = session.service().status(id, call)?;
    if target.run.is_some_and(|run| record.run_id != run)
        || target.task.is_some_and(|task| task != record.task_id)
    {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::NotFound,
            "attempt is not in that run/task",
        ));
    }
    if !command.trace
        && matches!(command.part, Part::Result | Part::Receipt)
        && let Some(placement) = cloud_owner::placement(&session.store, &record, call)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?
    {
        let job = cloud_owner::load(&session.store, id, call)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error))?;
        if command.path {
            if !matches!(command.part, Part::Result) {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "--path needs --part result, prompt or stderr",
                ));
            }
            if job.is_none() {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::NotFound,
                    "cloud job has no recorded result path",
                ));
            }
            let path = artifact(
                session.context.state_root.as_path(),
                &record,
                "cloud/cloud-job.json",
            );
            return Ok((
                json!({"attempt_id": id, "part": "result", "path": path.display().to_string()}),
                format!("{}\n", path.display()),
            ));
        }
        let view = job.as_ref().map(cloud_owner::view).unwrap_or_else(
            || json!({"target": placement.target, "state": {"state": "submitting"}}),
        );
        let encoded = serde_json::to_vec_pretty(&view)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
        let safe = provenant_confinement::redact::known_shapes(&encoded)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
        if command.offset > safe.len() as u64 {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                "offset past cloud result",
            ));
        }
        let start = if let Some(lines) = command.tail {
            let base = safe.len().saturating_sub(command.max_bytes as usize);
            base + tail_start(&safe[base..], lines.min(TAIL_MAX))
        } else {
            command.offset as usize
        };
        let end = start
            .saturating_add(command.max_bytes as usize)
            .min(safe.len());
        let bytes = safe[start..end].to_vec();
        let human = String::from_utf8_lossy(&bytes).into_owned();
        let data = json!({"attempt_id": id, "cloud": view, "part": "result", "offset": start, "next_offset": end, "length": safe.len(), "eof": end == safe.len(), "clipped": end < safe.len(), "complete": job.as_ref().is_some_and(|job| job.state.is_terminal()), "digest": provenant_records::CanonicalV1::file_digest(&safe), "bytes": provenant_fabric_runtime::coordination::facade::OutputBytes::from_bytes(bytes)});
        return Ok((data, human));
    }
    if command.trace {
        let grants = json!({
            "read_only_writable": record.read_only_writable,
            "write_grants": record.write_grants,
            "git_grants_digest": record.git_grants_digest,
            "user_skills": record.user_skills(),
        });
        let grant_lines = grants_lines(&record);
        let trace = trace_chain(&session.store, record, call)?;
        let human = trace
            .iter()
            .map(trace_line)
            .chain([grant_lines])
            .collect::<String>();
        return Ok((
            json!({ "attempt_id": id, "trace": trace, "grants": grants }),
            human,
        ));
    }
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
            let task = session
                .store
                .request(record.run_id, call)?
                .tasks
                .into_iter()
                .find(|task| task.task_id == record.task_id)
                .ok_or(run::RunError::NotFound)?;
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
                "selected": selected,
            }),
            format!("{reason}\n"),
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
    if let Some(lines) = tail {
        data["tail"] = json!(lines);
        data["tail_clamped"] = json!(command.tail != tail);
    }
    if matches!(command.part, Part::Result) && command.tail.is_none() && command.offset == 0 {
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
    if let Some(selected) = selected {
        data["selected"] = selected;
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

/// The attempt `output` reads for a RUN or RUN/TASK: the newest settled one,
/// else the newest. Several tasks in one run are ambiguous, so they are refused.
pub(super) fn newest_attempt(
    session: &Session,
    target: &Target,
    call: &run::Request<'_>,
) -> Result<(AttemptId, usize, &'static str), Fail> {
    let mut query = lane_query(session, std::slice::from_ref(target), true, 100, 0, call)?;
    query.newest_first = true;
    let page = session.service().lanes(&query, call)?;
    newest_settled_of(&page.lanes)
}

/// Choose from a page of attempts, newest first. Several tasks are ambiguous.
pub(super) fn newest_settled_of(
    lanes: &[run::AttemptRecord],
) -> Result<(AttemptId, usize, &'static str), Fail> {
    let mut tasks: Vec<_> = lanes.iter().map(|lane| lane.task_id).collect();
    tasks.sort();
    tasks.dedup();
    match tasks.len() {
        0 => {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::NotFound,
                "no attempt in that run",
            ));
        }
        1 => {}
        count => {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::Conflict,
                format!("run has {count} tasks; name one as RUN/TASK"),
            ));
        }
    }
    let settled = lanes
        .iter()
        .find(|lane| matches!(lane.state, run::AttemptState::Terminal { .. }));
    Ok(match settled {
        Some(lane) => (lane.attempt_id, lanes.len(), "newest settled"),
        None => (lanes[0].attempt_id, lanes.len(), "newest"),
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

/// Use the recorded base and final head, never the caller's current checkout
/// diff. Pruned/private objects and read-only attempts may have no evidence.
pub(super) fn changed_file_evidence(
    session: &Session,
    record: &run::AttemptRecord,
    call: &run::Request<'_>,
) -> (Value, Value) {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let read = || -> Option<(Vec<Option<provenant_records::StructuredPathWire>>, usize)> {
        let task = session
            .store
            .request(record.run_id, call)
            .ok()?
            .tasks
            .into_iter()
            .find(|task| task.task_id == record.task_id)?;
        let project = task.writer?.project;
        let base: provenant_core::GitOid = task.base?.parse().ok()?;
        let head = record.head?;
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
        let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(5));
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
        Some((
            all.iter()
                .take(limits::OUTPUT_PATHS)
                .map(|path| {
                    (path.len() <= limits::PATH_BYTES)
                        .then(|| project.join(path))
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

/// The attempt's trace, then each parent's, nearest first. The chain ends at an
/// attempt with no parent, or at a parent this store does not hold (a nested
/// run's submitter lives in its parent's store), which appears as a hop that
/// knows only its attempt ID. A parent that repeats is a corrupt store and
/// fails.
#[derive(Debug, thiserror::Error)]
pub(super) enum TraceError {
    #[error("trace parents loop back to attempt {0}")]
    Loop(AttemptId),
    #[error(transparent)]
    Run(#[from] run::RunError),
}

impl From<TraceError> for Fail {
    fn from(error: TraceError) -> Self {
        match error {
            TraceError::Loop(_) => Fail::new(Status::Error, ErrorKind::Io, error.to_string()),
            TraceError::Run(error) => error.into(),
        }
    }
}

pub(super) fn trace_chain(
    store: &run::RunStore,
    record: run::AttemptRecord,
    call: &run::Request<'_>,
) -> Result<Vec<provenant_records::Trace>, TraceError> {
    let mut seen = std::collections::HashSet::from([record.attempt_id]);
    let mut chain = vec![record.trace];
    while let Some(parent) = chain.last().and_then(|t| t.parent) {
        if !seen.insert(parent) {
            return Err(TraceError::Loop(parent));
        }
        match run::Service::new(store).status(parent, call) {
            Ok(record) => chain.push(record.trace),
            Err(run::RunError::NotFound) => {
                chain.push(provenant_records::Trace {
                    attempt: Some(parent),
                    ..Default::default()
                });
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(chain)
}

/// The grants the launch applied, one line each. Older attempts show none.
pub(super) fn grants_lines(record: &run::AttemptRecord) -> String {
    let mut text = String::new();
    for grant in &record.write_grants {
        let path = grant
            .path
            .to_path_buf()
            .map_or_else(|_| "-".to_owned(), |p| p.display().to_string());
        let [source, scope] = [
            serde_json::to_value(grant.source),
            serde_json::to_value(grant.scope),
        ]
        .map(|v| {
            v.ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        });
        text.push_str(&format!(
            "grant write source={source} scope={scope} path={}\n",
            plain_text(&path, usize::MAX)
        ));
    }
    for path in &record.read_only_writable {
        text.push_str(&format!(
            "grant read_only_writable path={}\n",
            plain_text(path, usize::MAX)
        ));
    }
    if let Some(digest) = record.git_grants_digest {
        text.push_str(&format!("grant git digest={digest}\n"));
    }
    if let Some(warnings) = record
        .applied_route
        .as_ref()
        .and_then(|route| route.extra.get("discovery_warnings"))
        .and_then(serde_json::Value::as_array)
    {
        for warning in warnings.iter().filter_map(serde_json::Value::as_str) {
            text.push_str(&format!("warning: {}\n", plain_text(warning, usize::MAX)));
        }
    }
    if let Some(tools) = record
        .applied_route
        .as_ref()
        .and_then(|route| route.extra.get("tool_closure"))
        .and_then(serde_json::Value::as_array)
    {
        for tool in tools.iter().filter(|tool| tool["outcome"] == "incomplete") {
            let name = tool["tool"].as_str().unwrap_or("unknown");
            if let Some(reasons) = tool["reasons"].as_array() {
                for reason in reasons.iter().filter_map(serde_json::Value::as_str) {
                    text.push_str(&format!(
                        "warning tool_closure_incomplete tool={} reason={}\n",
                        plain_text(name, 128),
                        plain_text(reason, 64)
                    ));
                }
            }
        }
    }
    if let Some(skills) = record.user_skills() {
        text.push_str(&format!(
            "grant user_skills {}\n",
            plain_text(&skills.to_string(), usize::MAX)
        ));
    }
    text
}

pub(super) fn trace_line(trace: &provenant_records::Trace) -> String {
    fn field(name: &str, value: Option<impl std::fmt::Display>) -> String {
        format!(
            "{name}={}",
            value.map_or_else(|| "-".to_owned(), |v| v.to_string())
        )
    }
    format!(
        "{} {} {} {} {} {} {}\n",
        field("host", trace.host),
        field("project", trace.project),
        field("run", trace.run),
        field("attempt", trace.attempt),
        field("operation", trace.operation.as_ref().map(|o| o.as_str())),
        field("actor", trace.actor),
        field("parent", trace.parent),
    )
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
    let target = Target {
        selection: None,
        run: None,
        task: None,
        attempt: Some(attempt),
    };
    let (session, selected) = targets::open(edge, &target, false, &call)?;
    let record = session
        .store
        .record(selected.attempt.unwrap_or(attempt), &call)?;
    Ok([&record.applied_route, &record.resolved_route]
        .into_iter()
        .flatten()
        .find_map(|route| route.provider.clone()))
}

#[cfg(test)]
mod tests {
    use super::{
        add_planner_fallback, fallback_reason_for_graph, output_summary, provider_error_from_stderr,
    };
    use serde_json::json;

    #[test]
    fn output_summary_names_planner_fallback_and_reason() {
        let data = json!({
            "attempt_id":"attempt",
            "state":"succeeded",
            "planner_fallback":{"stage":"static", "reason":"planner route failed"}
        });
        let summary = output_summary(&data, "fallback completed", true);
        let text = summary.lines.join("\n");
        assert!(text.contains("planner fallback: static (reason: planner route failed)"));
    }

    #[test]
    fn output_summary_sanitises_planner_fallback_reason() {
        let data = json!({
            "planner_fallback":{"stage":"static", "reason":"fragment \u{1b}[31minvalid"}
        });
        let summary = output_summary(&data, "fallback completed", true);
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
                 "outputs":{"schema_version":2,"result":null,"verdict":null,"findings":[],"branch":null,"commit":null,"warnings":[],
                    "plan":{"fallback_reason":"planner fragment invalid: invalid fragment"}}},
                {"id":"planner--fallback--static", "state":"succeeded", "attempts":[]},
                {"id":"synthesis", "state":"succeeded", "attempts":[]}
            ],
            "expansions":[{"source":"planner","continuation":false,"nodes":[{
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
