//! Deterministic graph identities and dependency policy. This is not yet the
//! complete graph document validator: prompt custody and worktree proof remain
//! with the shared filesystem/Git owners.
use crate::pending::OrderedMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fault {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub error: String,
    pub fix: String,
}
#[derive(Debug, Error)]
pub enum GraphError {
    #[error("graph number needs the lossless JavaScript numeric owner: {0}")]
    JavaScriptNumber(#[from] crate::pending::JsNumberBoundary),
    #[error("duplicate graph node {0}")]
    DuplicateNode(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub fn valid_graph_id(value: &str) -> bool {
    value.strip_prefix("g-").is_some_and(|suffix| {
        (6..=16).contains(&suffix.len())
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub fn valid_node_id(value: &str) -> bool {
    (1..=48).contains(&value.len())
        && value
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

pub fn canonical_json(value: &Value) -> Result<String, GraphError> {
    match value {
        Value::Number(value) => Ok(crate::pending::js_json_number(value)?),
        Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        )),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let entries = keys
                .into_iter()
                .map(|key| {
                    Ok(format!(
                        "{}:{}",
                        serde_json::to_string(key)?,
                        canonical_json(&values[key])?
                    ))
                })
                .collect::<Result<Vec<_>, GraphError>>()?;
            Ok(format!("{{{}}}", entries.join(",")))
        }
        _ => Ok(serde_json::to_string(value)?),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptKey {
    pub project: String,
    pub graph_id: String,
    pub node_id: String,
    pub revision: u64,
    pub attempt: u64,
    pub restart: u64,
}
pub fn attempt_task_id(key: &AttemptKey) -> Result<String, GraphError> {
    let digest = Sha256::digest(canonical_json(&serde_json::to_value(key)?)?.as_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(24)
        .collect::<String>();
    Ok(format!("{}.{digest}", key.graph_id))
}
pub fn fault_lines(faults: &[Fault]) -> String {
    faults
        .iter()
        .map(|fault| {
            format!(
                "{}{} · fix: {}",
                fault
                    .node
                    .as_deref()
                    .filter(|node| !node.is_empty())
                    .map_or(String::new(), |node| format!("node {node}: ")),
                fault.error,
                fault.fix
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn canonical_bytes_hashes_and_fault_lines_match_node() {
        let values = json!([{"z":[1,1.0,-0.0,0.000001,1e-7,1e20,1e21],"a":"quote\\\"\n"},{"\u{e000}":1,"🦀":2}]);
        let key = AttemptKey {
            project: "/repo".into(),
            graph_id: "g-abcdef".into(),
            node_id: "work".into(),
            revision: 1,
            attempt: 2,
            restart: 0,
        };
        let faults = vec![
            Fault {
                node: None,
                error: "cycle".into(),
                fix: "Break it".into(),
            },
            Fault {
                node: Some("work".into()),
                error: "route_unknown".into(),
                fix: "Pick one".into(),
            },
        ];
        let script = r#"import {canonicalJson,attemptTaskId,faultLines} from './src/graph/document.ts';const [values,key,faults]=process.argv.slice(1).map(JSON.parse);console.log(JSON.stringify({values:values.map(canonicalJson),id:attemptTaskId(key),faults:faultLines(faults)}));"#;
        let Some(expected) = crate::test_support::node(
            script,
            &[
                values.to_string(),
                serde_json::to_string(&key).unwrap(),
                serde_json::to_string(&faults).unwrap(),
            ],
        ) else {
            return;
        };
        assert_eq!(
            json!({"values":values.as_array().unwrap().iter().map(|value|canonical_json(value).unwrap()).collect::<Vec<_>>(),"id":attempt_task_id(&key).unwrap(),"faults":fault_lines(&faults)}),
            expected
        );
        assert!(canonical_json(&json!(9007199254740993_u64)).is_err());
    }
}

/// Inputs already separated from document shape/route validation. Worktree is
/// the resolved path when available, otherwise the submitted worktree key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeRelation {
    pub id: String,
    pub depends_on: Vec<String>,
    pub confidential: Option<bool>,
    pub writer_worktree: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphAnalysis {
    pub order: Vec<String>,
    pub height: OrderedMap<u64>,
    pub confidential: Vec<String>,
    pub faults: Vec<Fault>,
}

pub fn analyse_relations(
    nodes: &[NodeRelation],
    root_confidential: bool,
) -> Result<GraphAnalysis, GraphError> {
    use std::collections::{BTreeSet, VecDeque};
    let mut ids = BTreeSet::new();
    for node in nodes {
        if !ids.insert(node.id.clone()) {
            return Err(GraphError::DuplicateNode(node.id.clone()));
        }
    }
    let mut parents: Vec<Vec<usize>> = vec![vec![]; nodes.len()];
    let mut children: Vec<Vec<usize>> = vec![vec![]; nodes.len()];
    let mut result = GraphAnalysis::default();
    for (index, node) in nodes.iter().enumerate() {
        for dependency in &node.depends_on {
            let Some(parent) = nodes.iter().position(|node| node.id == *dependency) else {
                continue;
            };
            if !parents[index].contains(&parent) {
                parents[index].push(parent);
                children[parent].push(index);
            }
        }
    }
    let mut indegree: Vec<_> = parents.iter().map(Vec::len).collect();
    let mut queue: VecDeque<_> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    let mut order = vec![];
    while let Some(index) = queue.pop_front() {
        order.push(index);
        for child in &children[index] {
            indegree[*child] -= 1;
            if indegree[*child] == 0 {
                queue.push_back(*child);
            }
        }
    }
    if order.len() != nodes.len() {
        let mut cyclic = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| (!order.contains(&index)).then_some(node.id.as_str()))
            .collect::<Vec<_>>();
        cyclic.sort();
        result.faults.push(Fault {
            node: None,
            error: "cycle".into(),
            fix: format!("Break the dependency cycle through {}.", cyclic.join(", ")),
        });
    }
    let mut confidential = BTreeSet::new();
    for index in &order {
        let node = &nodes[*index];
        let inherited = root_confidential
            || parents[*index]
                .iter()
                .any(|parent| confidential.contains(parent));
        if inherited && node.confidential == Some(false) {
            let source = parents[*index]
                .iter()
                .filter(|parent| confidential.contains(*parent))
                .map(|parent| nodes[*parent].id.as_str())
                .collect::<Vec<_>>();
            result.faults.push(Fault {
                node: Some(node.id.clone()),
                error: "confidentiality_downgrade".into(),
                fix: format!(
                    "Remove confidential false: it consumes confidential {} output.",
                    if source.is_empty() {
                        "root".into()
                    } else {
                        source.join(", ")
                    }
                ),
            });
        }
        if inherited || node.confidential == Some(true) {
            confidential.insert(*index);
            result.confidential.push(node.id.clone());
        }
    }
    let ancestors = |index: usize| {
        let mut seen = BTreeSet::new();
        let mut stack = parents[index].clone();
        while let Some(parent) = stack.pop() {
            if seen.insert(parent) {
                stack.extend(&parents[parent]);
            }
        }
        seen
    };
    let mut writers: Vec<(&str, Vec<usize>)> = vec![];
    for (index, node) in nodes.iter().enumerate() {
        if let Some(path) = &node.writer_worktree {
            if let Some((_, list)) = writers.iter_mut().find(|(key, _)| *key == path) {
                list.push(index);
            } else {
                writers.push((path, vec![index]));
            }
        }
    }
    if order.len() == nodes.len() {
        for (path, list) in writers {
            for (i, left) in list.iter().enumerate() {
                for right in list.iter().skip(i + 1) {
                    if !ancestors(*left).contains(right) && !ancestors(*right).contains(left) {
                        result.faults.push(Fault {
                            node: Some(nodes[*right].id.clone()),
                            error: "writers_unordered".into(),
                            fix: format!(
                                "Order {} and {} by depends_on: both write {path}.",
                                nodes[*left].id, nodes[*right].id
                            ),
                        });
                    }
                }
            }
        }
    }
    let mut heights = vec![0_u64; nodes.len()];
    for index in order.iter().rev() {
        let height = 1 + children[*index]
            .iter()
            .map(|child| heights[*child])
            .max()
            .unwrap_or(0);
        heights[*index] = height;
        result.height.insert(nodes[*index].id.clone(), height);
    }
    result.order = order
        .into_iter()
        .map(|index| nodes[index].id.clone())
        .collect();
    Ok(result)
}
#[cfg(test)]
mod relation_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn dependency_height_cycles_confidentiality_and_writer_order_match_node() {
        let fixtures = [
            vec![
                NodeRelation {
                    id: "a".into(),
                    confidential: Some(true),
                    ..Default::default()
                },
                NodeRelation {
                    id: "b".into(),
                    depends_on: vec!["a".into(), "a".into()],
                    ..Default::default()
                },
            ],
            vec![
                NodeRelation {
                    id: "a".into(),
                    confidential: Some(true),
                    ..Default::default()
                },
                NodeRelation {
                    id: "b".into(),
                    depends_on: vec!["a".into()],
                    confidential: Some(false),
                    ..Default::default()
                },
            ],
            vec![
                NodeRelation {
                    id: "a".into(),
                    depends_on: vec!["b".into()],
                    ..Default::default()
                },
                NodeRelation {
                    id: "b".into(),
                    depends_on: vec!["a".into()],
                    ..Default::default()
                },
            ],
            vec![
                NodeRelation {
                    id: "a".into(),
                    writer_worktree: Some("shared".into()),
                    ..Default::default()
                },
                NodeRelation {
                    id: "b".into(),
                    writer_worktree: Some("shared".into()),
                    ..Default::default()
                },
            ],
        ];
        let docs:Vec<_>=fixtures.iter().map(|nodes|json!({"schema_version":1,"graph_id":"g-abcdef","revision":1,"nodes":nodes.iter().map(|node|{
            let mut value=json!({"id":node.id,"brief":{"prompt":"fixture"},"route":{"adapter":"codex","model":"m"},"depends_on":node.depends_on});
            if let Some(confidential)=node.confidential{value["authority"]=json!({"confidential":confidential});}
            if let Some(worktree)=&node.writer_worktree{value["authority"]=json!({"mode":"worktree_write"});value["worktree"]=json!(worktree);}
            value
        }).collect::<Vec<_>>()})).collect();
        let script = r#"import {validateGraph} from './src/graph/document.ts';
const identity={cwd:'/fixture',project:'/fixture',provider:'fixture',agentId:'fixture'};
const catalogue={adapters:[{name:'codex',models:['m'],aliases:{},model_details:[]}],endpoints:{},drift:[]};
console.log(JSON.stringify(JSON.parse(process.argv[1]).map(doc=>{const result=validateGraph(doc,identity,catalogue,{});return {faults:result.faults.filter(f=>['cycle','confidentiality_downgrade','writers_unordered'].includes(f.error)),...(result.plan?{height:result.plan.height,confidential:result.plan.nodes.filter(n=>n.authority.confidential).map(n=>n.id)}:{})}})));"#;
        let Some(expected) = crate::test_support::node(script, &[json!(docs).to_string()]) else {
            return;
        };
        let actual:Vec<_>=fixtures.iter().map(|nodes|{let analysis=analyse_relations(nodes,false).unwrap();if analysis.faults.is_empty(){json!({"faults":analysis.faults,"height":analysis.height,"confidential":analysis.confidential})}else{json!({"faults":analysis.faults})}}).collect();
        assert_eq!(json!(actual), expected);
    }
}
