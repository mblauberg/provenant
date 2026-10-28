use super::*;

/// Admit reviewers through the ordinary run scheduler and collate their results.
pub fn review(command: crate::Review, edge: &Edge<'_>) -> Reply {
    let mut data = json!({"reviewers": []});
    let outcome = review_inner(command, edge, &mut data);
    let collected = review_collect(edge, &mut data);
    let verdicts = aggregate_verdicts(&data);
    data["verdicts"] = verdicts;
    let human = review_text(&data);
    let (status, error) = match (outcome, collected) {
        (Err(error), _) | (Ok(_), Err(error)) => (error.status, Some(error)),
        (Ok(true), Ok(())) => (Status::Timeout, None),
        (Ok(false), Ok(()))
            if data["reviewers"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|r| r["state"].as_str() != Some("succeeded") || r["verdict"] != "approve")
            }) =>
        {
            (Status::Failed, None)
        }
        (Ok(false), Ok(())) => (Status::Ok, None),
    };
    let mut result = CommandResult::new("review", status, data);
    if let Some(error) = error {
        result.error = Some(CommandError {
            code: error.kind.code().into(),
            message: format!("{}\n{}", error.message, human),
            cause: None,
            repair: None,
        });
    }
    Reply { result, human }
}

#[allow(clippy::disallowed_methods)]
fn review_inner(command: crate::Review, edge: &Edge<'_>, data: &mut Value) -> Result<bool, Fail> {
    let invalid = |message: String| Fail::new(Status::Invalid, ErrorKind::Invalid, message);
    let git = |args: &[&str]| -> Result<String, Fail> {
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        let executable =
            provenant_os::git::git_executable().ok_or_else(|| invalid("Git unavailable".into()))?;
        let mut argv = vec![executable.into_os_string()];
        argv.extend(args.iter().map(std::ffi::OsString::from));
        let executor = SystemExecutor::new(1).map_err(|e| invalid(e.to_string()))?;
        let output = executor
            .execute(ExecOptions::new(&argv, edge.cwd, Duration::from_secs(30)))
            .map_err(|e| invalid(e.to_string()))?;
        if output.termination != Termination::Exit(0) || output.timed_out || output.stdout.truncated
        {
            return Err(invalid(format!(
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes).trim()
            )));
        }
        String::from_utf8(output.stdout.bytes)
            .map_err(|_| invalid("git output is not UTF-8".into()))
    };
    let head = git(&[
        "rev-parse",
        "--verify",
        "--end-of-options",
        &format!("{}^{{commit}}", command.target),
    ])?
    .trim()
    .to_owned();
    if command
        .range
        .as_deref()
        .is_some_and(|range| range.starts_with('-'))
    {
        return Err(invalid("git range must not begin with -".into()));
    }
    let paths: Vec<String> = if let Some(range) = &command.range {
        git(&["diff", "--no-renames", "--name-only", "-z", range, "--"])?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        vec![]
    };
    let brief = read_bounded(&command.brief_file)?;
    let domains = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|e| invalid(e.to_string()))?
        .domains;
    let selected = domains
        .routing
        .review
        .select_shape(
            command.shape.as_deref(),
            command.risk.as_deref(),
            command.kind.as_deref(),
            &paths,
        )
        .map_err(invalid)?;
    if selected
        .shape
        .reviewers
        .iter()
        .map(|group| group.count)
        .sum::<usize>()
        > 64
    {
        return Err(invalid("review shape exceeds 64 reviewers".into()));
    }
    data["shape"] = json!(selected.name);
    data["selection_source"] = json!(selected.source);
    data["target"] = json!(head);

    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| invalid(e.to_string()))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = open(edge, &call)?;
    let chair = if crate::fabric::in_lane() {
        None
    } else {
        let context = crate::fabric::Context::open(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|e| Fail::new(Status::Error, ErrorKind::Identity, e.to_string()))?;
        Some(run::ChairBinding {
            host: context.identity.inbox.host,
            actor: crate::workflow::session_actor(&context, None)
                .map_err(|e| Fail::new(Status::Error, ErrorKind::Identity, e))?,
        })
    };
    let mut slots = Vec::new();
    for (group_index, group) in selected.shape.reviewers.iter().enumerate() {
        for index in 0..group.count {
            let lens = group.lenses.get(index).cloned();
            let (operation_id, task_id) = review_identity(
                &head,
                selected.name,
                selected.shape,
                &brief,
                group_index,
                index,
            )?;
            data["reviewers"].as_array_mut().ok_or_else(|| invalid("invalid review result".into()))?.push(json!({
                "operation_id": operation_id, "run_id": null, "attempt_id": null, "pool": group.pool,
                "lens": lens, "group": group_index, "distinct_families": group.distinct_families,
                "state": "not_submitted", "applied_route": null, "family": null,
                "substitution": false, "verdict": "unknown", "result_path": null
            }));
            slots.push(ReviewSlot {
                group: group_index,
                pool: group.pool.clone(),
                lens,
                distinct: group.distinct_families,
                operation_id,
                task_id,
                run: None,
                ready: false,
                family: None,
            });
        }
    }
    let deadline = Instant::now() + Duration::from_secs(command.wait);
    // Recover every already-published slot before considering dependencies, so
    // a timed-out retry still reports lanes created by an earlier invocation.
    for (i, slot) in slots.iter_mut().enumerate() {
        let call = request(&clock, &cancel)?;
        if let Some(op) = session.store.operation(&slot.operation_id, &call)? {
            match session.store.request(op.run_id, &call) {
                Ok(_) => {
                    slot.run = Some(op.run_id);
                    data["reviewers"][i]["run_id"] = json!(op.run_id);
                    data["reviewers"][i]["state"] = json!("submitted");
                    launch(&session, op.run_id, edge, &call)?;
                }
                Err(run::RunError::NotFound) if op.state == run::OperationState::Prepared => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    // Each pass admits all independent slots before waiting for a distinct
    // group's preceding route. Other groups continue even when one is paced.
    loop {
        let mut progressed = false;
        for i in 0..slots.len() {
            if !can_submit(&slots, i) {
                continue;
            }
            let families: Vec<String> = slots[..i]
                .iter()
                .filter(|s| s.group == slots[i].group)
                .filter_map(|s| s.family.clone())
                .collect();
            let slot = &slots[i];
            let call = request(&clock, &cancel)?;
            // A published operation owns its immutable request. Launch any
            // still-queued work without reconstructing changing exclusions.
            let existing = session.store.operation(&slot.operation_id, &call)?;
            let published = if let Some(op) = existing {
                match session.store.request(op.run_id, &call) {
                    Ok(_) => Some(op),
                    Err(run::RunError::NotFound) if op.state == run::OperationState::Prepared => {
                        None
                    }
                    Err(error) => return Err(error.into()),
                }
            } else {
                None
            };
            let run_id = if let Some(op) = published {
                data["reviewers"][i]["run_id"] = json!(op.run_id);
                data["reviewers"][i]["state"] = json!("submitted");
                launch(&session, op.run_id, edge, &call)?;
                op.run_id
            } else {
                let submitted = submit(
                    &session,
                    edge,
                    Submission {
                        chair: chair.clone(),
                        operation_id: slot.operation_id.clone(),
                        concurrency: Some(1),
                        knobs: vec![],
                        tasks: vec![review_task(slot, &head, &brief, families)],
                    },
                    &call,
                );
                match submitted {
                    Ok((record, _)) => record.run_id,
                    Err(error) => {
                        if let Ok(Some(operation)) =
                            session.store.operation(&slot.operation_id, &call)
                        {
                            data["reviewers"][i]["run_id"] = json!(operation.run_id);
                            data["reviewers"][i]["state"] = json!("submitted");
                        }
                        return Err(error);
                    }
                }
            };
            slots[i].run = Some(run_id);
            data["reviewers"][i]["run_id"] = json!(run_id);
            data["reviewers"][i]["state"] = json!("submitted");
            progressed = true;
        }
        if slots.iter().all(|s| s.run.is_some()) {
            break;
        }
        for slot in &mut slots {
            if !slot.distinct || slot.ready {
                continue;
            }
            let Some(id) = slot.run else { continue };
            let target = Target {
                selection: None,
                run: Some(id),
                task: None,
                attempt: None,
            };
            let call = request(&clock, &cancel)?;
            let page = session.service().lanes(
                &lane_query(&session, &[target], true, 100, 0, &call)?,
                &call,
            )?;
            if let Some(lane) = page.lanes.first() {
                if let Some(route) = &lane.applied_route {
                    slot.family = route
                        .model
                        .as_deref()
                        .and_then(|m| ModelId::parse(m).ok())
                        .and_then(|m| catalogue.model(&m).and_then(|m| m.family.clone()));
                    slot.ready = true;
                    progressed = true;
                } else if matches!(lane.state, run::AttemptState::Terminal { .. }) {
                    slot.ready = true;
                    progressed = true;
                }
            }
        }
        if Instant::now() >= deadline {
            return Ok(true);
        }
        if !progressed {
            watch::pause(Duration::from_millis(500), deadline);
        }
    }
    let targets: Vec<Target> = slots
        .iter()
        .filter_map(|slot| slot.run)
        .map(|id| Target {
            selection: None,
            run: Some(id),
            task: None,
            attempt: None,
        })
        .collect();
    let (_, _, timed_out) = wait_lanes(
        &session,
        &targets,
        Condition::All,
        deadline.saturating_duration_since(Instant::now()).as_secs(),
        edge,
    )?;
    Ok(timed_out)
}

struct ReviewSlot {
    group: usize,
    pool: String,
    lens: Option<String>,
    distinct: bool,
    operation_id: OperationId,
    task_id: TaskId,
    run: Option<RunId>,
    ready: bool,
    family: Option<String>,
}

fn can_submit(slots: &[ReviewSlot], index: usize) -> bool {
    let slot = &slots[index];
    slot.run.is_none()
        && (!slot.distinct
            || !slots[..index]
                .iter()
                .any(|s| s.group == slot.group && !s.ready))
}

fn review_identity(
    head: &str,
    name: &str,
    shape: &provenant_routing::ReviewShape,
    brief: &str,
    group: usize,
    index: usize,
) -> Result<(OperationId, TaskId), Fail> {
    let digest = CanonicalV1::record_digest("review-slot", 1, &json!({
        "head": head, "name": name, "shape": shape, "brief": brief, "group": group, "index": index,
    })).map_err(|e| Fail::new(Status::Invalid, ErrorKind::Invalid, e.to_string()))?;
    let operation = OperationId::parse(&format!(
        "review-{}",
        provenant_core::hex(digest.as_bytes())
    ))
    .map_err(|e| Fail::new(Status::Invalid, ErrorKind::Invalid, e.to_string()))?;
    let mut task = [0; 16];
    task.copy_from_slice(&digest.as_bytes()[..16]);
    Ok((operation, TaskId::from_bytes(task)))
}

fn review_task(slot: &ReviewSlot, head: &str, brief: &str, families: Vec<String>) -> Submitted {
    let prompt = format!(
        "Review commit {head}. Work read-only.\nLens: {}\nStart with exactly one line: Verdict: approve, Verdict: request_changes, or Verdict: inconclusive. Then give findings.\n\n{brief}",
        slot.lens.as_deref().unwrap_or("general")
    );
    Submitted {
        build_heavy: false,
        title: Some(format!(
            "Review {}",
            slot.lens.as_deref().unwrap_or("general")
        )),
        task_id: slot.task_id,
        prompt,
        base: None,
        head: Some(head.into()),
        route: run::RequestedRoute::Review {
            name: slot.pool.clone(),
            effort: None,
            distinct_families: slot.distinct,
            excluded_families: if slot.distinct { families } else { vec![] },
        },
        account: None,
        fallback: vec![],
        provider_timeout: 3600,
        delivery_id: None,
        deliver: None,
        worktree: None,
        access: run::LaneAccess::default(),
    }
}

fn review_collect(edge: &Edge<'_>, data: &mut Value) -> Result<(), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = open(edge, &call)?;
    let catalogue = provenant_routing::Catalogue::bundled()
        .map_err(|e| Fail::new(Status::Error, ErrorKind::Config, e.to_string()))?;
    let mut seen: std::collections::BTreeMap<u64, Vec<String>> = std::collections::BTreeMap::new();
    let Some(rows) = data["reviewers"].as_array_mut() else {
        return Ok(());
    };
    for reviewer in rows {
        let Some(id) = reviewer["run_id"]
            .as_str()
            .and_then(|id| id.parse::<RunId>().ok())
        else {
            continue;
        };
        let target = Target {
            selection: None,
            run: Some(id),
            task: None,
            attempt: None,
        };
        let call = request(&clock, &cancel)?;
        let page = session.service().lanes(
            &lane_query(&session, &[target], true, 100, 0, &call)?,
            &call,
        )?;
        let (rendered, _) = render_lanes(
            &page.lanes,
            &[],
            None,
            session.context.state_root.as_path(),
            None,
        );
        let Some(row) = rendered["lanes"].as_array().and_then(|r| r.first()) else {
            continue;
        };
        let family = row["applied_route"]["model"]
            .as_str()
            .and_then(|m| ModelId::parse(m).ok())
            .and_then(|m| catalogue.model(&m).and_then(|m| m.family.clone()));
        let used = seen
            .entry(reviewer["group"].as_u64().unwrap_or_default())
            .or_default();
        let substitution = reviewer["distinct_families"].as_bool() == Some(true)
            && row["applied_route"]["model"].is_string()
            && family.as_ref().is_none_or(|f| used.contains(f));
        if let Some(f) = &family {
            used.push(f.clone());
        }
        let verdict = row["result_path"]
            .as_str()
            .and_then(|p| read_bounded(Path::new(p)).ok())
            .map(|s| review_verdict(&s))
            .unwrap_or("unknown");
        if let (Some(target), Some(source)) = (reviewer.as_object_mut(), row.as_object()) {
            target.extend(source.clone());
        }
        reviewer["family"] = json!(family);
        reviewer["substitution"] = json!(substitution);
        reviewer["verdict"] = json!(verdict);
    }
    Ok(())
}
fn review_verdict(output: &str) -> &'static str {
    let Some(line) = output.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return "unknown";
    };
    let stripped = line.replace('*', "");
    let line = stripped.trim_matches('_').trim();
    let Some((label, verdict)) = line.split_once(':') else {
        return "unknown";
    };
    if !label
        .trim_matches(|c| c == '*' || c == '_')
        .trim()
        .eq_ignore_ascii_case("verdict")
    {
        return "unknown";
    }
    match verdict
        .trim()
        .trim_matches(|c| c == '*' || c == '_')
        .to_ascii_lowercase()
        .as_str()
    {
        "approve" => "approve",
        "request_changes" => "request_changes",
        "inconclusive" => "inconclusive",
        _ => "unknown",
    }
}
fn aggregate_verdicts(data: &Value) -> Value {
    let mut counts = json!({"approve": 0, "request_changes": 0, "inconclusive": 0, "unknown": 0});
    if let Some(rows) = data["reviewers"].as_array() {
        for row in rows {
            let verdict = row["verdict"].as_str().unwrap_or("unknown");
            let key = match verdict {
                "approve" | "request_changes" | "inconclusive" => verdict,
                _ => "unknown",
            };
            counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
        }
    }
    counts
}
fn review_text(data: &Value) -> String {
    let mut text = format!(
        "review {} ({})\n",
        data["shape"].as_str().unwrap_or("unknown"),
        data["selection_source"].as_str().unwrap_or("unknown")
    );
    if let Some(rows) = data["reviewers"].as_array() {
        for row in rows {
            let model = row["applied_route"]["model"]
                .as_str()
                .unwrap_or("unresolved");
            let effort = row["applied_route"]["effort"]
                .as_str()
                .map(|e| format!("@{e}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  run={} operation={} {}{} state={} verdict={} output={}{}\n",
                row["run_id"].as_str().unwrap_or("unknown"),
                row["operation_id"].as_str().unwrap_or("unknown"),
                model,
                effort,
                row["state"].as_str().unwrap_or("pending"),
                row["verdict"].as_str().unwrap_or("unknown"),
                row["result_path"].as_str().unwrap_or("none"),
                if row["substitution"].as_bool() == Some(true) {
                    " (family substitution)"
                } else {
                    ""
                }
            ));
            if let Some(id) = row["run_id"].as_str()
                && matches!(
                    row["state"].as_str(),
                    Some("queued" | "starting" | "running" | "settling" | "submitted")
                )
            {
                text.push_str(&format!("    collect: provenant lanes --id {id} --wait 4m; cancel: provenant cancel {id}\n"));
            }
        }
    }
    if data["reviewers"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["state"] == "not_submitted"))
    {
        text.push_str("  Retry the same review command to submit remaining reviewers and reattach to existing runs.\n");
    }
    text
}

