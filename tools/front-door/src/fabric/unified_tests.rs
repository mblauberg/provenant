use super::*;
use provenant_fabric_runtime::graph::NodeState;
use provenant_fabric_runtime::graph::{
    GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
};

// Spec: docs/specs/cli.md#status-selection
#[test]
fn full_status_liveness_detail_names_event_source_directory_command_and_wait() {
    let attempt = json!({
        "state": "running",
        "last_event_at": "2026-11-20T10:00:00.000000Z",
        "last_event_source": "output",
        "attempt_directory": {
            "kind": "path",
            "root": "/",
            "relative": {"encoding": "utf8", "path": "state/attempts/abc"}
        },
        "child_command": "cargo build --release",
        "waiting_for": "waiting for a host build slot (12s)"
    });
    let detail = liveness_detail(&attempt);
    assert!(
        detail.contains("last event 2026-11-20T10:00:00.000000Z (output)"),
        "{detail}"
    );
    assert!(detail.contains("attempt /state/attempts/abc"), "{detail}");
    assert!(detail.contains("command cargo build --release"), "{detail}");
    assert!(
        detail.contains("wait waiting for a host build slot (12s)"),
        "{detail}"
    );

    let settled = json!({"state": "succeeded", "child_command": "cargo build"});
    assert!(liveness_detail(&settled).is_empty());
}

// Spec: docs/specs/cli.md#status-selection
#[test]
fn full_status_shows_liveness_below_the_digest_line_for_a_running_attempt() {
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint("g")), &target);
    super::status_keys(&mut rows, provenant_core::ProjectId::from_bytes([3; 16])).unwrap();
    assert!(
        !super::super::graph_status::node_digests(&rows).is_empty(),
        "fixture must take the digest path"
    );
    let attempt = rows
        .iter_mut()
        .find(|row| row["kind"] == "attempt")
        .unwrap();
    attempt["state"] = json!("running");
    attempt["last_event_at"] = json!("2026-11-20T10:01:00.000000Z");
    attempt["last_event_source"] = json!("started");

    let reading = render_graph_status(&rows);
    assert!(
        reading.contains("last event 2026-11-20T10:01:00.000000Z (started)"),
        "{reading}"
    );
}

// Spec: docs/specs/cli.md#status-selection
#[test]
fn full_status_shows_liveness_for_an_unattended_attempt() {
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint("g")), &target);
    super::status_keys(&mut rows, provenant_core::ProjectId::from_bytes([3; 16])).unwrap();
    let attempt = rows
        .iter_mut()
        .find(|row| row["kind"] == "attempt")
        .unwrap();
    attempt["state"] = json!("unattended");
    attempt["presence"] = json!("unattended");
    attempt["last_event_at"] = json!("2026-11-20T09:00:00.000000Z");
    attempt["last_event_source"] = json!("output");

    let reading = render_graph_status(&rows);
    assert!(
        reading.contains("last event 2026-11-20T09:00:00.000000Z (output)"),
        "{reading}"
    );
}

// Spec: docs/specs/cli.md#status-selection
#[test]
fn full_status_shows_no_liveness_for_a_settled_attempt() {
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint("g")), &target);
    super::status_keys(&mut rows, provenant_core::ProjectId::from_bytes([3; 16])).unwrap();
    let attempt = rows
        .iter_mut()
        .find(|row| row["kind"] == "attempt")
        .unwrap();
    attempt["state"] = json!("succeeded");
    attempt["last_event_at"] = json!("2026-11-20T10:01:00.000000Z");
    attempt["last_event_source"] = json!("output");

    let reading = render_graph_status(&rows);
    assert!(!reading.contains("last event"), "{reading}");
}

#[test]
fn admission_holder_reason_names_the_blocking_command() {
    let holder = provenant_os::admission::HeldInfo {
        class: "build".into(),
        slot: 2,
        pid: 42,
        start_unix_seconds: 0,
        attempt_id: Some("holder-attempt".into()),
        lane: None,
        command: Some("cargo build --release".into()),
    };
    let description = provenant_cli::run::admission_holder_description(&holder);
    assert!(description.contains("holder-attempt"), "{description}");
    assert!(
        description.contains("running cargo build --release"),
        "{description}"
    );
}

#[test]
fn composite_output_preserves_every_public_typed_export() {
    let commit = "1".repeat(40);
    let progress = composite_progress(json!({
        "schema_version": 2,
        "result": {
            "digest": "sha256:fixture",
            "excerpt": "result bytes",
            "truncated": false,
            "path": null
        },
        "verdict": "VERDICT: PASS",
        "findings": [],
        "findings_available": true,
        "branch": "feature/output",
        "commit": commit,
        "remaining": ["finish the export test"],
        "warnings": []
    }));
    let command = provenant_cli::Output {
        target: "typed-output/panel".parse().unwrap(),
        part: provenant_cli::Part::Result,
        to: None,
        path: false,
        offset: 0,
        tail: None,
        max_bytes: 64,
    };
    let reply = composite_graph_output(&command, "typed-output", &progress).unwrap();
    assert_eq!(reply.result.data["selected"]["branch"], "feature/output");
    assert_eq!(reply.result.data["selected"]["commit"], commit);
    assert_eq!(
        reply.result.data["selected"]["remaining"],
        json!(["finish the export test"])
    );
    let schema = provenant_records::wire::data_schema("output").unwrap();
    provenant_records::wire::schema::validate(&schema, &reply.result.data).unwrap();
}

fn composite_progress(outputs: Value) -> provenant_fabric_runtime::graph::NodeProgress {
    serde_json::from_value(json!({
        "id": "panel",
        "state": "succeeded",
        "worktree": null,
        "children": ["writer"],
        "attempts": [],
        "outputs": outputs
    }))
    .unwrap()
}

fn composite_output_command(offset: u64, max_bytes: u64) -> provenant_cli::Output {
    provenant_cli::Output {
        target: "typed-output/panel".parse().unwrap(),
        part: provenant_cli::Part::Result,
        to: None,
        path: false,
        offset,
        tail: None,
        max_bytes,
    }
}

