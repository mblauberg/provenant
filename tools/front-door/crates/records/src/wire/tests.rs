use super::schema::{absolute_path, path_wire, validate};
use super::*;
use serde_json::json;

fn row() -> LaneRow {
    LaneRow {
        run_id: "0".repeat(32),
        task_id: "1".repeat(32),
        attempt_id: "2".repeat(32),
        attempt_number: 1,
        retry_of: None,
        superseded_by: None,
        cause: None,
        state: "running".into(),
        exit_code: None,
        reason: None,
        settlement: None,
        lane_class: Some("writer".into()),
        waiting_for: None,
        requested_route: json!({"kind": "named", "name": "utility"}),
        host: None,
        resolved_route: Some(Value::Null),
        applied_route: Value::Null,
        skipped_routes: json!([]),
        account: Value::Null,
        observed_route: Value::Null,
        queued_at: json!("2026-11-02T00:00:00Z"),
        started_at: Value::Null,
        priority: Some(10),
        finished_at: Value::Null,
        last_output_at: Value::Null,
        runtime_deadline: Value::Null,
        overdue: false,
        stale: Some(false),
        partial_signals: json!([]),
        delivery_id: None,
        write_grants: json!([]),
        read_only_writable: json!([]),
        git_grants_digest: None,
        head: None,
        protected_paths: json!([]),
        delivery: Value::Null,
        continuation: Value::Null,
        cache_read_share: None,
        result_path: Value::Null,
        transcript: None,
        title: None,
        requested_priority: None,
        provider_seconds: None,
        handoff: None,
        provider_error: None,
    }
}

fn list(rows: Vec<LaneRow>) -> LaneList {
    LaneList {
        lanes: rows,
        next_cursor: None,
        capacity_cooldowns: None,
        review_exempt: None,
        condition_met: None,
        timed_out: None,
        reason: None,
        ignored_settled: None,
    }
}

fn accepted(schema: &Value, value: &impl serde::Serialize) {
    let value = serde_json::to_value(value).unwrap();
    if let Err(error) = validate(schema, &value) {
        panic!("{error}\n{value:#}");
    }
}

fn rejected(schema: &Value, value: Value) {
    assert!(validate(schema, &value).is_err(), "accepted {value}");
}

#[test]
fn lane_rows_validate_with_and_without_optional_members() {
    let schema = lane::lane_row_schema();
    let plain = row();
    accepted(&schema, &plain);
    let value = serde_json::to_value(&plain).unwrap();
    for omitted in ["title", "requested_priority", "provider_seconds"] {
        assert!(
            value.get(omitted).is_none(),
            "{omitted} must be omitted when absent"
        );
    }
    let mut full = row();
    full.state = "terminal".into();
    full.retry_of = Some("3".repeat(32));
    full.superseded_by = Some("4".repeat(32));
    full.cause = Some(crate::run::SettlementReason::DeclaredStop);
    full.exit_code = Some(0);
    full.reason = Some("done".into());
    full.settlement = Some(LaneSettlement {
        reason: Some("done".into()),
    });
    full.title = Some("Review\nchanges".into());
    full.priority = Some(15);
    full.requested_priority = Some(10);
    full.cache_read_share = Some(0.25);
    full.provider_seconds = Some(1.5);
    full.handoff = Some(Value::Null);
    full.provider_error = Some("failed".into());
    full.transcript = Some(LaneTranscript {
        adapter: "codex".into(),
        session: "s".into(),
        path: None,
    });
    accepted(&schema, &full);
    let value = serde_json::to_value(&full).unwrap();
    assert_eq!(value["requested_priority"], 10);
    assert_eq!(value["title"], "Review\nchanges");
    assert_eq!(value["cause"], "declared_stop");
    assert_eq!(value["retry_of"], "3".repeat(32));
    assert_eq!(value["superseded_by"], "4".repeat(32));
}

#[test]
fn lane_rows_reject_drift() {
    let schema = lane::lane_row_schema();
    let mut value = serde_json::to_value(row()).unwrap();
    value["invented"] = json!(1);
    rejected(&schema, value);
    let mut value = serde_json::to_value(row()).unwrap();
    value.as_object_mut().unwrap().remove("overdue");
    rejected(&schema, value);
    let mut value = serde_json::to_value(row()).unwrap();
    value["attempt_number"] = json!("1");
    rejected(&schema, value);
}

