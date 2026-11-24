use super::*;

/// The budget's unit and counted usage, projected from the same limit and
/// usage record that `status` uses, so `result` and `status` cannot disagree.
fn budget_usage(record: &run::AttemptRecord) -> Option<Value> {
    let usage = record.limits.as_ref()?.budget_usage(
        &record.usage,
        record
            .applied_route
            .as_ref()
            .and_then(|route| route.provider.as_deref()),
    )?;
    serde_json::to_value(usage).ok()
}

/// Build the one result projection from a host-owned read. Host facts come
/// from observed data; the handoff and verify are lane claims, never host
/// attestations. An unobserved fact stays unknown.
/// Spec: docs/specs/lane-contracts.md#result-projection
pub(super) fn result_projection(
    data: &Value,
    attempt: AttemptId,
) -> provenant_records::lane_contract::ResultProjection {
    use provenant_records::lane_contract::{
        Coverage, GitFacts, HostFacts, LaneClaims, Observed, ResultProjection,
    };

    let commands = run::observed_commands();
    let git_facts = GitFacts {
        base: data["writer_base"].as_str().map(str::to_owned),
        head: data["writer_head"]
            .as_str()
            .or_else(|| data["head"].as_str())
            .map(str::to_owned),
        commits: data["stop_work"]["ahead"]
            .as_u64()
            .into_iter()
            .map(|ahead| format!("{ahead} commit(s)"))
            .collect(),
        diffstat: data["changed_files_total"]
            .as_u64()
            .map(|total| format!("{total} changed file(s)")),
        shared_worktree: false,
        extra: Default::default(),
    };
    let git_complete = git_facts.base.is_some() || git_facts.head.is_some();
    let git = Observed {
        source: "host-observation".into(),
        completeness: if git_complete {
            Coverage::Partial
        } else {
            Coverage::Unknown
        },
        value: Some(git_facts),
        extra: Default::default(),
    };
    let ci: Observed<Value> = Observed {
        source: "unavailable".into(),
        completeness: Coverage::Unknown,
        value: None,
        extra: Default::default(),
    };
    let scope: Observed<provenant_records::handoff::WriteScopeReport> = Observed {
        source: "unavailable".into(),
        completeness: Coverage::Unknown,
        value: None,
        extra: Default::default(),
    };
    let handoff = serde_json::from_value::<provenant_records::handoff::Handoff>(
        data["handoff"]["Ok"].clone(),
    )
    .ok();
    let verify = handoff
        .as_ref()
        .map(|handoff| handoff.verify.clone())
        .or_else(|| {
            let verify = &data["selected"]["verify"];
            verify
                .is_array()
                .then(|| serde_json::from_value(verify.clone()).ok())
                .flatten()
        });
    ResultProjection::new(
        provenant_records::RecordHeader::V1,
        attempt,
        HostFacts {
            commands,
            git,
            ci,
            scope,
            extra: Default::default(),
        },
        LaneClaims {
            handoff,
            verify,
            extra: Default::default(),
        },
    )
}

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
            // A live attempt has no decoded result artifact yet. When nothing
            // is returned the caller labels the page as no-result-yet: an empty
            // page is not evidence that the attempt produced nothing.
            if headed
                && let Some(state) = data["state"].as_str()
                && matches!(state, "queued" | "starting" | "running" | "settling")
            {
                let state = state.to_owned();
                data["status"] = json!(state);
                if data["length"].as_u64().unwrap_or(0) == 0 {
                    data["result"] = json!("pending");
                    text = format!(
                        "{state}\nno result yet: the result is not sealed while the attempt is {state}\n{text}"
                    );
                } else {
                    data["result"] = json!("partial");
                    text = format!("{state}\npartial result\n{text}");
                }
            }
            if let Some(bytes) = receipt_tail {
                text = text[tail_start_bytes(text.as_bytes(), bytes)..].to_owned();
                data["tail"] = json!(bytes);
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
        let full = std::env::args_os().any(|arg| arg == "--full");
        if full {
            // The full view renders the owner's human text, so the host-fact
            // headers and the whole result become that text; compact keeps the
            // bounded summary.
            // Spec: docs/specs/lane-contracts.md#result-projection
            let summary = output_summary(&out.result.data, &out.human, headed);
            out.human = format!("{}\n", summary.lines.join("\n"));
            out.summary = Some(summary);
        } else {
            out.summary = Some(output_summary_default(&out.result.data, &out.human, headed));
        }
        out.result.next = output_next_step(&out.result.data, Some(&command.target))
            .into_iter()
            .collect();
        if warn_human
            && out
                .result
                .data
                .get("git_import_failure")
                .is_some_and(|value| !value.is_null())
            && let Some(reason) = out.result.data["reason"].as_str()
            && !out.human.contains(reason)
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
pub(super) fn output_summary(data: &Value, text: &str, headed: bool) -> crate::Summary {
    summary(data, text, headed, true)
}

/// The compact default view: verdict, findings and verify first, with the
/// host-fact narration and lane prose available through `--full`.
/// Spec: docs/specs/lane-contracts.md#result-projection
pub(super) fn output_summary_default(data: &Value, text: &str, headed: bool) -> crate::Summary {
    summary(data, text, headed, false)
}

fn summary(data: &Value, text: &str, headed: bool, full: bool) -> crate::Summary {
    let mut lines = Vec::new();
    if headed && let Some(verdict) = data["selected"]["verdict"].as_str() {
        lines.push(format!(
            "review verdict: {}",
            crate::bounded_text(verdict, 128)
        ));
    }
    if headed && let Some(findings) = data["selected"]["findings"].as_array() {
        let availability = match data["selected"]["findings_available"].as_bool() {
            Some(true) => "available",
            Some(false) => "incomplete",
            None => "availability unknown",
        };
        lines.push(format!("findings: {} ({availability})", findings.len()));
    }
    if headed {
        let verify = data["handoff"]["Ok"]["verify"]
            .as_array()
            .or_else(|| data["selected"]["verify"].as_array());
        if let Some(verify) = verify {
            lines.push(format!("verify: {} lane claim(s)", verify.len()));
        }
    }
    if headed
        && data.get("graph").is_some()
        && data.get("node").is_some()
        && data.get("attempt_id").is_none()
        && data["complete"] == json!(false)
    {
        lines.push("declared result excerpt is incomplete".into());
    }
    if full
        && headed
        && let Some(work) = data.get("stop_work").filter(|value| !value.is_null())
    {
        let head = work["head"].as_str().unwrap_or("unknown");
        let ahead = work["ahead"].as_u64();
        let pushed = work["push"] == "pushed";
        let mut line = match (ahead, pushed) {
            (Some(commits), true) => format!("{commits} commits pushed at {head}"),
            (Some(commits), false) => format!("{commits} commits unpushed at {head}"),
            (None, true) => format!("committed work pushed at {head}"),
            (None, false) => format!("stopped work at {head}; unpushed count unavailable"),
        };
        if let Some(dirty) = work["dirty_paths"].as_u64().filter(|count| *count > 0) {
            line.push_str(&format!("; {dirty} dirty paths left uncommitted"));
        }
        lines.push(line);
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
                    crate::bounded_text(error, PROVIDER_ERROR_BYTES)
                ));
            }
            if let Some(path) = structured_path_text(&data["result_path"]) {
                lines.push(format!("result: {}", crate::bounded_text(&path, 512)));
            }
        }
    }
    if let Some(error) = data.get("handoff").and_then(|value| value.get("Err"))
        && let Ok(error) =
            serde_json::from_value::<provenant_records::handoff::HandoffError>(error.clone())
    {
        lines.push(crate::bounded_text(&error.to_string(), 2048));
    }
    if full
        && headed
        && let Some(fallback) = data.get("planner_fallback")
    {
        lines.push(format!(
            "planner fallback: {} (reason: {})",
            fallback["stage"].as_str().unwrap_or("static"),
            plain_text(fallback["reason"].as_str().unwrap_or_default(), 160)
        ));
    }
    if full
        && headed
        && let Ok(access) = serde_json::from_value::<provenant_records::LaneAccessReceipt>(
            data["applied_route"]["lane_access"].clone(),
        )
    {
        lines.push(format!(
            "access: {} ({} {})",
            access.access.as_str(),
            access.key,
            access.source.as_str()
        ));
    }
    if full && headed {
        let availability = data["applied_route"]["availability_warning"].as_str();
        let launch = data["applied_route"]["launch_warnings"].as_array();
        let mut seen = std::collections::BTreeSet::new();
        for warning in availability
            .into_iter()
            .chain(launch.into_iter().flatten().filter_map(Value::as_str))
        {
            let warning = plain_text(warning, 2048);
            let warning = if warning.starts_with("warning:") {
                warning
            } else {
                format!("warning: {warning}")
            };
            if seen.insert(warning.clone()) {
                lines.push(warning);
            }
        }
    }
    if full && headed {
        // A local read records writer-specific fields; a peer read projects the
        // owning lane instead, where the committed head is `head` and a pushed
        // publication is evidence in `delivery`. Read both shapes so a peer's
        // real head and publication are not silently dropped. A missing or null
        // value is absent evidence: never render a header saying "unavailable".
        // Spec: docs/specs/lane-contracts.md#result-projection
        let lane_publication = (data["delivery"]["state"]["state"].as_str() == Some("delivered")
            && data["delivery"]["integration"]["remote"]
                .as_str()
                .is_some_and(|remote| !remote.is_empty()))
        .then_some("pushed");
        for (label, value) in [
            ("base", data["writer_base"].as_str()),
            (
                "committed head",
                data["writer_head"]
                    .as_str()
                    .or_else(|| data["head"].as_str()),
            ),
            (
                "publication",
                data["publication"].as_str().or(lane_publication),
            ),
            ("residual tree state", data["residual_tree"].as_str()),
        ] {
            if let Some(value) = value {
                lines.push(format!("{label}: {value}"));
            }
        }
    }
    if full
        && headed
        && let Some(paths) = data.get("changed_files").and_then(Value::as_array)
    {
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
        lines.push(format!("changed files ({total}): {listed}{more}"));
    }
    // An empty result uses its safe diagnostic as human text. The settlement
    // header already contains that detail, so render it only once.
    if !headed || data["reason"].as_str() != Some(text.trim()) {
        let body = if full {
            text.to_owned()
        } else {
            crate::head_and_tail(text, limits::OUTPUT_BYTES)
        };
        lines.extend(body.lines().map(str::to_owned));
    }
    crate::Summary { lines, omitted: 0 }
}

