//! Contracts for the dispatch cycle. Policy and effects remain with its owner.
//! Task/route objects are intentionally open: source validation is dynamic and
//! successful route records retain unknown members.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct DispatchTask(pub Map<String, Value>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct FastRoute(pub Map<String, Value>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRejection {
    pub task_id: Value,
    pub error: String,
    pub fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status")]
pub enum PreflightOutcome {
    #[serde(rename = "validated")]
    Validated { routes: Vec<FastRoute> },
    #[serde(rename = "rejected")]
    Rejected { error: String, fix: String, errors: Vec<TaskRejection> },
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{fix}")]
pub struct PreflightError {
    pub code: String,
    pub fix: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn source_results_retain_open_route_members_and_invalid_task_ids() {
        let cases=json!([
            {"status":"validated","routes":[{"status":"ok","resolved_model":"m","model_family":"openai","endpoint_provider":"openai","identity_source":"fixture","future":null,"effort":null}]},
            {"status":"validated","routes":[]},
            {"status":"rejected","error":"invalid_task_id","fix":"Pass unique task ids containing letters, numbers, '.', '_' or '-'.","errors":[{"task_id":[null],"error":"invalid_task_id","fix":"Pass unique task ids containing letters, numbers, '.', '_' or '-'."}]}
        ]);
        for value in cases.as_array().unwrap() {
            let contract:PreflightOutcome=serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(contract).unwrap(),*value);
        }
        // These are source parse_fast_route_json outputs, not an invented route schema.
        let routes=json!([{"status":"ok","resolved_model":"m","model_family":"openai","endpoint_provider":"openai","identity_source":"fixture","new":false},{"status":null,"effort":null},{}]);
        if let Some(expected)=crate::test_support::oracle("preflight_contract",&routes) {
            for (actual, expected) in routes.as_array().unwrap().iter().zip(expected.as_array().unwrap()) {
                let typed:FastRoute=serde_json::from_value(actual.clone()).unwrap();
                assert_eq!(serde_json::to_value(typed).unwrap(),*expected);
            }
        }
    }
}