#[test]
fn lanes_and_status_share_one_list_type() {
    let schema = lane::lane_list_schema();
    let mut lanes = list(vec![row(), row()]);
    lanes.capacity_cooldowns = Some(vec![json!({"account": "a"})]);
    accepted(&schema, &lanes);
    let mut status = list(vec![]);
    status.next_cursor = Some("7".into());
    status.review_exempt = Some(json!({"paths": []}));
    accepted(&schema, &status);
    let mut waited = list(vec![row()]);
    waited.condition_met = Some(false);
    waited.timed_out = Some(false);
    waited.reason = Some("provider silent; lane x is still running".into());
    waited.ignored_settled = Some(2);
    accepted(&schema, &waited);
    let value = serde_json::to_value(list(vec![])).unwrap();
    assert_eq!(value, json!({"lanes": [], "next_cursor": null}));
}

fn chunk() -> OutputChunk {
    OutputChunk {
        lane: row(),
        part: "result".into(),
        offset: 0,
        next_offset: 5,
        length: 5,
        eof: true,
        clipped: false,
        complete: true,
        digest: Some(format!("sha256:{}", "a".repeat(64))),
        bytes: ChunkBytes::Utf8("hello".into()),
        tail: None,
        tail_clamped: None,
        changed_files: None,
        changed_files_total: None,
        selected: None,
    }
}

#[test]
fn every_output_mode_matches_exactly_one_variant() {
    let schema = output::output_data_schema();
    let modes = vec![
        OutputData::Trace {
            lane: row(),
            trace: json!({"x": 1}),
            grants: json!({}),
        },
        OutputData::Path {
            lane: row(),
            part: "result".into(),
            path: crate::StructuredPathWire::from_path(std::path::Path::new("/s/result.md"))
                .unwrap(),
        },
        OutputData::Events {
            lane: row(),
            events: vec![json!({"revision": 1})],
        },
        OutputData::Receipt(Box::new(ReviewerReceipt {
            lane: row(),
            adapter: json!("codex"),
            model: json!("m"),
            effort: Value::Null,
            family: json!("f"),
            role: Value::Null,
            resolved_from: Value::Null,
            receipt: "line".into(),
            reviewer: json!({"provenance": "x"}),
            reviewer_provenance: json!("x"),
        })),
        OutputData::Chunk(Box::new(chunk())),
    ];
    for mode in &modes {
        accepted(&schema, mode); // `oneOf`: exactly one variant, so modes never overlap.
    }
}

#[test]
fn chunk_optional_members_and_encodings() {
    let schema = output::output_data_schema();
    let mut full = chunk();
    full.bytes = ChunkBytes::Base64("//8=".into());
    full.tail = Some(20);
    full.tail_clamped = Some(true);
    full.changed_files =
        Some(json!([{"kind":"relative", "relative":{"encoding":"utf8", "path":"a.rs"}}]));
    full.changed_files_total = Some(json!(1));
    full.lane.provider_error = Some("failed".into());
    full.lane.result_path =
        json!(crate::StructuredPathWire::from_path(std::path::Path::new("/s/result.md")).unwrap());
    full.selected = Some(json!({"attempt": "x"}));
    accepted(&schema, &OutputData::Chunk(Box::new(full)));
    let plain = serde_json::to_value(chunk()).unwrap();
    assert_eq!(plain["bytes"], json!({"encoding": "utf8", "data": "hello"}));
    assert!(plain.get("tail").is_none());
    let mut invalid = plain.clone();
    invalid["bytes"] = json!({"encoding": "base64", "data": "%%%"});
    rejected(&schema, invalid);
    let mut bad = plain;
    bad["bytes"] = json!({"encoding": "latin1", "data": "x"});
    rejected(&schema, bad);
}

