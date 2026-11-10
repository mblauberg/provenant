//! Typed templates: one schema for a leaf, a composite, a flavour and a preset.
//!
//! A template is TOML with parameters and either a leaf (brief, tier, access,
//! outputs) or `nodes`. A node is `use` plus overrides. Compilation lowers a
//! node to the compiled `GraphNode` form the kernel already executes: `over`
//! copies a node per named item with `${item}`, a template with nodes becomes a
//! composite and `repeat` becomes a loop. A dotted name is a flavour that
//! inherits its base (`review.security` extends `review`). Layers are bundled,
//! user, repository and inline; a later layer shadows an earlier one.
use super::*;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Nested `use` depth, which also bounds template recursion.
pub const MAX_TEMPLATE_DEPTH: usize = 16;
/// Copies one `over` may produce.
pub const MAX_OVER_ITEMS: usize = 256;
const ITEM: &str = "item";
/// Node field naming the template a leaf was compiled from; carried to attempts.
pub const TEMPLATE_FIELD: &str = "template";

/// Ask the compiled schema whether a field is recognised, independently of its
/// authored value (which may still contain parameter placeholders). Null either
/// decodes as a known optional field or fails its known type; an unknown field
/// instead survives in `extra`.
fn compiled_field(field: &str) -> bool {
    let mut probe = serde_json::json!({"id":"probe", "access":"read_only"});
    probe[field] = Value::Null;
    match serde_json::from_value::<GraphNode>(probe) {
        Ok(node) => !node.extra.contains_key(field),
        Err(_) => true,
    }
}

fn check_extensions(node: &Map<String, Value>) -> Result<(), String> {
    if let Some(extensions) = node.get("extensions") {
        let fields = extensions
            .as_object()
            .ok_or("node extensions must be a table")?;
        for field in fields.keys() {
            if compiled_field(field) || template_syntax(field) || field == "extensions" {
                return Err(format!("reserved node field {field} in extensions"));
            }
        }
    }
    Ok(())
}

fn flatten_extensions(node: &mut Map<String, Value>) -> Result<(), String> {
    check_extensions(node)?;
    if let Some(extensions) = node.remove("extensions") {
        node.extend(
            extensions
                .as_object()
                .ok_or("node extensions must be a table")?
                .clone(),
        );
    }
    Ok(())
}

/// Validate supplied document metadata before lowering an authored document.
pub fn validate_document_schema_version(document: &Value) -> Result<(), String> {
    if let Some(version) = document.get("schema_version")
        && !matches!(version.as_u64(), Some(1 | 2))
    {
        return Err("schema_version must be the integer 1 or 2".into());
    }
    Ok(())
}

/// Validate and remove graph-document controls before checking a shorthand node.
pub fn separate_document_fields(node: &mut Value) -> Result<(), String> {
    validate_document_schema_version(node)?;
    if let Some(fields) = node.as_object_mut() {
        for key in [
            "schema_version",
            "graph_id",
            "templates",
            "integration_branch",
            "limits",
            "delivery",
            "review_base",
            "context",
            "decision_session",
            "review",
            "retry",
            "exclude",
            "network",
            "capability",
            "no_capability",
        ] {
            fields.remove(key);
        }
    }
    Ok(())
}

fn template_syntax(field: &str) -> bool {
    matches!(
        field,
        "use" | "with" | "over" | "repeat" | "after" | "tier" | "min_ok" | "reduce_verdict"
    )
}

fn check_authored_fields(
    node: &Map<String, Value>,
    stage: &str,
    instance: bool,
) -> Result<(), String> {
    check_extensions(node).map_err(|error| format!("node {stage}: {error}"))?;
    for field in node.keys() {
        // This control selects findings collection for review-panel instances.
        if field != "extensions"
            && !matches!(field.as_str(), "after" | "tier")
            && !(instance && (template_syntax(field) || field == "repair_panel"))
            && !compiled_field(field)
        {
            return Err(format!("node {stage}: unknown field {field}"));
        }
    }
    for (container, members) in [("composite", "nodes"), ("loop", "body")] {
        if let Some(children) = node
            .get(container)
            .and_then(|value| value.get(members))
            .and_then(Value::as_array)
        {
            for child in children {
                let fields = child.as_object().ok_or("template node must be a table")?;
                let stage = fields.get("id").and_then(Value::as_str).unwrap_or(stage);
                check_authored_fields(fields, stage, true)?;
            }
        }
    }
    Ok(())
}

/// Turn an already compiled node back into authored input without presenting
/// retained provenance or runtime metadata as undeclared node fields.
pub(super) fn preserve_compiled_metadata(node: &mut Value) -> Result<(), String> {
    for pointer in ["/composite/nodes", "/loop/body"] {
        if let Some(children) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
            for child in children {
                preserve_compiled_metadata(child)?;
            }
        }
    }
    let fields = node
        .as_object_mut()
        .ok_or("compiled node must be a table")?;
    let metadata: Map<String, Value> = fields
        .iter()
        .filter(|(field, _)| !compiled_field(field))
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect();
    for field in metadata.keys() {
        fields.remove(field);
    }
    if !metadata.is_empty() {
        fields.insert("extensions".into(), Value::Object(metadata));
    }
    Ok(())
}

fn merge_node_fields(base: &mut Map<String, Value>, own: &Map<String, Value>) {
    for (key, value) in own {
        if key == "extensions"
            && let Some(inherited) = base.get_mut(key).and_then(Value::as_object_mut)
            && let Some(entries) = value.as_object()
        {
            inherited.extend(entries.clone());
        } else {
            base.insert(key.clone(), value.clone());
        }
    }
}

#[path = "template_composites.rs"]
mod composites;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateParameter {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(rename = "type")]
    pub kind: ParameterKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Value>,
    #[serde(default, rename = "enum", skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKind {
    String,
    Integer,
    Number,
    Boolean,
    List,
    Table,
}

impl ParameterKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::List => "list",
            Self::Table => "table",
        }
    }
}

impl TemplateParameter {
    /// The shape this parameter accepts, read from its declaration alone, so a
    /// refusal never echoes the value that was supplied.
    fn expected(&self) -> String {
        let article = if self.kind == ParameterKind::Integer {
            "an"
        } else {
            "a"
        };
        let mut shape = format!("{article} {}", self.kind.name());
        match (
            self.min.as_ref().and_then(Value::as_f64),
            self.max.as_ref().and_then(Value::as_f64),
        ) {
            (Some(min), Some(max)) => shape.push_str(&format!(" from {min} to {max}")),
            (Some(min), None) => shape.push_str(&format!(" of at least {min}")),
            (None, Some(max)) => shape.push_str(&format!(" of at most {max}")),
            (None, None) => {}
        }
        if !self.allowed.is_empty() {
            shape.push_str(&format!(", one of {}", Value::Array(self.allowed.clone())));
        }
        shape
    }
}