fn output_chunk_bytes(data: &Value) -> Vec<u8> {
    match data["bytes"]["encoding"].as_str() {
        Some("utf8") => data["bytes"]["data"]
            .as_str()
            .expect("UTF-8 chunk data")
            .as_bytes()
            .to_vec(),
        Some("base64") => provenant_fabric_runtime::handoff::decode_base64(
            data["bytes"]["data"].as_str().expect("base64 chunk data"),
        )
        .expect("valid base64 chunk"),
        encoding => panic!("unexpected composite chunk encoding: {encoding:?}"),
    }
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn composite_output_paginates_multibyte_text_as_bounded_bytes() {
    let excerpt = "🧪éx";
    let progress = composite_progress(json!({
        "schema_version": 2,
        "result": {
            "digest": "sha256:fixture",
            "excerpt": excerpt,
            "truncated": false,
            "path": null
        },
        "findings": [],
        "warnings": []
    }));
    let schema = provenant_records::wire::data_schema("output").unwrap();

    // Starting inside the four-byte character returns its next raw byte,
    // losslessly, instead of slicing the Rust string at that offset.
    let middle =
        composite_graph_output(&composite_output_command(1, 1), "typed-output", &progress).unwrap();
    assert_eq!(middle.result.status, provenant_records::Outcome::Ok);
    assert_eq!(middle.result.data["offset"], 1);
    assert_eq!(middle.result.data["next_offset"], 2);
    assert_eq!(output_chunk_bytes(&middle.result.data).len(), 1);
    provenant_records::wire::schema::validate(&schema, &middle.result.data).unwrap();

    let mut offset = 0;
    let mut bytes = Vec::new();
    while offset < excerpt.len() as u64 {
        let reply = composite_graph_output(
            &composite_output_command(offset, 1),
            "typed-output",
            &progress,
        )
        .unwrap();
        let data = &reply.result.data;
        assert_eq!(data["offset"], offset);
        let next_offset = data["next_offset"].as_u64().expect("next byte offset");
        assert!(next_offset > offset);
        let chunk = output_chunk_bytes(data);
        assert_eq!(chunk.len() as u64, next_offset - offset);
        assert!(chunk.len() <= 1);
        assert_eq!(data["eof"], next_offset == excerpt.len() as u64);
        provenant_records::wire::schema::validate(&schema, data).unwrap();
        bytes.extend(chunk);
        offset = next_offset;
    }
    assert_eq!(bytes, excerpt.as_bytes());
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn composite_output_returns_typed_exports_without_result_text() {
    let commit = "1".repeat(40);
    let progress = composite_progress(json!({
        "schema_version": 2,
        "result": null,
        "verdict": "VERDICT: PASS",
        "findings": [],
        "findings_available": true,
        "review_partial": true,
        "warnings": ["typed-only source"],
        "branch": "feature/output",
        "commit": commit,
        "remaining": ["finish the export test"]
    }));
    let reply = composite_graph_output(&composite_output_command(0, 16), "typed-output", &progress)
        .unwrap();

    assert_eq!(reply.result.status, provenant_records::Outcome::Ok);
    let data = &reply.result.data;
    assert_eq!(data["graph"], "typed-output");
    assert_eq!(data["node"], "panel");
    assert_eq!(data["selected"]["verdict"], "VERDICT: PASS");
    assert_eq!(data["selected"]["findings"], json!([]));
    assert_eq!(data["selected"]["findings_available"], true);
    assert_eq!(data["selected"]["partial"], true);
    assert_eq!(data["selected"]["warnings"], json!(["typed-only source"]));
    assert_eq!(data["selected"]["branch"], "feature/output");
    assert_eq!(data["selected"]["commit"], commit);
    assert_eq!(
        data["selected"]["remaining"],
        json!(["finish the export test"])
    );
    assert_eq!(data["bytes"], json!({"encoding":"utf8","data":""}));
    assert_eq!(data["offset"], 0);
    assert_eq!(data["next_offset"], 0);
    assert_eq!(data["length"], Value::Null);
    assert_eq!(data["digest"], Value::Null);
    assert_eq!(data["eof"], true);
    assert_eq!(data["complete"], true);
    assert!(data.get("attempt_id").is_none());
    let schema = provenant_records::wire::data_schema("output").unwrap();
    provenant_records::wire::schema::validate(&schema, data).unwrap();
}

#[test]
fn retry_brief_validates_the_resolved_target_and_single_node_before_input() {
    let graph: provenant_cli::Target = "work".parse().unwrap();
    let rows = [
        json!({"id":"work","kind":"graph"}),
        json!({"id":"work/inspect","kind":"node"}),
    ];
    assert!(validate_retry_brief_target(&graph, &rows).is_ok());
    let multiple_nodes = [
        json!({"id":"work","kind":"graph"}),
        json!({"id":"work/inspect","kind":"node"}),
        json!({"id":"work/test","kind":"node"}),
    ];
    assert!(validate_retry_brief_target(&graph, &multiple_nodes).is_err());

    let node: provenant_cli::Target = "work/inspect".parse().unwrap();
    assert!(validate_retry_brief_target(&node, &rows).is_ok());
    assert!(validate_retry_brief_target(&node, &[]).is_err());
}

#[test]
fn unreadable_attempt_keeps_other_status_rows() {
    let mut rows = [
        json!({"kind":"attempt", "state":"running"}),
        json!({"kind":"graph", "state":"running"}),
    ];
    mark_unreadable_attempt(
        &mut rows[0],
        "attempt-id",
        &provenant_records::DecodeError::Typed {
            line: 2,
            column: 3,
            field: None,
            schema_version: None,
        },
    );
    assert_eq!(rows[0]["state"], "unreadable");
    assert!(
        rows[0]["reason"]
            .as_str()
            .unwrap()
            .contains("run store record for attempt attempt-id: decode error")
    );
    assert_eq!(rows[1]["state"], "running");
}

fn checkpoint(graph: &str) -> GraphCheckpoint {
    serde_json::from_value(json!({"schema_version":2,"graph_id":graph,"plan_digest":"digest","nodes":[
            {"id":"parent","state":"running","worktree":null,"attempts":[
                {"launched":true,"key":{"graph_id":graph,"node_id":"parent","route_index":0, "attempt": 1},"route":{"alias":"senior"},"state":"running","attempt_ref":"11111111111111111111111111111111/22222222222222222222222222222222","launch_actor":"actor"}
            ]},
            {"id":"parent--child","state":"pending","worktree":null,"attempts":[]},
            {"id":"parent--keep","state":"pending","worktree":null,"attempts":[]},
            {"id":"other","state":"pending","worktree":null,"attempts":[]}
        ]})).unwrap()
}

fn document(graph: &str) -> GraphDocument {
    serde_json::from_value(json!({"schema_version":1,"graph_id":graph,"nodes":[
            {"id":"parent","access":"read_only","prompt":"parent","route":{"alias":"senior"}},
            {"id":"parent--child","access":"read_only","prompt":"child","route":{"alias":"senior"}},
            {"id":"parent--keep","access":"read_only","prompt":"keep","route":{"alias":"senior"},"detached":true},
            {"id":"other","access":"read_only","prompt":"other","route":{"alias":"senior"}}
        ]})).unwrap()
}

#[test]
fn lane_status_names_another_projects_admission_holder() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let waiting_scratch = temp.path().join("project-b/waiting");
    let holding_scratch = temp.path().join("project-a/holding");
    provenant_os::admission::register_lane(
        temp.path(),
        "build",
        &holding_scratch,
        "22222222222222222222222222222222".parse()?,
        provenant_os::admission::LaneIdentity {
            project: temp.path().join("project-a"),
            name: "builder-a".to_owned(),
        },
    )?;
    let _held = provenant_os::admission::mark_held(
        &holding_scratch,
        "build",
        2,
        42,
        Some("holder-attempt"),
    )?;
    let _wait = provenant_os::admission::mark_waiting(
        &waiting_scratch,
        "build",
        43,
        Some("waiting-attempt"),
    )?;
    let id = "11111111111111111111111111111111";
    let digest = format!("sha256:{}", "ab".repeat(32));
    let mut attempt: provenant_records::run::AttemptRecord = serde_json::from_value(json!({
        "schema_version":1,"attempt_id":id,"project_id":id,
        "key":{"project":id,"graph":"graph","node":"node","attempt":1},"attempt_number":1,"revision":1,"generation":1,
        "requested_route":{"kind":"named","name":"senior"},
        "authority_digest":digest,"queued_at":"2026-10-29T00:00:00.000000Z",
        "state":{"state":"running"},"usage":{},"read_only_writable":[],
        "protected_paths":[],"trace":{},
        "prompt":{"path":{"encoding":"utf8","path":"prompt.md"},
            "digest":digest,"bytes":0,"complete":true,"redacted":false}
    }))?;
    attempt
        .write_grants
        .push(provenant_records::run::WriteGrantRecord {
            extra: Default::default(),
            source: provenant_records::run::WriteSource::Scratch,
            path: provenant_records::AbsolutePath::new(&waiting_scratch)?,
            scope: provenant_records::run::PathScope::Subtree,
        });
    let row = provenant_cli::run::project_lane_with_admission(
        &attempt,
        &[],
        temp.path(),
        None,
        true,
        Err(()),
        &[],
    );
    let reason = row.waiting_for.ok_or("missing admission wait")?;
    assert!(
        reason.contains("build")
            && reason.contains("project-a")
            && reason.contains("builder-a")
            && reason.contains("slot 2"),
        "{reason}"
    );
    Ok(())
}

#[test]
fn graph_peer_status_preserves_typed_wait_facts() -> Result<(), Box<dyn std::error::Error>> {
    let id = "11111111111111111111111111111111";
    let digest = format!("sha256:{}", "ab".repeat(32));
    let attempt: provenant_records::run::AttemptRecord = serde_json::from_value(json!({
        "schema_version":1,"attempt_id":id,"project_id":id,
        "key":{"project":id,"graph":"graph","node":"node","attempt":1},"attempt_number":1,"revision":1,"generation":1,
        "requested_route":{"kind":"named","name":"senior"},
        "authority_digest":digest,"queued_at":"2026-10-29T00:00:00.000000Z",
        "state":{"state":"starting"},"usage":{},"read_only_writable":[],
        "protected_paths":[],"trace":{},
        "prompt":{"path":{"encoding":"utf8","path":"prompt.md"},
            "digest":digest,"bytes":0,"complete":true,"redacted":false}
    }))?;
    let waits = vec![provenant_records::run::LaneWait {
        extra: Default::default(),
        attempt_id: attempt.attempt_id,
        lane_class: provenant_records::run::LaneClass::Writer,
        waiting_for: provenant_records::run::LaneWaitReason::RecoveryWorkerRefused {
            used: 5,
            limit: 10,
        },
    }];
    let response = RunResult::Status {
        extra: Default::default(),
        lane_waits: waits.clone(),
        attempt: attempt.clone(),
    };
    let decoded: RunResult = serde_json::from_value(serde_json::to_value(response)?)?;
    let RunResult::Status {
        attempt: peer_attempt,
        lane_waits: peer_waits,
        ..
    } = decoded
    else {
        return Err("status response changed kind".into());
    };
    assert_eq!(peer_waits, waits);
    let local =
        provenant_cli::run::project_lane(&attempt, &waits, Path::new("/unused"), None, false);
    let peer = provenant_cli::run::project_lane(
        &peer_attempt,
        &peer_waits,
        Path::new("/unused"),
        None,
        false,
    );
    assert!(local.waiting_for.is_some());
    assert_eq!(peer.waiting_for, local.waiting_for);
    assert_eq!(peer.reason, local.reason);
    Ok(())
}

fn listed(name: &str, settled: bool, changed_micros: i64) -> ListedGraph {
    ListedGraph {
        name: name.into(),
        state: if settled { "succeeded" } else { "pending" }.into(),
        settled,
        template: None,
        nodes: 1,
        created_micros: Some(changed_micros),
        changed_micros: Some(changed_micros),
        reason: None,
        digests: Vec::new(),
        open_asks: Vec::new(),
    }
}

#[test]
fn the_listing_never_hides_active_work_and_counts_what_it_omits() {
    let day = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
    let now = 10 * day;
    for (active, recent, old, limit, all) in [
        (25_usize, 3_usize, 2_usize, 20, false),
        (2, 30, 4, 20, false),
        (0, 3, 40, 20, true),
        (5, 0, 0, 1, false),
    ] {
        let mut graphs = Vec::new();
        graphs.extend((0..active).map(|n| listed(&format!("a{n}"), false, now - 2 * day)));
        graphs.extend(
            (0..recent)
                .zip(0_i64..)
                .map(|(n, age)| listed(&format!("r{n}"), true, now - age)),
        );
        graphs.extend(
            (0..old)
                .zip(0_i64..)
                .map(|(n, age)| listed(&format!("o{n}"), true, now - 2 * day - age)),
        );
        let listing = select_listing(graphs, now, all, limit, None);
        let shown_active = listing.rows.iter().filter(|row| !row.settled).count();
        let shown_settled = listing.rows.len() - shown_active;
        assert_eq!(shown_active, active, "every active graph is listed");
        assert_eq!(
            listing.omitted.values().sum::<usize>(),
            recent + old - shown_settled,
            "the omission count is the settled rows not shown"
        );
        assert!(
            if all {
                shown_settled <= limit
            } else {
                shown_settled == 0 || listing.rows.len() <= limit
            },
            "settled rows respect the page allowance while every active graph stays visible"
        );
    }
    // `--all` pages through every settled graph, newest first.
    let graphs: Vec<_> = (0..5).map(|n| listed(&format!("s{n}"), true, n)).collect();
    let first = select_listing(graphs.clone(), 3 * day, true, 2, None);
    assert_eq!(first.rows[0].name, "s4");
    let second = select_listing(graphs, 3 * day, true, 2, first.next);
    assert_eq!(second.rows[0].name, "s2");
}

fn digest(presence: Option<&str>) -> super::super::graph_status::NodeDigest {
    super::super::graph_status::NodeDigest {
        key: provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([7; 16]),
            "g",
            "n",
            1,
        )
        .expect("valid work key"),
        outcome: "running".into(),
        cause: None,
        branch: None,
        pr: json!({"state":"unknown","reference":null}),
        ci: json!({"state":"unavailable"}),
        result_path: None,
        provider_error: None,
        review_verdict: None,
        findings_count: None,
        findings_available: None,
        review_partial: None,
        attempt_state: Some("running".into()),
        attempt_presence: presence.map(str::to_owned),
        line: "  n: running".into(),
        state: "running".into(),
        settled: false,
        handoff_status: None,
        open_asks: Vec::new(),
        evidence_error: None,
    }
}

