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
pub fn render_output(command: Output, read: Result<(Value, String), Fail>, full: bool) -> Reply {
    let receipt_tail = matches!(command.part, Part::Receipt)
        .then(|| command.tail_selection())
        .flatten();
    let headed = matches!(command.part, Part::Result);
    let warn_human = headed && !command.path && command.to.is_none();
    let to = command.to.clone();
    let final_result = command.final_result;
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
            if let Some(tail) = command.tail_selection() {
                match tail {
                    crate::Tail::Bytes(bytes) => data["tail"] = json!(bytes),
                    crate::Tail::Lines(lines) => data["tail_lines"] = json!(lines),
                }
                data["tail_clamped"] = json!(command.tail_clamped());
            }
            if let Some(tail) = receipt_tail {
                let start = match tail {
                    crate::Tail::Bytes(bytes) => tail_start_bytes(text.as_bytes(), bytes),
                    crate::Tail::Lines(lines) => tail_start_lines(text.as_bytes(), lines),
                };
                text = text[start..].to_owned();
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
                if final_result {
                    data["bytes"] = json!({"encoding": "utf8", "data": ""});
                }
                text = format!("wrote {} bytes to {}\n", bytes.len(), path.display());
            }
            Ok((data, text))
        }),
    );
    out.read_only = true;
    if out.result.error.is_none() {
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
    full: bool,
) -> Reply {
    let read = if command.final_result {
        collect_final(&command, |page| {
            output_read(page, edge, session, attempt, call)
        })
    } else {
        output_read(command.clone(), edge, session, attempt, call)
    };
    render_output(command, read, full)
}

/// Read a peer attempt using the owning service's budget and cancellation.
pub fn output_from_host(
    command: Output,
    host: provenant_core::HostId,
    attempt: provenant_core::AttemptId,
    mut read: impl FnMut(
        provenant_fabric_runtime::coordination::facade::OutputInput,
    )
        -> Result<provenant_fabric_runtime::coordination::facade::OutputResult, Fail>,
    full: bool,
) -> Reply {
    let result = if command.final_result {
        collect_final(&command, |page| {
            remote::read_output_with(&page, host, attempt, &mut read)
        })
    } else {
        remote::read_output_with(&command, host, attempt, read)
    };
    render_output(command, result, full)
}

/// Collect a sealed result by stable, contiguous pages before writing the file.
pub fn collect_final(
    command: &Output,
    mut read: impl FnMut(Output) -> Result<(Value, String), Fail>,
) -> Result<(Value, String), Fail> {
    if !matches!(command.part, Part::Result) {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "--final requires --part result",
        ));
    }
    let corrupt = || {
        Fail::new(
            Status::Error,
            ErrorKind::StateCorrupt,
            "final result changed or returned an invalid page",
        )
    };
    let mut page = command.clone();
    page.final_result = false;
    page.to = None;
    page.offset = 0;
    let mut bytes = Vec::new();
    let mut first = None;
    let mut digest = None;
    let mut attempt = None;
    loop {
        let (data, _) = read(page.clone())?;
        if matches!(
            data["state"].as_str(),
            Some("queued" | "starting" | "running" | "settling")
        ) {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                "--final requires a settled result",
            ));
        }
        let page_attempt = data["attempt_id"].as_str().ok_or_else(|| {
            Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                "--final needs a lane attempt",
            )
        })?;
        if attempt
            .as_deref()
            .is_some_and(|expected| expected != page_attempt)
        {
            return Err(corrupt());
        }
        attempt = Some(page_attempt.to_owned());
        let page_digest = data
            .get("digest")
            .filter(|d| !d.is_null())
            .ok_or_else(corrupt)?;
        if digest
            .as_ref()
            .is_some_and(|expected| expected != page_digest)
        {
            return Err(corrupt());
        }
        digest = Some(page_digest.clone());
        let offset = data["offset"].as_u64().ok_or_else(corrupt)?;
        let next = data["next_offset"].as_u64().ok_or_else(corrupt)?;
        let chunk = chunk_bytes(&data).ok_or_else(corrupt)?;
        if offset != page.offset || next != offset + chunk.len() as u64 {
            return Err(corrupt());
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() > provenant_records::run::RESULT_LIMIT as usize {
            return Err(corrupt());
        }
        let eof = data["eof"].as_bool().ok_or_else(corrupt)?;
        if first.is_none() {
            first = Some(data);
        }
        if eof {
            break;
        }
        if next <= offset {
            return Err(corrupt());
        }
        page.offset = next;
    }
    let mut data = first.ok_or_else(corrupt)?;
    data["next_offset"] = json!(bytes.len());
    data["length"] = json!(bytes.len());
    data["eof"] = json!(true);
    data["clipped"] = json!(false);
    data["bytes"] =
        json!(provenant_fabric_runtime::coordination::facade::OutputBytes::from_bytes(bytes));
    Ok((data, String::new()))
}

