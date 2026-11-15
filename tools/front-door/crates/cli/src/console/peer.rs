//! Composition of bounded, host-owned board observations.
use provenant_core::{HostId, ProjectId};
use provenant_records::WorkKey;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The result of a peer read, supplied by the transport adapter.
#[derive(Debug)]
pub enum BoardObservation {
    Page(Value),
    Unreachable,
    Unsupported,
}

/// The configured host is authoritative for the identities in its reply.
#[derive(Debug)]
pub struct PeerBoard {
    pub host: HostId,
    pub observation: BoardObservation,
}

/// Compose peer pages without changing the local page or its cursor.
/// Peer pages are bounded to 100 rows. Invalid and duplicate rows are counted
/// in coverage, and a peer continuation is evidence of incomplete coverage.
pub fn compose_board(mut local: Value, peers: Vec<PeerBoard>) -> Result<Value, &'static str> {
    let mut seen = BTreeSet::new();
    for row in local["lanes"].as_array().ok_or("invalid local board")? {
        seen.insert(identity(row).ok_or("invalid local lane identity")?);
    }
    let mut rows = Vec::new();
    let mut coverage = Vec::new();
    let mut hosts = BTreeSet::new();
    for peer in peers {
        let host = peer.host;
        if !hosts.insert(host) {
            coverage.push(json!({"host":host,"state":"partial","reason":"duplicate_host"}));
            continue;
        }
        let page = match peer.observation {
            BoardObservation::Unreachable => {
                coverage.push(json!({"host":host,"state":"unreachable"}));
                continue;
            }
            BoardObservation::Unsupported => {
                coverage.push(json!({"host":host,"state":"unsupported"}));
                continue;
            }
            BoardObservation::Page(page) => page,
        };
        let Some(lanes) = page["lanes"].as_array() else {
            coverage.push(json!({"host":host,"state":"partial","reason":"invalid_page"}));
            continue;
        };
        let (mut invalid, mut duplicates, mut included) = (0usize, 0usize, 0usize);
        for row in lanes.iter().take(100) {
            let Some(key) = identity(row) else {
                invalid += 1;
                continue;
            };
            if key.0 != Some(host) {
                invalid += 1;
            } else if !seen.insert(key) {
                duplicates += 1;
            } else {
                rows.push(row.clone());
                included += 1;
            }
        }
        let truncated =
            lanes.len() > 100 || page["truncated"] != false || !page["next_cursor"].is_null();
        let source_incomplete = !matches!(
            page["coverage"]["runs"].as_str(),
            Some("complete" | "empty")
        ) || page["coverage"]["chairs"] != "complete"
            || page["coverage"]["observations"] != "complete"
            || page["coverage"]["host"] != json!(host);
        let incomplete = truncated || invalid != 0 || duplicates != 0 || source_incomplete;
        coverage.push(
            json!({"host":host,"state":if incomplete { "partial" } else { "complete" },
            "included":included,"invalid_rows":invalid,"duplicate_rows":duplicates,
            "truncated":truncated,"source_incomplete":source_incomplete,"next_cursor":page["next_cursor"]}),
        );
    }
    local["lanes"]
        .as_array_mut()
        .ok_or("invalid local board")?
        .extend(rows);
    local["coverage"]["peers"] = json!(coverage);
    Ok(local)
}

