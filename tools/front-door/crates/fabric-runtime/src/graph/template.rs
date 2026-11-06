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

/// One template document. A leaf sets `brief` (and usually `tier`); a
/// composite sets `nodes`. Nodes stay JSON until their parameters are bound,
/// because a typed list field such as `write_scope` may be one `${scope}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
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
    /// Executor for a deterministic or planning leaf (`gate`, `decide`, `land`, `plan`).
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
        if self.is_composite() && (self.brief.is_some() || self.tier.is_some()) {
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
    pub fn bind(&self, name: &str, supplied: &Value) -> Result<Map<String, Value>, String> {
        let supplied = match supplied {
            Value::Null => Map::new(),
            Value::Object(map) => map.clone(),
            _ => return Err(format!("template {name} parameters must be a table")),
        };
        if let Some(unknown) = supplied
            .keys()
            .find(|key| !self.parameters.contains_key(key.as_str()))
        {
            return Err(format!("template {name} has no parameter {unknown}"));
        }
        let mut effective = Map::new();
        for (key, parameter) in &self.parameters {
            let value = supplied
                .get(key)
                .or(parameter.default.as_ref())
                .ok_or_else(|| format!("template {name} requires parameter {key}"))?;
            check_value(parameter, value)
                .map_err(|reason| format!("template {name} parameter {key}: {reason}"))?;
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
        base.node.extend(own.node.clone());
        if own.is_composite() {
            base.nodes = own.nodes.clone();
            base.exports = own.exports.clone();
            base.brief = None;
            base.tier = None;
        }
        base.min_ok = own.min_ok.or(base.min_ok);
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
        "review.security",
        include_str!("../../assets/templates/review.security.toml"),
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
            return Err(format!("template name {name:?} is invalid"));
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
    pub fn lower(&self, node: &Value) -> Result<Value, String> {
        let lowered = self.lower_in(node, &Map::new(), &mut Vec::new())?;
        if lowered.to_string().contains("${item}") {
            return Err("${item} is bound only inside `over`".into());
        }
        Ok(lowered)
    }

    pub fn compile(&self, node: &Value) -> Result<GraphNode, String> {
        let lowered = self.lower(node)?;
        let id = lowered
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_owned();
        serde_json::from_value(lowered).map_err(|error| format!("node {id}: {error}"))
    }

    pub fn compile_all(&self, nodes: &[Value]) -> Result<Vec<GraphNode>, String> {
        nodes.iter().map(|node| self.compile(node)).collect()
    }

    fn lower_in(
        &self,
        raw: &Value,
        bindings: &Map<String, Value>,
        stack: &mut Vec<String>,
    ) -> Result<Value, String> {
        if stack.len() > MAX_TEMPLATE_DEPTH {
            return Err(format!(
                "template nesting exceeds {MAX_TEMPLATE_DEPTH} levels: {}",
                stack.join(" > ")
            ));
        }
        let object = raw.as_object().ok_or("template node must be a table")?;
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
                ));
            }
            let template = self.library.resolve(&name)?;
            let with = node.remove("with").unwrap_or(Value::Null);
            let parameters = template.bind(&name, &with)?;
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
    ) -> Result<Value, String> {
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
        let min_ok = copy.remove("min_ok");
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
        for item in &items {
            let mut scoped = bindings.clone();
            scoped.insert(ITEM.into(), Value::String(item.clone()));
            let mut member = copy.clone();
            member.insert("id".into(), Value::String(item_id(item, &mut ids)));
            children.push(self.lower_in(&Value::Object(member), &scoped, stack)?);
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
    ) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Repeat {
            until: String,
            max_rounds: usize,
            #[serde(default)]
            max_extension_rounds: Option<usize>,
        }
        let repeat: Repeat =
            serde_json::from_value(repeat).map_err(|error| format!("invalid repeat: {error}"))?;
        let mut boundary = Map::new();
        for key in [
            "id",
            "title",
            "after",
            "depends_on",
            "inputs",
            "skip_without_findings",
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
    ) -> Result<Value, String> {
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
                ));
            }
            let mut children = Vec::with_capacity(template.nodes.len());
            for member in &template.nodes {
                children.push(self.lower_in(member, parameters, stack)?);
            }
            let single_review = name == "review-panel"
                && self.library.layer(name) == Some(Layer::Bundled)
                && composites::single_review(&mut children);
            let exports = if single_review {
                vec![serde_json::json!({"node":"review", "outputs":["result", "findings", "verdict"]})]
            } else if template.exports.is_empty() {
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
            let mut boundary = Map::new();
            boundary.insert("kind".into(), Value::String("composite".into()));
            boundary.insert("access".into(), Value::String("read_only".into()));
            boundary.insert("composite".into(), Value::Object(composite));
            boundary.extend(overrides);
            return self.finish_shallow(boundary);
        }
        if overrides.contains_key("min_ok") {
            return Err(format!(
                "min_ok applies to a composite or an `over` node, not leaf template {name}"
            ));
        }
        let mut leaf = Map::new();
        for (key, value) in &template.node {
            leaf.insert(key.clone(), interpolate(value, parameters)?);
        }
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
        leaf.extend(overrides);
        self.finish(leaf, stack)
    }

    /// Normalise node spelling and lower nested members.
    fn finish(
        &self,
        mut node: Map<String, Value>,
        stack: &mut Vec<String>,
    ) -> Result<Value, String> {
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

    fn finish_shallow(&self, mut node: Map<String, Value>) -> Result<Value, String> {
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
                    return Err(format!("a node sets `{alias}` and `prompt`; keep one"));
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
        ["use", "over", "repeat", "after", "tier", "with"]
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
                        || child.get("kind").and_then(Value::as_str) == Some("composite")
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