/// What an agent needs from a read: how the attempt settled, what it changed,
/// and the whole final text up to a generous bound, then its head and tail.
/// Missing changed-file evidence stays null; it must never read as an empty diff.
pub(super) fn output_summary(data: &Value, text: &str, headed: bool) -> crate::Summary {
    summary(data, text, headed, true)
}

/// The compact default view: verdict, findings and verify first, then a
/// bounded view of the result page with a head and tail, marking the omitted
/// middle; host facts remain behind `--full`.
/// Spec: docs/specs/lane-contracts.md#result-projection
pub(super) fn output_summary_default(data: &Value, text: &str, headed: bool) -> crate::Summary {
    summary(data, text, headed, false)
}

/// One typed review finding as a bounded, terminal-safe human line. Every field
/// comes from the same structured finding the JSON envelope carries; a missing
/// location or suggested fix leaves no placeholder behind. Control, format and
/// bidi characters are stripped before the byte bound so one finding cannot
/// forge or reorder human lines.
/// Spec: docs/specs/cli.md#output-references-and-bounded-tails
pub fn finding_human_line(finding: &Value) -> String {
    const FIELD_BYTES: usize = 256;
    let field = |key: &str| {
        finding[key]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| plain_text(value, usize::MAX))
            .filter(|value| !value.is_empty())
            .map(|value| crate::bounded_text(&value, FIELD_BYTES))
    };
    let severity = field("severity").unwrap_or_else(|| "unknown".into());
    let mut line = format!("finding: [{severity}]");
    match (field("file"), finding["line"].as_u64()) {
        (Some(file), Some(number)) => line.push_str(&format!(" {file}:{number}")),
        (Some(file), None) => line.push_str(&format!(" {file}")),
        (None, Some(number)) => line.push_str(&format!(" line {number}")),
        (None, None) => {}
    }
    if let Some(summary) = field("summary") {
        line.push_str(&format!(" — {summary}"));
    }
    if let Some(fix) = field("proposed_fix") {
        line.push_str(&format!(" (fix: {fix})"));
    }
    line
}

/// A bounded view of a result page. The marker names omitted bytes; `--final`
/// exports the complete sealed result across pages.
/// Spec: docs/specs/cli.md#output-references-and-bounded-tails
fn final_result_view(text: &str) -> String {
    let final_head = crate::limits::FINAL_HEAD_BYTES;
    let final_tail = crate::limits::FINAL_TAIL_BYTES;
    if text.len() <= final_head + final_tail {
        return text.to_owned();
    }
    let mut head = final_head.min(text.len());
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    if let Some(cut) = text[..head].rfind('\n') {
        head = cut + 1;
    }
    let mut tail = text.len().saturating_sub(final_tail);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    if let Some(cut) = text[tail..].find('\n') {
        let snap = tail + cut + 1;
        // Only snap forward when a whole line follows; the last newline ends
        // the final line and snapping there would drop the handoff.
        if snap < text.len() {
            tail = snap;
        }
    }
    let omitted = tail.saturating_sub(head);
    format!(
        "{}[... {} bytes omitted; use --full for all of it ...]\n{}",
        &text[..head],
        omitted,
        &text[tail..]
    )
}