/// The owner's suggested command travels on the envelope's `next` array, never
/// as a line inside the compact summary. The edge renders it once from the argv.
pub(super) fn output_next_step(
    data: &Value,
    target: Option<&crate::Target>,
) -> Option<provenant_records::command_result::NextStep> {
    use provenant_records::command_result::NextStep;
    let target = target?;
    let state = data["state"].as_str().unwrap_or_default();
    if data["eof"] == json!(false) {
        let offset = data["next_offset"].as_u64()?;
        let part = data["part"].as_str().unwrap_or("result");
        Some(NextStep::from_args(
            &[
                "provenant",
                "output",
                &target.to_string(),
                "--part",
                part,
                "--offset",
                &offset.to_string(),
            ],
            "read the next page of this output",
        ))
    } else if matches!(state, "queued" | "starting" | "running" | "settling") {
        let handle = target.to_string();
        Some(NextStep::from_args(
            &["provenant", "wait", &handle],
            "wait for this work to settle",
        ))
    } else {
        None
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
                for field in ["user_skills", "lane_access"] {
                    grants.insert(
                        field.into(),
                        route.get(field).cloned().unwrap_or(Value::Null),
                    );
                }
            }
        }
        data.extend(row.clone());
    }
    data["result_projection"] =
        serde_json::to_value(result_projection(&data, id)).unwrap_or(Value::Null);
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
        let mut data = json!({
            "attempt_id": id, "state": state_and_reason(&record.state).0,
            "reason": reason, "part": "result", "offset": 0, "next_offset": 0,
            "length": null, "eof": true, "clipped": null, "complete": null,
            "digest": null, "bytes": json!({"encoding":"utf8", "data":""}),
        });
        if let Some(stop_work) = &record.stop_work {
            data["stop_work"] = json!(stop_work);
        }
        if let Some(usage) = budget_usage(&record) {
            data["budget_usage"] = usage;
        }
        return Ok((data, format!("{reason}\n")));
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
    let tail = command.tail;
    if let Some(bytes) = tail {
        range.offset = chunk.len.saturating_sub(command.max_bytes);
        // A short transcript's first page already includes the requested tail.
        if !matches!(part, run::ArtifactPart::Transcript) || range.offset != chunk.offset {
            chunk = session.service().output(id, part, range, call)?;
        }
        let start = tail_start_bytes(&chunk.bytes, bytes);
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
    if let Some(usage) = budget_usage(&record) {
        data["budget_usage"] = usage;
    }
    if evicted {
        // Retention removed the bulk payload; the record, result and capture
        // tails remain.
        data["payload"] = json!("evicted");
        if matches!(command.part, Part::Stderr) && text.is_empty() {
            text = "payload evicted by retention; the retained capture tail is empty\n".into();
        }
    }
    if let Some(bytes) = tail {
        data["tail"] = json!(bytes);
        data["tail_clamped"] = json!(false);
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
        // The wire schema declares `stop_work` an optional object: absent,
        // never null, when no stop evidence was recorded.
        if let Some(stop_work) = &record.stop_work {
            data["stop_work"] = json!(stop_work);
        }
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

/// The last non-empty stderr line of a failed attempt, bounded as reasons are.
/// The tail is read so a long capture still shows its final error.
pub(super) fn provider_error_line(
    session: &Session,
    id: AttemptId,
    call: &run::Request<'_>,
) -> Option<String> {
    provider_error_with(|part, range| session.service().output(id, part, range, call).ok())
}

/// Shared bounded provider evidence for output and status readers.
pub(super) fn provider_error_with(
    mut read: impl FnMut(run::ArtifactPart, run::ByteRange) -> Option<run::OutputChunk>,
) -> Option<String> {
    const TAIL_BYTES: u64 = 8192;
    if let Some(chunk) = read(
        run::ArtifactPart::Result,
        run::ByteRange {
            offset: 0,
            max_bytes: PROVIDER_ERROR_BYTES as u64,
        },
    ) && let Some(message) = provider_error_from_result(&chunk.bytes)
    {
        return Some(message);
    }
    let length = read(
        run::ArtifactPart::Stderr,
        run::ByteRange {
            offset: 0,
            max_bytes: 1,
        },
    )?
    .len;
    let tail = read(
        run::ArtifactPart::Stderr,
        run::ByteRange {
            offset: length.saturating_sub(TAIL_BYTES),
            max_bytes: TAIL_BYTES,
        },
    )?;
    provider_error_from_stderr(&tail.bytes)
}

const PROVIDER_ERROR_BYTES: usize = 1024;

/// The provider's whole error message from a failed attempt's result, with its
/// lines joined and the text bounded.
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
    Some(crate::bounded_text(&joined, PROVIDER_ERROR_BYTES))
}

/// Select a cause from captured stderr without promoting known Codex skill
/// discovery and other warning-level diagnostics to the failure headline.
fn provider_error_from_stderr(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty() && !warning_level_stderr_line(line))?;
    Some(crate::bounded_text(line, 160))
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
    // A deprecated key in the inherited provider config is advice. Codex
    // prints it on stderr without a level prefix; it is shown once as a
    // launch warning, never promoted to the attempt's error headline.
    if provenant_providers::codex::config_deprecation_notice(line) {
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

/// Byte offset that leaves at most `max_bytes` of `bytes`, aligned forward to
/// a UTF-8 boundary so the returned tail is whole text.
pub(super) fn tail_start_bytes(bytes: &[u8], max_bytes: u64) -> usize {
    let mut start = bytes.len().saturating_sub(max_bytes as usize);
    while start < bytes.len() && std::str::from_utf8(&bytes[start..]).is_err() {
        start += 1;
    }
    start
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
        Status, add_planner_fallback, fallback_reason_for_graph, output_summary,
        output_summary_default, provider_error_from_result, provider_error_from_stderr,
        tail_start_bytes,
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
            assert_eq!(
                rendered.result.next[0].argv,
                [
                    "provenant",
                    "output",
                    "repair/writer#2",
                    "--part",
                    "result",
                    "--offset",
                    "42"
                ]
            );
            data["eof"] = json!(true);
            let rendered = super::render_output(command, Ok((data, String::new())));
            assert_eq!(
                rendered.result.next[0].argv,
                ["provenant", "wait", "repair/writer#2"]
            );
        }
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn a_running_result_envelope_is_ok_and_labels_partial_or_pending() {
        let command = crate::Output {
            target: "repair/writer#2".parse().unwrap(),
            part: crate::Part::Result,
            to: None,
            path: false,
            offset: 0,
            tail: None,
            max_bytes: 65536,
        };
        // An unsealed result with no bytes is a typed pending page, never an
        // empty partial result.
        let pending = json!({
            "attempt_id": "22222222222222222222222222222222",
            "state": "running",
            "reason": null,
            "part": "result",
            "offset": 0, "next_offset": 0, "length": 0,
            "eof": true, "clipped": false, "complete": false,
            "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "bytes": {"encoding": "utf8", "data": ""},
        });
        let reply = super::render_output(command.clone(), Ok((pending, String::new())));
        assert_eq!(reply.result.status, Status::Ok);
        assert_eq!(reply.result.data["status"], "running");
        assert_eq!(reply.result.data["state"], "running");
        assert_eq!(reply.result.data["result"], "pending");
        assert!(
            reply
                .human
                .starts_with("running\nno result yet: the result is not sealed"),
            "{}",
            reply.human
        );
        assert!(!reply.human.contains("partial result"), "{}", reply.human);

        // Bytes that did arrive before settlement are labelled partial.
        let partial = json!({
            "attempt_id": "22222222222222222222222222222222",
            "state": "running",
            "reason": null,
            "part": "result",
            "offset": 0, "next_offset": 13, "length": 13,
            "eof": true, "clipped": false, "complete": false,
            "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "bytes": {"encoding": "utf8", "data": "half a result"},
        });
        let reply = super::render_output(command.clone(), Ok((partial, "half a result".into())));
        assert_eq!(reply.result.status, Status::Ok);
        assert_eq!(reply.result.data["result"], "partial");
        assert!(reply.human.starts_with("running\npartial result\n"));
        assert!(reply.human.contains("half a result"));
        let summary = reply
            .summary
            .expect("a headed result has a summary")
            .lines
            .join("\n");
        assert!(summary.contains("partial result"), "{summary}");

        let settled = json!({
            "attempt_id": "22222222222222222222222222222222",
            "state": "succeeded",
            "part": "result",
            "eof": true, "complete": true,
            "bytes": {"encoding": "utf8", "data": "all of it"},
        });
        let reply = super::render_output(command, Ok((settled, "all of it".into())));
        assert!(reply.result.data.get("status").is_none());
        assert!(reply.result.data.get("result").is_none());
        assert!(!reply.human.contains("partial result"));
        assert!(!reply.human.contains("no result yet"));
    }

    #[test]
    fn output_summary_reports_stopped_commits_and_dirty_paths() {
        let unpushed = serde_json::json!({
            "state":"cancelled",
            "stop_work":{"head":"abcdef0123456789","ahead":2,"dirty_paths":3,"push":"not_granted"}
        });
        let summary = output_summary(&unpushed, "", true);
        assert!(
            summary.lines.iter().any(|line| line
                == "2 commits unpushed at abcdef0123456789; 3 dirty paths left uncommitted")
        );
        let pushed = serde_json::json!({
            "state":"cancelled",
            "stop_work":{"head":"abcdef0123456789","ahead":2,"dirty_paths":null,"push":"pushed"}
        });
        let summary = output_summary(&pushed, "", true);
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line == "2 commits pushed at abcdef0123456789")
        );
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn output_summary_prints_headers_only_for_non_null_values() {
        let null_headers = json!({
            "state":"succeeded",
            "writer_base":null, "writer_head":null, "publication":null,
            "residual_tree":null, "changed_files":null
        });
        let summary = output_summary(&null_headers, "answer", true);
        for key in [
            "base",
            "committed head",
            "publication",
            "residual tree state",
            "changed files",
        ] {
            let prefix = format!("{key}:");
            assert!(
                !summary.lines.iter().any(|line| line.starts_with(&prefix)),
                "null {key} still rendered a header: {:?}",
                summary.lines
            );
        }

        let real_headers = json!({
            "state":"succeeded",
            "writer_base":"aaaa", "writer_head":"bbbb", "publication":"pushed",
            "residual_tree":"clean (current observation)",
            "changed_files":["src/main.rs"], "changed_files_total":1
        });
        let summary = output_summary(&real_headers, "", true);
        for (label, value) in [
            ("base", "aaaa"),
            ("committed head", "bbbb"),
            ("publication", "pushed"),
            ("residual tree state", "clean (current observation)"),
        ] {
            let expected = format!("{label}: {value}");
            assert!(
                summary.lines.contains(&expected),
                "non-null {label} lost its header: {:?}",
                summary.lines
            );
        }
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.starts_with("changed files (1):"))
        );
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn output_summary_keeps_local_headers_without_unavailable_lines() {
        // The local read path (`output_read_part`) always inserts the writer
        // keys, as null on a read-only or unpublished lane. Drive that shape
        // through `render_output`: non-applicable headers are omitted, and no
        // line says "unavailable". This invariant already held before the gate
        // removal; keep it as a regression guard for the local path.
        let command = crate::Output {
            target: "repair/writer#2".parse().unwrap(),
            part: crate::Part::Result,
            to: None,
            path: false,
            offset: 0,
            tail: None,
            max_bytes: 65536,
        };
        let local = json!({
            "state":"succeeded", "part":"result",
            "writer_base":null, "writer_head":null,
            "residual_tree":null, "publication":null, "changed_files":null
        });
        let rendered = super::render_output(command, Ok((local, "answer".into())));
        let summary = output_summary(&rendered.result.data, &rendered.human, true);
        for label in [
            "base",
            "committed head",
            "publication",
            "residual tree state",
        ] {
            assert!(
                !summary
                    .lines
                    .iter()
                    .any(|line| line.starts_with(&format!("{label}:"))),
                "null {label} rendered a header: {:?}",
                summary.lines
            );
        }
        assert!(
            !summary
                .lines
                .iter()
                .any(|line| line.contains("unavailable")),
            "a local null writer field rendered as unavailable: {:?}",
            summary.lines
        );

        // An unobserved base must not hide a real committed head or publication.
        let with_real_values = json!({
            "state":"succeeded", "writer_base":null,
            "writer_head":"cccc", "publication":"pushed"
        });
        let summary = output_summary(&with_real_values, "", true);
        for expected in ["committed head: cccc", "publication: pushed"] {
            assert!(
                summary.lines.iter().any(|line| line == expected),
                "{expected} lost when base was unobserved: {:?}",
                summary.lines
            );
        }
        assert!(
            !summary.lines.iter().any(|line| line.starts_with("base:")),
            "absent base rendered a header: {:?}",
            summary.lines
        );
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn peer_output_renders_committed_head_and_publication_from_lane_projection() {
        use provenant_fabric_runtime::coordination::facade::{
            AttemptTarget, OutputBytes, OutputInput, OutputPart, OutputResult,
        };
        let attempt = provenant_core::AttemptId::from_bytes([2; 16]);
        let host = provenant_core::HostId::from_bytes([3; 16]);
        let command = crate::Output {
            target: "repair/writer#2".parse().unwrap(),
            part: crate::Part::Result,
            to: None,
            path: false,
            offset: 0,
            tail: None,
            max_bytes: 65536,
        };
        // A peer chunk's forward evidence is the owning lane projection: it
        // carries `head` and `delivery`, never `writer_*` fields. Feed that
        // shape through the real peer read so the assertion pins the path a
        // reader actually takes, not a hand-built summary.
        let extra: provenant_records::ExtraFields = serde_json::from_value(json!({
            "head": "cccc",
            "delivery": {
                "schema_version": 1,
                "remote": "origin",
                "branch": "lane",
                "state": {"state": "delivered"},
                "integration": {
                    "remote": "origin", "target": "refs/heads/main",
                    "previous": null, "integrated": "dddd"
                }
            }
        }))
        .unwrap();
        let chunk = OutputResult::Chunk {
            peer: Default::default(),
            length: Some(2),
            planner_fallback: None,
            extra,
            target: AttemptTarget {
                extra: Default::default(),
                attempt,
                host: Some(host),
            },
            part: OutputPart::Result,
            state: "succeeded".into(),
            reason: None,
            offset: 0,
            next_offset: 2,
            eof: true,
            digest: provenant_core::Digest::from_bytes([6; 32]),
            bytes: OutputBytes::Utf8("ok".into()),
        };
        let reply =
            super::output_from_host(command, host, attempt, |_: OutputInput| Ok(chunk.clone()));
        assert_eq!(reply.result.status, Status::Ok);
        assert_eq!(reply.result.data["head"], json!("cccc"));
        assert_eq!(
            reply.result.data["delivery"]["state"]["state"],
            json!("delivered")
        );
        let summary = output_summary(&reply.result.data, &reply.human, true);
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line == "committed head: cccc"),
            "peer committed head was dropped: {:?}",
            summary.lines
        );
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line == "publication: pushed"),
            "peer publication was dropped: {:?}",
            summary.lines
        );
        assert!(
            !summary
                .lines
                .iter()
                .any(|line| line.contains("unavailable")),
            "peer lane fields rendered as unavailable: {:?}",
            summary.lines
        );
    }

    #[test]
    fn output_summary_shows_malformed_handoff_field_diagnostics() {
        let error =
            provenant_records::handoff::HandoffError::new("spec_concerns[0]", "expected concern");
        let data = json!({"state":"review_required", "cause":"handoff_malformed", "handoff":{"Err":error}});
        let summary = output_summary(&data, "captured answer", true);
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.contains("spec_concerns[0]"))
        );
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.contains("expected concern"))
        );
    }

    #[test]
    fn output_summary_surfaces_structured_route_warnings_once() {
        let data = json!({
            "state":"succeeded",
            "applied_route":{
                "availability_warning":"account work availability is stale; the provider will check it",
                "launch_warnings":[
                    "warning: pinned model is outside the configured provider scope",
                    "warning: image input is not supported"
                ]
            }
        });
        let summary = output_summary(&data, "answer", true);
        assert_eq!(
            summary
                .lines
                .iter()
                .filter(|line| line.as_str()
                    == "warning: pinned model is outside the configured provider scope")
                .count(),
            1
        );
        assert!(summary.lines.iter().any(|line| {
            line == "warning: account work availability is stale; the provider will check it"
        }));
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line == "warning: image input is not supported")
        );
    }

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
    fn local_provider_errors_are_raw_in_off_and_secrets_modes() {
        let text = b"failure ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        for mode in [provenant_redact::Mode::Off, provenant_redact::Mode::Secrets] {
            let _mode = provenant_redact::scoped(mode);
            assert_eq!(
                provider_error_from_result(text).unwrap(),
                String::from_utf8_lossy(text)
            );
            assert_eq!(
                provider_error_from_stderr(text).unwrap(),
                String::from_utf8_lossy(text)
            );
        }
    }

    #[test]
    fn local_provider_errors_keep_credential_shapes_and_assignments_raw() {
        let text = b"provider failed: sk-abcdefghijklmnopqrstuvwxyz password=\"printed-value\"";
        for mode in [provenant_redact::Mode::Off, provenant_redact::Mode::Secrets] {
            let _mode = provenant_redact::scoped(mode);
            for reason in [
                provider_error_from_result(text),
                provider_error_from_stderr(text),
            ] {
                let reason = reason.unwrap();
                assert!(reason.contains("sk-abcdefghijklmnopqrstuvwxyz"));
                assert!(reason.contains("printed-value"));
            }
        }
    }

    #[test]
    fn provider_error_keeps_multiline_context() {
        let message = provider_error_from_result(
            b"Model not recognized: gemini-3.8-pro\npass: fix the code\nAvailable models:\nGemini 2.5 Flash\nGPT-OSS 120B (Medium)\n",
        )
        .unwrap();
        assert!(message.starts_with("Model not recognized: gemini-3.8-pro; "));
        assert!(message.ends_with("GPT-OSS 120B (Medium)"));
        assert!(message.contains("pass: fix the code"));
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
            provider_error_from_stderr(b"test-failed WARN: pass: fix the code\n").as_deref(),
            Some("test-failed WARN: pass: fix the code")
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

    // Spec: docs/specs/lane-contracts.md#result-projection
    #[test]
    fn default_view_leads_with_verdict_findings_and_verify() {
        let data = json!({
            "state": "succeeded",
            "selected": {
                "verdict": "PASS",
                "findings": [{"id":"F1"}],
                "findings_available": true
            },
            "handoff": {"Ok": {
                "schema_version": 2, "status": "done", "note": "done",
                "remaining": [], "decisions": [], "verify": ["ran tests"]
            }}
        });
        let summary = output_summary_default(&data, "narration", true);
        let lines = &summary.lines;
        let verdict = lines
            .iter()
            .position(|line| line.starts_with("review verdict:"));
        let findings = lines.iter().position(|line| line.starts_with("findings:"));
        let verify = lines.iter().position(|line| line.starts_with("verify:"));
        assert!(
            verdict.is_some() && findings.is_some() && verify.is_some(),
            "{lines:?}"
        );
        assert!(
            verdict.unwrap() < findings.unwrap() && findings.unwrap() < verify.unwrap(),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.starts_with("base:")),
            "{lines:?}"
        );
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn tail_returns_at_most_the_requested_bytes() {
        let bytes = b"0123456789abcdef";
        for n in [0u64, 1, 4, 15, 16, 64] {
            let start = tail_start_bytes(bytes, n);
            assert!(bytes.len() - start <= n as usize, "n={n}, start={start}");
            assert!(std::str::from_utf8(&bytes[start..]).is_ok(), "n={n}");
        }
    }
}
