//! Closed disclosure facts: caller text cannot be stored in a refusal cause.
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};

/// A caller identifier may be disclosed only after its grammar has been checked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Option<String>", into = "Option<String>")]
pub struct GraphIdentifier(Option<String>);

impl TryFrom<String> for GraphIdentifier {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if super::valid_node_identifier(&value) {
            // This is a disclosure fact, never the graph's control identity.
            // Keep the typed cause even when its identifier needs masking.
            let safe = match provenant_redact::publication_text(value.as_bytes(), []) {
                Ok(bytes) if bytes == value.as_bytes() => Some(value),
                _ => None,
            };
            Ok(Self(safe))
        } else {
            Err("invalid graph identifier")
        }
    }
}
impl TryFrom<Option<String>> for GraphIdentifier {
    type Error = &'static str;
    fn try_from(value: Option<String>) -> Result<Self, Self::Error> {
        value.map_or(Ok(Self(None)), Self::try_from)
    }
}
impl From<GraphIdentifier> for Option<String> {
    fn from(value: GraphIdentifier) -> Self {
        value.0
    }
}
impl fmt::Display for GraphIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_deref().unwrap_or_default())
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

/// Find secret-shaped text for an advisory policy rule.
/// Warnings disclose only a schema path and the reason, never the value.
pub fn input_warnings(
    value: &serde_json::Value,
    policy: &provenant_records::policy::Policy,
) -> Vec<String> {
    use provenant_records::policy::{Effect, Facts};

    fn check(
        path: &SchemaPath,
        text: &str,
        policy: &provenant_records::policy::Policy,
        warnings: &mut Vec<String>,
    ) {
        if super::patch::scan_for_secrets(text).is_ok() {
            return;
        }
        let fact = Facts::violation(format!("{path} looks like a credential"));
        match policy.check("SECRET_SHAPED_INPUT", fact) {
            Effect::Proceed => {}
            Effect::Warn(message) => {
                warnings.push(message);
            }
            // Keep the input scanner advisory even if a caller constructs a
            // stale or otherwise unsupported strict policy directly. Exact
            // credential matches are handled by their owning publication
            // boundary before any mutation.
            Effect::Refuse(_) => {
                let rule = policy.rule("SECRET_SHAPED_INPUT");
                warnings.push(format!(
                    "[SECRET_SHAPED_INPUT] {path} looks like a credential (strength=Warn, source={:?}, knob={})",
                    rule.source,
                    rule.rule.knob.as_deref().unwrap_or("unknown")
                ));
            }
        }
    }

    fn walk(
        value: &serde_json::Value,
        path: &SchemaPath,
        policy: &provenant_records::policy::Policy,
        seen: &mut HashSet<String>,
        warnings: &mut Vec<String>,
        matched: &mut bool,
    ) {
        match value {
            serde_json::Value::String(text) => {
                if seen.insert(text.clone()) && super::patch::scan_for_secrets(text).is_err() {
                    *matched = true;
                    check(path, text, policy, warnings);
                }
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, &path.index(index), policy, seen, warnings, matched);
                }
            }
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    let child = path.key(key);
                    if seen.insert(key.clone()) && super::patch::scan_for_secrets(key).is_err() {
                        *matched = true;
                        check(&child, key, policy, warnings);
                    }
                    walk(value, &child, policy, seen, warnings, matched);
                }
            }
            _ => {}
        }
    }

    let mut warnings = Vec::new();
    let mut matched = false;
    walk(
        value,
        &SchemaPath::default(),
        policy,
        &mut HashSet::new(),
        &mut warnings,
        &mut matched,
    );
    // Preserve coverage for credentials assembled across adjacent fragments.
    // Leaf matches already have better paths, so this is only reported when
    // none of the decoded fields matched individually.
    if !matched
        && let Ok(encoded) = serde_json::to_string(value)
        && super::patch::scan_for_secrets(&encoded).is_err()
    {
        check(&SchemaPath::default(), &encoded, policy, &mut warnings);
    }
    warnings
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
                .and_then(|name| super::normalise_prompt_text(name).ok()),
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
            );
            assert_eq!(decoded.is_ok(), constructed.is_ok());
            if let Ok(identifier) = constructed {
                assert_eq!(identifier.to_string(), value);
                assert_eq!(serde_json::to_value(identifier).unwrap(), value);
            }
        }
    }

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn secret_shaped_identifiers_keep_cycle_causes_and_registered_values_are_masked() {
        let example = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let graph: super::super::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":2,"graph_id":"shape-cycle","nodes":[
                {"id":example,"prompt":"work","access":"read_only","route":{"alias":"senior"},"depends_on":["other"]},
                {"id":"other","prompt":"work","access":"read_only","route":{"alias":"senior"},"depends_on":[example]}
            ]
        })).unwrap();
        let error = super::super::admit_candidate(
            &graph,
            None,
            &[],
            &Default::default(),
            &Default::default(),
            None,
        )
        .err()
        .expect("cycle must still refuse");
        assert!(
            matches!(error, super::super::CandidateError::Cycle { ref node } if node.to_string() == example)
        );
        for value in [example, "redacted"] {
            let _scope = provenant_redact::publication_scope([value.as_bytes().to_vec()]).unwrap();
            let masked = GraphIdentifier::try_from(value.to_owned()).unwrap();
            assert!(masked.to_string().is_empty());
            assert!(serde_json::to_value(&masked).unwrap().is_null());
            let restored: GraphIdentifier =
                serde_json::from_value(serde_json::to_value(masked).unwrap()).unwrap();
            assert!(restored.to_string().is_empty());
        }
    }

    #[test]
    fn schema_paths_never_resume_schema_names_inside_caller_maps() {
        for value in [
            serde_json::json!({"nodes":[{"CALLER_MARKER":{"brief":"password=fixture-secret-3391"}}]}),
            serde_json::json!({"context":{"brief":{"prompt":"password=fixture-secret-3391"}}}),
        ] {
            let policy = provenant_records::policy::Policy::default();
            let warnings = input_warnings(&value, &policy);
            assert_eq!(warnings.len(), 1);
            let text = &warnings[0];
            assert!(text.contains("[<key>][<key>]"), "{text}");
            assert!(!text.contains("CALLER_MARKER"));
            assert!(text.contains("looks like a credential"));
        }
    }

    #[test]
    fn secret_shaped_inputs_warn_without_disclosing_values_under_every_policy() {
        use provenant_records::policy::{Policy, Source, Strength};
        use std::collections::BTreeMap;

        let marker = "fixture-secret-3391";
        let input = serde_json::json!({"nodes":[{"prompt":format!("password={marker}")}]});
        let warnings = input_warnings(&input, &Policy::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("nodes[0].prompt"));
        assert!(warnings[0].contains("looks like a credential"));
        assert!(!warnings[0].contains(marker));

        let mut strict = Policy::default();
        strict.overlay(
            &BTreeMap::from([("SECRET_SHAPED_INPUT".into(), Strength::Refuse)]),
            Source::Project,
        );
        assert_eq!(
            strict.rule("SECRET_SHAPED_INPUT").rule.strength,
            Strength::Warn
        );
        let warnings = input_warnings(&input, &strict);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("nodes[0].prompt"));
        assert!(warnings[0].contains("looks like a credential"));
        assert!(!warnings[0].contains(marker));

        // A directly constructed legacy policy can still say Refuse. The
        // scanner itself remains advisory even in that unsupported state.
        let mut legacy = Policy::default();
        let rule = legacy
            .rules
            .iter_mut()
            .find(|rule| rule.rule.code == "SECRET_SHAPED_INPUT")
            .expect("compiled scanner policy");
        rule.rule.refusable = true;
        rule.rule.strength = Strength::Refuse;
        let warnings = input_warnings(&input, &legacy);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("strength=Warn"));
        assert!(!warnings[0].contains(marker));
    }
}