fn identity(row: &Value) -> Option<(Option<HostId>, ProjectId, WorkKey)> {
    let host = match &row["lane_id"]["host"] {
        Value::Null => None,
        Value::String(host) => Some(HostId::parse(host).ok()?),
        _ => return None,
    };
    let key: WorkKey = serde_json::from_value(row["lane_id"]["key"].clone()).ok()?;
    if let Some(project) = row["metadata"]["attempt"]["project_id"].as_str() {
        (ProjectId::parse(project).ok()? == key.project()).then_some((host, key.project(), key))
    } else {
        Some((host, key.project(), key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, AttemptId, HumanRequestId};
    use serde_json::json;

    #[derive(Clone, Copy)]
    enum Surface {
        Board,
        Inbox,
        Tree,
    }

    #[derive(Clone, Copy)]
    enum Observation {
        Complete,
        Truncated,
        Unreachable,
        Unsupported,
        InvalidOwner,
        MissingKey,
        DuplicateRow,
        DuplicateHost,
    }

    struct Expectation {
        state: &'static str,
        included: usize,
        invalid: usize,
        duplicates: usize,
        truncated: bool,
        reason: Option<&'static str>,
    }

    const OBSERVATIONS: &[(Observation, Expectation)] = &[
        (
            Observation::Complete,
            Expectation {
                state: "complete",
                included: 1,
                invalid: 0,
                duplicates: 0,
                truncated: false,
                reason: None,
            },
        ),
        (
            Observation::Truncated,
            Expectation {
                state: "partial",
                included: 1,
                invalid: 0,
                duplicates: 0,
                truncated: true,
                reason: None,
            },
        ),
        (
            Observation::Unreachable,
            Expectation {
                state: "unreachable",
                included: 0,
                invalid: 0,
                duplicates: 0,
                truncated: false,
                reason: None,
            },
        ),
        (
            Observation::Unsupported,
            Expectation {
                state: "unsupported",
                included: 0,
                invalid: 0,
                duplicates: 0,
                truncated: false,
                reason: None,
            },
        ),
        (
            Observation::InvalidOwner,
            Expectation {
                state: "partial",
                included: 0,
                invalid: 1,
                duplicates: 0,
                truncated: false,
                reason: None,
            },
        ),
        (
            Observation::MissingKey,
            Expectation {
                state: "partial",
                included: 0,
                invalid: 1,
                duplicates: 0,
                truncated: false,
                reason: None,
            },
        ),
        (
            Observation::DuplicateRow,
            Expectation {
                state: "partial",
                included: 1,
                invalid: 0,
                duplicates: 1,
                truncated: false,
                reason: None,
            },
        ),
        (
            Observation::DuplicateHost,
            Expectation {
                state: "partial",
                included: 0,
                invalid: 0,
                duplicates: 0,
                truncated: false,
                reason: Some("duplicate_host"),
            },
        ),
    ];

    fn request(host: HostId, id: u8, project_key: Option<&str>) -> Value {
        json!({
            "schema_version": 1,
            "id": HumanRequestId::from_bytes([id; 16]),
            "revision": 1,
            "operation_id": "operation",
            "created_at": "2026-01-01T00:00:00.000000Z",
            "updated_at": "2026-01-01T00:00:00.000000Z",
            "project_id": ProjectId::from_bytes([9; 16]),
            "project_key": project_key,
            "owner_host": host,
            "chair_session_id": "session",
            "chair_actor": ActorId::from_bytes([8; 16]),
            "request": { "kind": "decision", "title": "title", "body": "body" },
            "state": "open"
        })
    }

    fn peer_facts(
        surface: Surface,
        observation: Observation,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let local_host = HostId::from_bytes([1; 16]);
        let remote = HostId::from_bytes([2; 16]);
        let wrong = HostId::from_bytes([3; 16]);
        let project = ProjectId::from_bytes([6; 16]);
        let attempt = AttemptId::from_bytes([7; 16]);
        let key = WorkKey::new(project, "graph", "node", 1)?;
        let lane = |host: HostId| {
            json!({
                "lane_id": { "host": host, "project_id": project, "key": key, "attempt_id": attempt },
                "metadata": { "attempt": { "project_id": project } }
            })
        };
        let board_coverage = json!({
            "host": remote, "runs": "complete", "chairs": "complete", "observations": "complete"
        });
        let board_page = |lanes: Vec<Value>, truncated: bool| {
            json!({
                "lanes": lanes, "truncated": truncated,
                "next_cursor": if truncated { json!("peer-cursor") } else { Value::Null },
                "coverage": board_coverage
            })
        };
        let rows = match observation {
            Observation::InvalidOwner => vec![lane(wrong)],
            Observation::MissingKey => {
                let mut missing = lane(remote);
                missing["lane_id"]
                    .as_object_mut()
                    .ok_or("lane identity")?
                    .remove("key");
                vec![missing]
            }
            Observation::DuplicateRow => vec![lane(remote), lane(remote)],
            _ => vec![lane(remote)],
        };
        let coverage = match surface {
            Surface::Board => {
                let peer = |observation| PeerBoard {
                    host: remote,
                    observation,
                };
                let peers = match observation {
                    Observation::Unreachable => vec![peer(BoardObservation::Unreachable)],
                    Observation::Unsupported => vec![peer(BoardObservation::Unsupported)],
                    Observation::DuplicateHost => vec![
                        peer(BoardObservation::Unreachable),
                        peer(BoardObservation::Page(board_page(
                            vec![lane(remote)],
                            false,
                        ))),
                    ],
                    _ => vec![peer(BoardObservation::Page(board_page(
                        rows,
                        matches!(observation, Observation::Truncated),
                    )))],
                };
                let local = json!({
                    "lanes": [lane(local_host)], "next_cursor": "local-cursor", "coverage": { "scope": "local" }
                });
                let result = compose_board(local, peers)?;
                assert_eq!(result["next_cursor"], "local-cursor");
                assert_eq!(result["lanes"][0]["lane_id"]["host"], json!(local_host));
                result["coverage"]["peers"]
                    .as_array()
                    .ok_or("board peers")?
                    .last()
                    .cloned()
                    .ok_or("board peer")?
            }
            Surface::Inbox => {
                let local_item = serde_json::from_value(request(local_host, 1, Some("local")))?;
                let local = provenant_fabric_runtime::inbox::InboxResult::Listed {
                    extra: Default::default(),
                    items: vec![local_item],
                    next_cursor: None,
                    truncated: true,
                    coverage: "local_only".into(),
                    projects: Default::default(),
                    peers: Default::default(),
                };
                let peer = |observation| super::super::peer_inbox::PeerInbox {
                    host: remote,
                    label: "remote".into(),
                    observation,
                };
                let page = |items: Vec<Value>, truncated: bool| {
                    super::super::peer_inbox::InboxObservation::Page(json!({
                        "items": items, "truncated": truncated, "projects": {}
                    }))
                };
                let peer_observations = match observation {
                    Observation::Unreachable => vec![peer(
                        super::super::peer_inbox::InboxObservation::Unreachable("timeout"),
                    )],
                    Observation::Unsupported => vec![peer(
                        super::super::peer_inbox::InboxObservation::Unsupported,
                    )],
                    Observation::DuplicateHost => vec![
                        peer(super::super::peer_inbox::InboxObservation::Unreachable(
                            "timeout",
                        )),
                        peer(page(vec![request(remote, 2, Some("remote"))], false)),
                    ],
                    Observation::InvalidOwner => {
                        vec![peer(page(vec![request(wrong, 2, Some("remote"))], false))]
                    }
                    Observation::MissingKey => {
                        vec![peer(page(vec![request(remote, 2, None)], false))]
                    }
                    Observation::DuplicateRow => vec![peer(page(
                        vec![
                            request(remote, 2, Some("remote")),
                            request(remote, 2, Some("remote")),
                        ],
                        false,
                    ))],
                    Observation::Truncated => {
                        vec![peer(page(vec![request(remote, 2, Some("remote"))], true))]
                    }
                    Observation::Complete => {
                        vec![peer(page(vec![request(remote, 2, Some("remote"))], false))]
                    }
                };
                let result = super::super::peer_inbox::compose_inbox(local, peer_observations)?;
                let value = serde_json::to_value(result)?;
                assert_eq!(value["truncated"], true);
                assert_eq!(value["coverage"], "local_only");
                assert_eq!(value["items"][0]["owner_host"], json!(local_host));
                value["peers"]
                    .as_array()
                    .ok_or("inbox peers")?
                    .last()
                    .cloned()
                    .ok_or("inbox peer")?
            }
            Surface::Tree => {
                let node_rows = match observation {
                    Observation::InvalidOwner => vec![lane(wrong)],
                    Observation::MissingKey => {
                        let mut missing = lane(remote);
                        missing["lane_id"]
                            .as_object_mut()
                            .ok_or("lane identity")?
                            .remove("key");
                        vec![missing]
                    }
                    Observation::DuplicateRow => vec![lane(remote), lane(remote)],
                    _ => vec![lane(remote)],
                };
                let tree = |nodes: Vec<Value>, truncated: bool| {
                    json!({
                        "nodes": nodes, "edges": [], "truncated": truncated,
                        "coverage": { "host": remote, "runs": "complete", "nested_runs": "complete" },
                        "totals": { "attempts": 1 }, "identity_conflicts": [], "unfetched_children": [], "children_absent": []
                    })
                };
                let peer = |observation| super::super::tree_peer::PeerTree {
                    host: remote,
                    observation,
                };
                let peers = match observation {
                    Observation::Unreachable => {
                        vec![peer(super::super::tree_peer::TreeObservation::Unreachable)]
                    }
                    Observation::Unsupported => {
                        vec![peer(super::super::tree_peer::TreeObservation::Unsupported)]
                    }
                    Observation::DuplicateHost => vec![
                        peer(super::super::tree_peer::TreeObservation::Unreachable),
                        peer(super::super::tree_peer::TreeObservation::Tree(tree(
                            vec![lane(remote)],
                            false,
                        ))),
                    ],
                    _ => vec![peer(super::super::tree_peer::TreeObservation::Tree(tree(
                        node_rows,
                        matches!(observation, Observation::Truncated),
                    )))],
                };
                let local = json!({
                    "nodes": [lane(local_host)], "edges": [], "totals": { "attempts": 1 }, "coverage": { "scope": "local" }
                });
                let result = super::super::tree_peer::compose_tree(local, peers)?;
                assert_eq!(result["totals"]["attempts"], 1);
                assert_eq!(result["nodes"][0]["lane_id"]["host"], json!(local_host));
                result["coverage"]["peers"]
                    .as_array()
                    .ok_or("tree peers")?
                    .last()
                    .cloned()
                    .ok_or("tree peer")?
            }
        };
        Ok(coverage)
    }

    #[test]
    fn peer_coverage_is_consistent_across_surfaces_and_observations()
    -> Result<(), Box<dyn std::error::Error>> {
        let surfaces = [Surface::Board, Surface::Inbox, Surface::Tree];
        for surface in surfaces {
            for (observation, expected) in OBSERVATIONS {
                let facts = peer_facts(surface, *observation)?;
                assert_eq!(facts["state"], expected.state);
                assert_eq!(
                    facts["included"].as_u64().unwrap_or(0),
                    expected.included as u64
                );
                assert_eq!(
                    facts["invalid_rows"].as_u64().unwrap_or(0),
                    expected.invalid as u64
                );
                assert_eq!(
                    facts["duplicate_rows"].as_u64().unwrap_or(0),
                    expected.duplicates as u64
                );
                assert_eq!(facts["truncated"] == true, expected.truncated);
                if let Some(reason) = expected.reason {
                    assert_eq!(facts["reason"], reason);
                } else if matches!(
                    (surface, *observation),
                    (Surface::Inbox, Observation::Unreachable)
                ) {
                    assert_eq!(facts["reason"], "timeout");
                } else {
                    assert!(facts["reason"].is_null());
                }
            }
        }
        Ok(())
    }
}