/// A supplied parameter that a template cannot bind. Names and shape descriptions
/// remain untrusted and must be redacted before disclosure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterRefusal {
    /// The graph node whose `with` failed, when it is known.
    pub node: Option<String>,
    pub template: String,
    pub fault: ParameterFault,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParameterExample(String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParameterFault {
    /// The `with` table is not a table.
    NotTable,
    /// A key the template does not declare. `key` is `None` when it is not an
    /// identifier, so the supplied text is never echoed.
    Unknown {
        key: Option<String>,
        accepted: Vec<String>,
        suggestion: Option<String>,
    },
    Missing {
        #[serde(default)]
        key: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        example: Option<Box<ParameterExample>>,
    },
    Shape {
        key: String,
        expected: String,
    },
}

impl ParameterRefusal {
    pub fn example(&self) -> Option<String> {
        match &self.fault {
            ParameterFault::Missing { example, .. } => {
                example.as_ref().map(|value| value.0.clone())
            }
            _ => None,
        }
    }

    pub(crate) fn redacted(&self) -> Self {
        let clean = |text: &str| provenant_confinement::redact::bounded_line(text, 4096);
        let mut safe = self.clone();
        safe.node = safe.node.as_deref().map(clean);
        safe.template = clean(&safe.template);
        match &mut safe.fault {
            ParameterFault::NotTable => {}
            ParameterFault::Unknown {
                key,
                accepted,
                suggestion,
            } => {
                *key = key.as_deref().map(clean);
                for name in accepted {
                    *name = clean(name);
                }
                *suggestion = suggestion.as_deref().map(clean);
            }
            ParameterFault::Missing { key, keys, example } => {
                *example = example
                    .as_ref()
                    .map(|value| Box::new(ParameterExample(clean(&value.0))));
                *key = clean(key);
                for name in keys {
                    *name = clean(name);
                }
            }
            ParameterFault::Shape { key, expected } => {
                *key = clean(key);
                *expected = clean(expected);
            }
        }
        safe
    }
}

fn template_example(
    template: &str,
    parameters: &BTreeMap<String, TemplateParameter>,
    supplied: &Map<String, Value>,
    missing: &[String],
) -> Option<String> {
    let mut args = Vec::new();
    for (key, parameter) in parameters {
        if !supplied.contains_key(key) && !missing.contains(key) {
            continue;
        }
        let value = parameter
            .allowed
            .first()
            .cloned()
            .unwrap_or_else(|| match parameter.kind {
                ParameterKind::String => serde_json::json!(if key == "worktree" {
                    "/path/to/worktree"
                } else {
                    "value"
                }),
                ParameterKind::Integer => {
                    serde_json::json!(parameter.min.as_ref().and_then(Value::as_i64).unwrap_or(1))
                }
                ParameterKind::Number => parameter
                    .min
                    .clone()
                    .unwrap_or_else(|| serde_json::json!(1)),
                ParameterKind::Boolean => serde_json::json!(true),
                ParameterKind::List => serde_json::json!(["src"]),
                ParameterKind::Table => serde_json::json!({}),
            });
        if check_value(parameter, &value).is_err() {
            return None;
        }
        let literal = if value.is_object() {
            if value.as_object()?.is_empty() {
                "{}".to_owned()
            } else {
                return None;
            }
        } else {
            value.to_string()
        };
        let binding = format!("{key}={literal}");
        args.push(format!("--set '{}'", binding.replace('\'', "'\\''")));
    }
    Some(format!("provenant run --use {template} {}", args.join(" ")))
}

impl std::fmt::Display for ParameterRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(node) = &self.node {
            write!(f, "node {node}: ")?;
        }
        let template = &self.template;
        match &self.fault {
            ParameterFault::NotTable => write!(f, "template {template} parameters must be a table"),
            ParameterFault::Unknown {
                key,
                accepted,
                suggestion,
            } => {
                match key {
                    Some(key) => write!(f, "template {template} has no parameter {key}")?,
                    None => write!(f, "template {template} has no such parameter")?,
                }
                if accepted.is_empty() {
                    return write!(f, "; template {template} takes no parameters");
                }
                write!(f, "; accepted parameters: {}", accepted.join(", "))?;
                match suggestion {
                    Some(name) => write!(f, "; did you mean {name}?"),
                    None => Ok(()),
                }
            }
            ParameterFault::Missing { key, keys, example } => {
                let list = if keys.is_empty() {
                    std::slice::from_ref(key)
                } else {
                    keys.as_slice()
                };
                if list.len() > 1 {
                    write!(
                        f,
                        "template {template} requires parameters {}",
                        list.join(", ")
                    )?;
                } else {
                    write!(f, "template {template} requires parameter {}", list[0])?;
                }
                if let Some(example) = example {
                    write!(f, "; example: {}", example.0)?;
                }
                Ok(())
            }
            ParameterFault::Shape { key, expected } => {
                write!(f, "template {template} parameter {key} expects {expected}")
            }
        }
    }
}

/// A compile failure. A refused parameter keeps its structure so the front
/// door can name it; everything else stays a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    Message(String),
    Parameter(ParameterRefusal),
}

impl From<String> for CompileError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for CompileError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

impl From<ParameterRefusal> for CompileError {
    fn from(refusal: ParameterRefusal) -> Self {
        Self::Parameter(refusal)
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => f.write_str(message),
            Self::Parameter(refusal) => refusal.fmt(f),
        }
    }
}

/// The declared name closest to an unknown key. A key that extends a declared
/// name with a dot (`subject.commit`) matches that name; otherwise a close
/// edit distance does.
fn nearest_name<'a>(key: &str, accepted: impl IntoIterator<Item = &'a str>) -> Option<String> {
    accepted
        .into_iter()
        .filter_map(|name| {
            let extends = key
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('.'));
            let distance = if extends { 0 } else { edit_distance(key, name) };
            let limit = (key.len().max(name.len()) / 3).max(1);
            (distance <= limit).then_some((distance, name))
        })
        .min()
        .map(|(_, name)| name.to_owned())
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, lc) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, rc) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if lc == *rc {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[right.len()]
}

/// Identity of a top-level submission, inherited by dotted flavours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionIdentity {
    /// Each invocation starts work, even when its brief is unchanged.
    #[default]
    Fresh,
    /// Equal bound content (including cwd and resolved HEAD) reuses work.
    Content,
}

impl SubmissionIdentity {
    /// An explicit operation ID replays independently of freshness and HEAD.
    /// The caller supplies bound content and a fresh nonce, never clock time.
    pub fn graph_id(
        self,
        content: &Value,
        nonce: &str,
        operation_id: Option<&str>,
    ) -> Result<String, String> {
        let identity = if let Some(operation) = operation_id {
            serde_json::json!({"operation_id": operation})
        } else {
            match self {
                Self::Fresh => serde_json::json!({"content":content,"nonce":nonce}),
                Self::Content => serde_json::json!({"content":content}),
            }
        };
        let digest =
            provenant_records::CanonicalV1::record_digest("graph-submission", 1, &identity)
                .map_err(|error| error.to_string())?;
        Ok(format!(
            "g-{}",
            &provenant_core::hex(digest.as_bytes())[..30]
        ))
    }
}

/// Review protocol selected by a template, inherited by dotted flavours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewBehaviour {
    Lens,
    Verify,
    Synthesis,
}

impl ReviewBehaviour {
    fn apply(self, node: &mut Map<String, Value>) {
        let instruction = match self {
            Self::Lens => {
                "Report concrete defects against the pinned subject and base. Include id, severity, file, summary, status, evidence and proposed_fix for each finding; line is optional. Include trigger {sequence: real steps reaching the state, reach: default_path or operator_mistake or attacker_or_unreachable, observed: boolean}. Findings without a trigger are notes; off-default-path findings are notes except ADR 0035 boundary breaches (class boundary) and data loss (class data_loss). Use kind spec_defect for evidenced specification problems routed to the chair. Set design_defect=true with evidence when the design cannot hold or findings share a flaw, and name the invariant and structural alternative."
            }
            Self::Verify => {
                "Check every input finding against the pinned subject and its callers. Preserve its qualified id and evidence, set verification to upheld, falsified or unverifiable, and give a reason. Mark newly discovered findings verification=new. For every finding emit triage {decision: confirmed or rejected, reason: code evidence, worth_fixing: boolean, root_cause: invariant under repair, blocking_class: acceptance or default_path_defect or boundary or data_loss or none, shared_flaw: boolean}. Rejected findings stay in the record. Check the trigger sequence and reach; absent or off-default triggers are notes except ADR 0035 boundaries and data loss."
            }
            Self::Synthesis => {
                "Reconcile every input finding by code evidence. Preserve qualified ids and assign each finding disposition accepted, rejected or deferred with a non-empty reason. Emit checked triage for every finding: decision confirmed or rejected, reason, worth_fixing boolean, root_cause naming the invariant, blocking_class acceptance/default_path_defect/boundary/data_loss/none, shared_flaw boolean. Group confirmed worthwhile findings by shared cause before a writer starts. Keep rejected findings in the record. Evaluate disputed repair_items by evidence; a dispute is not automatically unresolved. A design-cannot-hold outcome or shared design flaw routes to principal design. Never decide by majority vote."
            }
        };
        node.remove("role");
        node.insert("review_behaviour".into(), serde_json::json!(self));
        node.insert("access".into(), "read_only".into());
        node.insert("require_findings".into(), true.into());
        node.insert("required_verdict".into(), "VERDICT: PASS".into());
        if self != Self::Lens {
            node.remove("review_lens");
        }
        if self == Self::Synthesis {
            node.insert("kind".into(), "synthesise".into());
            node.insert(
                "dispositions".into(),
                serde_json::json!(["accepted", "rejected", "deferred"]),
            );
        }
        let prompt = node.entry("prompt").or_insert_with(|| "".into());
        let brief = prompt.as_str().unwrap_or_default();
        *prompt = format!("{brief}\n{instruction}\nReturn a fenced findings JSON block with schema_version 1 and a findings array (at most 32 entries). Use [] when clean. Keep qualified IDs and propagation markers. Read full member results and the input index when bounded exports are truncated; disclose unavailable evidence and lenses. For overflow, follow the supplied DISPOSITION protocol. End with exactly one final non-empty line: VERDICT: PASS, VERDICT: FAIL or VERDICT: INCONCLUSIVE. PASS requires complete acceptance evidence and no repair-eligible findings; missing evidence is INCONCLUSIVE.").into();
    }
}