#[test]
fn cancel_receipts_cover_every_state_and_error_rows() {
    let schema = cancel::cancel_data_schema();
    let id = "3".repeat(32);
    let receipts = [
        CancelReceiptWire::Cancelled,
        CancelReceiptWire::AlreadyTerminal,
        CancelReceiptWire::Requested,
        CancelReceiptWire::AlreadyRequested,
        CancelReceiptWire::Unobservable {
            reason: "no custody".into(),
        },
    ];
    let mut names = Vec::new();
    let mut rows = Vec::new();
    for receipt in receipts {
        names.push(
            serde_json::to_value(&receipt).unwrap()["state"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        rows.push(CancelRow::Receipt {
            attempt_id: id.clone(),
            host: None,
            receipt,
        });
    }
    assert_eq!(names, CANCEL_STATES);
    rows.push(CancelRow::Error {
        attempt_id: id.clone(),
        host: None,
        error: CancelRowError {
            code: "NOT_FOUND".into(),
            message: "gone".into(),
            cause: None,
            repair: None,
            diagnostic: None,
        },
    });
    let mut data = CancelData {
        operation_id: "op-1".into(),
        receipts: rows,
        host: None,
        recovered: None,
        lanes: None,
        condition_met: None,
        timed_out: None,
        reason: None,
    };
    accepted(&schema, &data);
    assert!(
        serde_json::to_value(&data)
            .unwrap()
            .get("recovered")
            .is_none()
    );
    data.recovered = Some(vec![CancelRecovered {
        attempt_id: id,
        found: cancel::ReconciledWire::Settling {
            phase: "delivering".into(),
            blocking_reason: None,
        },
    }]);
    data.lanes = Some(vec![row()]);
    data.condition_met = Some(true);
    data.timed_out = Some(false);
    data.reason = Some("r".into());
    accepted(&schema, &data);
    // Values use the run owner's tagged Reconciled representation.
    for found in [
        json!({"state": "queued"}),
        json!({"state": "held"}),
        json!({"state": "deadline_stop"}),
        json!({"state": "settling", "phase": "delivery", "blocking_reason": null}),
        json!({"state": "abandoned", "reason": "released"}),
        json!({"state": "finished"}),
        json!({"state": "temporary_removed"}),
        json!({"state": "temporary_retained", "path": {"kind":"path", "root":"/", "relative":{"encoding":"utf8", "path":"tmp/a"}}, "reason": "retained"}),
        json!({"state": "withdrawn"}),
        json!({"state": "escaped", "pids": [123]}),
        json!({"state": "uncertain", "reason": {"kind": "no_launch_summary"}}),
    ] {
        validate(&cancel::reconciled_schema(), &found).unwrap();
    }
    rejected(
        &schema,
        json!({"operation_id": "o", "receipts": [{"attempt_id": "a", "receipt": {"state": "requested", "reason": "x"}}]}),
    );
    rejected(
        &schema,
        json!({"operation_id": "o", "receipts": [{"attempt_id": "a", "receipt": {"state": "unobservable"}}]}),
    );
}

#[test]
fn envelopes_validate_for_every_command_and_for_errors() {
    let id = "4".repeat(32);
    for command in COMMANDS {
        assert!(command_schema(command).is_some(), "{command}");
    }
    let lanes = command_schema("lanes").unwrap();
    let ok = json!({
        "schema_version": ENVELOPE_VERSION, "command": "lanes", "status": "ok",
        "data": serde_json::to_value(list(vec![row()])).unwrap(), "error": null
    });
    validate(&lanes, &ok).unwrap();
    // A failed command carries null data and the error envelope.
    let failed = json!({
        "schema_version": ENVELOPE_VERSION, "command": "lanes", "status": "failed", "data": null,
        "error": {"code": "NOT_FOUND", "message": "m", "cause": null, "repair": null}
    });
    validate(&lanes, &failed).unwrap();
    let errors = error_envelope_schema();
    validate(&errors, &failed).unwrap();
    let mut with_repair = failed.clone();
    with_repair["error"]["cause"] = json!("c");
    with_repair["error"]["repair"] = json!("run reconcile");
    with_repair["data"] = json!({"operation_id": id, "receipts": []});
    validate(&errors, &with_repair).unwrap();
    // An error envelope needs its error; a success cannot pass as one.
    assert!(validate(&errors, &ok).is_err());
    let mut invalid = ok.clone();
    invalid["data"] = Value::Null;
    rejected(&lanes, invalid);
    // The CLI emits non-ok statuses without an error on some paths.
    let mut unerrored = failed.clone();
    unerrored["error"] = Value::Null;
    validate(&lanes, &unerrored).unwrap();
    assert!(validate(&errors, &unerrored).is_err());
    let mut ok_with_error = ok.clone();
    ok_with_error["error"] = failed["error"].clone();
    rejected(&lanes, ok_with_error);
    let mut planned = ok.clone();
    planned["schema_version"] = json!(SCHEMA_VERSION);
    rejected(&lanes, planned);
    let mut wrong = ok;
    wrong["command"] = json!("status");
    assert!(validate(&lanes, &wrong).is_err());
    let mut partial = failed;
    partial["error"] = json!({"code": "X", "message": "m"});
    assert!(
        validate(&lanes, &partial).is_err(),
        "cause and repair are always present"
    );
}

#[test]
fn status_values_match_the_envelope_enum() {
    // Exhaustive: a new Outcome fails to compile here until it gets an index,
    // and then ALL must list every index.
    const VARIANTS: usize = 10;
    let index = |outcome: Outcome| match outcome {
        Outcome::Ok => 0,
        Outcome::Failed => 1,
        Outcome::Refused => 2,
        Outcome::Invalid => 3,
        Outcome::Error => 4,
        Outcome::Unavailable => 5,
        Outcome::Timeout => 6,
        Outcome::Cancelled => 7,
        Outcome::Pending => 8,
        Outcome::Waiting => 9,
    };
    let listed: std::collections::BTreeSet<usize> = Outcome::ALL.into_iter().map(index).collect();
    assert_eq!(listed, (0..VARIANTS).collect());
    let unique: std::collections::BTreeSet<_> = outcome_names().into_iter().collect();
    assert_eq!(unique.len(), Outcome::ALL.len());
}

#[test]
fn edge_error_codes_are_unique_and_described() {
    let mut seen = std::collections::BTreeSet::new();
    for entry in EDGE_ERROR_CODES {
        assert!(seen.insert(entry.code), "duplicate {}", entry.code);
        assert!(
            !entry.summary.is_empty() && entry.summary.ends_with('.'),
            "{}",
            entry.code
        );
        assert_eq!(
            entry.kind == CodeKind::LegacySpelling,
            entry.code.chars().any(|c| c.is_ascii_lowercase()),
            "{}",
            entry.code
        );
    }
}

#[test]
fn path_encoding_and_provider_seconds_match_the_runtime_forms() {
    use std::path::Path;
    // Contained paths: `PathWire` is emitted as {encoding, path}.
    let relative = provenant_core::RelativePath::parse("dir/file.txt").unwrap();
    let wire = crate::PathWire::from_relative(&relative).unwrap();
    validate(&path_wire(), &serde_json::to_value(&wire).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let native = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b'f', 0xff]));
        let relative = provenant_core::RelativePath::parse(native).unwrap();
        let wire =
            serde_json::to_value(crate::PathWire::from_relative(&relative).unwrap()).unwrap();
        assert_eq!(wire["encoding"], "native_bytes");
        validate(&path_wire(), &wire).unwrap();
    }
    // Absolute paths: the explicit root-plus-path-wire envelope.
    let absolute = crate::AbsolutePath::new(Path::new("/repo/worktree")).unwrap();
    validate(&absolute_path(), &serde_json::to_value(absolute).unwrap()).unwrap();
    // MCP encodes provider time as seconds; the helper keeps fractional values.
    assert_eq!(provider_seconds(Some(1500)), Some(1.5));
    assert_eq!(provider_seconds(None), None);
    assert_eq!(provider_seconds(Some(0)), Some(0.0));
}

