//! Pool selection precedence and expansion, separated from the routing owner.
use crate::{
    execution_input::{native_adapter, tier_alias, InputError},
    field::Field,
    identity::Identity,
    pending::{trim_ecmascript, OrderedMap},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const POOL_FIELDS: &[&str] = &["route", "rotate", "council", "models"];
pub type PoolTask = OrderedMap<Value>;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pick {
    pub adapter: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub effort: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub family: Field<String>,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PickResult {
    Ok {
        picks: Vec<Pick>,
        warnings: Vec<String>,
        #[serde(default, skip_serializing_if = "Field::is_missing")]
        route: Field<String>,
        #[serde(default, skip_serializing_if = "Field::is_missing")]
        confidential: Field<bool>,
    },
    Rejected {
        error: String,
        fix: String,
    },
}
#[derive(Debug, Error)]
pub enum PoolError {
    #[error("Route pools returned an invalid answer.")]
    InvalidAnswer,
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Input(#[from] InputError),
}
pub trait PoolPicker {
    fn pick(
        &mut self,
        requests: &[PoolTask],
        identity: &Identity,
    ) -> Result<Vec<PickResult>, PoolError>;
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Expanded {
    pub tasks: Vec<PoolTask>,
    pub council: bool,
    pub errors: Vec<Value>,
    pub warnings: Vec<String>,
}

pub fn rotate_override_from_knobs(knobs: Option<&[String]>) -> Option<bool> {
    let mode = knobs?
        .iter()
        .filter_map(|item| item.split_once('='))
        .rfind(|(name, _)| *name == "routing.pool_mode")?
        .1;
    match mode {
        "rotate" => Some(true),
        "top" => Some(false),
        _ => None,
    }
}
pub fn uses_pool(input: &PoolTask) -> bool {
    ["route", "models", "council"]
        .iter()
        .any(|key| input.get(key).is_some())
        || input.get("rotate") == Some(&Value::Bool(true))
}
pub fn without_empty_selectors(mut task: PoolTask) -> PoolTask {
    task.0.retain(|(key, value)| {
        !(["adapter", "model", "alias", "route"].contains(&key.as_str())
            && value
                .as_str()
                .is_some_and(|value| trim_ecmascript(value).is_empty()))
    });
    task
}
fn text(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
    }
}
pub fn settle(input: PoolTask) -> Result<(PoolTask, Vec<String>), InputError> {
    let mut warnings = vec![];
    let (fields, winner) = if let Some(models) = input.get("models") {
        if !models
            .as_array()
            .is_some_and(|models| models.iter().all(Value::is_string))
        {
            return Err(InputError::new(
                "models_invalid",
                "Pass models as a list of adapter/model[@effort] strings.",
            ));
        }
        (
            vec!["alias", "model", "council", "rotate"],
            "models names the council".into(),
        )
    } else if input.get("model").is_some() {
        (
            vec!["route", "council", "rotate"],
            format!("model {} was named explicitly", text(input.get("model"))),
        )
    } else if input.get("alias").is_some() && input.get("route").is_some() {
        (
            vec!["alias"],
            format!("route {} was named", text(input.get("route"))),
        )
    } else if input.get("alias").is_some() {
        (
            vec!["council", "rotate"],
            format!(
                "alias {} names one model; pass route for a pool",
                text(input.get("alias"))
            ),
        )
    } else {
        return Ok((input, warnings));
    };
    let ignored: Vec<_> = fields
        .into_iter()
        .filter(|key| {
            input
                .get(key)
                .is_some_and(|value| value != &Value::Bool(false))
        })
        .collect();
    if !ignored.is_empty() {
        warnings.push(format!("{} ignored: {winner}", ignored.join(", ")));
    }
    let mut task = input;
    task.0.retain(|(key, _)| !ignored.contains(&key.as_str()));
    Ok((task, warnings))
}
pub fn native_first(
    task: PoolTask,
    identity: &Identity,
) -> Result<(PoolTask, Vec<String>), InputError> {
    if native_adapter(identity).is_none() {
        return Ok((task, vec![]));
    }
    let mut current = without_empty_selectors(task);
    let mut warnings = vec![];
    if uses_pool(&current) {
        match settle(current.clone()) {
            Ok((task, notes)) => {
                current = task;
                warnings.extend(notes);
            }
            Err(_) => return Ok((current, warnings)),
        }
    }
    if ["adapter", "model", "task_class"]
        .iter()
        .any(|key| current.get(key).is_some())
        || uses_pool(&current)
    {
        return Ok((current, warnings));
    }
    let alias = match current.get("alias") {
        None => Some("senior"),
        Some(Value::String(alias)) => tier_alias(alias)?,
        Some(_) => None,
    };
    if let Some(alias) = alias {
        current.remove("alias");
        current.insert("route".into(), Value::String(alias.into()));
        current.insert("native_default".into(), Value::String(alias.into()));
    }
    Ok((current, warnings))
}

pub fn expand_pools(
    tasks: Vec<PoolTask>,
    identity: &Identity,
    picker: &mut impl PoolPicker,
) -> Result<Expanded, PoolError> {
    let mut result = Expanded::default();
    let mut settled = vec![];
    for (index, task) in tasks.into_iter().enumerate() {
        if !uses_pool(&task) {
            settled.push(Some(task));
            continue;
        }
        let id = task
            .get("id")
            .filter(|value| !value.is_null())
            .map(|value| text(Some(value)))
            .unwrap_or_else(|| format!("task-{}", index + 1));
        match settle(task) {
            Ok((task, warnings)) => {
                result.warnings.extend(warnings);
                settled.push(Some(task));
            }
            Err(error) => {
                result
                    .errors
                    .push(serde_json::json!({"task_id":id,"error":error.code,"fix":error.fix}));
                settled.push(None);
            }
        }
    }
    let mut pending = vec![];
    let mut requests = vec![];
    for (index, task) in settled.iter().enumerate() {
        let Some(task) = task else {
            continue;
        };
        if !uses_pool(task) {
            continue;
        }
        let mut request = PoolTask::default();
        for key in POOL_FIELDS {
            if let Some(value) = task.get(key) {
                request.insert((*key).into(), value.clone());
            }
        }
        let knobs = task.get("knobs").and_then(Value::as_array).map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        if task.get("rotate").is_none() {
            if let Some(value) = rotate_override_from_knobs(knobs.as_deref()) {
                request.insert("rotate".into(), Value::Bool(value));
            }
        }
        for key in ["adapter", "effort", "confidential"] {
            if let Some(value) = task.get(key) {
                request.insert(key.into(), value.clone());
            }
        }
        if let Some(native) = native_adapter(identity) {
            request.insert("native".into(), Value::String(native.into()));
        }
        request.insert(
            "project".into(),
            Value::String(identity.project.to_string_lossy().into_owned()),
        );
        pending.push(index);
        requests.push(request);
    }
    let answers = if requests.is_empty() {
        vec![]
    } else {
        picker.pick(&requests, identity)?
    };
    if answers.len() != pending.len() {
        return Err(PoolError::InvalidAnswer);
    }
    let mut answers = pending.into_iter().zip(answers).peekable();
    for (index, task) in settled.into_iter().enumerate() {
        let Some(mut task) = task else {
            continue;
        };
        if !uses_pool(&task) {
            result.tasks.push(task);
            continue;
        }
        let Some((position, _)) = answers.peek() else {
            continue;
        };
        if *position != index {
            continue;
        }
        let Some((_, answer)) = answers.next() else {
            continue;
        };
        let id = task
            .get("id")
            .filter(|value| !value.is_null())
            .map(|value| text(Some(value)))
            .unwrap_or_else(|| format!("task-{}", index + 1));
        let PickResult::Ok {
            picks,
            warnings,
            route,
            confidential,
        } = answer
        else {
            if let PickResult::Rejected { error, fix } = answer {
                result
                    .errors
                    .push(serde_json::json!({"task_id":id,"error":error,"fix":fix}));
            }
            continue;
        };
        if let Some(alias) = task.get("native_default") {
            result.warnings.push(format!(
                "{} taken from the route pool: {} models run as native subagents",
                text(Some(alias)),
                native_adapter(identity).unwrap_or("undefined")
            ));
        }
        result.warnings.extend(warnings);
        let members = task.get("council").is_some() || task.get("models").is_some();
        result.council |= members;
        task.0.retain(|(key, _)| {
            !POOL_FIELDS.contains(&key.as_str())
                && !["alias", "native_default"].contains(&key.as_str())
        });
        for (member, pick) in picks.into_iter().enumerate() {
            let mut expanded = task.clone();
            if confidential == Field::Value(true) {
                expanded.insert("confidential".into(), Value::Bool(true));
            }
            if route == Field::Value("utility".into()) {
                expanded.insert("fallback".into(), Value::Bool(false));
            }
            expanded.insert(
                "id".into(),
                Value::String(if members {
                    format!("{id}-{}", member + 1)
                } else {
                    id.clone()
                }),
            );
            expanded.insert("adapter".into(), Value::String(pick.adapter));
            expanded.insert("model".into(), Value::String(pick.model));
            if let Field::Value(effort) = pick.effort {
                expanded.insert("effort".into(), Value::String(effort));
            }
            expanded.insert("pick_reason".into(), Value::String(pick.reason));
            result.tasks.push(expanded);
        }
    }
    let mut unique = vec![];
    for warning in result.warnings {
        if !unique.contains(&warning) {
            unique.push(warning);
        }
    }
    result.warnings = unique;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn identity(root: &std::path::Path) -> Identity {
        Identity {
            project: root.into(),
            cwd: root.into(),
            agent_id: "fixture".into(),
            provider: "codex".into(),
            registered_projects: None,
        }
    }
    struct Picker {
        answers: Vec<PickResult>,
        requests: Vec<PoolTask>,
    }
    impl PoolPicker for Picker {
        fn pick(
            &mut self,
            requests: &[PoolTask],
            _: &Identity,
        ) -> Result<Vec<PickResult>, PoolError> {
            self.requests = requests.to_vec();
            Ok(self.answers.clone())
        }
    }
    #[test]
    fn pool_expansion_matches_node_mixed_selectors_councils_errors_and_request_order() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("scripts")).unwrap();
        std::fs::write(root.path().join("scripts/model_route.py"),"import json,pathlib,sys\np=json.load(sys.stdin)\nroot=pathlib.Path(__file__).parent\n(root/'requests.json').write_text(json.dumps(p))\nprint((root/'answers.json').read_text())\n").unwrap();
        let answers = json!([{"status":"ok","picks":[{"adapter":"codex","model":"a","effort":"high","reason":"picked"},{"adapter":"cursor","model":"b","reason":"picked"}],"warnings":["same","same"],"route":"utility","confidential":true},{"status":"rejected","error":"route_empty","fix":"Choose another route"}]);
        std::fs::write(
            root.path().join("scripts/answers.json"),
            json!({"status":"ok","results":answers}).to_string(),
        )
        .unwrap();
        let wire = r#"[{"id":"council","models":["codex/a","cursor/b"],"route":"senior","alias":"junior","model":"x","council":2,"rotate":true,"prompt":"p"},{"id":"one","model":"explicit","route":"senior","rotate":true},{"id":"bad","models":"bad"},{"id":"empty","route":"principal","native_default":"senior","knobs":["routing.pool_mode=rotate","routing.pool_mode=top"]}]"#;
        let identity = identity(root.path());
        let mut picker = Picker {
            answers: serde_json::from_value(answers).unwrap(),
            requests: vec![],
        };
        let actual =
            expand_pools(serde_json::from_str(wire).unwrap(), &identity, &mut picker).unwrap();
        let script = r#"import {expandPools} from './src/pools.ts';
const [root,tasks,identity]=process.argv.slice(1);
console.log(JSON.stringify(await expandPools(JSON.parse(tasks),'python3',root,JSON.parse(identity),{PATH:process.env.PATH},new AbortController().signal)));"#;
        let Some(expected) = crate::test_support::node(
            script,
            &[
                root.path().to_str().unwrap().into(),
                wire.into(),
                serde_json::to_string(&identity).unwrap(),
            ],
        ) else {
            return;
        };
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
        let requests: Value = serde_json::from_slice(
            &std::fs::read(root.path().join("scripts/requests.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(json!({"requests":picker.requests}), requests);
    }
    #[test]
    fn native_first_empty_selectors_and_knob_override_match_node() {
        let root = tempfile::tempdir().unwrap();
        let identity = identity(root.path());
        let tasks = r#"[{},{"model":" ","alias":"senir"},{"model":"explicit","route":"junior"},{"alias":"custom"},{"alias":"junior","rotate":true},{"models":false}]"#;
        let script = r#"import {nativeFirst,rotateOverrideFromKnobs} from './src/pools.ts';const [tasks,identity]=process.argv.slice(1).map(JSON.parse);console.log(JSON.stringify({native:tasks.map(t=>nativeFirst(t,identity)),knobs:[undefined,[],['routing.pool_mode=rotate'],['routing.pool_mode=rotate','routing.pool_mode=top'],['routing.pool_mode=rotate','routing.pool_mode=bad']].map(k=>rotateOverrideFromKnobs(k)??null)}));"#;
        let Some(expected) = crate::test_support::node(
            script,
            &[tasks.into(), serde_json::to_string(&identity).unwrap()],
        ) else {
            return;
        };
        let native: Vec<_> = serde_json::from_str::<Vec<PoolTask>>(tasks)
            .unwrap()
            .into_iter()
            .map(|task| {
                let (task, warnings) = native_first(task, &identity).unwrap();
                json!({"task":task,"warnings":warnings})
            })
            .collect();
        let knobs = [
            None,
            Some(vec![]),
            Some(vec!["routing.pool_mode=rotate".into()]),
            Some(vec![
                "routing.pool_mode=rotate".into(),
                "routing.pool_mode=top".into(),
            ]),
            Some(vec![
                "routing.pool_mode=rotate".into(),
                "routing.pool_mode=bad".into(),
            ]),
        ];
        assert_eq!(
            json!({"native":native,"knobs":knobs.iter().map(|knobs|rotate_override_from_knobs(knobs.as_deref())).collect::<Vec<_>>()}),
            expected
        );
    }
}