pub(super) fn review_behaviour(node: &PlannedNode) -> Option<ReviewBehaviour> {
    node.extra
        .get("review_behaviour")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

pub(super) fn review_verifier(node: &PlannedNode) -> bool {
    review_behaviour(node) == Some(ReviewBehaviour::Verify)
        || node.role.as_deref() == Some("review.verify")
}

pub(super) fn review_node(node: &PlannedNode) -> bool {
    review_behaviour(node).is_some()
        || node.role.as_deref().is_some_and(|role| {
            role == "lens" || role.starts_with("lens.") || role.starts_with("review.")
        })
}

/// Optional compiler behaviour declared by a composite template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositeBehaviour {
    #[serde(rename = "panel")]
    ReviewPanel,
    Repair,
    ReviewRepair,
}

/// One template document. A leaf sets `brief` (and usually `tier`); a
/// composite sets `nodes`. Nodes stay JSON until their parameters are bound,
/// because a typed list field such as `write_scope` may be one `${scope}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite_behaviour: Option<CompositeBehaviour>,
    /// Top-level run identity. Unspecified templates start fresh work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<SubmissionIdentity>,
    /// Review output protocol, enforced independently of the brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewBehaviour>,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Set when templates travel as a list, as in a planner catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, TemplateParameter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// Route alias for the leaf: `junior`, `senior`, `principal` or `web`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessMode>,
    /// Outputs the leaf promises. `findings` makes the findings block required.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputKind>,
    /// Executor for a deterministic or planning leaf (`gate`, `decide`, `land`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NodeKind>,
    /// Further compiled-node fields for a leaf, such as `condition` or `plan`.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub node: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<Value>,
    /// Composite exports; absent, every sink exports its result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<GraphInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ok: Option<usize>,
    /// Reduce exported review verdicts without a synthesis attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduce_verdict: Option<bool>,
}