#[test]
fn listing_leads_with_live_lanes_and_counts_settled_graphs() {
    let now = 1_000_000_000;
    let mut live = listed("live", false, now - 3);
    live.digests.push(digest(Some("live")));
    let idle = listed("idle", false, now - 2);
    let settled = listed("settled", true, now - 1);
    let mut graphs = vec![settled, idle, live];
    graphs.reverse(); // oldest first; finish_listing owns presentation order
    let settling = finish_listing(
        Listing {
            rows: graphs.clone(),
            omitted: Default::default(),
            next: None,
            admission_holders: Vec::new(),
            admission_waiters: Vec::new(),
            stranded_work: Vec::new(),
        },
        1,
        false,
    );
    assert_eq!(
        settling
            .rows
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        vec!["live", "idle"],
        "live lanes lead and settled graphs are not listed"
    );
    assert_eq!(settling.omitted.get("settled"), Some(&1));

    let full = finish_listing(
        Listing {
            rows: graphs,
            omitted: Default::default(),
            next: None,
            admission_holders: Vec::new(),
            admission_waiters: Vec::new(),
            stranded_work: Vec::new(),
        },
        1,
        true,
    );
    assert_eq!(
        full.rows
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        vec!["live", "idle", "settled"],
        "--full lists settled graphs after live lanes"
    );
    assert!(full.omitted.is_empty());
}

#[test]
fn default_listing_keeps_a_settled_graph_with_a_live_lane() {
    let now = 1_000_000_000;
    let mut settled_live = listed("settled-live", true, now - 1);
    settled_live.digests.push(digest(Some("live")));
    let settled_idle = listed("settled-idle", true, now - 2);
    let active = listed("active", false, now - 3);
    let listing = finish_listing(
        Listing {
            rows: vec![active, settled_live, settled_idle],
            omitted: Default::default(),
            next: None,
            admission_holders: Vec::new(),
            admission_waiters: Vec::new(),
            stranded_work: Vec::new(),
        },
        2,
        false,
    );
    assert_eq!(
        listing
            .rows
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        vec!["settled-live", "active"],
        "a settled graph with a live lane is never hidden from the default listing"
    );
    assert_eq!(
        listing.omitted.get("settled"),
        Some(&1),
        "only the settled graph without a live lane is counted as omitted"
    );
}

#[test]
fn an_old_settled_graph_with_a_live_lane_is_listed_and_counted() {
    // Mirror graph_listing: enrich digests first, then select, then finish.
    // A cancelled graph well outside the settled window still has a running
    // lane, and active work already fills the display allowance.
    let day = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
    let now = 10 * day;
    let mut cancelled_live = listed("cancelled-live", true, now - 5 * day);
    cancelled_live.state = "cancelled".into();
    cancelled_live.digests.push(digest(Some("live")));
    let mut graphs: Vec<_> = (0..5)
        .map(|n| listed(&format!("active{n}"), false, now - n))
        .collect();
    graphs.push(cancelled_live);
    let selected = select_listing(graphs, now, false, 5, None);
    let listing = finish_listing(selected, 1, false);
    assert_eq!(
        listing.rows.first().map(|row| row.name.as_str()),
        Some("cancelled-live"),
        "a live lane leads the listing even when its graph is old settled and the page is full"
    );
    assert_eq!(
        live_lanes(&listing),
        1,
        "the live lane of the old settled graph is counted"
    );
    assert!(
        listing.omitted.values().sum::<usize>() == 0,
        "a settled graph kept for its live lane is not omitted"
    );
}

#[test]
fn omitted_count_covers_settled_graphs_outside_the_window() {
    let day = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
    let now = 10 * day;
    let mut graphs: Vec<_> = (0..2)
        .map(|n| listed(&format!("recent{n}"), true, now - n))
        .collect();
    graphs.extend((0..3).map(|n| listed(&format!("old{n}"), true, now - 3 * day - n)));
    let selected = select_listing(graphs.clone(), now, false, 20, None);
    let listing = finish_listing(selected, graphs.len(), false);
    assert!(
        listing.rows.is_empty(),
        "settled graphs without a live lane are hidden from the default listing"
    );
    assert_eq!(
        listing.omitted.get("settled"),
        Some(&5),
        "the omitted total includes settled graphs outside the window, not just rows dropped here"
    );
}

#[test]
fn only_old_settled_graphs_are_all_counted_as_omitted() {
    let day = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
    let now = 10 * day;
    let graphs: Vec<_> = (0..3)
        .map(|n| listed(&format!("old{n}"), true, now - 3 * day - n))
        .collect();
    let selected = select_listing(graphs.clone(), now, false, 20, None);
    let listing = finish_listing(selected, graphs.len(), false);
    assert!(listing.rows.is_empty());
    assert_eq!(
        listing.omitted.get("settled"),
        Some(&3),
        "a window with only old settled graphs still reports the omitted total"
    );
}

fn listing_attempt_row(state: &str) -> Value {
    let key = provenant_records::WorkKey::new(
        provenant_core::ProjectId::from_bytes([7; 16]),
        "g",
        "n",
        1,
    )
    .expect("valid work key");
    json!({
        "id": "g/n#1",
        "kind": "attempt",
        "graph_id": "g",
        "node_id": "n",
        "attempt_id": "g/n#1",
        "state": state,
        "work_key": key,
    })
}

#[test]
fn terminal_settled_history_is_not_enriched_and_is_counted_as_omitted() {
    // The graph_listing path: cheap projection rows for every graph,
    // evidence only for active work and possibly-live settled graphs, then
    // selection and finish. The fake enricher counts which graphs pay for
    // evidence and marks a live presence on a still-running attempt.
    let day = provenant_cli::limits::LISTING_SETTLED_SECONDS * 1_000_000;
    let now = 10 * day;
    let mut graphs = vec![
        listed("active", false, now - 1),
        listed("recent-terminal", true, now - 1),
        listed("old-terminal", true, now - 3 * day),
        listed("old-live", true, now - 4 * day),
    ];
    let mut projections = std::collections::BTreeMap::new();
    projections.insert("active".to_owned(), vec![listing_attempt_row("running")]);
    projections.insert(
        "recent-terminal".to_owned(),
        vec![listing_attempt_row("terminal")],
    );
    projections.insert(
        "old-terminal".to_owned(),
        vec![listing_attempt_row("terminal")],
    );
    projections.insert("old-live".to_owned(), vec![listing_attempt_row("running")]);

    let mut enriched_names = Vec::new();
    let mut enrich = |name: &str, rows: &mut [Value]| -> Result<(), FacadeError> {
        enriched_names.push(name.to_owned());
        for row in rows.iter_mut() {
            if row["kind"] == "attempt" && row["state"] == "running" {
                row["presence"] = json!("live");
            }
        }
        Ok(())
    };
    let enriched = super::enrich_liveness_candidates(&mut graphs, &mut projections, &mut enrich);
    assert_eq!(
        enriched_names,
        ["active", "old-live"],
        "only active work and a settled graph naming a running attempt are enriched"
    );
    assert_eq!(
        enriched,
        std::collections::BTreeSet::from(["active".to_owned(), "old-live".to_owned()])
    );

    let settled_count = graphs.iter().filter(|graph| graph.settled).count();
    let selected = select_listing(graphs, now, false, 20, None);
    let listing = finish_listing(selected, settled_count, false);
    assert!(
        listing.rows.iter().any(|row| row.name == "active"),
        "the active graph is listed"
    );
    assert!(
        listing.rows.iter().any(|row| row.name == "old-live"),
        "the old settled graph with a live lane is still listed"
    );
    assert_eq!(
        live_lanes(&listing),
        2,
        "the active graph's lane and the old settled graph's live lane are both counted"
    );
    assert_eq!(
        listing.omitted.get("settled"),
        Some(&2),
        "both terminal settled graphs, including the one outside the page window, are omitted"
    );
}

#[test]
fn listing_live_lane_count_matches_digest_presence() {
    let mut first = listed("first", false, 10);
    first.digests.push(digest(Some("live")));
    first.digests.push(digest(Some("unattended")));
    let mut second = listed("second", true, 20);
    second.digests.push(digest(Some("live")));
    second.digests.push(digest(Some("live")));
    let listing = Listing {
        rows: vec![first, second],
        omitted: Default::default(),
        next: None,
        admission_holders: Vec::new(),
        admission_waiters: Vec::new(),
        stranded_work: Vec::new(),
    };
    assert_eq!(live_lanes(&listing), 3);
    let summary = listing_summary(&listing, 50, 0);
    assert!(
        summary.lines[0].starts_with("3 live lanes"),
        "headline: {}",
        summary.lines[0]
    );
    assert_eq!(
        summary.lines[0].matches("live lanes").count(),
        1,
        "the live count leads the headline once"
    );
}

// Spec: docs/specs/cli.md#status-selection
#[test]
fn live_attempt_graph_is_running_in_the_summary_and_the_detail() {
    let key = json!(
        provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([7; 16]),
            "rep-identity",
            "implement",
            1,
        )
        .expect("valid work key")
    );
    let rows = vec![
        json!({
            "id":"rep-identity/implement","kind":"node","graph_id":"rep-identity",
            "node_id":"implement","work_key":key,"state":"running"
        }),
        json!({
            "id":"rep-identity/implement#1","kind":"attempt","graph_id":"rep-identity",
            "node_id":"implement","work_key":key,"state":"running","presence":"live",
            "latest_attempt":true,"graph_state":"running"
        }),
    ];

    let digests = super::super::graph_status::node_digests(&rows);
    assert_eq!(digests.len(), 1);
    assert_eq!(digests[0].display_state(), "running");
    assert!(
        digests[0]
            .line
            .contains("running node_state=running attempt_state=running"),
        "detail line: {}",
        digests[0].line
    );
    assert!(digests[0].line.contains("attempt_presence=live"));

    // The checkpoint still reads `pending`, but the listing must use the
    // lane's own state, the same one the detail line uses.
    let mut graph = listed("rep-identity", false, 10);
    assert_eq!(graph.state, "pending");
    graph.digests = digests;
    let listing = Listing {
        rows: vec![graph],
        omitted: Default::default(),
        next: None,
        admission_holders: Vec::new(),
        admission_waiters: Vec::new(),
        stranded_work: Vec::new(),
    };
    let summary = listing_summary(&listing, 50, 0);
    assert_eq!(live_lanes(&listing), 1);
    assert!(
        summary.lines[0].contains("1 running"),
        "headline: {}",
        summary.lines[0]
    );
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line.starts_with("rep-identity running")),
        "rows: {:?}",
        summary.lines
    );

    let detail_summary = status_summary(&rows);
    assert!(
        detail_summary.lines[0].contains("1 running"),
        "detail headline: {}",
        detail_summary.lines[0]
    );
}

