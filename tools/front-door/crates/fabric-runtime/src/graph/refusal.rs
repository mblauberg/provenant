//! Closed disclosure facts: caller text cannot be stored in a refusal cause.
use serde::{Deserialize, Serialize};
use std::fmt;

/// A caller identifier may be disclosed only after its grammar has been checked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GraphIdentifier(String);

impl TryFrom<String> for GraphIdentifier {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if super::valid_node_identifier(&value) && super::patch::scan_for_secrets(&value).is_ok() {
            Ok(Self(value))
        } else {
            Err("invalid graph identifier")
        }
    }
}
impl From<GraphIdentifier> for String {
    fn from(value: GraphIdentifier) -> Self {
        value.0
    }
}
impl fmt::Display for GraphIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaField {
    RequesterPath,
    Document,
    Patch,
    Nodes,
    Replace,
    Templates,
    With,
    Context,
    Extra,
    Brief,
    Prompt,
    Template,
    Id,
    Title,
    Reason,
    Description,
    Instructions,
    Inputs,
    Outputs,
    Config,
    Route,
    Routes,
    Model,
    Account,
    Source,
    Capabilities,
    Attachments,
    ReadRoots,
    Loop,
    Body,
    Composite,
    Plan,
    Use,
    Parameters,
    Default,
    Content,
    Review,
    Extend,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Segment {
    Field(SchemaField),
    Index(usize),
    Key,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaPath(Vec<Segment>);

impl SchemaPath {
    pub fn root(field: SchemaField) -> Self {
        Self(vec![Segment::Field(field)])
    }
    fn index(&self, index: usize) -> Self {
        let mut next = self.clone();
        next.0.push(Segment::Index(index));
        next
    }
    fn key(&self, key: &str) -> Self {
        let mut next = self.clone();
        // These positions are maps, even when a caller key happens to spell a field.
        let map = self.0.iter().any(|segment| matches!(segment, Segment::Key))
            || matches!(
                self.0.last(),
                Some(Segment::Field(
                    SchemaField::Templates
                        | SchemaField::With
                        | SchemaField::Context
                        | SchemaField::Extra
                        | SchemaField::Parameters
                ))
            );
        let field = (!map)
            .then(|| {
                serde_json::from_value::<SchemaField>(serde_json::Value::String(key.into())).ok()
            })
            .flatten();
        next.0
            .push(field.map(Segment::Field).unwrap_or(Segment::Key));
        next
    }
}

impl fmt::Display for SchemaPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("input");
        }
        for (i, segment) in self.0.iter().enumerate() {
            match segment {
                Segment::Field(field) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    // Enum serialisation is a closed schema vocabulary.
                    let value = serde_json::to_value(field).map_err(|_| fmt::Error)?;
                    f.write_str(value.as_str().ok_or(fmt::Error)?)?;
                }
                Segment::Index(index) => write!(f, "[{index}]")?,
                Segment::Key => f.write_str("[<key>]")?,
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretRule {
    #[default]
    Secret,
}
impl fmt::Display for SecretRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("secret")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("the {field} contains a credential-shaped value (rule {rule})")]
pub struct SecretCause {
    pub field: SchemaPath,
    pub rule: SecretRule,
}

/// Walk strings and keys, retaining schema positions only. Scanner prose is discarded.
pub fn scan_value(
    value: &serde_json::Value,
    scan: &dyn Fn(&str) -> Result<(), String>,
) -> Result<(), SecretCause> {
    fn walk<'a>(
        value: &'a serde_json::Value,
        path: &SchemaPath,
        scan: &dyn Fn(&str) -> Result<(), String>,
        scanned: &mut std::collections::HashSet<&'a str>,
    ) -> Result<(), SecretCause> {
        let refuse = |field| SecretCause {
            field,
            rule: SecretRule::Secret,
        };
        match value {
            serde_json::Value::String(text) if scanned.insert(text) && scan(text).is_err() => {
                Err(refuse(path.clone()))
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, &path.index(index), scan, scanned)?;
                }
                Ok(())
            }
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    let child = path.key(key);
                    if scanned.insert(key) && scan(key).is_err() {
                        return Err(refuse(child));
                    }
                    walk(value, &child, scan, scanned)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    // Snapshots can retain the same prompt in nodes, template bindings and
    // submission patches. Its first occurrence supplies the schema position;
    // rescanning identical text cannot discover another secret.
    walk(value, &SchemaPath::default(), scan, &mut Default::default())?;
    // Keep the whole-record scan too: a scanner may recognise encoded or
    // cross-field forms that are absent from individual decoded strings.
    if let Ok(encoded) = serde_json::to_string(value) {
        scan(&encoded).map_err(|_| SecretCause {
            field: SchemaPath::default(),
            rule: SecretRule::Secret,
        })?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    Budget,
    Loop,
    Limits,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRule {
    Syntax,
    Key,
    Value,
    Unconfigured,
    HostOwned,
    Exhaustion,
    TerminalDecision,
    NonIncreasing,
    MaxExtensionRounds,
    Envelope,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    PerNode,
    PerGraph,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetField {
    Tokens,
    CostUsd,
    WallTimeSeconds,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionLimit {
    Budget {
        scope: BudgetScope,
        field: BudgetField,
    },
    Host(super::GraphLimitKey),
    LoopRounds,
    LoopDecision,
    Unknown,
}
impl ExtensionLimit {
    pub fn parse(extension: &str) -> Self {
        let key = extension.split_once('=').map_or(extension, |(key, _)| key);
        if let Some(key) = key.strip_prefix("limits.") {
            return serde_json::from_value::<super::GraphLimitKey>(serde_json::Value::String(
                key.into(),
            ))
            .map(Self::Host)
            .unwrap_or(Self::Unknown);
        }
        if let Some(key) = key.strip_prefix("loop.") {
            return match key.rsplit_once('.').map(|(_, field)| field) {
                Some("rounds") => Self::LoopRounds,
                Some("decision") => Self::LoopDecision,
                _ => Self::Unknown,
            };
        }
        let key = key.strip_prefix("budget.").unwrap_or(key);
        let Some((scope, field)) = key.split_once('.') else {
            return Self::Unknown;
        };
        let scope = match scope {
            "per_node" => BudgetScope::PerNode,
            "per_graph" => BudgetScope::PerGraph,
            _ => return Self::Unknown,
        };
        let field = match field {
            "tokens" => BudgetField::Tokens,
            "cost_usd" => BudgetField::CostUsd,
            "wall_time_seconds" => BudgetField::WallTimeSeconds,
            _ => return Self::Unknown,
        };
        Self::Budget { scope, field }
    }
}
impl fmt::Display for ExtensionLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Budget { scope, field } => write!(
                f,
                "{}.{}",
                match scope {
                    BudgetScope::PerNode => "per_node",
                    BudgetScope::PerGraph => "per_graph",
                },
                match field {
                    BudgetField::Tokens => "tokens",
                    BudgetField::CostUsd => "cost_usd",
                    BudgetField::WallTimeSeconds => "wall_time_seconds",
                }
            ),
            Self::Host(key) => write!(f, "limits.{}", key.name()),
            Self::LoopRounds => f.write_str("loop[<key>].rounds"),
            Self::LoopDecision => f.write_str("loop[<key>].decision"),
            Self::Unknown => f.write_str("extend[<key>]"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("extend: {kind:?} extension {limit}{identifier} refused (rule {rule:?})", identifier = .loop_id.as_ref().map(|id| format!(" for loop {id}")).unwrap_or_default())]
pub struct ExtensionCause {
    pub kind: ExtensionKind,
    pub rule: ExtensionRule,
    pub limit: ExtensionLimit,
    pub loop_id: Option<String>,
}
impl ExtensionCause {
    pub fn host_limits(extension: &str) -> Self {
        Self {
            kind: ExtensionKind::Limits,
            rule: ExtensionRule::HostOwned,
            limit: ExtensionLimit::parse(extension),
            loop_id: None,
        }
    }
    pub fn loop_rule(extension: &str, rule: ExtensionRule) -> Self {
        Self {
            kind: ExtensionKind::Loop,
            rule,
            limit: ExtensionLimit::parse(extension),
            loop_id: extension
                .split_once('=')
                .map_or(extension, |(key, _)| key)
                .strip_prefix("loop.")
                .and_then(|key| key.rsplit_once('.'))
                .map(|(name, _)| name)
                .filter(|name| super::valid_node_identifier(name))
                .map(str::to_owned),
        }
    }

    pub fn next_action(&self) -> &'static str {
        match self.rule {
            ExtensionRule::NonIncreasing => {
                "choose a loop allowance greater than the current allowance"
            }
            ExtensionRule::MaxExtensionRounds => {
                "choose an increase within max_extension_rounds, then extend again after exhaustion"
            }
            ExtensionRule::TerminalDecision => {
                "the loop already has a terminal decision; start a new graph to change it"
            }
            ExtensionRule::Exhaustion => {
                "wait until the loop is exhausted before extending or deciding it"
            }
            ExtensionRule::HostOwned => "adjust the host graph limit in configuration",
            ExtensionRule::Unconfigured => "configure the budget before extending it",
            _ => "correct the named extension and retry",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn disclosed_graph_identifiers_require_the_grammar_on_every_entry() {
        for value in [
            "a",
            "loop.revise--2_review",
            "",
            "caller text",
            "key=value",
            "x/y",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        ] {
            let constructed = GraphIdentifier::try_from(value.to_owned());
            let decoded = serde_json::from_value::<GraphIdentifier>(serde_json::json!(value));
            assert_eq!(
                constructed.is_ok(),
                super::super::valid_node_identifier(value)
                    && super::super::patch::scan_for_secrets(value).is_ok()
            );
            assert_eq!(decoded.is_ok(), constructed.is_ok());
            if let Ok(identifier) = constructed {
                assert_eq!(identifier.to_string(), value);
                assert_eq!(serde_json::to_value(identifier).unwrap(), value);
            }
        }
    }

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn schema_paths_never_resume_schema_names_inside_caller_maps() {
        for value in [
            serde_json::json!({"nodes":[{"CALLER_MARKER":{"brief":"credential"}}]}),
            serde_json::json!({"context":{"brief":{"prompt":"credential"}}}),
        ] {
            let cause = scan_value(&value, &|text| {
                if text.contains("credential") {
                    Err("SCANNER_MARKER".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            let text = cause.to_string();
            assert!(text.contains("[<key>][<key>]"), "{text}");
            assert!(!text.contains("CALLER_MARKER"));
            assert!(!text.contains("SCANNER_MARKER"));
        }
    }
}