impl Template {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let template: Self =
            toml_edit::de::from_str(text).map_err(|error| format!("invalid template: {error}"))?;
        template.check()?;
        Ok(template)
    }

    pub fn is_composite(&self) -> bool {
        !self.nodes.is_empty()
    }

    /// Structural checks every layer shares.
    pub fn check(&self) -> Result<(), String> {
        if let Some(name) = self.extra.keys().next() {
            return Err(format!("unknown template field {name}"));
        }
        if self.is_composite()
            && (self.brief.is_some() || self.tier.is_some() || self.review.is_some())
        {
            return Err("a template sets either a leaf brief and tier or nodes, not both".into());
        }
        for (name, parameter) in &self.parameters {
            if !valid_identifier(name) {
                return Err(format!("template parameter name {name:?} is invalid"));
            }
            if let Some(default) = &parameter.default {
                check_value(parameter, default)
                    .map_err(|reason| format!("template parameter {name} default {reason}"))?;
            }
            if parameter
                .min
                .as_ref()
                .is_some_and(|value| !value.is_number())
                || parameter
                    .max
                    .as_ref()
                    .is_some_and(|value| !value.is_number())
            {
                return Err(format!("template parameter {name} bound must be a number"));
            }
            if let (Some(min), Some(max)) = (
                parameter.min.as_ref().and_then(Value::as_f64),
                parameter.max.as_ref().and_then(Value::as_f64),
            ) && min > max
            {
                return Err(format!("template parameter {name} bounds are reversed"));
            }
            if parameter
                .allowed
                .iter()
                .any(|value| !value.is_number() && !value.is_string())
            {
                return Err(format!("template parameter {name} enum must be scalar"));
            }
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            let id = node
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| valid_identifier(id))
                .ok_or("template node requires a valid id")?;
            if !ids.insert(id) {
                return Err(format!("template node {id} is repeated"));
            }
        }
        for export in &self.exports {
            if !ids.contains(export.node.as_str()) || export.outputs.is_empty() {
                return Err(format!(
                    "template export {} must select outputs of a template node",
                    export.node
                ));
            }
        }
        let mut names = Vec::new();
        placeholders(&serde_json::to_value(self).unwrap_or_default(), &mut names)?;
        if let Some(name) = names
            .iter()
            .find(|name| *name != ITEM && !self.parameters.contains_key(name.as_str()))
        {
            return Err(format!("template uses undeclared parameter {name}"));
        }
        Ok(())
    }

    /// A template whose parameters an untrusted author supplies, such as a
    /// planner catalogue entry, cannot vary authority by parameter: access,
    /// role, source, roots and routes are fixed text, apart from a route or
    /// read root drawn from a closed string enum.
    pub fn check_authority(&self) -> Result<(), String> {
        fn check_node(template: &Template, node: &Value) -> Result<(), String> {
            if let Some(fields) = node.as_object() {
                check_extensions(fields)?;
            }
            for key in [
                "kind",
                "access",
                "role",
                "source",
                "read_roots",
                "attachments",
                "route",
                "routes",
                "tier",
                "use",
            ] {
                let mut named = Vec::new();
                if let Some(value) = node.get(key) {
                    placeholders(value, &mut named)?;
                }
                let closed = matches!(key, "route" | "routes" | "read_roots" | "tier")
                    && named.iter().all(|name| {
                        template.parameters.get(name).is_some_and(|parameter| {
                            parameter.kind == ParameterKind::String
                                && !parameter.allowed.is_empty()
                                && parameter.allowed.iter().all(Value::is_string)
                        })
                    });
                if !named.is_empty() && !closed {
                    return Err(format!(
                        "template cannot vary authority field {key} by parameter"
                    ));
                }
                if key == "read_roots" && !named.is_empty() {
                    closed_read_roots(template, node)?;
                }
            }
            for children in [node.pointer("/composite/nodes"), node.pointer("/loop/body")]
                .into_iter()
                .flatten()
                .filter_map(Value::as_array)
            {
                for child in children {
                    check_node(template, child)?;
                }
            }
            for key in ["panel_templates", "lens_templates"] {
                if let Some(templates) = node.get(key).and_then(Value::as_object) {
                    for nested in templates.values().filter(|nested| !nested.is_null()) {
                        check_node(template, nested)?;
                    }
                }
            }
            Ok(())
        }
        self.check()?;
        for node in &self.nodes {
            check_node(self, node)?;
        }
        let mut leaf = Map::new();
        leaf.insert("access".into(), serde_json::json!(self.access));
        leaf.insert("tier".into(), serde_json::json!(self.tier));
        leaf.insert("kind".into(), serde_json::json!(self.kind));
        leaf.extend(self.node.clone());
        check_node(self, &Value::Object(leaf))
    }

    /// Effective parameters: supplied values over defaults, each checked.
    pub fn bind(
        &self,
        name: &str,
        supplied: &Value,
    ) -> Result<Map<String, Value>, ParameterRefusal> {
        let refuse = |fault: ParameterFault| ParameterRefusal {
            node: None,
            template: name.to_owned(),
            fault,
        };
        let supplied = match supplied {
            Value::Null => Map::new(),
            Value::Object(map) => map.clone(),
            _ => return Err(refuse(ParameterFault::NotTable)),
        };
        if let Some(unknown) = supplied
            .keys()
            .find(|key| !self.parameters.contains_key(key.as_str()))
        {
            let accepted: Vec<String> = self.parameters.keys().cloned().collect();
            return Err(refuse(ParameterFault::Unknown {
                key: valid_identifier(unknown).then(|| unknown.clone()),
                suggestion: nearest_name(unknown, self.parameters.keys().map(String::as_str)),
                accepted,
            }));
        }
        let mut missing = Vec::new();
        for (key, parameter) in &self.parameters {
            if supplied.get(key).or(parameter.default.as_ref()).is_none() {
                missing.push(key.clone());
            }
        }
        if !missing.is_empty() {
            let key = missing[0].clone();
            let example = template_example(name, &self.parameters, &supplied, &missing);
            return Err(refuse(ParameterFault::Missing {
                key,
                keys: missing,
                example: example.map(|value| Box::new(ParameterExample(value))),
            }));
        }
        let mut effective = Map::new();
        for (key, parameter) in &self.parameters {
            let Some(value) = supplied.get(key).or(parameter.default.as_ref()) else {
                continue;
            };
            if check_value(parameter, value).is_err() {
                return Err(refuse(ParameterFault::Shape {
                    key: key.clone(),
                    expected: parameter.expected(),
                }));
            }
            effective.insert(key.clone(), value.clone());
        }
        Ok(effective)
    }

    /// One summary line per parameter, for planner briefs and `templates`.
    pub fn parameter_summary(&self) -> String {
        self.parameters
            .iter()
            .map(|(name, parameter)| {
                let mut details = vec![match &parameter.default {
                    Some(value) => format!("default={value}"),
                    None => "required".into(),
                }];
                if let Some(value) = &parameter.min {
                    details.push(format!("min={value}"));
                }
                if let Some(value) = &parameter.max {
                    details.push(format!("max={value}"));
                }
                if !parameter.allowed.is_empty() {
                    details.push(format!("enum={}", Value::Array(parameter.allowed.clone())));
                }
                if let Some(doc) = &parameter.doc {
                    details.push(doc.clone());
                }
                format!("{name}:{} ({})", parameter.kind.name(), details.join(", "))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A flavour over its base: set fields win, parameters and leaf fields merge.
    fn flavour(mut base: Self, own: &Self) -> Self {
        base.extra.extend(own.extra.clone());
        base.composite_behaviour = own.composite_behaviour.or(base.composite_behaviour);
        base.review = own.review.or(base.review);
        base.identity = own.identity.or(base.identity);
        base.name = own.name.clone().or(base.name);
        base.doc = own.doc.clone().or(base.doc);
        base.parameters.extend(own.parameters.clone());
        base.brief = own.brief.clone().or(base.brief);
        base.tier = own.tier.clone().or(base.tier);
        base.access = own.access.or(base.access);
        if !own.outputs.is_empty() {
            base.outputs = own.outputs.clone();
        }
        base.kind = own.kind.or(base.kind);
        merge_node_fields(&mut base.node, &own.node);
        if own.is_composite() {
            base.nodes = own.nodes.clone();
            base.exports = own.exports.clone();
            base.brief = None;
            base.tier = None;
        }
        base.min_ok = own.min_ok.or(base.min_ok);
        base.reduce_verdict = own.reduce_verdict.or(base.reduce_verdict);
        base
    }
}

/// Where a template was defined. A later layer shadows an earlier one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Bundled,
    User,
    Repository,
    Inline,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplateLibrary {
    entries: BTreeMap<String, BTreeMap<Layer, Template>>,
}

const BUNDLED: &[(&str, &str)] = &[
    ("task", include_str!("../../assets/templates/task.toml")),
    (
        "steward",
        include_str!("../../assets/templates/steward.toml"),
    ),
    ("plan", include_str!("../../assets/templates/plan.toml")),
    (
        "explore",
        include_str!("../../assets/templates/explore.toml"),
    ),
    (
        "research",
        include_str!("../../assets/templates/research.toml"),
    ),
    ("design", include_str!("../../assets/templates/design.toml")),
    (
        "implement",
        include_str!("../../assets/templates/implement.toml"),
    ),
    (
        "implement.fix",
        include_str!("../../assets/templates/implement.fix.toml"),
    ),
    ("review", include_str!("../../assets/templates/review.toml")),
    (
        "review.verify",
        include_str!("../../assets/templates/review.verify.toml"),
    ),
    (
        "review.synthesis",
        include_str!("../../assets/templates/review.synthesis.toml"),
    ),
    (
        "review.security",
        include_str!("../../assets/templates/review.security.toml"),
    ),
    (
        "implement.plan",
        include_str!("../../assets/templates/implement.plan.toml"),
    ),
    (
        "implement.issue",
        include_str!("../../assets/templates/implement.issue.toml"),
    ),
    (
        "review.correctness",
        include_str!("../../assets/templates/review.correctness.toml"),
    ),
    (
        "review.tests",
        include_str!("../../assets/templates/review.tests.toml"),
    ),
    (
        "review.contract",
        include_str!("../../assets/templates/review.contract.toml"),
    ),
    (
        "review.design",
        include_str!("../../assets/templates/review.design.toml"),
    ),
    (
        "review.adversarial",
        include_str!("../../assets/templates/review.adversarial.toml"),
    ),
    ("verify", include_str!("../../assets/templates/verify.toml")),
    (
        "synthesise",
        include_str!("../../assets/templates/synthesise.toml"),
    ),
    ("repair", include_str!("../../assets/templates/repair.toml")),
];

pub fn valid_template_name(name: &str) -> bool {
    name.len() <= 64
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

impl TemplateLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    /// The node library shipped with Provenant.
    pub fn bundled() -> Self {
        let mut library = Self::new();
        for (name, text) in BUNDLED {
            // Bundled assets are checked by the template tests.
            if let Ok(template) = Template::from_toml(text) {
                library
                    .entries
                    .entry((*name).into())
                    .or_default()
                    .insert(Layer::Bundled, template);
            }
        }
        // Invalid bundled assets are detected by the catalogue tests.
        let _ = composites::install(&mut library);
        library
    }

    pub fn insert(&mut self, layer: Layer, name: &str, template: Template) -> Result<(), String> {
        if !valid_template_name(name) {
            return Err(format!("template name {name:?} is invalid"));
        }
        template
            .check()
            .map_err(|error| format!("template {name}: {error}"))?;
        self.entries
            .entry(name.into())
            .or_default()
            .insert(layer, template);
        Ok(())
    }

    pub fn insert_toml(&mut self, layer: Layer, name: &str, text: &str) -> Result<(), String> {
        let template = Template::from_toml(text).map_err(|error| format!("{name}: {error}"))?;
        self.insert(layer, name, template)
    }

    /// Load `<name>.toml` files from one directory. A missing directory is empty.
    pub fn load_dir(&mut self, layer: Layer, directory: &std::path::Path) -> Result<usize, String> {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(format!("{}: {error}", directory.display())),
        };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        paths.sort();
        for path in &paths {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| format!("{}: template name is not UTF-8", path.display()))?;
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            self.insert_toml(layer, name, &text)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Ok(paths.len())
    }

    /// This library with `templates` added at `layer`.
    pub fn with(
        &self,
        layer: Layer,
        templates: &BTreeMap<String, Template>,
    ) -> Result<Self, String> {
        let mut library = self.clone();
        for (name, template) in templates {
            library.insert(layer, name, template.clone())?;
        }
        Ok(library)
    }

    /// Effective layers, retaining flavour overrides for deterministic replay.
    pub fn snapshot(&self) -> BTreeMap<String, Template> {
        self.entries
            .iter()
            .filter_map(|(name, layers)| {
                layers
                    .values()
                    .next_back()
                    .map(|template| (name.clone(), template.clone()))
            })
            .collect()
    }

    pub fn snapshot_layers(&self) -> BTreeMap<String, Layer> {
        self.entries
            .keys()
            .filter_map(|name| self.layer(name).map(|layer| (name.clone(), layer)))
            .collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// The layer that supplies `name` itself, ignoring its base.
    pub fn layer(&self, name: &str) -> Option<Layer> {
        self.entries
            .get(name)
            .and_then(|layers| layers.keys().next_back().copied())
    }

    fn own(&self, name: &str) -> Option<&Template> {
        self.entries
            .get(name)
            .and_then(|layers| layers.values().next_back())
    }

    /// Resolve a name, applying each flavour over its base.
    pub fn resolve(&self, name: &str) -> Result<Template, String> {
        if !valid_template_name(name) {
            return Err(
                "invalid template name; use letters, digits, underscores, hyphens and dots".into(),
            );
        }
        let own = self.own(name);
        match name.rsplit_once('.') {
            Some((base, _)) => {
                let base = self.resolve(base)?;
                let own = own.ok_or_else(|| format!("unknown template flavour {name}"))?;
                Ok(Template::flavour(base, own))
            }
            None => own
                .cloned()
                .ok_or_else(|| format!("unknown template {name}")),
        }
    }
}

/// Launch-time inputs to compilation.
#[derive(Clone, Copy, Debug)]
pub struct Compiler<'a> {
    pub library: &'a TemplateLibrary,
    /// Lists for `over = "@name"`, chosen by the host at launch.
    pub resolvers: &'a BTreeMap<String, Vec<String>>,
}

static NO_RESOLVERS: BTreeMap<String, Vec<String>> = BTreeMap::new();

impl<'a> Compiler<'a> {
    pub fn new(library: &'a TemplateLibrary) -> Self {
        Self {
            library,
            resolvers: &NO_RESOLVERS,
        }
    }

    /// Lower one node document to compiled-node JSON. A raw node with no
    /// `use`, `over` or `repeat` passes through unchanged, apart from nested
    /// composite and loop members, which are lowered in turn.
    pub fn lower(&self, node: &Value) -> Result<Value, CompileError> {
        let lowered = self.lower_in(node, &Map::new(), &mut Vec::new())?;
        if lowered.to_string().contains("${item}") {
            return Err("${item} is bound only inside `over`".into());
        }
        Ok(lowered)
    }

    pub fn compile(&self, node: &Value) -> Result<GraphNode, CompileError> {
        let lowered = self.lower(node)?;
        let id = lowered
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_owned();
        serde_json::from_value(lowered)
            .map_err(|error| CompileError::from(format!("node {id}: {error}")))
    }

    pub fn compile_all(&self, nodes: &[Value]) -> Result<Vec<GraphNode>, CompileError> {
        nodes.iter().map(|node| self.compile(node)).collect()
    }

    fn lower_in(
        &self,
        raw: &Value,
        bindings: &Map<String, Value>,
        stack: &mut Vec<String>,
    ) -> Result<Value, CompileError> {
        if stack.len() > MAX_TEMPLATE_DEPTH {
            return Err(format!(
                "template nesting exceeds {MAX_TEMPLATE_DEPTH} levels: {}",
                stack.join(" > ")
            )
            .into());
        }
        let object = raw.as_object().ok_or("template node must be a table")?;
        if object.contains_key("use") || !stack.is_empty() {
            check_authored_fields(
                object,
                object
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>"),
                true,
            )?;
        }
        // A constrained direct review uses the same host-resolved panel as an
        // explicit fan-out. Keep ordinary leaf reviews unchanged, and do not
        // expand the individual lens copies a second time.
        if !object.contains_key("over")
            && object.get("use").and_then(Value::as_str) == Some("review")
            && self.resolvers.contains_key("review-family-resolution")
            && !bindings.contains_key(ITEM)
        {
            let mut panel = object.clone();
            let with = panel
                .entry("with")
                .or_insert_with(|| Value::Object(Map::new()));
            if with.is_null() {
                *with = Value::Object(Map::new());
            }
            let with = with
                .as_object_mut()
                .ok_or("`with` must be a table of template parameters")?;
            with.insert("lens".into(), Value::String("${item}".into()));
            with.entry("advice")
                .or_insert_with(|| serde_json::json!({"resolver": "review-advice-${item}"}));
            return self.lower_over(
                &panel,
                &Value::String("@review-panel".into()),
                bindings,
                stack,
            );
        }
        if let Some(over) = object.get("over") {
            return self.lower_over(object, &interpolate(over, bindings)?, bindings, stack);
        }
        let mut node = Map::new();
        for (key, value) in object {
            // A planner catalogue keeps its own placeholders for run time.
            let value = if bindings.is_empty() || key == "plan" {
                value.clone()
            } else {
                interpolate(value, bindings)?
            };
            node.insert(key.clone(), value);
        }
        if object.contains_key("use") || !stack.is_empty() {
            check_authored_fields(
                &node,
                node.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>"),
                true,
            )?;
        }
        if let Some(repeat) = node.remove("repeat") {
            return self.lower_repeat(node, repeat, stack);
        }
        if let Some(name) = node.remove("use") {
            let name = name
                .as_str()
                .ok_or("`use` must name a template")?
                .to_owned();
            if stack.contains(&name) {
                return Err(format!(
                    "template {name} uses itself: {} > {name}",
                    stack.join(" > ")
                )
                .into());
            }
            let template = self.library.resolve(&name)?;
            let mut with = node.remove("with").unwrap_or(Value::Null);
            if let Some(values) = with.as_object_mut() {
                // A string parameter can bind one launch-time resolver value.
                // Table parameters retain their literal structure.
                for (key, value) in values.iter_mut() {
                    if !template
                        .parameters
                        .get(key)
                        .is_some_and(|parameter| parameter.kind == ParameterKind::String)
                    {
                        continue;
                    }
                    if let Some(resolver) = value
                        .as_object()
                        .filter(|object| object.len() == 1)
                        .and_then(|object| object.get("resolver"))
                        .and_then(Value::as_str)
                    {
                        let items = self.resolvers.get(resolver).ok_or_else(|| {
                            format!("resolver @{resolver} is not available at this launch")
                        })?;
                        if items.len() != 1 {
                            return Err(format!(
                                "resolver @{resolver} must return one parameter value"
                            )
                            .into());
                        }
                        *value = Value::String(items[0].clone());
                    }
                }
            }
            let mut parameters = template.bind(&name, &with).map_err(|mut refusal| {
                refusal.node = node.get("id").and_then(Value::as_str).map(str::to_owned);
                CompileError::Parameter(refusal)
            })?;
            // The surrounding `over` binds item throughout the instantiated
            // template, even when it is not a declared input parameter.
            if let Some(item) = bindings.get(ITEM) {
                parameters.entry(ITEM).or_insert_with(|| item.clone());
            }
            if template.composite_behaviour == Some(CompositeBehaviour::ReviewRepair)
                && with.get("lenses").is_none()
                && let Some(lenses) = self.resolvers.get("review-panel")
            {
                parameters.insert("lenses".into(), serde_json::json!(lenses));
            }
            stack.push(name.clone());
            let lowered = self.instantiate(&name, &template, node, &parameters, stack);
            stack.pop();
            return lowered;
        }
        if node.contains_key("with") {
            return Err("`with` binds the parameters of a `use`d template".into());
        }
        self.finish(node, stack)
    }

    /// `over` copies the node once per item inside a composite boundary, which
    /// keeps the node's identity, dependencies and inputs.
    fn lower_over(
        &self,
        object: &Map<String, Value>,
        over: &Value,
        bindings: &Map<String, Value>,
        stack: &mut Vec<String>,
    ) -> Result<Value, CompileError> {
        let items = self.items(over)?;
        let mut copy = object.clone();
        copy.remove("over");
        let mut boundary = Map::new();
        for key in [
            "id",
            "title",
            "after",
            "depends_on",
            "inputs",
            "source",
            "read_roots",
        ] {
            if let Some(value) = copy.remove(key) {
                let value = if bindings.is_empty() {
                    value
                } else {
                    interpolate(&value, bindings)?
                };
                boundary.insert(key.into(), value);
            }
        }
        let min_ok = copy
            .remove("min_ok")
            .map(|value| interpolate(&value, bindings))
            .transpose()?;
        let reduce_verdict = copy.remove("reduce_verdict");
        // A used template that declares `item` receives it unless set.
        if let Some(name) = copy.get("use").and_then(Value::as_str)
            && self
                .library
                .resolve(name)
                .is_ok_and(|template| template.parameters.contains_key(ITEM))
        {
            let with = copy
                .entry("with")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(with) = with.as_object_mut() {
                with.entry(ITEM)
                    .or_insert_with(|| Value::String(format!("${{{ITEM}}}")));
            }
        }
        let mut ids = BTreeSet::new();
        let mut children = Vec::with_capacity(items.len());
        let panel_routes = if over.as_str() == Some("@review-panel")
            && copy
                .get("use")
                .and_then(Value::as_str)
                .and_then(|name| self.library.resolve(name).ok())
                .is_some_and(|template| template.review == Some(ReviewBehaviour::Lens))
        {
            self.resolvers.get("review-panel-routes")
        } else {
            None
        };
        if panel_routes.is_some_and(|routes| routes.len() != items.len()) {
            return Err("review panel routes do not match its lenses".into());
        }
        for (index, item) in items.iter().enumerate() {
            let mut scoped = bindings.clone();
            scoped.insert(ITEM.into(), Value::String(item.clone()));
            let mut member = copy.clone();
            if let Some(routes) = panel_routes {
                let fields: Map<String, Value> = serde_json::from_str(&routes[index])
                    .map_err(|_| "invalid review panel route")?;
                if fields.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "route"
                            | "routes"
                            | "family_group"
                            | "distinct_from"
                            | "distinct_strength"
                            | "require_findings"
                            | "required_verdict"
                    )
                }) || fields.contains_key("route") == fields.contains_key("routes")
                {
                    return Err("invalid review panel route fields".into());
                }
                member.remove("family_group");
                member.extend(fields);
            }
            member.insert("id".into(), Value::String(item_id(item, &mut ids)));
            let mut child = self.lower_in(&Value::Object(member), &scoped, stack)?;
            if !Value::Object(copy.clone()).to_string().contains("${item}")
                && copy
                    .get("use")
                    .and_then(Value::as_str)
                    .and_then(|name| self.library.resolve(name).ok())
                    .is_some_and(|template| !template.parameters.contains_key(ITEM))
                && let Some(prompt) = child
                    .get("prompt")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            {
                child["prompt"] = Value::String(format!("{prompt}\n\nItem: {item}"));
            }
            children.push(child);
        }
        let exports = default_exports(&children);
        boundary.insert("kind".into(), Value::String("composite".into()));
        boundary.insert("access".into(), Value::String("read_only".into()));
        let mut composite = Map::new();
        composite.insert("nodes".into(), Value::Array(children));
        composite.insert("exports".into(), Value::Array(exports));
        if let Some(min_ok) = min_ok {
            composite.insert("min_successful".into(), min_ok);
        }
        if let Some(reduce_verdict) = reduce_verdict {
            composite.insert("reduce_verdict".into(), reduce_verdict);
        }
        boundary.insert("composite".into(), Value::Object(composite));
        self.finish(boundary, stack)
    }

    fn items(&self, over: &Value) -> Result<Vec<String>, String> {
        let items: Vec<String> = match over {
            Value::Array(values) => values
                .iter()
                .map(|value| match value {
                    Value::String(text) => Ok(text.clone()),
                    Value::Number(number) => Ok(number.to_string()),
                    _ => Err("`over` items must be strings"),
                })
                .collect::<Result<_, _>>()?,
            Value::String(text) => {
                let name = text
                    .strip_prefix('@')
                    .ok_or("`over` takes a list or a resolver named `@name`")?;
                self.resolvers
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("resolver @{name} is not available at this launch"))?
            }
            _ => return Err("`over` takes a list or a resolver named `@name`".into()),
        };
        if items.is_empty() || items.len() > MAX_OVER_ITEMS {
            return Err(format!("`over` requires 1..={MAX_OVER_ITEMS} items"));
        }
        if items
            .iter()
            .any(|item| item.trim().is_empty() || item.contains('\0'))
        {
            return Err("`over` items must be non-empty text".into());
        }
        Ok(items)
    }

    /// `repeat` turns the node into a loop whose body is the node's members,
    /// or the node itself for a leaf, until the named body gate passes.
    fn lower_repeat(
        &self,
        mut node: Map<String, Value>,
        repeat: Value,
        stack: &mut Vec<String>,
    ) -> Result<Value, CompileError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Repeat {
            until: String,
            max_rounds: usize,
            #[serde(default)]
            max_extension_rounds: Option<usize>,
            #[serde(default)]
            review_repair: bool,
        }
        let repeat: Repeat =
            serde_json::from_value(repeat).map_err(|error| format!("invalid repeat: {error}"))?;
        let review_repair = repeat.review_repair
            || node
                .get("use")
                .and_then(Value::as_str)
                .and_then(|name| self.library.resolve(name).ok())
                .is_some_and(|template| {
                    template.composite_behaviour == Some(CompositeBehaviour::Repair)
                });
        let mut boundary = Map::new();
        for key in [
            "id",
            "title",
            "after",
            "depends_on",
            "inputs",
            "skip_without_findings",
            "on_fail",
        ] {
            if let Some(value) = node.remove(key) {
                boundary.insert(key.into(), value);
            }
        }
        let id = boundary.get("id").cloned().unwrap_or(Value::Null);
        node.insert("id".into(), id);
        let lowered = self.lower_in(&Value::Object(node), &Map::new(), stack)?;
        let body = match lowered.pointer("/composite/nodes") {
            Some(Value::Array(members)) => members.clone(),
            _ => vec![lowered],
        };
        let mut spec = Map::new();
        spec.insert("body".into(), Value::Array(body));
        spec.insert("until".into(), Value::String(repeat.until));
        spec.insert("max_rounds".into(), repeat.max_rounds.into());
        if review_repair {
            if repeat.max_rounds == 0 {
                return Err("review repair cycles must be at least 1".into());
            }
            spec.insert("review_repair".into(), Value::Bool(true));
        }
        if let Some(rounds) = repeat.max_extension_rounds {
            spec.insert("max_extension_rounds".into(), rounds.into());
        }
        boundary.insert("kind".into(), Value::String("loop".into()));
        boundary.insert("access".into(), Value::String("read_only".into()));
        boundary.insert("loop".into(), Value::Object(spec));
        self.finish(boundary, stack)
    }

    fn instantiate(
        &self,
        name: &str,
        template: &Template,
        mut overrides: Map<String, Value>,
        parameters: &Map<String, Value>,
        stack: &mut Vec<String>,
    ) -> Result<Value, CompileError> {
        let stage = overrides
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        check_authored_fields(&template.node, stage, false)?;
        check_authored_fields(&overrides, stage, true)?;
        if template.is_composite() {
            if let Some(key) = ["brief", "goal", "prompt", "tier", "route", "routes"]
                .into_iter()
                .find(|key| overrides.contains_key(*key))
            {
                return Err(format!(
                    "node {} overrides {key}, but template {name} is a composite; set it on its nodes",
                    overrides
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("<unknown>")
                ).into());
            }
            let mut children = Vec::with_capacity(template.nodes.len());
            for member in &template.nodes {
                children.push(self.lower_in(member, parameters, stack)?);
            }
            if template.composite_behaviour == Some(CompositeBehaviour::ReviewPanel)
                && overrides.get("repair_panel") == Some(&Value::Bool(true))
            {
                // Findings are a completed review, not an unavailable reviewer.
                // The enclosing repair gate adjudicates the panel verdict.
                fn collect_findings(value: &mut Value) {
                    match value {
                        Value::Object(fields) => {
                            fields.remove("required_verdict");
                            for child in fields.values_mut() {
                                collect_findings(child);
                            }
                        }
                        Value::Array(items) => {
                            for child in items {
                                collect_findings(child);
                            }
                        }
                        _ => {}
                    }
                }
                for child in &mut children {
                    collect_findings(child);
                }
            }

            if template.composite_behaviour == Some(CompositeBehaviour::Repair)
                && parameters.get("writer").and_then(Value::as_str) == Some("")
            {
                // A standalone or integration repair has no PR writer to continue.
                if let Some(fix) = children.first_mut().and_then(Value::as_object_mut) {
                    fix.remove("continues");
                }
            }
            if template.composite_behaviour == Some(CompositeBehaviour::Repair)
                && let Some(branch) = parameters
                    .get("integration_branch")
                    .and_then(Value::as_str)
                    .filter(|branch| !branch.is_empty())
            {
                let fix = children
                    .first_mut()
                    .and_then(Value::as_object_mut)
                    .ok_or("repair behaviour requires a writer node")?;
                fix.insert(
                    "delivery".into(),
                    Value::String(format!("integrate:{branch}")),
                );
            }
            if template.composite_behaviour == Some(CompositeBehaviour::ReviewRepair) {
                // Bind a host-authored slice to the same literal write authority.
                // The read-only continuation planner cannot invent writer authority.
                let mut fix = children
                    .first()
                    .and_then(|node| node.pointer("/loop/body/0"))
                    .filter(|node| node.is_object())
                    .cloned()
                    .ok_or("review_repair behaviour requires a loop with a writer node")?;
                let continuation = children
                    .get_mut(2)
                    .and_then(Value::as_object_mut)
                    .ok_or("review_repair behaviour requires a continuation planner at node 3")?;
                let plan = continuation.entry("plan").or_insert(Value::Null);
                if plan.is_null() {
                    *plan = Value::Object(Map::new());
                }
                if !plan.is_object() {
                    return Err("review_repair continuation plan must be a table".into());
                }
                fix["id"] = Value::String("fix".into());
                fix.as_object_mut()
                    .ok_or("repair writer must be a node")?
                    .remove("inputs");
                // The principal supplies a new acceptance brief, not the
                // previous round's grouped fix items.
                fix.as_object_mut()
                    .ok_or("repair writer must be a node")?
                    .remove("repair_writer");
                fix["acceptance_checks"] = serde_json::json!([
                    "Meet the principal's acceptance evidence within the approved scope"
                ]);
                let prompt = fix
                    .as_object_mut()
                    .ok_or("repair writer must be a node")?
                    .remove("prompt");
                let access = fix
                    .as_object_mut()
                    .ok_or("repair writer must be a node")?
                    .remove("access");
                fix.as_object_mut()
                    .ok_or("repair writer must be a node")?
                    .remove("id");
                // Re-authoring a compiled writer must retain its provenance as
                // explicit metadata, rather than as unknown authored fields.
                preserve_compiled_metadata(&mut fix)?;
                children[2]["plan"]["max_nodes"] = serde_json::json!(32);
                children[2]["plan"]["patterns"] = serde_json::json!([{
                    "name": "repair-slice", "brief": prompt, "access": access, "node": fix
                }]);
                children[2]["plan"]["wire_exports"] = Value::Bool(true);
                let mut resolvers = self.resolvers.clone();
                let lenses = parameters
                    .get("lenses")
                    .and_then(Value::as_array)
                    .ok_or("repair lenses must be a list")?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or("repair lens must be a string")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                for lens in &lenses {
                    resolvers
                        .entry(format!("review-advice-{lens}"))
                        .or_insert_with(|| vec![String::new()]);
                }
                resolvers.insert("review-panel".into(), lenses);
                children[2]["plan"]["resolvers"] = serde_json::json!(resolvers);
            }
            let exports = if template.exports.is_empty() {
                default_exports(&children)
            } else {
                serde_json::to_value(&template.exports)
                    .ok()
                    .and_then(|value| value.as_array().cloned())
                    .unwrap_or_default()
            };
            let mut composite = Map::new();
            composite.insert("nodes".into(), Value::Array(children));
            composite.insert("exports".into(), Value::Array(exports));
            if let Some(min_ok) = overrides
                .remove("min_ok")
                .or_else(|| template.min_ok.map(Value::from))
            {
                composite.insert("min_successful".into(), min_ok);
            }
            if let Some(reduce) = overrides
                .remove("reduce_verdict")
                .or_else(|| template.reduce_verdict.map(Value::from))
            {
                composite.insert("reduce_verdict".into(), reduce);
            }
            let mut boundary = Map::new();
            boundary.insert("kind".into(), Value::String("composite".into()));
            boundary.insert("access".into(), Value::String("read_only".into()));
            boundary.insert("composite".into(), Value::Object(composite));
            boundary.extend(overrides);
            return self.finish(boundary, stack);
        }
        if overrides.contains_key("min_ok") {
            return Err(format!(
                "min_ok applies to a composite or an `over` node, not leaf template {name}"
            )
            .into());
        }
        let mut leaf = Map::new();
        for (key, value) in &template.node {
            leaf.insert(key.clone(), interpolate(value, parameters)?);
        }
        check_authored_fields(&leaf, stage, false)?;
        if let Some(brief) = &template.brief {
            leaf.insert(
                "prompt".into(),
                interpolate(&Value::String(brief.clone()), parameters)?,
            );
        }
        if let Some(tier) = &template.tier {
            leaf.insert(
                "tier".into(),
                interpolate(&Value::String(tier.clone()), parameters)?,
            );
        }
        leaf.insert(TEMPLATE_FIELD.into(), Value::String(name.to_owned()));
        leaf.insert(
            "access".into(),
            serde_json::to_value(template.access.unwrap_or(AccessMode::ReadOnly))
                .map_err(|error| error.to_string())?,
        );
        if let Some(kind) = template.kind {
            leaf.insert(
                "kind".into(),
                serde_json::to_value(kind).map_err(|error| error.to_string())?,
            );
        }
        if template.outputs.contains(&OutputKind::Findings) {
            leaf.insert("require_findings".into(), Value::Bool(true));
        }
        // Node overrides win; a brief override replaces the template's.
        for key in ["brief", "goal", "prompt"] {
            if let Some(brief) = overrides.remove(key) {
                leaf.insert("prompt".into(), brief);
            }
        }
        if overrides.contains_key("route") || overrides.contains_key("routes") {
            leaf.remove("tier");
        }
        merge_node_fields(&mut leaf, &overrides);
        if template.brief.is_some()
            && !leaf.contains_key("acceptance_checks")
            && let Some(brief) = leaf.get("prompt").cloned()
        {
            leaf.insert("acceptance_checks".into(), Value::Array(vec![brief]));
        }
        flatten_extensions(&mut leaf)?;
        if let Some(review) = template.review {
            review.apply(&mut leaf);
        }
        self.finish(leaf, stack)
    }

    /// Normalise node spelling and lower nested members.
    fn finish(
        &self,
        mut node: Map<String, Value>,
        stack: &mut Vec<String>,
    ) -> Result<Value, CompileError> {
        for pointer in ["composite", "loop"] {
            let key = if pointer == "composite" {
                "nodes"
            } else {
                "body"
            };
            if let Some(members) = node
                .get_mut(pointer)
                .and_then(|spec| spec.get_mut(key))
                .and_then(Value::as_array_mut)
            {
                for member in members.iter_mut() {
                    if needs_lowering(member) {
                        *member = self.lower_in(member, &Map::new(), stack)?;
                    }
                }
            }
        }
        self.finish_shallow(node)
    }

    fn finish_shallow(&self, mut node: Map<String, Value>) -> Result<Value, CompileError> {
        flatten_extensions(&mut node)?;
        if let Some(after) = node.remove("after") {
            let after = after.as_array().ok_or("`after` must be a list")?;
            let depends = node
                .entry("depends_on")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or("depends_on must be a list")?;
            for parent in after {
                if !depends.contains(parent) {
                    depends.push(parent.clone());
                }
            }
        }
        if let Some(tier) = node.remove("tier") {
            if node.contains_key("route") || node.contains_key("routes") {
                return Err("a node sets either `tier` or `route`, not both".into());
            }
            let tier = tier.as_str().ok_or("`tier` must name a route alias")?;
            node.insert("route".into(), serde_json::json!({"alias": tier}));
        }
        for alias in ["brief", "goal"] {
            if let Some(brief) = node.remove(alias) {
                if node.contains_key("prompt") {
                    return Err(format!("a node sets `{alias}` and `prompt`; keep one").into());
                }
                node.insert("prompt".into(), brief);
            }
        }
        Ok(Value::Object(node))
    }
}