// Spec: docs/specs/cli.md#status-selection
#[test]
fn graph_waiting_for_admission_is_pending_in_the_summary_and_the_detail() {
    let key = json!(
        provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([7; 16]),
            "rep-identity",
            "implement",
            1,
        )
        .expect("valid work key")
    );
    let wait = "waiting for a host build slot (12s)";
    let rows = vec![
        json!({
            "id":"rep-identity/implement","kind":"node","graph_id":"rep-identity",
            "node_id":"implement","work_key":key,"state":"waiting","waiting_for":wait,
            "reason":wait
        }),
        json!({
            "id":"rep-identity/implement#1","kind":"attempt","graph_id":"rep-identity",
            "node_id":"implement","work_key":key,"state":"queued","latest_attempt":true
        }),
    ];

    let digests = super::super::graph_status::node_digests(&rows);
    assert_eq!(digests.len(), 1);
    assert_eq!(digests[0].display_state(), "pending");
    assert!(
        digests[0]
            .line
            .contains("pending node_state=waiting attempt_state=queued"),
        "detail line: {}",
        digests[0].line
    );

    let mut graph = listed("rep-identity", false, 10);
    graph.digests = digests;
    let listing = Listing {
        rows: vec![graph],
        omitted: Default::default(),
        next: None,
        admission_holders: Vec::new(),
        admission_waiters: Vec::new(),
        stranded_work: Vec::new(),
    };
    let summary = listing_summary(&listing, 50, 0);
    assert_eq!(live_lanes(&listing), 0);
    assert!(
        summary.lines[0].starts_with("1 pending"),
        "headline: {}",
        summary.lines[0]
    );
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line.starts_with("rep-identity pending")),
        "rows: {:?}",
        summary.lines
    );

    let detail_summary = status_summary(&rows);
    assert!(
        detail_summary.lines[0].starts_with("1 pending"),
        "detail headline: {}",
        detail_summary.lines[0]
    );
}

#[test]
fn graph_age_uses_creation_time_after_later_changes() {
    let mut graph = listed("running", false, 110_000_000);
    graph.created_micros = Some(10_000_000);
    assert_eq!(graph_age(120_000_000, &graph), "1m");
}

#[test]
fn missing_project_target_error_has_one_code_and_a_repair() {
    let target = "graph-name".parse().expect("valid graph target");
    let fault = outside_project_fault(&target);
    assert_eq!(fault.code, "NOT_FOUND");
    assert!(fault.message.contains("no project found"));
    assert!(fault.message.contains("pass --project PATH"));
    assert!(!fault.message.starts_with("NOT_FOUND:"));
}

#[test]
fn a_stop_cause_is_kept_whole_in_the_row_and_bounded_in_the_summary() {
    // A row builder must not clip before the render mode is known: --full prints
    // this whole reason, while the compact summary applies SUMMARY_ROW_BYTES.
    let reason = "x".repeat(4096);
    let row = json!({"kind":"node","id":"fix-login/writer#2","state":"failed",
            "cause_code":"budget_exhausted","reason":reason,"waiting_for":null});
    let line = status_line("fix-login/writer#2", &row, Some("codex/gpt"));
    assert_eq!(line.lines().count(), 1);
    assert!(
        line.contains(&reason),
        "the row builder clipped the reason: {line}"
    );
    assert!(line.contains("cause=budget_exhausted"), "{line}");

    let summary = status_summary(&[row]);
    let bounded = &summary.lines[1];
    assert!(
        bounded.len() <= provenant_cli::limits::SUMMARY_ROW_BYTES,
        "{bounded}"
    );
    assert!(bounded.ends_with("..."), "no clip marker: {bounded}");
}

#[test]
fn a_long_result_excerpt_is_kept_whole_in_the_row() -> Result<(), Box<dyn std::error::Error>> {
    // Past the old 200-byte projection cap: the row keeps the fact, and only
    // the compact renderer clips it. --full must be able to print it whole.
    let excerpt = "x".repeat(200 + 256);
    let mut value = serde_json::to_value(checkpoint("g"))?;
    value["nodes"][0]["outputs"] = json!({
        "schema_version":1,
        "result":{"digest":"sha256:test","excerpt":excerpt,"truncated":false,"path":"result.txt"},
        "verdict":null,"findings":[],"findings_available":null,
        "branch":"feat/work","commit":"abc123","warnings":[]
    });
    let checkpoint: GraphCheckpoint = serde_json::from_value(value)?;
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let rows = status_rows(&graph_snapshot(checkpoint), &target);
    let node = rows
        .iter()
        .find(|row| row["kind"] == "node")
        .expect("a node row");
    assert_eq!(
        node["result_summary"], excerpt,
        "the row projection clipped the result summary before rendering"
    );
    Ok(())
}

// Spec: docs/specs/cli.md#status-selection
#[test]
// Spec: docs/specs/cli.md#output-references-and-bounded-tails
fn result_renders_retained_head_and_checks_after_collection()
-> Result<(), Box<dyn std::error::Error>> {
    let mut value = serde_json::to_value(checkpoint("g"))?;
    let node = &mut value["nodes"][0];
    node["state"] = json!("succeeded");
    node["outputs"] = json!({"schema_version":1,"result":{"digest":"sha256:test","excerpt":"result excerpt","truncated":false,"path":"result.txt"},"verdict":null,"findings":[],"findings_available":null,"branch":"feat/work","commit":"abc123","warnings":[]});
    node["handoff"] = json!({"schema_version":2,"status":"done","note":"finished","remaining":[],"decisions":["choice"],"verify":["first check","second check"]});
    let checkpoint: GraphCheckpoint = serde_json::from_value(value)?;
    let graph = GraphTarget {
        graph: "g".into(),
        node: Some("parent".into()),
        attempt: None,
    };
    let target = "g/parent".parse()?;
    let rows = status_rows(&graph_snapshot(checkpoint), &graph);
    for collected in [false, true] {
        let mut projected = rows.clone();
        if !collected {
            let attempt = projected
                .iter_mut()
                .find(|row| row["kind"] == "attempt")
                .unwrap();
            attempt["head"] = json!("abc123");
        }
        let reply = result_reply(&target, &projected);
        let mut output = Vec::new();
        provenant_cli::render(
            &reply.result,
            provenant_cli::Globals::default().render_mode(
                &reply.human,
                reply.read_only,
                reply.summary.as_ref(),
            ),
            &mut output,
        )?;
        let output = String::from_utf8(output)?;
        for expected in [
            "succeeded",
            "feat/work",
            "abc123",
            "first check",
            "second check",
        ] {
            assert!(output.contains(expected), "{output}");
        }
        assert_eq!(output.matches("g/parent:").count(), 1, "{output}");
        assert_eq!(reply.result.data["rows"][0]["head"], "abc123");
        assert!(reply.human.contains("head: abc123"));
    }
    Ok(())
}

// Spec: docs/specs/run-settlement.md#review-verdict-wording
#[test]
fn review_result_reports_the_reviewed_head_not_the_launch_base()
-> Result<(), Box<dyn std::error::Error>> {
    let mut value = serde_json::to_value(review_checkpoint("VERDICT: PASS", json!([])))?;
    value["nodes"][0]["review_settlement"] = json!({
        "attempt_id": "7".repeat(32),
        "head": "b".repeat(40),
        "base": "a".repeat(40),
        "verdict": "approve",
        "independent": true,
    });
    let checkpoint: GraphCheckpoint = serde_json::from_value(value)?;
    let rows = status_rows(
        &graph_snapshot(checkpoint),
        &GraphTarget {
            graph: "g".into(),
            node: Some("parent".into()),
            attempt: None,
        },
    );
    let target = "g/parent".parse()?;
    let reply = result_reply(&target, &rows);
    let reviewed = "b".repeat(40);
    assert_eq!(reply.result.data["rows"][0]["head"], reviewed.as_str());
    assert!(reply.human.contains(&format!("head: {reviewed}")));
    Ok(())
}

fn review_checkpoint(verdict: &str, findings: Value) -> GraphCheckpoint {
    serde_json::from_value(json!({
            "schema_version": 2, "graph_id": "g", "plan_digest": "digest", "nodes": [
                {"id": "parent", "state": "succeeded", "worktree": null, "attempts": [],
                 "outputs": {
                    "schema_version": 1,
                    "result": {"digest": "sha256:test", "excerpt": "review done", "truncated": false, "path": null},
                    "verdict": verdict,
                    "findings": findings,
                    "findings_available": true,
                    "review_partial": verdict != "VERDICT: PASS",
                    "branch": "feat/review", "commit": "abc123", "warnings": []
                 },
                 "handoff": {"schema_version": 2, "status": "done", "note": "reviewed", "remaining": [], "decisions": [], "verify": ["checked"]}}
            ]
        }))
        .unwrap()
}