#[cfg(test)]
mod review_shape_tests {
    use super::*;
    #[test]
    fn verdict_requires_an_exact_first_line() {
        assert_eq!(review_verdict("Verdict: approve\nNothing found"), "approve");
        assert_eq!(
            review_verdict("Verdict: request_changes\nFinding"),
            "request_changes"
        );
        assert_eq!(review_verdict("I approve\nVerdict: approve"), "unknown");
        assert_eq!(review_verdict("Verdict: approved"), "unknown");
        assert_eq!(review_verdict("\n **Verdict: APPROVE**\n"), "approve");
        assert_eq!(
            review_verdict("**Verdict:** *Request_Changes*"),
            "request_changes"
        );
    }
    #[test]
    fn identity_is_stable_and_changes_with_review_inputs() -> Result<(), Fail> {
        let shape = &provenant_routing::ReviewConfig::default().shapes["quick"];
        let first = review_identity("head", "quick", shape, "brief", 0, 0)?;
        assert_eq!(
            first,
            review_identity("head", "quick", shape, "brief", 0, 0)?
        );
        assert_ne!(
            first,
            review_identity("head", "quick", shape, "changed", 0, 0)?
        );
        assert_ne!(
            first,
            review_identity("head", "quick", shape, "brief", 0, 1)?
        );
        Ok(())
    }
    #[test]
    fn review_has_no_extra_write_grants() {
        let slot = ReviewSlot {
            group: 0,
            pool: "junior".into(),
            lens: None,
            distinct: false,
            operation_id: OperationId::parse("test").unwrap(),
            task_id: TaskId::from_bytes([1; 16]),
            run: None,
            ready: false,
            family: None,
        };
        let task = review_task(&slot, "head", "brief", vec![]);
        assert!(task.access.write_roots.is_empty());
        assert!(task.access.is_default());
        assert!(task.worktree.is_none());
    }
    #[test]
    fn blocked_family_group_does_not_block_other_reviewers() {
        let make = |group, distinct, run| ReviewSlot {
            group,
            pool: "junior".into(),
            lens: None,
            distinct,
            operation_id: OperationId::parse("test").unwrap(),
            task_id: TaskId::from_bytes([1; 16]),
            run,
            ready: false,
            family: None,
        };
        let slots = vec![
            make(0, true, Some(RunId::from_bytes([1; 16]))),
            make(0, true, None),
            make(1, false, None),
            make(1, false, None),
            make(2, true, None),
        ];
        assert!(!can_submit(&slots, 1));
        assert!(can_submit(&slots, 2));
        assert!(can_submit(&slots, 3));
        assert!(can_submit(&slots, 4));
    }
    #[test]
    fn verdict_summary_counts_unparsed_and_not_submitted() {
        let data = json!({"reviewers":[{"verdict":"approve"},{"verdict":"request_changes"},{"verdict":"unknown"},{}]});
        assert_eq!(
            aggregate_verdicts(&data),
            json!({"approve":1,"request_changes":1,"inconclusive":0,"unknown":2})
        );
    }
    #[test]
    fn text_reports_partial_run_and_actual_receipt() {
        let data = json!({"shape":"deep","selection_source":"by_risk","reviewers":[{"run_id":"abc","operation_id":"op","applied_route":{"model":"codex/test","effort":"high"},"state":"failed","verdict":"unknown","result_path":"/output","substitution":true}]});
        let text = review_text(&data);
        for expected in [
            "deep (by_risk)",
            "run=abc",
            "codex/test@high",
            "state=failed",
            "verdict=unknown",
            "/output",
            "family substitution",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }
}
