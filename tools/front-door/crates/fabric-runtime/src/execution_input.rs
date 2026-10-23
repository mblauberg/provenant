//! Execution request policy. Route normalisation and owner preflight remain
//! partial until the catalogue and guarded prompt owners can be integrated.
use crate::{identity::Identity, pending::trim_ecmascript};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

pub const DISPATCH_ADAPTERS: &[&str] = &[
    "agy", "claude", "codex", "copilot", "cursor", "kiro", "opencode",
];
pub const ACCESS_MODES: &[&str] = &["read_only", "worktree_write"];
pub const DISPATCH_CAPABILITIES: &[&str] = &["postgres", "browser"];
const ROLE_ALIASES: &[&str] = &["principal", "senior", "junior"];
const RENAMED_TIERS: &[(&str, &str)] = &[
    ("scout", "junior"),
    ("bulk", "junior"),
    ("workhorse", "senior"),
    ("strong", "senior"),
    ("review", "senior"),
    ("flagship", "principal"),
];
const NATIVE_ADAPTERS: &[(&str, &str)] = &[("claude", "claude"), ("codex", "codex")];
const MODE_SYNONYMS: &[(&str, &str)] = &[
    ("write", "worktree_write"),
    ("worktree", "worktree_write"),
    ("rw", "worktree_write"),
    ("read", "read_only"),
    ("ro", "read_only"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{fix}")]
pub struct InputError {
    pub code: String,
    pub fix: String,
}
impl InputError {
    pub fn new(code: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            fix: fix.into(),
        }
    }
    pub fn rejected(&self) -> Value {
        serde_json::json!({"status":"rejected","error":self.code,"fix":self.fix})
    }
}
pub fn native_adapter(identity: &Identity) -> Option<&'static str> {
    NATIVE_ADAPTERS
        .iter()
        .find(|(name, _)| *name == identity.provider)
        .map(|(_, adapter)| *adapter)
}
pub fn canonical_mode(value: Option<&str>) -> Option<&str> {
    value.map(|value| {
        MODE_SYNONYMS
            .iter()
            .find(|(alias, _)| *alias == value)
            .map_or(value, |(_, mode)| *mode)
    })
}
fn route_key(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}
/// JavaScript indexes UTF-16 code units, including supplementary characters.
pub fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<_> = left.encode_utf16().collect();
    let right: Vec<_> = right.encode_utf16().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, left) in left.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, right) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(left != right));
            diagonal = above;
        }
    }
    row[right.len()]
}
pub fn tier_alias(alias: &str) -> Result<Option<&'static str>, InputError> {
    let key = route_key(alias);
    if let Some((_, renamed)) = RENAMED_TIERS.iter().find(|(name, _)| *name == key) {
        return Err(InputError::new(
            "alias_renamed",
            format!("alias '{alias}' was renamed to '{renamed}'; pass {renamed}."),
        ));
    }
    Ok(ROLE_ALIASES
        .iter()
        .copied()
        .find(|candidate| edit_distance(&route_key(candidate), &key) <= 1))
}
pub fn names_explicit_route(input: &Map<String, Value>) -> Result<bool, InputError> {
    let named = |key| {
        input
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !trim_ecmascript(value).is_empty())
    };
    if named("adapter") || named("model") {
        return Ok(true);
    }
    Ok(named("alias")
        && tier_alias(
            input
                .get("alias")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )?
        .is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn selectors_match_node_case_typos_retired_tiers_and_utf16_distance() {
        let aliases = [
            "Senior",
            "senir",
            "principal",
            "scout",
            "bulk",
            "review",
            "custom",
            "",
            "İunior",
        ];
        let distances = [
            ("🦀", ""),
            ("kitten", "sitting"),
            ("café", "cafe"),
            ("🦀a", "🦀b"),
        ];
        let script = r#"import {tierAlias,canonicalMode,editDistance,namesExplicitRoute} from './src/execution-input.ts';
const [aliases,distances]=process.argv.slice(1).map(JSON.parse);
const capture=f=>{try{return f()??null}catch(e){return {code:e.code,fix:e.fix}}};
console.log(JSON.stringify({aliases:aliases.map(a=>capture(()=>tierAlias(a))),distances:distances.map(([a,b])=>editDistance(a,b)),modes:['write','worktree','rw','read','ro','read_only','bad'].map(canonicalMode),explicit:[{}, {alias:'senior'},{alias:'custom'},{model:'\uFEFF'},{adapter:'codex'}].map(i=>namesExplicitRoute(i))}));"#;
        let Some(expected) = crate::test_support::node(
            script,
            &[json!(aliases).to_string(), json!(distances).to_string()],
        ) else {
            return;
        };
        let aliases: Vec<_> = aliases
            .iter()
            .map(|alias| match tier_alias(alias) {
                Ok(value) => json!(value),
                Err(error) => json!({"code":error.code,"fix":error.fix}),
            })
            .collect();
        let explicit = [
            json!({}),
            json!({"alias":"senior"}),
            json!({"alias":"custom"}),
            json!({"model":"\u{feff}"}),
            json!({"adapter":"codex"}),
        ]
        .map(|input| names_explicit_route(input.as_object().unwrap()).unwrap());
        assert_eq!(
            json!({"aliases":aliases,"distances":distances.map(|(a,b)|edit_distance(a,b)),"modes":(["write","worktree","rw","read","ro","read_only","bad"].map(|mode|canonical_mode(Some(mode)))),"explicit":explicit}),
            expected
        );
    }
}