/// Raw passthrough nodes (as presets lower them) stay byte-identical.
fn needs_lowering(node: &Value) -> bool {
    node.as_object().is_some_and(|object| {
        [
            "use",
            "over",
            "repeat",
            "after",
            "tier",
            "with",
            "extensions",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
            || object
                .get("composite")
                .and_then(|spec| spec.get("nodes"))
                .and_then(Value::as_array)
                .is_some_and(|members| members.iter().any(needs_lowering))
            || object
                .get("loop")
                .and_then(|spec| spec.get("body"))
                .and_then(Value::as_array)
                .is_some_and(|members| members.iter().any(needs_lowering))
    })
}

/// Every member no sibling depends on exports its result, and its findings
/// when it is required to report them.
fn default_exports(children: &[Value]) -> Vec<Value> {
    let consumed: BTreeSet<&str> = children
        .iter()
        .flat_map(|child| {
            let depends = child
                .get("depends_on")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str);
            let inputs = child
                .get("inputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|input| input.get("node").and_then(Value::as_str));
            depends.chain(inputs).collect::<Vec<_>>()
        })
        .collect();
    children
        .iter()
        .filter_map(|child| child.get("id").and_then(Value::as_str))
        .filter(|id| !consumed.contains(id))
        .map(|id| {
            let findings = children
                .iter()
                .find(|child| child.get("id").and_then(Value::as_str) == Some(id))
                .is_some_and(|child| {
                    child.get("require_findings") == Some(&Value::Bool(true))
                        || child
                            .pointer("/composite/exports")
                            .and_then(Value::as_array)
                            .is_some_and(|exports| {
                                exports.iter().any(|export| {
                                    export.get("outputs").and_then(Value::as_array).is_some_and(
                                        |outputs| {
                                            outputs
                                                .iter()
                                                .any(|output| output.as_str() == Some("findings"))
                                        },
                                    )
                                })
                            })
                });
            let outputs = if findings {
                serde_json::json!(["result", "findings"])
            } else {
                serde_json::json!(["result"])
            };
            serde_json::json!({"node": id, "outputs": outputs})
        })
        .collect()
}

/// A node ID for one `over` item: path separators and dots become hyphens.
fn item_id(item: &str, taken: &mut BTreeSet<String>) -> String {
    let mut id = String::new();
    for character in item.chars() {
        let character = if character.is_ascii_alphanumeric() || character == '_' {
            character
        } else {
            '-'
        };
        if !(character == '-' && (id.is_empty() || id.ends_with('-'))) {
            id.push(character);
        }
        if id.len() >= 32 {
            break;
        }
    }
    let base = id.trim_end_matches('-').to_owned();
    let base = if base.is_empty() {
        "item".to_owned()
    } else {
        base
    };
    let mut candidate = base.clone();
    let mut suffix = 2;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

fn closed_read_roots(template: &Template, node: &Value) -> Result<(), String> {
    const REFUSED: &str = "template read roots must be a bare parameter over absolute paths";
    let Some(roots) = node.get("read_roots").and_then(Value::as_array) else {
        return Err(REFUSED.into());
    };
    for root in roots {
        let text = root.as_str().ok_or(REFUSED)?;
        if !text.contains("${") {
            continue;
        }
        let name = text
            .strip_prefix("${")
            .and_then(|rest| rest.strip_suffix('}'))
            .filter(|name| !name.contains(['$', '{', '}']))
            .ok_or(REFUSED)?;
        let parameter = template.parameters.get(name).ok_or(REFUSED)?;
        for value in &parameter.allowed {
            let path = std::path::Path::new(value.as_str().ok_or(REFUSED)?);
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
                || path.to_string_lossy().contains(['\0', '$'])
            {
                return Err(REFUSED.into());
            }
        }
    }
    Ok(())
}

pub(super) fn placeholders(value: &Value, names: &mut Vec<String>) -> Result<(), String> {
    match value {
        Value::String(text) => {
            let mut remaining = text.as_str();
            while let Some((_, rest)) = remaining.split_once("${") {
                let (name, after) = rest
                    .split_once('}')
                    .ok_or("unclosed template placeholder")?;
                names.push(name.to_owned());
                remaining = after;
            }
        }
        Value::Array(values) => {
            for value in values {
                placeholders(value, names)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                // Planner catalogues carry placeholders bound at run time.
                if key != "plan" {
                    placeholders(value, names)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn check_value(parameter: &TemplateParameter, value: &Value) -> Result<(), String> {
    let fits = match parameter.kind {
        ParameterKind::String => value.is_string(),
        ParameterKind::Integer => value.is_i64() || value.is_u64(),
        ParameterKind::Number => value.is_number(),
        ParameterKind::Boolean => value.is_boolean(),
        ParameterKind::List => value.is_array(),
        ParameterKind::Table => value.is_object(),
    };
    if !fits {
        return Err(format!("must be {}", parameter.kind.name()));
    }
    let size = match value {
        Value::Number(number) => number.as_f64(),
        Value::Array(list) => Some(list.len() as f64),
        Value::String(text) => Some(text.chars().count() as f64),
        _ => None,
    };
    let (min, max) = (
        parameter.min.as_ref().and_then(Value::as_f64),
        parameter.max.as_ref().and_then(Value::as_f64),
    );
    if let Some(size) = size {
        match (min, max) {
            (Some(min), Some(max)) if size < min || size > max => {
                return Err(format!("must be {min} to {max}"));
            }
            (Some(min), None) if size < min => return Err(format!("must be at least {min}")),
            (None, Some(max)) if size > max => return Err(format!("must be at most {max}")),
            _ => {}
        }
    }
    if !parameter.allowed.is_empty() && !parameter.allowed.contains(value) {
        return Err(format!(
            "must be one of {}, received {value}",
            Value::Array(parameter.allowed.clone())
        ));
    }
    Ok(())
}

/// Replace `${name}` placeholders. A string that is exactly one placeholder
/// takes the value's type, so a list parameter can fill a list field. An
/// unbound `${item}` is left for an enclosing `over` to fill.
pub(super) fn interpolate(value: &Value, parameters: &Map<String, Value>) -> Result<Value, String> {
    match value {
        Value::String(text) => {
            if let Some(name) = text
                .strip_prefix("${")
                .and_then(|rest| rest.strip_suffix('}'))
                && !name.contains(['{', '}'])
            {
                return match parameters.get(name) {
                    Some(value) => Ok(value.clone()),
                    None if name == ITEM => Ok(value.clone()),
                    None => Err(format!("unknown template parameter: {name}")),
                };
            }
            let mut remaining = text.as_str();
            let mut rendered = String::new();
            while let Some((before, rest)) = remaining.split_once("${") {
                rendered.push_str(before);
                let (name, after) = rest
                    .split_once('}')
                    .ok_or("unclosed template placeholder")?;
                match parameters.get(name) {
                    Some(Value::String(text)) => rendered.push_str(text),
                    Some(value @ (Value::Number(_) | Value::Bool(_))) => {
                        rendered.push_str(&value.to_string());
                    }
                    Some(_) => {
                        return Err(format!(
                            "template parameter {name} must be scalar when embedded in text"
                        ));
                    }
                    None if name == ITEM => rendered.push_str("${item}"),
                    None => return Err(format!("unknown template parameter: {name}")),
                }
                remaining = after;
            }
            rendered.push_str(remaining);
            Ok(Value::String(rendered))
        }
        Value::Array(values) => values
            .iter()
            .map(|value| interpolate(value, parameters))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), interpolate(value, parameters)?)))
            .collect::<Result<Map<_, _>, String>>()
            .map(Value::Object),
        value => Ok(value.clone()),
    }
}
