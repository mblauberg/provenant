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
    JavaScriptNumber(String),
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
// port: belongs in provenant-records
fn number(number: &serde_json::Number) -> Result<String, GraphError> {
    if !number.is_f64() {
        if number
            .as_i64()
            .is_some_and(|n| (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n))
            || number.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)
        {
            return Ok(number.to_string());
        }
        return Err(GraphError::JavaScriptNumber(number.to_string()));
    }
    let text = number.to_string();
    let (mantissa, exponent) = text.split_once('e').map_or((text.as_str(), 0), |(m, e)| {
        (m, e.parse::<i32>().unwrap_or(0))
    });
    let (sign, mantissa) = mantissa
        .strip_prefix('-')
        .map_or(("", mantissa), |m| ("-", m));
    let mut point = mantissa.find('.').unwrap_or(mantissa.len()) as i32 + exponent;
    let digits = mantissa.replace('.', "");
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Ok("0".into());
    }
    point -= (digits.len() - significant.len()) as i32;
    let digits = significant.trim_end_matches('0');
    let exponent = point - 1;
    Ok(if !(-6..21).contains(&exponent) {
        let (first, rest) = digits.split_at(1);
        let mantissa = if rest.is_empty() {
            first.into()
        } else {
            format!("{first}.{rest}")
        };
        if exponent >= 0 {
            format!("{sign}{mantissa}e+{exponent}")
        } else {
            format!("{sign}{mantissa}e{exponent}")
        }
    } else if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{sign}{whole}.{fraction}")
    })
}
pub fn canonical_json(value: &Value) -> Result<String, GraphError> {
    match value {
        Value::Number(value) => number(value),
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
    let digest = format!("{digest:x}").chars().take(24).collect::<String>();
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