fn render_result(reply: &provenant_cli::run::Reply, json: bool) -> String {
    let globals = provenant_cli::Globals {
        json,
        ..Default::default()
    };
    let mode = globals.render_mode(&reply.human, reply.read_only, reply.summary.as_ref());
    let mut output = Vec::new();
    provenant_cli::render(&reply.result, mode, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

fn result_rows(reply: &provenant_cli::run::Reply) -> Vec<Value> {
    reply.result.data["rows"]
        .as_array()
        .expect("result data rows")
        .clone()
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn result_json_reports_accepted_and_findings_required_reviews() {
    let accepted = result_rows(&result_reply(
        &"g/parent".parse().unwrap(),
        &status_rows(
            &graph_snapshot(review_checkpoint("VERDICT: PASS", json!([]))),
            &GraphTarget {
                graph: "g".into(),
                node: Some("parent".into()),
                attempt: None,
            },
        ),
    ));
    assert_eq!(accepted.len(), 1);
    let row = &accepted[0];
    assert_eq!(row["verdict"], "VERDICT: PASS");
    assert_eq!(row["findings"], json!([]));
    assert_eq!(row["findings_available"], true);
    assert_eq!(row["partial"], false);

    let findings = json!([
        {"id":"F1","severity":"high","file":"src/a.rs","line":12,"summary":"critical bug","status":"open","lens":"security"},
        {"id":"F2","severity":"low","file":"src/b.rs","line":3,"summary":"style","status":"open","lens":"security"}
    ]);
    let reply = result_reply(
        &"g/parent".parse().unwrap(),
        &status_rows(
            &graph_snapshot(review_checkpoint("VERDICT: FAIL", findings.clone())),
            &GraphTarget {
                graph: "g".into(),
                node: Some("parent".into()),
                attempt: None,
            },
        ),
    );
    let required = result_rows(&reply);
    assert_eq!(required.len(), 1);
    let row = &required[0];
    assert_eq!(row["verdict"], "VERDICT: FAIL");
    assert_eq!(row["findings"], findings);
    assert_eq!(row["findings_available"], true);
    assert_eq!(row["partial"], true);

    // --json is an independent transport: the whole command envelope carries
    // the same structured review fields as the in-process reply.
    let encoded: Value = serde_json::from_str(&render_result(&reply, true)).unwrap();
    assert_eq!(encoded["command"], "result");
    let json_row = &encoded["data"]["rows"][0];
    assert_eq!(json_row["verdict"], "VERDICT: FAIL");
    assert_eq!(json_row["findings"], findings);
    assert_eq!(json_row["partial"], true);
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn result_compact_shows_verdict_and_counts_findings_by_severity() {
    let findings = json!([
        {"id":"F1","severity":"high","file":"src/a.rs","line":12,"summary":"critical bug","status":"open","lens":"security"},
        {"id":"F2","severity":"high","file":"src/c.rs","line":9,"summary":"another bug","status":"open","lens":"security"},
        {"id":"F3","severity":"low","file":"src/b.rs","line":3,"summary":"style","status":"open","lens":"security"}
    ]);
    let reply = result_reply(
        &"g/parent".parse().unwrap(),
        &status_rows(
            &graph_snapshot(review_checkpoint("VERDICT: FAIL", findings)),
            &GraphTarget {
                graph: "g".into(),
                node: Some("parent".into()),
                attempt: None,
            },
        ),
    );
    let compact = render_result(&reply, false);
    assert!(compact.contains("VERDICT: FAIL"), "{compact}");
    assert!(compact.contains("2 high"), "{compact}");
    assert!(compact.contains("1 low"), "{compact}");
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn result_on_panel_lists_each_lens_verdict() {
    let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version": 2, "graph_id": "g", "plan_digest": "digest", "nodes": [
                {"id": "security", "state": "succeeded", "worktree": null, "attempts": [],
                 "outputs": {"schema_version":1,"result":null,"verdict":"VERDICT: FAIL",
                   "findings":[{"id":"F1","severity":"high","file":"a.rs","line":1,"summary":"s","status":"open","lens":"security"}],
                   "findings_available":true,"review_partial":false,"branch":null,"commit":null,"warnings":[]},
                 "handoff":{"schema_version":2,"status":"done","note":"","remaining":[],"decisions":[],"verify":[]}},
                {"id": "correctness", "state": "succeeded", "worktree": null, "attempts": [],
                 "outputs": {"schema_version":1,"result":null,"verdict":"VERDICT: PASS",
                   "findings":[],"findings_available":true,"review_partial":false,"branch":null,"commit":null,"warnings":[]},
                 "handoff":{"schema_version":2,"status":"done","note":"","remaining":[],"decisions":[],"verify":[]}}
            ]
        }))
        .unwrap();
    let target: provenant_cli::Target = "g".parse().unwrap();
    let graph = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let reply = result_reply(&target, &status_rows(&graph_snapshot(checkpoint), &graph));
    let rows = result_rows(&reply);
    let verdicts: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap(),
                row["verdict"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        verdicts,
        vec![
            ("g/security", "VERDICT: FAIL"),
            ("g/correctness", "VERDICT: PASS")
        ]
    );
}

#[test]
fn unified_status_uses_one_row_contract_and_selects_attempts() {
    // A projection may omit earlier rows; each displayed handle must still
    // resolve to the WorkKey and reference that produced it.
    let mut projected = checkpoint("g");
    projected.nodes[0].attempts[0].key.attempt = 7;
    projected.nodes[0].warnings.push("retry warning".into());
    let run = graph_snapshot(projected);
    let graph = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let rows = status_rows(&run, &graph);
    let keys = rows[0].as_object().unwrap().keys().collect::<Vec<_>>();
    assert!(
        rows.iter()
            .all(|row| row.as_object().unwrap().keys().collect::<Vec<_>>() == keys)
    );
    for row in &rows {
        if row["kind"] == "node" && row["node_id"] == "parent" {
            assert_eq!(row["warnings"], json!(["retry warning"]));
        } else if row["kind"] != "node" {
            assert!(row["warnings"].is_null());
        }
    }
    assert_eq!(rows[0]["kind"], "graph");
    assert_eq!(rows[1]["id"], "g/parent");
    for row in rows.iter().filter(|row| row["kind"] == "attempt") {
        let target = provenant_cli::Target::parse(row["id"].as_str().unwrap()).unwrap();
        let resolved =
            provenant_cli::target::resolve(&target, Want::Attempt { settled: false }, &[], |_| {
                Ok::<_, std::convert::Infallible>(Some(run.checkpoint.clone()))
            })
            .unwrap();
        assert_eq!(resolved.attempt_ref.as_deref(), row["attempt_id"].as_str());
        assert_eq!(
            resolved.number.map(|number| number.get()),
            Some(run.checkpoint.nodes[0].attempts[0].key.attempt)
        );
    }
    let attempt = GraphTarget {
        graph: "g".into(),
        node: Some("parent".into()),
        attempt: Some("11111111111111111111111111111111/22222222222222222222222222222222".into()),
    };
    let rows = status_rows(&run, &attempt);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "attempt");
    assert_eq!(rows[0]["node_id"], "parent");

    let mut checkpoint = checkpoint("g");
    let old = checkpoint.nodes[0].attempts[0].clone();
    let mut latest = old.clone();
    latest.key.attempt += 1;
    latest.attempt_ref = None;
    checkpoint.nodes[0].attempts.push(latest);
    let rows = status_rows(&graph_snapshot(checkpoint), &graph);
    let old = rows.iter().find(|row| row["kind"] == "attempt").unwrap();
    assert_eq!(old["latest_attempt"], false);
}

#[test]
fn graph_status_keeps_admission_reclaim_visible_on_the_running_parent() {
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint("g")), &target);
    let wait = "admission: reclaiming host and project capacity or startup spacing";
    let attempt = rows
        .iter_mut()
        .find(|row| row["kind"] == "attempt")
        .unwrap();
    attempt["graph_state"] = json!("running");
    attempt["waiting_for"] = json!(wait);
    project_node_waits(&mut rows);
    assert_eq!(rows[1]["waiting_for"], wait);
    let reading = render_graph_status(&rows);
    // The node line (`NAME/NODE#N STATE MODEL`) carries the wait, and the
    // next step is the wait verb on that node.
    let parent = reading
        .lines()
        .find(|line| line.starts_with("g/parent#"))
        .unwrap();
    assert!(
        parent.contains(" running ") && parent.ends_with(wait),
        "{reading}"
    );
    assert_eq!(rows[1]["next"], "provenant wait g/parent");

    // A historical attempt's wait must not leak onto a later live parent.
    rows[1]["waiting_for"] = Value::Null;
    let attempt = rows
        .iter_mut()
        .find(|row| row["kind"] == "attempt")
        .unwrap();
    attempt["state"] = json!("terminal");
    project_node_waits(&mut rows);
    assert!(rows[1]["waiting_for"].is_null());
}

#[test]
fn graph_status_projects_unobserved_writer_as_unknown_on_attempt_and_node() {
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint("g")), &target);
    let attempt = rows
        .iter_mut()
        .find(|row| row["kind"] == "attempt")
        .unwrap();
    attempt["latest_attempt"] = json!(true);
    attempt["recorded_state"] = json!("running");
    attempt["state"] = json!("unknown");
    attempt["presence"] = json!("unknown");
    attempt["reason"] = json!(
        "the writer process state cannot be verified; run `provenant reconcile g/parent` to inspect recovery before retrying"
    );
    project_node_liveness(&mut rows);

    let node = rows.iter().find(|row| row["kind"] == "node").unwrap();
    assert_eq!(node["recorded_state"], "running");
    assert_eq!(node["state"], "unknown");
    assert_eq!(node["presence"], "unknown");
    assert!(
        node["next"]
            .as_str()
            .unwrap()
            .contains("provenant reconcile")
    );
    let reading = render_graph_status(&rows);
    let parent = reading
        .lines()
        .find(|line| line.starts_with("g/parent#"))
        .unwrap();
    assert!(parent.contains(" unknown "), "{reading}");
    assert!(!parent.contains(" running "), "{reading}");
}

#[test]
fn remote_status_failure_downgrades_the_current_running_node_to_unknown() {
    // Spec: docs/specs/cli.md#status-selection
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let mut rows = status_rows(&graph_snapshot(checkpoint("g")), &target);
    let attempt = rows
        .iter_mut()
        .find(|row| row["kind"] == "attempt")
        .unwrap();
    attempt["latest_attempt"] = json!(true);
    attempt["state"] = json!("running");
    attempt["evidence_error"] = json!("peer status timed out");

    project_remote_attempt_unknown(attempt);
    project_node_liveness(&mut rows);

    let attempt = rows.iter().find(|row| row["kind"] == "attempt").unwrap();
    assert_eq!(attempt["recorded_state"], "running");
    assert_eq!(attempt["state"], "unknown");
    assert_eq!(attempt["presence"], "unknown");
    assert_eq!(attempt["evidence_error"], "peer status timed out");
    let node = rows.iter().find(|row| row["kind"] == "node").unwrap();
    assert_eq!(node["recorded_state"], "running");
    assert_eq!(node["state"], "unknown");
    assert_eq!(node["presence"], "unknown");
    let reading = render_graph_status(&rows);
    let parent = reading
        .lines()
        .find(|line| line.starts_with("g/parent#"))
        .unwrap();
    assert!(parent.contains(" unknown "), "{reading}");
    assert!(!parent.contains(" running "), "{reading}");
}