#[test]
fn mcp_results_validate_by_tag_and_allow_unknown_members() {
    let run = mcp::run_result_schema();
    let id = "5".repeat(32);
    let attempt = json!({
        "attempt_id": id, "run_id": id, "task_id": id, "state": {"state": "running"},
        "provider_seconds": 1.5, "future_member": true
    });
    for value in [
        json!({"result": "status", "attempt": attempt}),
        json!({"result": "cancel", "attempt": attempt}),
        json!({"result": "resume", "attempt": attempt}),
        json!({"result": "wait", "condition": {"kind": "all"}, "attempts": [attempt], "satisfied": true, "timed_out": false}),
        json!({"result": "list", "page": {"items": [attempt], "next_after": null, "truncated": false}}),
    ] {
        validate(&run, &value).unwrap();
    }
    let mut without_seconds = attempt.clone();
    without_seconds
        .as_object_mut()
        .unwrap()
        .remove("provider_seconds");
    assert!(
        validate(
            &run,
            &json!({"result": "status", "attempt": without_seconds})
        )
        .is_err()
    );
    assert!(validate(&run, &json!({"result": "other"})).is_err());

    let out = mcp::output_result_schema();
    let target = json!({"attempt": id, "host": null});
    validate(&out, &json!({
        "kind": "chunk", "target": target, "part": "result", "state": "terminal", "reason": null,
        "offset": 0, "next_offset": 2, "eof": true, "digest": "sha256:00",
        "bytes": {"encoding": "utf8", "data": "ok"}
    })).unwrap();
    validate(
        &out,
        &json!({
            "kind": "artifact", "target": target, "digest": "sha256:00",
            "artifact": {"path": {"encoding": "utf8", "path": "result.md"}, "digest": null},
            "bytes": {"encoding": "base64", "data": "AA=="}
        }),
    )
    .unwrap();
    rejected(
        &out,
        json!({
            "kind": "artifact", "target": target, "digest": "sha256:00",
            "artifact": {"path": {"encoding": "utf8", "path": "result.md"}, "digest": null},
            "bytes": {"encoding": "base64", "data": "%%%"}
        }),
    );
}

