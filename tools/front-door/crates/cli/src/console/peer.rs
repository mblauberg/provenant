//! Composition of bounded, host-owned board observations.
use provenant_core::{AttemptId, HostId, ProjectId};
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

fn identity(row: &Value) -> Option<(Option<HostId>, ProjectId, AttemptId)> {
    let host = match &row["lane_id"]["host"] {
        Value::Null => None,
        Value::String(host) => Some(HostId::parse(host).ok()?),
        _ => return None,
    };
    Some((
        host,
        ProjectId::parse(row["metadata"]["attempt"]["project_id"].as_str()?).ok()?,
        AttemptId::parse(row["lane_id"]["attempt_id"].as_str()?).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_board_coverage() {
        let local_host = HostId::from_bytes([1; 16]);
        let remote = HostId::from_bytes([2; 16]);
        let partial = HostId::from_bytes([3; 16]);
        let offline = HostId::from_bytes([4; 16]);
        let old = HostId::from_bytes([5; 16]);
        let project = ProjectId::from_bytes([6; 16]);
        let attempt = AttemptId::from_bytes([7; 16]);
        let row = |host: HostId| {
            json!({"lane_id":{"host":host,"attempt_id":attempt},
            "metadata":{"attempt":{"project_id":project}}})
        };
        let page_coverage = |host| json!({"host":host,"runs":"complete","chairs":"complete","observations":"complete"});
        let local = json!({"lanes":[row(local_host)],"next_cursor":"local-cursor",
            "coverage":{"scope":"local"}});
        let duplicate = row(remote);
        let result = compose_board(local, vec![
            PeerBoard { host: remote, observation: BoardObservation::Page(json!({"lanes":[duplicate.clone(), duplicate],"truncated":false,"next_cursor":null})) },
            PeerBoard { host: partial, observation: BoardObservation::Page(json!({"lanes":[row(partial)],"truncated":true,"next_cursor":"peer-cursor"})) },
            PeerBoard { host: offline, observation: BoardObservation::Unreachable },
            PeerBoard { host: old, observation: BoardObservation::Unsupported },
        ]).unwrap();
        assert_eq!(result["lanes"].as_array().unwrap().len(), 3);
        assert_eq!(result["lanes"][0]["lane_id"]["host"], json!(local_host));
        assert_eq!(result["next_cursor"], "local-cursor");
        assert_eq!(result["coverage"]["peers"][0]["state"], "partial");
        assert_eq!(result["coverage"]["peers"][0]["duplicate_rows"], 1);
        assert_eq!(result["coverage"]["peers"][1]["state"], "partial");
        assert_eq!(result["coverage"]["peers"][2]["state"], "unreachable");
        assert_eq!(result["coverage"]["peers"][3]["state"], "unsupported");
        let complete = compose_board(
            json!({"lanes":[],"coverage":{}}),
            vec![PeerBoard {
                host: remote,
                observation: BoardObservation::Page(
                    json!({"lanes":[row(remote)],"truncated":false,"next_cursor":null,"coverage":page_coverage(remote)}),
                ),
            }],
        )
        .unwrap();
        assert_eq!(complete["coverage"]["peers"][0]["state"], "complete");
        let invalid = compose_board(
            json!({"lanes":[row(local_host)],"coverage":{}}),
            vec![PeerBoard {
                host: remote,
                observation: BoardObservation::Page(json!({"lanes":[row(local_host)],
                    "truncated":false,"next_cursor":null,"coverage":page_coverage(remote)})),
            }],
        )
        .unwrap();
        assert_eq!(invalid["lanes"].as_array().unwrap().len(), 1);
        assert_eq!(invalid["coverage"]["peers"][0]["invalid_rows"], 1);
        assert_eq!(invalid["coverage"]["peers"][0]["state"], "partial");
    }
}
