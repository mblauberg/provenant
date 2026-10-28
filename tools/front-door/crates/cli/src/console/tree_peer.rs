//! Composition of bounded, host-owned tree observations.
use provenant_core::{AttemptId, HostId, ProjectId};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Debug)]
pub enum TreeObservation {
    Tree(Value),
    Unreachable,
    Unsupported,
}

#[derive(Debug)]
pub struct PeerTree {
    pub host: HostId,
    pub observation: TreeObservation,
}

/// Preserve the local tree and totals; peer totals and gaps live in coverage.
pub fn compose_tree(mut local: Value, peers: Vec<PeerTree>) -> Result<Value, &'static str> {
    let mut seen = BTreeSet::new();
    for node in local["nodes"].as_array().ok_or("invalid local tree")? {
        seen.insert(identity(node).ok_or("invalid local node identity")?);
    }
    if !local["edges"].is_array() {
        return Err("invalid local tree");
    }
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut coverage = Vec::new();
    let mut hosts = BTreeSet::new();
    for peer in peers {
        let host = peer.host;
        if !hosts.insert(host) {
            coverage.push(json!({"host":host,"state":"partial","reason":"duplicate_host"}));
            continue;
        }
        let tree = match peer.observation {
            TreeObservation::Unreachable => {
                coverage.push(json!({"host":host,"state":"unreachable"}));
                continue;
            }
            TreeObservation::Unsupported => {
                coverage.push(json!({"host":host,"state":"unsupported"}));
                continue;
            }
            TreeObservation::Tree(tree) => tree,
        };
        let (Some(remote_nodes), Some(remote_edges)) =
            (tree["nodes"].as_array(), tree["edges"].as_array())
        else {
            coverage.push(json!({"host":host,"state":"partial","reason":"invalid_tree"}));
            continue;
        };
        let (mut invalid, mut duplicates, mut included) = (0usize, 0usize, 0usize);
        let mut accepted = BTreeSet::new();
        for node in remote_nodes.iter().take(100) {
            let Some(key) = identity(node) else {
                invalid += 1;
                continue;
            };
            if key.0 != Some(host) {
                invalid += 1;
            } else if !seen.insert(key) {
                duplicates += 1;
            } else {
                accepted.insert((key.1, key.2));
                nodes.push(node.clone());
                included += 1;
            }
        }
        for edge in remote_edges.iter().take(300) {
            let project = edge["project_id"]
                .as_str()
                .and_then(|s| ProjectId::parse(s).ok());
            let child = edge["child_attempt"]
                .as_str()
                .and_then(|s| AttemptId::parse(s).ok());
            let valid = project
                .zip(child)
                .is_some_and(|key| accepted.contains(&key))
                && edge["parent_attempt"]
                    .as_str()
                    .and_then(|s| AttemptId::parse(s).ok())
                    .is_some()
                && matches!(
                    edge["kind"].as_str(),
                    Some("dispatch" | "continuation" | "retry")
                )
                && matches!(edge["parent_status"].as_str(), Some("present" | "missing"));
            if valid {
                let mut edge = edge.clone();
                edge["host"] = json!(host);
                edges.push(edge);
            } else {
                invalid += 1;
            }
        }
        let truncated =
            remote_nodes.len() > 100 || remote_edges.len() > 300 || tree["truncated"] != false;
        let source_incomplete = !matches!(
            tree["coverage"]["runs"].as_str(),
            Some("complete" | "empty")
        ) || tree["coverage"]["nested_runs"] != "complete"
            || tree["coverage"]["host"] != json!(host);
        let incomplete = truncated || invalid != 0 || duplicates != 0 || source_incomplete;
        coverage.push(json!({"host":host,"state":if incomplete { "partial" } else { "complete" },
            "included":included,"invalid_rows":invalid,"duplicate_rows":duplicates,"truncated":truncated,
            "source_incomplete":source_incomplete,"totals":tree["totals"],
            "identity_conflicts":tree["identity_conflicts"],"unfetched_children":tree["unfetched_children"],
            "children_absent":tree["children_absent"]}));
    }
    local["nodes"]
        .as_array_mut()
        .ok_or("invalid local tree")?
        .extend(nodes);
    local["edges"]
        .as_array_mut()
        .ok_or("invalid local tree")?
        .extend(edges);
    local["coverage"]["peers"] = json!(coverage);
    Ok(local)
}

fn identity(node: &Value) -> Option<(Option<HostId>, ProjectId, AttemptId)> {
    let host = match &node["lane_id"]["host"] {
        Value::Null => None,
        Value::String(host) => Some(HostId::parse(host).ok()?),
        _ => return None,
    };
    Some((
        host,
        ProjectId::parse(node["lane_id"]["project_id"].as_str()?).ok()?,
        AttemptId::parse(node["lane_id"]["attempt_id"].as_str()?).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_tree_coverage() {
        let local_host = HostId::from_bytes([1; 16]);
        let remote = HostId::from_bytes([2; 16]);
        let partial = HostId::from_bytes([3; 16]);
        let offline = HostId::from_bytes([4; 16]);
        let old = HostId::from_bytes([5; 16]);
        let project = ProjectId::from_bytes([6; 16]);
        let attempt = AttemptId::from_bytes([7; 16]);
        let node =
            |host| json!({"lane_id":{"host":host,"project_id":project,"attempt_id":attempt}});
        let tree = |host, truncated| {
            json!({"nodes":[node(host)],"edges":[],"truncated":truncated,
            "coverage":{"host":host,"runs":"complete","nested_runs":"complete"},"totals":{"attempts":1}})
        };
        let local = json!({"nodes":[node(local_host)],"edges":[],"totals":{"attempts":1},"coverage":{"scope":"local"}});
        let result = compose_tree(
            local,
            vec![
                PeerTree {
                    host: remote,
                    observation: TreeObservation::Tree(tree(remote, false)),
                },
                PeerTree {
                    host: partial,
                    observation: TreeObservation::Tree(tree(partial, true)),
                },
                PeerTree {
                    host: offline,
                    observation: TreeObservation::Unreachable,
                },
                PeerTree {
                    host: old,
                    observation: TreeObservation::Unsupported,
                },
            ],
        )
        .unwrap();
        assert_eq!(result["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(result["totals"]["attempts"], 1);
        assert_eq!(result["coverage"]["peers"][0]["state"], "complete");
        assert_eq!(result["coverage"]["peers"][1]["state"], "partial");
        assert_eq!(result["coverage"]["peers"][2]["state"], "unreachable");
        assert_eq!(result["coverage"]["peers"][3]["state"], "unsupported");

        let edge = json!({"project_id":project,"child_attempt":attempt,"parent_attempt":attempt,
            "kind":"retry","parent_status":"present"});
        let mut page = tree(remote, false);
        page["nodes"] = json!([node(remote), node(remote), node(local_host)]);
        page["edges"] = json!([edge]);
        let result = compose_tree(
            json!({"nodes":[node(local_host)],"edges":[],"coverage":{}}),
            vec![PeerTree {
                host: remote,
                observation: TreeObservation::Tree(page),
            }],
        )
        .unwrap();
        assert_eq!(result["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(result["edges"][0]["host"], json!(remote));
        assert_eq!(result["coverage"]["peers"][0]["state"], "partial");
        assert_eq!(result["coverage"]["peers"][0]["duplicate_rows"], 1);
        assert_eq!(result["coverage"]["peers"][0]["invalid_rows"], 1);
    }
}
