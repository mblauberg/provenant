//! Shared graph launch helpers: route lowering and confined project roots.
use provenant_fabric_runtime::read_roots::ReadRoots;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Lower a tier (a pool name, an exact `ADAPTER/ID[@effort]`, a route table or
/// a route list) to the `route` or `routes` entry a node carries. A read-only
/// pool name repeats once, so a usage-limited first member falls back to the
/// next resolution. Writers keep one entry. A route table that names neither
/// an alias nor an adapter and model is refused rather than dropped.
pub(crate) fn tier_route(
    id: &str,
    tier: &Value,
    effort: Option<&Value>,
    read_only: bool,
) -> Result<(&'static str, Value), String> {
    if tier.is_array() {
        if effort.is_some() {
            return Err(format!(
                "node {id}: route lists cannot be combined with effort"
            ));
        }
        return Ok(("routes", tier.clone()));
    }
    let mut route = match tier {
        Value::String(name) if !name.is_empty() => {
            if let Some((adapter, model)) = name.split_once('/') {
                let (model, embedded_effort) = model
                    .split_once('@')
                    .map_or((model, None), |(model, effort)| (model, Some(effort)));
                if adapter.is_empty() || model.is_empty() || embedded_effort == Some("") {
                    return Err(format!("node {id}: invalid exact-model tier {name}"));
                }
                if embedded_effort.is_some() && effort.is_some() {
                    return Err(format!(
                        "node {id}: exact-model tier and effort both specify effort"
                    ));
                }
                let mut route = json!({"adapter":adapter,"model":model});
                if let Some(embedded_effort) = embedded_effort {
                    route["effort"] = json!(embedded_effort);
                }
                route
            } else {
                json!({"alias":name})
            }
        }
        Value::Object(table) => {
            let named = |key: &str| table.get(key).is_some_and(|value| value.is_string());
            if !named("alias") && !(named("adapter") && named("model")) {
                return Err(format!(
                    "node {id}: route table needs an alias, or an adapter and a model"
                ));
            }
            tier.clone()
        }
        _ => {
            return Err(format!(
                "node {id}: tier must be a name, route table or route list"
            ));
        }
    };
    if let Some(effort) = effort {
        route["effort"] = effort.clone();
    }
    let alias_only = route.get("alias").is_some() && route.get("model").is_none();
    if read_only && alias_only {
        Ok(("routes", json!([route.clone(), route])))
    } else {
        Ok(("route", route))
    }
}

/// The worktrees of the project `directory` belongs to, main checkout first,
/// which a graph node may always read. Outside a repository the project is only
/// its own directory.
pub(crate) fn project_worktrees(directory: &Path) -> Vec<PathBuf> {
    let read = || -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
        let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
        let environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !key.to_str()
                    .is_some_and(|key| crate::fabric::GIT_REDIRECTS.contains(&key))
            })
            .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
            .collect();
        let executor = provenant_os::exec::SystemExecutor::new(1)?;
        Ok(
            provenant_confinement::worktrees::RepoFacts::default().discover_worktree_paths(
                directory,
                &git,
                &environment,
                &executor,
                std::time::Duration::from_secs(60),
                &provenant_core::CancelToken::new(),
            )?,
        )
    };
    read().unwrap_or_else(|_| vec![directory.to_path_buf()])
}

/// Whether a read root is exactly one `${parameter}`, with nothing around it.
pub(crate) fn bare_placeholder(text: &str) -> bool {
    text.strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(|name| !name.is_empty() && !name.contains(['$', '{', '}']))
}

/// A planner pattern names a read root by a closed-list parameter. The planner
/// can pick any listed value, so every one is confined to the project's
/// registered worktrees and allow-list now, exactly like a literal root, and a
/// root that is not exactly one parameter is refused. Each value is replaced by
/// the canonical path it confined to, so a link retargeted after admission
/// cannot move the grant, and the planner's catalogue lists the same paths.
pub(crate) fn confine_pattern_roots(node: &mut Value, policy: &ReadRoots) -> Result<(), String> {
    let Some(patterns) = node
        .pointer_mut("/plan/patterns")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    let mut renamed: Vec<(String, String)> = Vec::new();
    for pattern in patterns {
        let name = pattern
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let mut parameters = Vec::new();
        fn collect_roots<'a>(value: &'a Value, roots: &mut Vec<&'a Value>) {
            match value {
                Value::Object(map) => {
                    if let Some(items) = map.get("read_roots").and_then(Value::as_array) {
                        roots.extend(items);
                    }
                    for (key, child) in map {
                        if key != "read_roots" {
                            collect_roots(child, roots);
                        }
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        collect_roots(item, roots);
                    }
                }
                _ => {}
            }
        }
        let mut roots = Vec::new();
        collect_roots(&pattern["nodes"], &mut roots);
        for root in roots {
            let text = root.as_str().ok_or("read_roots entries must be strings")?;
            if !text.contains("${") {
                continue;
            }
            if !bare_placeholder(text) {
                return Err(format!(
                    "plan pattern {name}: read root {text} must be exactly one parameter"
                ));
            }
            parameters.push(text[2..text.len() - 1].to_owned());
        }
        for parameter in parameters {
            let values = pattern
                .pointer_mut(&format!("/parameters/{parameter}/enum"))
                .and_then(Value::as_array_mut)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| {
                    format!("plan pattern {name}: read root ${{{parameter}}} needs a closed list")
                })?;
            for value in values {
                let given = value
                    .as_str()
                    .ok_or("read root values must be strings")?
                    .to_owned();
                if !Path::new(&given).is_absolute() {
                    return Err(format!(
                        "plan pattern {name}: read root {given} must be absolute"
                    ));
                }
                let canonical = policy
                    .confine(Path::new(&given))?
                    .to_str()
                    .ok_or("read root is not UTF-8")?
                    .to_owned();
                if canonical != given {
                    renamed.push((given, canonical.clone()));
                    *value = json!(canonical);
                }
            }
            // A default that named a link follows its value.
            if let Some(default) = pattern.pointer_mut(&format!("/parameters/{parameter}/default"))
                && let Some((_, canonical)) = default
                    .as_str()
                    .and_then(|text| renamed.iter().find(|(given, _)| given == text))
            {
                *default = json!(canonical);
            }
        }
    }
    // The planner reads the same canonical paths the catalogue will accept.
    if let Some(prompt) = node.get("prompt").and_then(Value::as_str) {
        let mut text = prompt.to_owned();
        for (given, canonical) in &renamed {
            text = replace_path(&text, given, canonical);
        }
        node["prompt"] = json!(text);
    }
    Ok(())
}

/// Replace `from` with `to` where `from` is a whole path, not the tail of a
/// longer one such as `/var/x` inside `/private/var/x`.
fn replace_path(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().next_back().or(out.chars().next_back());
        let after = rest[at + from.len()..].chars().next();
        let inside = |c: char| c.is_alphanumeric() || matches!(c, '/' | '_' | '-');
        out.push_str(&rest[..at]);
        if before.is_some_and(|c| inside(c) || c == '.') || after.is_some_and(inside) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn format_family_group(group_index: usize) -> String {
    format!("group-{group_index}")
}