#[test]
fn graph_status_full_reading_keeps_the_reason_and_compact_bounds_it() {
    let mut checkpoint = checkpoint("g");
    checkpoint.nodes[0].state = NodeState::Failed;
    checkpoint.nodes[0].advance_error_code = Some("AUTH_REQUIRED".into());
    // Far past the old 1024-character row cap: a row builder must keep the
    // reason whole so --full can print it, and only the compact renderer clips.
    let long_reason = format!("credentials unavailable {}", "界".repeat(4096));
    checkpoint.nodes[0].advance_error = Some(long_reason.clone());
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let rows = status_rows(&graph_snapshot(checkpoint), &target);
    assert_eq!(
        rows[1]["reason"], long_reason,
        "the row builder clipped the reason before the render mode was known"
    );
    // One graph row, then many node and attempt rows.
    let mut many = rows.clone();
    for _ in 0..100 {
        many.extend(rows.iter().filter(|row| row["kind"] != "graph").cloned());
    }
    let reading = render_graph_status(&many);
    // The row count stays bounded: the graph line, at most LISTING_ROWS node
    // lines and the omission count. Line *length*, however, is the renderer's
    // choice: --full keeps the whole reason, so no per-line byte cap belongs
    // in the row builder.
    use provenant_cli::limits::{LISTING_ROWS, SUMMARY_ROW_BYTES};
    let lines: Vec<_> = reading.lines().collect();
    let omissions = lines
        .iter()
        .filter(|line| line.contains(" nodes not shown;"))
        .count();
    assert!(lines.len() <= 1 + LISTING_ROWS + omissions, "{reading}");
    assert!(
        reading.contains(&long_reason),
        "the full reading clipped the failure reason: {reading}"
    );
    // Failed work is listed first, after the graph line.
    let first_node = reading.lines().nth(1).unwrap();
    assert!(first_node.starts_with("g/parent"), "{reading}");
    assert!(first_node.contains(" failed "), "{reading}");
    assert!(first_node.contains("AUTH_REQUIRED"), "{reading}");

    // The default view bounds the same rows and shows the clip marker.
    let summary = status_summary(&many);
    let result = provenant_records::CommandResult::new(
        "status",
        provenant_records::Outcome::Ok,
        json!({"rows": many}),
    );
    let mut out = Vec::new();
    provenant_cli::render(
        &result,
        provenant_cli::RenderMode::Compact {
            text: &reading,
            read_only: true,
            summary: Some(&summary),
        },
        &mut out,
    )
    .expect("compact render");
    let compact = String::from_utf8(out).expect("utf8");
    assert!(
        compact
            .lines()
            .skip(1)
            .all(|line| line.len() <= SUMMARY_ROW_BYTES),
        "a compact row exceeded the bound: {compact}"
    );
    assert!(compact.contains("..."), "no clip marker: {compact}");

    // --full renders the same rows through the human mode, which passes the
    // owner's text through verbatim: the reason must survive that boundary.
    let mut full_out = Vec::new();
    provenant_cli::render(
        &result,
        provenant_cli::RenderMode::Human {
            text: &reading,
            read_only: true,
            summary: Some(&summary),
        },
        &mut full_out,
    )
    .expect("full render");
    let full = String::from_utf8(full_out).expect("utf8");
    assert!(
        full.contains(&long_reason),
        "the full render clipped the failure reason: {full}"
    );

    let step = graph_status_next(&rows).expect("a failed graph offers a next step");
    assert_eq!(step.argv, ["provenant", "retry", "g/parent"]);
    // Per-state omission counts are checked at the CLI boundary; do not
    // pin the human omission sentence here.
    assert_eq!(rows[1]["cause_code"], "AUTH_REQUIRED");
    assert_eq!(rows[1]["next"], "provenant retry g/parent");
}

#[test]
fn graph_failure_rows_prefer_owner_code_and_fall_back_for_old_checkpoints() {
    let mut checkpoint = checkpoint("g");
    checkpoint.nodes[0].state = NodeState::Blocked;
    checkpoint.nodes[0].advance_error_code = Some("launch_refused".into());
    checkpoint.nodes[0].advance_owner_code = Some("invalid_input".into());
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let rows = status_rows(&graph_snapshot(checkpoint.clone()), &target);
    assert_eq!(rows[1]["cause_code"], "invalid_input");
    let mut old = serde_json::to_value(checkpoint).unwrap();
    old["nodes"][0]
        .as_object_mut()
        .unwrap()
        .remove("advance_owner_code");
    let old = serde_json::from_value(old).unwrap();
    let rows = status_rows(&graph_snapshot(old), &target);
    assert_eq!(rows[1]["cause_code"], "launch_refused");
}

#[test]
fn graph_failure_rows_preserve_the_owner_cause_and_action() {
    let mut checkpoint = checkpoint("g");
    checkpoint.nodes[0].state = NodeState::Blocked;
    checkpoint.nodes[0].advance_error_code = Some("launch_retry_exhausted".into());
    checkpoint.nodes[0].advance_owner_code = Some("LAUNCH_UNKNOWN".into());
    checkpoint.nodes[0].advance_error = Some("[LAUNCH_UNKNOWN] inspect the receipt for operation op-123 before retrying with the same operation ID".into());
    let target = GraphTarget {
        graph: "g".into(),
        node: None,
        attempt: None,
    };
    let rows = status_rows(&graph_snapshot(checkpoint), &target);
    let node = &rows[1];
    assert_eq!(node["cause_code"], "LAUNCH_UNKNOWN");
    assert!(node["next"].as_str().unwrap().contains("op-123"));
    assert!(
        !node["next"]
            .as_str()
            .unwrap()
            .starts_with("provenant retry")
    );
    let reply = with_graph_failures(
        provenant_cli::run::Reply {
            result: provenant_records::CommandResult::new(
                "output",
                provenant_records::Outcome::Ok,
                json!({"outputs":[]}),
            ),
            human: String::new(),
            summary: Some(provenant_cli::Summary::verbatim("attempt output")),
            read_only: true,
        },
        std::slice::from_ref(node),
    );
    assert_eq!(
        reply.result.data["graph_failures"][0]["reason"],
        node["reason"]
    );
    assert!(reply.human.contains("op-123"));
    assert!(
        reply
            .summary
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("op-123"))
    );

    for (cause, code, reason) in [
        (
            json!("command_unavailable"),
            "command_unavailable",
            "configure the provider command",
        ),
        (
            json!({"provider_capture_defect":{"line":52}}),
            "provider_capture_defect",
            "provider capture evidence defect at stdout.log line 52; replay incomplete",
        ),
    ] {
        let mut rows = vec![
            json!({"kind":"node", "graph_id":"g", "node_id":"a", "state":"failed", "reason":null, "cause_code":"adapter_unavailable"}),
            json!({"kind":"attempt", "graph_id":"g", "node_id":"a", "graph_state":"terminal", "latest_attempt":true, "terminal_cause":cause, "reason":reason}),
        ];
        project_attempt_failure(&mut rows[1]);
        assert_eq!(rows[1]["cause_code"], code);
        assert_eq!(rows[1]["next"], reason);
        project_node_failures(&mut rows);
        assert_eq!(rows[0]["cause_code"], code);
        assert_eq!(rows[0]["reason"], reason);
        assert_eq!(rows[0]["next"], reason);
        let status = render_graph_status(&rows[..1]);
        assert!(status.contains(reason), "{status}");
        assert!(status.contains(code), "{status}");

        // An older terminal attempt must not supply the cause for a newer
        // attempt whose state has not settled or whose record was collected.
        rows[0]["reason"] = Value::Null;
        rows[0]["cause_code"] = json!("adapter_unavailable");
        rows[1]["latest_attempt"] = json!(false);
        rows.push(json!({"kind":"attempt", "graph_id":"g", "node_id":"a", "graph_state":"starting", "latest_attempt":true, "reason":null}));
        project_node_failures(&mut rows);
        assert!(rows[0]["reason"].is_null());
        assert_eq!(rows[0]["cause_code"], "adapter_unavailable");
    }
}

#[test]
// Spec: docs/specs/cli.md#results
fn launch_refusal_propagates_cause_and_retry_to_node() {
    let mut rows = vec![
        json!({"kind":"node", "graph_id":"g", "node_id":"a", "state":"failed", "reason":null}),
        json!({"kind":"attempt", "id":"g/a#1", "graph_id":"g", "node_id":"a", "graph_state":"terminal", "latest_attempt":true, "terminal_cause":"handoff_refused", "reason":"repository lock busy"}),
    ];
    project_attempt_failure(&mut rows[1]);
    project_node_failures(&mut rows);
    assert_eq!(rows[0]["reason"], "repository lock busy");
    assert_eq!(rows[0]["next"], "provenant retry g/a#1");
}

#[test]
fn recovered_attempt_failure_does_not_recommend_retrying_successful_work() {
    let rows = vec![
        json!({"kind":"graph","id":"g","state":"complete","outcome":"succeeded","next":"provenant output g"}),
        json!({"kind":"node","id":"g/a","state":"succeeded","next":"provenant output g/a"}),
        json!({"kind":"attempt","id":"old","state":"terminal","outcome":"task_failed","next":"provenant retry g/a"}),
    ];
    let reading = render_graph_status(&rows);
    assert!(reading.starts_with("g complete"), "{reading}");
    let step = graph_status_next(&rows).expect("a settled graph offers a next step");
    assert_eq!(step.argv, ["provenant", "output", "g"]);
}