fn summary(data: &Value, text: &str, headed: bool, full: bool) -> crate::Summary {
    let mut lines = Vec::new();
    // How the attempt settled leads: it is a host observation, not a lane
    // claim, and it explains why the rest of the projection is present.
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
    if headed && let Some(result) = data["result"].as_str() {
        // An unsealed attempt has no prose yet; its typed state still belongs
        // in the compact view instead of a head-and-tail of the raw text.
        lines.push(format!("result: {}", crate::bounded_text(result, 64)));
    }
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
        if full {
            lines.extend(
                findings
                    .iter()
                    .map(|finding| format!("  {}", finding_human_line(finding))),
            );
        }
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
    // Other parts are the requested bytes, not narration: a transcript or a
    // stderr read still shows its text in the compact view. A result's final
    // message may span pages, so the compact view keeps both ends of this page
    // and marks the middle it omitted. An empty result
    // repeats its diagnostic as text, already carried by the settlement line,
    // so render it only once.
    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    if !headed {
        let body = if full {
            text.to_owned()
        } else {
            crate::head_and_tail(text, limits::OUTPUT_BYTES)
        };
        lines.extend(body.lines().map(str::to_owned));
    } else if data["reason"].as_str() != Some(text.trim()) {
        let body = if full {
            text.to_owned()
        } else {
            final_result_view(text)
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

pub(super) fn receipt_family(
    catalogue: Option<&provenant_routing::Catalogue>,
    model: Option<&str>,
) -> String {
    catalogue
        .and_then(|catalogue| {
            ModelId::parse(model?)
                .ok()
                .and_then(|id| catalogue.model(&id).and_then(|entry| entry.family.clone()))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// The effort that actually ran, paired with the evidence that justifies it.
/// A recorded applied effort wins. An adapter warning supplies the recorded
/// applied default or drop; without either, the effort and its source are unknown.
pub(super) fn receipt_effort(record: &run::AttemptRecord) -> (String, String) {
    let route = record.applied_route.as_ref();
    if let Some(effort) = route
        .and_then(|route| route.effort.as_deref())
        .filter(|value| !value.is_empty())
    {
        return (effort.to_owned(), "applied_route.effort".into());
    }
    if let Some(warning) = route.and_then(|route| route.effort_warning.as_ref()) {
        let source = format!("effort_warning:{}", warning.source);
        if let Some(applied) = warning.applied.as_deref().filter(|value| !value.is_empty()) {
            return (applied.to_owned(), source);
        }
        // The adapter dropped the requested effort: report the applied value
        // explicitly rather than presenting the requested effort as applied.
        return ("none".into(), source);
    }
    ("unknown".into(), "unknown".into())
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
    // Dispatched lanes name a route rather than an exact model. The launch
    // record carries the decision that selected the applied model, so use it
    // instead of reporting an unknown resolution for every named lane.
    match route.source.as_deref() {
        Some("pin") => return "explicit".into(),
        Some("pool") => return "pool".into(),
        Some("named_route") => {
            return match &record.requested_route {
                run::RequestedRoute::Named { name, .. }
                | run::RequestedRoute::Review { name, .. } => format!("route {name}"),
                _ => "route".into(),
            };
        }
        Some("fallback") => return "fallback".into(),
        Some("continuation") => return "continuation".into(),
        _ => {}
    }
    // A model pin may predate the recorded launch source.
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
    catalogue: Option<&provenant_routing::Catalogue>,
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
    let (effort, effort_source) = receipt_effort(record);
    let family = receipt_family(catalogue, qualified_model.as_deref());
    let resolved_from = receipt_resolution(record);
    let reviewer = run::fact(record.clone())?.reviewer;
    let reviewer_model = record
        .observed_route
        .as_ref()
        .and_then(|route| receipt_model(route.provider.as_deref(), route.model.as_deref()));
    let reviewer_family = receipt_family(catalogue, reviewer_model.as_deref());
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
        "{label}: {qualified_model}@{effort} ({family}; resolved from {resolved_from}; effort source {effort_source}), attempt {} {}",
        record.key, record.attempt_id
    );
    let attribution = format!(
        "Reviewer provenance: {provenance}, actor {actor}, model {reviewer_model}, family {reviewer_family}"
    );
    Ok((
        json!({"key": record.key, "attempt_id": record.attempt_id, "adapter": adapter, "model": model, "effort": effort, "effort_source": effort_source, "family": family, "role": role, "resolved_from": resolved_from, "receipt": line, "reviewer": {"provenance": provenance, "actor_id": actor, "model": reviewer_model, "family": reviewer_family}, "reviewer_provenance": attribution}),
        format!("{line}\n{attribution}\n"),
    ))
}

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
        data.remove("settlement_class");
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
            let catalogue = crate::model_discovery::effective(edge.roots).ok();
            return reviewer_receipt(&record, task.writer.is_some(), catalogue.as_ref());
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
            let tail = command.tail_selection();
            if tail.is_some() {
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
                    if let Some(crate::Tail::Lines(lines)) = tail {
                        let excess = events.len().saturating_sub(lines as usize);
                        events.drain(..excess);
                    }
                }
                if let Some(crate::Tail::Lines(lines)) = tail {
                    let excess = events.len().saturating_sub(lines as usize);
                    events.drain(..excess);
                }
                let byte_bound = match tail {
                    Some(crate::Tail::Bytes(bytes)) => command.max_bytes.min(bytes),
                    _ => command.max_bytes,
                };
                while !events.is_empty()
                    && serde_json::to_vec(&events)
                        .map_err(|error| {
                            Fail::new(Status::Error, ErrorKind::Io, error.to_string())
                        })?
                        .len() as u64
                        > byte_bound
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
        && command.tail_selection().is_none()
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
    let tail = command.tail_selection();
    if let Some(tail) = tail {
        range.offset = chunk.len.saturating_sub(command.max_bytes);
        // A short transcript's first page already includes the requested tail.
        if !matches!(part, run::ArtifactPart::Transcript) || range.offset != chunk.offset {
            chunk = session.service().output(id, part, range, call)?;
        }
        let start = match tail {
            crate::Tail::Bytes(bytes) => tail_start_bytes(&chunk.bytes, bytes),
            crate::Tail::Lines(lines) => tail_start_lines(&chunk.bytes, lines),
        };
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
        && command.tail_selection().is_none()
        && command.to.is_none()
        && let Some(reason) = &reason
    {
        text = format!("{reason}\n");
    }
    let mut data = json!({
            "attempt_id": id,
            "state": state,
            "reason": reason,
            "settlement_class": record.settlement_class,
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
    if matches!(command.part, Part::Result)
        && command.tail_selection().is_none()
        && command.offset == 0
    {
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
pub fn tail_start_bytes(bytes: &[u8], max_bytes: u64) -> usize {
    let mut start = bytes.len().saturating_sub(max_bytes as usize);
    while start < bytes.len() && std::str::from_utf8(&bytes[start..]).is_err() {
        start += 1;
    }
    start
}

/// Byte offset that leaves at most the last `lines` lines of `bytes`. A
/// trailing newline ends the last line rather than starting an empty one, and
/// a newline byte cannot occur inside UTF-8, so the offset is a text boundary.
pub fn tail_start_lines(bytes: &[u8], lines: u64) -> usize {
    if lines == 0 {
        return bytes.len();
    }
    let mut end = bytes.len();
    if bytes.last() == Some(&b'\n') {
        end -= 1;
    }
    let mut seen = 0u64;
    while end > 0 {
        end -= 1;
        if bytes[end] == b'\n' {
            seen += 1;
            if seen >= lines {
                return end + 1;
            }
        }
    }
    0
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
        .find_map(|route| {
            if route.host_native.is_some() {
                Some(provenant_records::Harness::HostNative.spec().name.into())
            } else {
                route.provider.clone()
            }
        }))
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