pub fn validate_prompt(prompt: Option<&str>, prompt_file: Option<&str>) -> Result<(), InputError> {
    if prompt.is_some() == prompt_file.is_some() {
        Err(InputError::new(
            "prompt_required",
            "Pass exactly one of prompt or prompt_file.",
        ))
    } else {
        Ok(())
    }
}
pub fn timeout_seconds(value: Option<f64>, mode: Option<&str>) -> Result<f64, InputError> {
    let timeout = value.unwrap_or(if mode == Some("worktree_write") {
        10800.0
    } else {
        3600.0
    });
    if timeout.is_finite() && timeout > 0.0 {
        Ok(timeout)
    } else {
        Err(InputError::new(
            "invalid_input",
            "timeout_seconds must be finite and positive",
        ))
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DispatchDefaults {
    #[serde(default, skip_serializing_if = "crate::field::Field::is_missing")]
    pub add_dirs: crate::field::Field<Vec<String>>,
    #[serde(default, skip_serializing_if = "crate::field::Field::is_missing")]
    pub network: crate::field::Field<bool>,
    #[serde(default, skip_serializing_if = "crate::field::Field::is_missing")]
    pub timeout_seconds: crate::field::Field<serde_json::Number>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DefaultsResult {
    pub defaults: DispatchDefaults,
    pub warnings: Vec<String>,
}

pub fn dispatch_defaults(workspace: &std::path::Path) -> DefaultsResult {
    use crate::field::Field;
    use crate::pending::OrderedMap;
    const POLICY: &str = ".agents/fabric-policy.json";
    let unreadable = || DefaultsResult {
        warnings: vec![format!(
            "{POLICY} is unreadable; no dispatch defaults applied"
        )],
        ..Default::default()
    };
    let text = match std::fs::read_to_string(workspace.join(POLICY)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DefaultsResult::default()
        }
        Err(_) => return unreadable(),
    };
    let policy: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return unreadable(),
    };
    let Some(raw) = policy.get("dispatch_defaults") else {
        return DefaultsResult::default();
    };
    if !raw.is_object() {
        return DefaultsResult {
            warnings: vec![format!(
                "{POLICY} dispatch_defaults is not an object; ignored"
            )],
            ..Default::default()
        };
    }
    #[derive(Deserialize)]
    struct Policy {
        dispatch_defaults: OrderedMap<Value>,
    }
    let policy: Policy = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return unreadable(),
    };
    let mut result = DefaultsResult::default();
    for (key, value) in policy.dispatch_defaults.0 {
        let valid = match (key.as_str(), &value) {
            ("add_dirs", Value::Array(values))
                if values
                    .iter()
                    .all(|value| value.as_str().is_some_and(|value| !value.is_empty())) =>
            {
                result.defaults.add_dirs = Field::Value(
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                );
                true
            }
            ("network", Value::Bool(value)) => {
                result.defaults.network = Field::Value(*value);
                true
            }
            ("timeout_seconds", Value::Number(value))
                if value
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value > 0.0) =>
            {
                result.defaults.timeout_seconds = Field::Value(value.clone());
                true
            }
            _ => false,
        };
        if !valid {
            result.warnings.push(format!("{POLICY} dispatch_defaults.{key} ignored: set add_dirs, network or timeout_seconds"));
        }
    }
    result
}
pub fn apply_dispatch_defaults(
    route: &mut crate::pending::OrderedMap<Value>,
    defaults: &DispatchDefaults,
) {
    use crate::field::Field;
    if route.get("add_dirs").is_none() {
        if let Field::Value(paths) = &defaults.add_dirs {
            route.insert("add_dirs".into(), serde_json::json!(paths));
        }
    }
    if route.get("network").is_none()
        && route.get("adapter").and_then(Value::as_str) == Some("codex")
    {
        if let Field::Value(value) = defaults.network {
            route.insert("network".into(), Value::Bool(value));
        }
    }
}
/// Order is part of argv, so this contract accepts an ordered route rather
/// than rebuilding it through serde_json's sorted generic map.
pub fn route_arguments(
    route: &crate::pending::OrderedMap<Value>,
) -> Result<Vec<String>, InputError> {
    let mut args = vec![];
    for (key, value) in &route.0 {
        if key == "warnings" || (key == "alias" && route.get("model").is_some()) {
            continue;
        }
        match key.as_str() {
            "preface" => {
                if value == &Value::Bool(false) {
                    args.push("--no-preface".into());
                }
            }
            "add_dirs" | "read_roots" | "knobs" => {
                let flag = match key.as_str() {
                    "add_dirs" => "--add-dir",
                    "read_roots" => "--read-root",
                    _ => "--knob",
                };
                let Some(values) = value.as_array() else {
                    return Err(InputError::new(
                        "invalid_input",
                        format!("{key} must be a list"),
                    ));
                };
                for value in values {
                    let Some(value) = value.as_str() else {
                        return Err(InputError::new(
                            "invalid_input",
                            format!("{key} must contain strings"),
                        ));
                    };
                    args.extend([flag.into(), value.into()]);
                }
            }
            "allow_secrets" | "confidential" => {
                if value == &Value::Bool(true) {
                    args.push(format!("--{}", key.replace('_', "-")));
                }
            }
            _ => args.extend([
                format!("--{}", key.replace('_', "-")),
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            ]),
        }
    }
    Ok(args)
}

