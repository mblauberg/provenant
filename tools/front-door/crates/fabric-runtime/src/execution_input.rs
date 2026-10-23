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