#[test]
fn status_step_why_names_the_state_behind_every_projected_command() {
    // The next command is only useful when its sentence names the state
    // that selected it. Every arm of status_step_why a projection can reach
    // is pinned here, so a new row state cannot silently inherit the
    // generic fallback sentence.
    use provenant_cli::next_steps_text;
    use provenant_workflow::session::CommandPrefix;

    for (row, why, command) in [
        (
            json!({"kind":"node","id":"g/parent","state":"failed","outcome":null,"next":"provenant retry g/parent"}),
            "the graph failed",
            "provenant retry g/parent",
        ),
        (
            json!({"kind":"node","id":"g/parent","state":"blocked","outcome":null,"next":"provenant retry g/parent"}),
            "the graph failed",
            "provenant retry g/parent",
        ),
        (
            json!({"kind":"graph","id":"g","state":"paused","outcome":null,"next":"provenant retry g"}),
            "the graph needs input",
            "provenant retry g",
        ),
        (
            json!({"kind":"graph","id":"g","state":"cancelled","outcome":null,"next":"provenant output g"}),
            "the graph was cancelled",
            "provenant output g",
        ),
        (
            json!({"kind":"graph","id":"g","state":"complete","outcome":"succeeded","next":"provenant output g"}),
            "the graph is complete",
            "provenant output g",
        ),
        (
            json!({"kind":"graph","id":"g","state":"complete","outcome":"failed","next":"provenant retry g"}),
            "the graph did not succeed",
            "provenant retry g",
        ),
        (
            json!({"kind":"node","id":"g/parent","state":"running","outcome":null,"next":"provenant wait g/parent"}),
            "the graph is still running",
            "provenant wait g/parent",
        ),
        (
            json!({"kind":"node","id":"g/parent","state":"skipped","outcome":null,"next":"provenant output g/parent"}),
            "the graph needs attention",
            "provenant output g/parent",
        ),
    ] {
        let step = graph_status_next(std::slice::from_ref(&row))
            .expect("each projected state offers one next step");
        assert_eq!(
            step.argv,
            command.split_whitespace().collect::<Vec<_>>(),
            "{row}"
        );
        assert_eq!(step.why, why, "{row}");
        assert_eq!(
            next_steps_text(&[step], &CommandPrefix::default()),
            format!("next: {why}; {command}\n"),
            "{row}"
        );
    }
}

#[test]
fn subtree_stop_preserves_detached_children_and_unrelated_nodes()
-> Result<(), Box<dyn std::error::Error>> {
    if super::super::peer_tests::host_test(
        "fabric::unified::tests::subtree_stop_preserves_detached_children_and_unrelated_nodes",
    )? {
        return Ok(());
    }
    struct FreeWorkers;
    impl WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|path| (path, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    let rig = crate::worktree_tests::support::TestRig::new()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
    let project = context.identity.project;
    let factory = Factory {
        context,
        project_attribution: false,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &FreeWorkers,
    };
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut journal =
        FileJournal::open(&roots.state, project, "g", budget, &cancel, &factory.clock)?;
    journal.bind(GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: rig.project.clone(),
        document: document("g"),
    })?;
    let mut before = checkpoint("g");
    before.nodes[0].attempts.clear();
    journal.prepare_fixture_checkpoint(&mut before)?;
    journal.save(&before)?;
    drop(journal);
    let events = FileJournal::read_events(&roots.state, project, "g")?;
    let stopped = factory.stop_target(
        &GraphTarget {
            graph: "g".into(),
            node: Some("parent".into()),
            attempt: None,
        },
        None,
        budget,
        &cancel,
    )?;
    assert_eq!(stopped.checkpoint.nodes[0].state, NodeState::Skipped);
    assert_eq!(stopped.checkpoint.nodes[1].state, NodeState::Skipped);
    assert_eq!(stopped.checkpoint.nodes[2], before.nodes[2]);
    assert_eq!(stopped.checkpoint.nodes[3], before.nodes[3]);
    let mut durable = stopped.checkpoint.clone();
    durable.cancellation_request_receipt = None;
    assert_eq!(
        FileJournal::read_checkpoint(&roots.state, project, "g")?,
        Some(durable)
    );
    assert_ne!(
        FileJournal::read_events(&roots.state, project, "g")?,
        events
    );
    Ok(())
}