#[cfg(test)]
mod request_policy_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn defaults_warnings_and_argv_order_match_node() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".agents")).unwrap();
        std::fs::write(root.path().join(".agents/fabric-policy.json"),r#"{"dispatch_defaults":{"z":1,"network":true,"add_dirs":["shared"],"bad":false,"timeout_seconds":1.5}}"#).unwrap();
        let wire = r#"{"adapter":"codex","model":"m","alias":"senior","preface":false,"read_roots":["/r"],"allow_secrets":false,"confidential":true,"fallback":false,"knobs":["x=y"]}"#;
        let mut route: crate::pending::OrderedMap<Value> = serde_json::from_str(wire).unwrap();
        let defaults = dispatch_defaults(root.path());
        apply_dispatch_defaults(&mut route, &defaults.defaults);
        let script = r#"import {dispatchDefaults,applyDispatchDefaults,routeArguments,validatePrompt,timeoutSeconds} from './src/execution-input.ts';
const [root,route]=process.argv.slice(1); const defaults=dispatchDefaults(root);
const capture=f=>{try{return f()??null}catch(e){return {code:e.code,fix:e.fix}}};
console.log(JSON.stringify({defaults,args:routeArguments(applyDispatchDefaults(JSON.parse(route),defaults.defaults)),prompts:[[undefined,undefined],['',undefined],[undefined,''],['a','b']].map(([a,b])=>capture(()=>validatePrompt(a,b))),timeouts:[capture(()=>timeoutSeconds(undefined,'write')),capture(()=>timeoutSeconds(undefined,'worktree_write')),capture(()=>timeoutSeconds(0)),capture(()=>timeoutSeconds(1.5))]}));"#;
        let Some(expected) =
            crate::test_support::node(script, &[root.path().to_str().unwrap().into(), wire.into()])
        else {
            return;
        };
        let err = |result: Result<(), InputError>| {
            result
                .err()
                .map(|error| json!({"code":error.code,"fix":error.fix}))
        };
        let timeout = |result: Result<f64, InputError>| match result {
            Ok(value) => {
                if value.fract() == 0.0 {
                    json!(value as u64)
                } else {
                    json!(value)
                }
            }
            Err(error) => json!({"code":error.code,"fix":error.fix}),
        };
        assert_eq!(
            json!({"defaults":defaults,"args":route_arguments(&route).unwrap(),"prompts":[err(validate_prompt(None,None)),err(validate_prompt(Some(""),None)),err(validate_prompt(None,Some(""))),err(validate_prompt(Some("a"),Some("b")))],"timeouts":[timeout(timeout_seconds(None,Some("write"))),timeout(timeout_seconds(None,Some("worktree_write"))),timeout(timeout_seconds(Some(0.0),None)),timeout(timeout_seconds(Some(1.5),None))]}),
            expected
        );
    }
}