#[test]
fn published_set_is_stable_versioned_and_self_describing() {
    let files = published();
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "lanes.schema.json",
            "status.schema.json",
            "output.schema.json",
            "cancel.schema.json",
            "error.schema.json",
            "mcp/fabric_run.schema.json",
            "mcp/fabric_output.schema.json"
        ]
    );
    for (name, schema) in &files {
        assert_eq!(schema["$schema"], schema::DIALECT, "{name}");
        assert!(
            schema["$id"].as_str().unwrap().starts_with(SCHEMA_ID_BASE),
            "{name}"
        );
        assert!(
            schema["$id"].as_str().unwrap().ends_with(name.as_str()),
            "{name}"
        );
        assert_eq!(files.iter().filter(|(other, _)| other == name).count(), 1);
    }
    for (name, schema) in files.iter().filter(|(name, _)| !name.starts_with("mcp/")) {
        assert_eq!(
            schema["properties"]["schema_version"]["const"], ENVELOPE_VERSION,
            "{name}"
        );
    }
    assert_eq!(files, published(), "publication must be deterministic");
    assert!(command_schema("worktree").is_none());
    assert!(mcp_output_schema("fabric_mail").is_none());
}

fn cli_envelope(command: &str, status: &str, data: Value, error: Value) -> Value {
    json!({
        "schema_version": 1, "command": command, "status": status,
        "data": data, "error": error
    })
}

#[test]
fn current_cli_failure_envelopes_validate() {
    let lanes = command_schema("lanes").unwrap();
    let cancel = command_schema("cancel").unwrap();
    let list = serde_json::to_value(list(vec![row()])).unwrap();
    // `lanes --wait` timing out: partial data, no error object.
    validate(
        &lanes,
        &cli_envelope("lanes", "timeout", list.clone(), Value::Null),
    )
    .unwrap();
    validate(&lanes, &cli_envelope("lanes", "ok", list, Value::Null)).unwrap();
    // Cancel with an unobservable receipt: failed, no error object.
    let unobservable = json!({"operation_id": "o", "receipts": [
        {"attempt_id": "a", "receipt": {"state": "unobservable", "reason": "no launch summary"}}
    ]});
    validate(
        &cancel,
        &cli_envelope("cancel", "failed", unobservable, Value::Null),
    )
    .unwrap();
    // Cancel timeout without an error.
    validate(
        &cancel,
        &cli_envelope(
            "cancel",
            "timeout",
            json!({"operation_id": "o", "receipts": [], "timed_out": true}),
            Value::Null,
        ),
    )
    .unwrap();
    // Cancel failure: `data` is `{operation_id}` only, with an error set.
    let failure = cli_envelope(
        "cancel",
        "failed",
        json!({"operation_id": "o"}),
        json!({"code": "NOT_FOUND", "message": "m", "cause": null, "repair": null}),
    );
    validate(&cancel, &failure).unwrap();
    validate(&error_envelope_schema(), &failure).unwrap();
    // The success shape still needs its receipts, and malformed data is refused.
    rejected(
        &cancel,
        cli_envelope("cancel", "ok", json!({"operation_id": "o"}), Value::Null),
    );
    rejected(
        &cancel,
        cli_envelope("cancel", "ok", json!({"receipts": []}), Value::Null),
    );
    rejected(
        &lanes,
        cli_envelope("lanes", "ok", Value::Null, Value::Null),
    );
    rejected(
        &lanes,
        cli_envelope("lanes", "timeout", Value::Null, json!({"code": "X"})),
    );
    rejected(
        &lanes,
        cli_envelope("lanes", "bogus", Value::Null, Value::Null),
    );
}

#[test]
fn version_one_envelopes_validate_and_version_two_is_planned_only() {
    let lanes = command_schema("lanes").unwrap();
    let cancel = command_schema("cancel").unwrap();
    let list = serde_json::to_value(list(vec![row()])).unwrap();
    let receipts = json!({"operation_id": "o", "receipts": []});
    validate(
        &lanes,
        &cli_envelope("lanes", "ok", list.clone(), Value::Null),
    )
    .unwrap();
    validate(
        &cancel,
        &cli_envelope("cancel", "ok", receipts.clone(), Value::Null),
    )
    .unwrap();
    for (schema, command, data) in [(&lanes, "lanes", list), (&cancel, "cancel", receipts)] {
        let mut planned = cli_envelope(command, "ok", data, Value::Null);
        planned["schema_version"] = json!(SCHEMA_VERSION);
        rejected(schema, planned);
    }
}