#[test]
fn pending_attempt_status_and_numbered_stop_keep_one_launch()
-> Result<(), Box<dyn std::error::Error>> {
    if super::super::peer_tests::host_test(
        "fabric::unified::tests::pending_attempt_status_and_numbered_stop_keep_one_launch",
    )? {
        return Ok(());
    }
    struct FreeWorkers;
    impl WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|path| (path, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    // Medium: real host journal and admitted run store, with only provider
    // launch deferred. The `NAME/NODE#1` target controls that run.
    {
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots.clone(), &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock::new(),
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let input = json!({
            "op":"run", "brief":"Inspect the parser", "name":"stop-by-attempt",
            "document":{"use":"task", "routes":[
                {"adapter":"fixture", "model":"fixture/first"},
                {"adapter":"fixture", "model":"fixture/fallback"}
            ]}
        });
        let run = provenant_cli::run::with_deferred_provider_launch(|| {
            factory.graph_document(input.clone(), budget, &cancel)
        })?;
        assert_eq!(run["status"], "pending");
        assert_eq!(run["timed_out"], false, "submission did not request a wait");
        let graph = run["checkpoint"]["graph_id"]
            .as_str()
            .ok_or("graph ID missing")?;
        let reference = run["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
            .as_str()
            .ok_or("accepted attempt missing")?;
        let id = reference.rsplit('/').next().ok_or("attempt ID missing")?;
        let node_id = run["checkpoint"]["nodes"][0]["id"]
            .as_str()
            .ok_or("node ID missing")?;
        let selector = format!("{graph}/{node_id}#1");
        for selected in [graph, selector.as_str()] {
            let status =
                factory.graph_document(json!({"op":"status", "id":selected}), budget, &cancel)?;
            assert_eq!(status["timed_out"], false, "status did not request a wait");
            assert!(
                status["rows"]
                    .as_array()
                    .ok_or("status rows missing")?
                    .iter()
                    .any(|row| row["kind"] == "attempt" && row["attempt_id"] == reference)
            );
        }
        let stopped = factory.graph_document(
            json!({"op":"stop", "id":selector, "reason":"stop the task"}),
            budget,
            &cancel,
        )?;
        assert_eq!(stopped["timed_out"], false);
        let checkpoint =
            FileJournal::read_checkpoint(&roots.state, factory.context.identity.project, graph)?
                .ok_or("stopped checkpoint missing")?;
        let node = checkpoint.nodes.first().ok_or("stopped node missing")?;
        assert_eq!(node.state, NodeState::Skipped);
        assert_eq!(node.subtree_cancel.as_deref(), Some(node.id.as_str()));
        assert_eq!(node.attempts.len(), 1);
        assert_eq!(node.attempts[0].attempt_ref.as_deref(), Some(reference));
        let owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store(&cancel)?;
        let record = store.record(id.parse()?, &owner.call(&cancel))?;
        assert!(matches!(
            record.state,
            provenant_run::AttemptState::Terminal {
                outcome: provenant_run::Outcome::Cancelled,
                ..
            }
        ));
        // Replaying the named launch returns the stopped graph and starts
        // nothing: its unused second route stays unused after the slot is
        // freed.
        let replayed = provenant_cli::run::with_deferred_provider_launch(|| {
            factory.graph_document(input, budget, &cancel)
        })?;
        let after: GraphCheckpoint = serde_json::from_value(replayed["checkpoint"].clone())?;
        assert_eq!(after.nodes[0].state, NodeState::Skipped);
        assert_eq!(after.nodes[0].attempts, node.attempts);
        let events =
            FileJournal::read_events(&roots.state, factory.context.identity.project, graph)?;
        // A reservation and its later saved reference both emit `started`.
        // Count route identities, rather than observations of the same one.
        let started: std::collections::BTreeSet<_> = events
            .iter()
            .filter(|event| {
                event.kind == provenant_fabric_runtime::graph::GraphEventKind::Attempt
                    && event.code == "started"
            })
            .map(|event| {
                (
                    event.subject.as_str(),
                    event
                        .details
                        .as_ref()
                        .and_then(|details| details["route_index"].as_u64()),
                )
            })
            .collect();
        assert_eq!(
            started,
            std::collections::BTreeSet::from([(node.id.as_str(), Some(0))])
        );
    }
    Ok(())
}

#[test]
fn a_numbered_target_reaches_its_recorded_attempt_through_the_journal()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
    inputs.state_dir = Some(base.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    let project = provenant_core::ProjectId::from_bytes([2; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    for graph in ["g", "h"] {
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let mut journal = FileJournal::open(&roots.state, project, graph, budget, &cancel, &clock)?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: base.clone(),
            document: document(graph),
        })?;
        let mut checkpoint = checkpoint(graph);
        if graph == "h" {
            checkpoint.nodes[0].attempts.clear();
        }
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
        journal.save(&checkpoint)?;
    }
    let target = |text: &str| text.parse::<provenant_cli::Target>();
    let g = "11111111111111111111111111111111";
    let a = "22222222222222222222222222222222";
    // `NAME/NODE#1` reaches the one recorded attempt through the journal,
    // and steering reaches the same attempt while it is live.
    let attempt = resolve(
        &roots.state,
        project,
        &target("g/parent#1")?,
        Want::Attempt { settled: false },
    )?;
    assert_eq!(
        attempt.attempt_ref.as_deref(),
        Some(format!("{g}/{a}").as_str())
    );
    let checkpoint =
        FileJournal::read_status_checkpoint(&roots.state, project, "g")?.ok_or("checkpoint")?;
    assert_eq!(
        provenant_cli::inbox::live_steer_reference(&checkpoint, &attempt),
        Ok(format!("{g}/{a}").as_str())
    );
    // A node with no recorded attempt has no `#1`, and an unknown graph
    // is not found.
    assert!(matches!(
        resolve(&roots.state, project, &target("h/parent#1")?, Want::Work),
        Err(ResolveError::NotFound { .. })
    ));
    assert!(matches!(
        resolve(&roots.state, project, &target("missing")?, Want::Work),
        Err(ResolveError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn lane_not_found_is_an_invalid_target_result() {
    let error = ServiceError::Facade(FacadeError::NotFound);
    let reply = output_error(&error);

    assert_eq!(reply.result.status, provenant_records::Outcome::Invalid);
    assert_eq!(
        reply.result.error.as_ref().map(|error| error.code.as_ref()),
        Some("NOT_FOUND")
    );
    assert_eq!(provenant_cli::exit_status(&reply.result.status), 2);
}

#[test]
fn compact_output_never_dumps_raw_json_braces_and_status_retains_failed_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let mut compact_cases = Vec::new();

    // Case A: templates (list)
    let templates_data = json!({
        "templates": [
            {
                "name": "task",
                "layer": "bundled",
                "template": {
                    "doc": "Do the brief.",
                    "brief": "Do the brief and report what changed.",
                    "tier": "senior"
                }
            },
            {
                "name": "repair",
                "layer": "bundled",
                "template": {
                    "doc": "Repair qualified findings.",
                    "nodes": [{"id": "fix", "use": "implement.fix"}]
                }
            }
        ]
    });
    let templates_sum = crate::graph_interface::templates_summary(&templates_data, None).unwrap();
    let cmd_res =
        provenant_records::CommandResult::new("templates", provenant_records::Outcome::Ok, ());
    let mut out = Vec::new();
    provenant_cli::render(
        &cmd_res,
        provenant_cli::RenderMode::Compact {
            text: &serde_json::to_string_pretty(&templates_data)?,
            read_only: true,
            summary: Some(&templates_sum),
        },
        &mut out,
    )?;
    let text = String::from_utf8(out)?;
    compact_cases.push(("templates", text.clone()));
    assert!(text.contains("bundled task: Do the brief."));
    assert!(text.contains("bundled repair: Repair qualified findings."));

    // Case B: templates show
    let templates_data_with_params = json!({
        "templates": [
            {
                "name": "repair",
                "layer": "bundled",
                "template": {
                    "doc": "Repair qualified findings.",
                    "nodes": [{"id": "fix", "use": "implement.fix"}],
                    "parameters": {
                        "branch": {"default": "main"},
                        "retries": 3,
                        "review_base": {"type": "string"}
                    }
                }
            }
        ]
    });
    let show_sum =
        crate::graph_interface::templates_summary(&templates_data_with_params, Some("repair"))
            .unwrap();
    let mut out = Vec::new();
    provenant_cli::render(
        &cmd_res,
        provenant_cli::RenderMode::Compact {
            text: &serde_json::to_string_pretty(&templates_data_with_params)?,
            read_only: true,
            summary: Some(&show_sum),
        },
        &mut out,
    )?;
    let text = String::from_utf8(out)?;
    compact_cases.push(("templates show", text.clone()));
    assert!(text.contains("repair (bundled)"));
    assert!(text.contains("nodes: fix (implement.fix)"));
    assert!(text.contains("branch=\"main\""));
    assert!(text.contains("retries=3"));
    assert!(text.contains("review_base (no default)"));

    // Case C: status
    let status_rows = vec![
        json!({"id": "g/a", "kind": "node", "state": "failed", "cause_code": "timeout", "reason": "timed out"}),
        json!({"id": "g/b", "kind": "node", "state": "running"}),
    ];
    let stat_sum = status_summary(&status_rows);
    let stat_res = provenant_records::CommandResult::new(
        "status",
        provenant_records::Outcome::Ok,
        json!({"rows": status_rows}),
    );
    let mut out = Vec::new();
    provenant_cli::render(
        &stat_res,
        provenant_cli::RenderMode::Compact {
            text: "g/a failed\ng/b running\n",
            read_only: true,
            summary: Some(&stat_sum),
        },
        &mut out,
    )?;
    let text = String::from_utf8(out)?;
    compact_cases.push(("status", text.clone()));
    assert!(text.contains("1 failed, 1 running"));
    assert!(text.contains("g/a failed"));

    // Case D: result
    let target: provenant_cli::Target = "g/parent".parse()?;
    let res_rows = vec![json!({
        "id": "g/parent",
        "kind": "node",
        "state": "failed",
        "outcome": "failed",
        "cause_code": "budget_exhausted",
        "review_verdict": "VERDICT: FAIL",
        "result_path": provenant_records::StructuredPathWire::from_path(std::path::Path::new("/tmp/error.txt"))?,
        "branch": "feat/fix",
        "head": "def456",
        "handoff": null
    })];
    let reply = result_reply(&target, &res_rows);
    let mut out = Vec::new();
    provenant_cli::render(
        &reply.result,
        provenant_cli::Globals::default().render_mode(
            &reply.human,
            reply.read_only,
            reply.summary.as_ref(),
        ),
        &mut out,
    )?;
    let text = String::from_utf8(out)?;
    compact_cases.push(("result", text.clone()));
    assert!(text.contains("g/parent: failed"));
    assert!(text.contains("cause: budget_exhausted"));
    assert!(text.contains("verdict: VERDICT: FAIL"));
    assert!(text.contains("result: /tmp/error.txt"));

    // Case E: listing (status across graphs)
    let listing = Listing {
        rows: vec![
            ListedGraph {
                name: "g-fail".to_string(),
                state: "failed".to_string(),
                template: Some("task".to_string()),
                nodes: 2,
                settled: false,
                created_micros: None,
                changed_micros: None,
                reason: Some("worker timed out".to_string()),
                digests: vec![],
                open_asks: vec![],
            },
            ListedGraph {
                name: "g-run".to_string(),
                state: "running".to_string(),
                template: Some("repair".to_string()),
                nodes: 1,
                settled: false,
                created_micros: None,
                changed_micros: None,
                reason: None,
                digests: vec![],
                open_asks: vec![],
            },
        ],
        next: None,
        omitted: Default::default(),
        admission_holders: Vec::new(),
        admission_waiters: Vec::new(),
        stranded_work: Vec::new(),
    };
    let list_sum = listing_summary(&listing, 50, 0);
    let list_res =
        provenant_records::CommandResult::new("status", provenant_records::Outcome::Ok, ());
    let mut out = Vec::new();
    provenant_cli::render(
        &list_res,
        provenant_cli::RenderMode::Compact {
            text: "g-fail failed\ng-run running\n",
            read_only: true,
            summary: Some(&list_sum),
        },
        &mut out,
    )?;
    let text = String::from_utf8(out)?;
    compact_cases.push(("status listing", text.clone()));
    assert!(text.contains("1 failed, 1 running"));
    assert!(text.contains("g-fail failed task 2: worker timed out"));

    for (command, output) in compact_cases {
        assert!(
            !output.contains('{') && !output.contains('}'),
            "{command}: {output}"
        );
    }

    // Fixture with 150 rows includes failures, attention and blocked rows.
    let mut large_rows = Vec::new();
    for i in 0..150 {
        let state = if i == 10 || i == 50 || i == 120 {
            "failed"
        } else if i == 140 {
            "attention"
        } else if i == 149 {
            "blocked"
        } else {
            "succeeded"
        };
        large_rows.push(json!({
            "id": format!("g/node_{i}"),
            "kind": "node",
            "state": state,
            "reason": (state == "failed").then(|| format!("failure at {i}")),
            "cause_code": (state == "failed").then_some("crash"),
            "outcome": (state == "succeeded").then_some("succeeded"),
        }));
    }
    let large_sum = status_summary(&large_rows);
    assert_eq!(large_sum.omitted, 145);
    let large_res = provenant_records::CommandResult::new(
        "status",
        provenant_records::Outcome::Ok,
        json!({"rows": large_rows}),
    );
    let mut out = Vec::new();
    provenant_cli::render(
        &large_res,
        provenant_cli::RenderMode::Compact {
            text: "large text...",
            read_only: true,
            summary: Some(&large_sum),
        },
        &mut out,
    )?;
    let text = String::from_utf8(out)?;
    assert!(!text.contains('{') && !text.contains('}'));
    assert!(
        text.contains("3 failed, 1 blocked, 1 attention, 145 succeeded"),
        "headline: {text}"
    );
    assert!(text.contains("g/node_10 failed"), "node 10 missing: {text}");
    assert!(text.contains("g/node_50 failed"), "node 50 missing: {text}");
    assert!(
        text.contains("g/node_120 failed"),
        "node 120 missing: {text}"
    );
    assert!(
        text.contains("145 more not shown; use --full for all detail"),
        "{text}"
    );

    assert!(text.contains("g/node_140 attention"), "{text}");
    assert!(text.contains("g/node_149 blocked"), "{text}");

    // --json output is byte-identical for the same fixture before and after
    let mut json_out = Vec::new();
    provenant_cli::render(&large_res, provenant_cli::RenderMode::Json, &mut json_out)?;
    let mut direct_json = serde_json::to_vec(&large_res)?;
    direct_json.push(b'\n');
    assert_eq!(json_out, direct_json);

    Ok(())
}

#[test]
fn state_ranking_and_predicates_behave_consistently() {
    // Ranking order: failed (0) < blocked (1) < attention (2) < live (3) < done (4) < other (5)
    assert_eq!(StateRank::of("failed"), StateRank::Failed);
    assert_eq!(StateRank::of("unreadable"), StateRank::Failed);
    assert_eq!(StateRank::of("unresolved"), StateRank::Failed);
    assert_eq!(StateRank::of("blocked"), StateRank::Blocked);
    assert_eq!(StateRank::of("paused"), StateRank::Attention);
    assert_eq!(StateRank::of("input_required"), StateRank::Attention);
    assert_eq!(StateRank::of("refused"), StateRank::Attention);
    assert_eq!(StateRank::of("attention"), StateRank::Attention);
    assert_eq!(StateRank::of("running"), StateRank::Live);
    assert_eq!(StateRank::of("starting"), StateRank::Live);
    assert_eq!(StateRank::of("pending"), StateRank::Live);
    assert_eq!(StateRank::of("launched"), StateRank::Live);
    assert_eq!(StateRank::of("succeeded"), StateRank::Done);
    assert_eq!(StateRank::of("complete"), StateRank::Done);
    assert_eq!(StateRank::of("custom_state"), StateRank::Other);

    assert!(StateRank::Failed < StateRank::Blocked);
    assert!(StateRank::Blocked < StateRank::Attention);
    assert!(StateRank::Attention < StateRank::Live);
    assert!(StateRank::Live < StateRank::Done);
    assert!(StateRank::Done < StateRank::Other);

    // Predicates
    for state in [
        "failed",
        "unreadable",
        "unresolved",
        "blocked",
        "paused",
        "input_required",
        "refused",
        "attention",
    ] {
        assert!(
            StateRank::of(state).needs_attention(),
            "expected needs_attention for {state}"
        );
        assert!(
            !StateRank::of(state).is_live(),
            "expected not is_live for {state}"
        );
    }

    for state in ["running", "starting", "pending", "launched"] {
        assert!(
            !StateRank::of(state).needs_attention(),
            "expected not needs_attention for {state}"
        );
        assert!(
            StateRank::of(state).is_live(),
            "expected is_live for {state}"
        );
    }

    for state in ["succeeded", "complete", "unknown"] {
        assert!(!StateRank::of(state).needs_attention());
        assert!(!StateRank::of(state).is_live());
    }
}
