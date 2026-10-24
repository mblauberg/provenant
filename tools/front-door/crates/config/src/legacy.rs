//! Compiled, finite ownership evidence for the one-time harness cut-over.
//!
//! Paths in this registry are templates rooted only at captured HOME and the
//! caller-supplied legacy checkout. It never probes either tree.
use crate::{
    managed::{HarnessTarget, InstallError, SelectorChange},
    splice,
};
use provenant_records::{CanonicalV1, FileDigest, Harness, Limits, decode_jsonc};
use serde::Deserialize;
use serde_json::{Map, Number, Value};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

const LEGACY_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../assets/install/legacy.toml"
));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyArea {
    Skills,
    Workflows,
    Instruction,
    Companion,
    GlobalInstruction,
    OpenCodeInstruction,
    GitHook,
}

/// One exact old link or instruction selector that the selected harness may
/// own. `name == ""` denotes the whole-directory link case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyLink {
    pub area: LegacyArea,
    pub name: String,
    pub target: PathBuf,
    pub retained: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u64,
    skills: Vec<SkillEntry>,
    directory_links: Vec<DirectoryLink>,
    workflows: Vec<WorkflowLink>,
    instruction_links: Vec<InstructionLink>,
    git_hooks: Vec<GitHookLink>,
    hooks: Vec<HookEntry>,
    #[serde(default)]
    mcp: Vec<McpEntry>,
    #[serde(default)]
    utility_hook_interpreters: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillEntry {
    name: String,
    source: String,
    harnesses: Vec<Harness>,
    disposition: Disposition,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryLink {
    harness: Harness,
    area: DirectoryArea,
    source: String,
    disposition: Disposition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DirectoryArea {
    Skills,
    Workflows,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowLink {
    harness: Harness,
    name: String,
    source: String,
    disposition: Disposition,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstructionLink {
    harness: Harness,
    kind: InstructionKind,
    source: String,
    disposition: Disposition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InstructionKind {
    GlobalDoctrine,
    CheckoutDoctrine,
    Companion,
    #[serde(rename = "opencode_home_doctrine")]
    OpenCodeHomeDoctrine,
    #[serde(rename = "opencode_companion")]
    OpenCodeCompanion,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHookLink {
    name: String,
    source: String,
    disposition: Disposition,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookEntry {
    id: String,
    harness: Harness,
    event: String,
    matcher: Option<String>,
    command_form: CommandForm,
    source: String,
    args: Vec<String>,
    timeout: Option<u64>,
    #[serde(rename = "disposition")]
    _disposition: Disposition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandForm {
    #[serde(rename = "python3_double_quoted")]
    DoubleQuotedPython,
    #[serde(rename = "python3_shell_quoted")]
    ShellPython,
    #[serde(rename = "absolute_python_shell_quoted")]
    RecordedInterpreter,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    Replace,
    Remove,
}
impl Disposition {
    fn retained(&self) -> bool {
        matches!(self, Self::Replace)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpEntry {
    harness: Harness,
    selector: String,
    command: String,
    command_array: bool,
    environment_field: String,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    state_directory: String,
    seat: String,
    client_label: String,
}

fn manifest() -> Result<Manifest, InstallError> {
    let parsed: Manifest = toml_edit::de::from_str(LEGACY_MANIFEST)
        .map_err(|_| InstallError::Manifest("compiled legacy registry is invalid"))?;
    if parsed.schema_version != 1 {
        return Err(InstallError::Manifest(
            "compiled legacy registry has an unsupported version",
        ));
    }
    Ok(parsed)
}

/// Digest the exact compiled manifest bytes stored in installation receipts.
pub(crate) fn manifest_digest() -> FileDigest {
    CanonicalV1::file_digest(LEGACY_MANIFEST.as_bytes())
}

/// The complete skill-name inventory, independent of the release's current
/// retained skill set.
#[cfg(test)]
pub(crate) fn skill_names() -> Result<Vec<String>, InstallError> {
    let mut names: Vec<_> = manifest()?
        .skills
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    Ok(names)
}

/// Compile only entries for the selected harness. `home` is captured once at
/// the binary edge; caller-provided target overrides remain the destination
/// authority for skill and instruction links.
pub(crate) fn legacy_links(
    home: &Path,
    checkout: &Path,
    target: &HarnessTarget,
) -> Result<Vec<LegacyLink>, InstallError> {
    let registry = manifest()?;
    let harness = target.harness;
    let mut links = Vec::new();

    for entry in registry.skills {
        if entry.harnesses.contains(&harness) {
            links.push(LegacyLink {
                area: LegacyArea::Skills,
                name: entry.name,
                target: checkout.join(checked_relative(&entry.source)?),
                retained: entry.disposition.retained(),
            });
        }
    }
    for entry in registry.directory_links {
        if entry.harness != harness {
            continue;
        }
        let area = match entry.area {
            DirectoryArea::Skills => LegacyArea::Skills,
            DirectoryArea::Workflows => LegacyArea::Workflows,
        };
        links.push(LegacyLink {
            area,
            name: String::new(),
            target: checkout.join(checked_relative(&entry.source)?),
            retained: entry.disposition.retained(),
        });
    }
    for entry in registry.workflows {
        if entry.harness == harness {
            links.push(LegacyLink {
                area: LegacyArea::Workflows,
                name: entry.name,
                target: checkout.join(checked_relative(&entry.source)?),
                retained: entry.disposition.retained(),
            });
        }
    }
    for entry in registry.instruction_links {
        if entry.harness != harness {
            continue;
        }
        let (area, name, path) = match entry.kind {
            InstructionKind::GlobalDoctrine => (
                LegacyArea::GlobalInstruction,
                "AGENTS.md",
                home.join(".agents/AGENTS.md"),
            ),
            InstructionKind::CheckoutDoctrine => (
                LegacyArea::Instruction,
                "AGENTS.md",
                checkout.join(checked_relative(&entry.source)?),
            ),
            InstructionKind::Companion => (
                LegacyArea::Companion,
                "HARNESS.md",
                checkout.join(checked_relative(&entry.source)?),
            ),
            InstructionKind::OpenCodeHomeDoctrine => (
                LegacyArea::OpenCodeInstruction,
                "home-doctrine",
                home.join(".agents/AGENTS.md"),
            ),
            InstructionKind::OpenCodeCompanion => (
                LegacyArea::OpenCodeInstruction,
                "companion",
                checkout.join(checked_relative(&entry.source)?),
            ),
        };
        links.push(LegacyLink {
            area,
            name: name.to_owned(),
            target: path,
            retained: entry.disposition.retained(),
        });
    }
    for entry in registry.git_hooks {
        links.push(LegacyLink {
            area: LegacyArea::GitHook,
            name: entry.name,
            target: checkout.join(checked_relative(&entry.source)?),
            retained: entry.disposition.retained(),
        });
    }
    Ok(links)
}

fn checked_relative(value: &str) -> Result<PathBuf, InstallError> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(InstallError::Manifest(
            "compiled legacy path is not a normalized relative path",
        ));
    }
    Ok(path.to_path_buf())
}

/// True only for a complete, exact old registration in the finite manifest.
/// `existing` is the value at the selected client's `fabric` selector.
pub(crate) fn mcp_matches(
    existing: &Value,
    harness: Harness,
    home: &Path,
    _checkout: &Path,
) -> bool {
    let Ok(registry) = manifest() else {
        return false;
    };
    let Some(spec) = registry.mcp.iter().find(|entry| entry.harness == harness) else {
        return false;
    };
    let expected_selector = match harness {
        Harness::Claude | Harness::Cursor | Harness::Agy => "mcpServers.fabric",
        Harness::Codex => "mcp_servers.fabric",
        Harness::Opencode => "mcp.fabric",
    };
    if spec.selector != expected_selector {
        return false;
    }
    let Some(object) = existing.as_object() else {
        return false;
    };
    let Ok(command) = expand_home(&spec.command, home) else {
        return false;
    };
    let Ok(state_directory) = expand_home(&spec.state_directory, home) else {
        return false;
    };
    let command_value = if spec.command_array {
        Value::Array(vec![Value::String(command)])
    } else {
        Value::String(command)
    };
    let environment = Value::Object(Map::from_iter([
        (
            "AGENT_FABRIC_STATE_DIRECTORY".to_owned(),
            Value::String(state_directory),
        ),
        (
            "AGENT_FABRIC_SEAT".to_owned(),
            Value::String(spec.seat.clone()),
        ),
        (
            "AGENT_FABRIC_CLIENT_LABEL".to_owned(),
            Value::String(spec.client_label.clone()),
        ),
    ]));

    let mut expected_len = 2usize;
    let mut matches = object.get("command") == Some(&command_value)
        && object.get(&spec.environment_field) == Some(&environment);
    if let Some(kind) = &spec.r#type {
        expected_len += 1;
        matches &= object.get("type") == Some(&Value::String(kind.clone()));
    }
    if let Some(enabled) = spec.enabled {
        expected_len += 1;
        matches &= object.get("enabled") == Some(&Value::Bool(enabled));
    }
    if let Some(args) = &spec.args {
        if args == "absent_or_empty" {
            expected_len += usize::from(object.contains_key("args"));
            matches &= object
                .get("args")
                .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty));
        } else if args == "absent" {
            matches &= !object.contains_key("args");
        } else {
            return false;
        }
    }
    matches && object.len() == expected_len
}

fn expand_home(template: &str, home: &Path) -> Result<String, InstallError> {
    let home = home.to_str().ok_or(InstallError::InvalidConfig(
        "legacy home path must be UTF-8",
    ))?;
    let Some(suffix) = template.strip_prefix("{home}") else {
        return Err(InstallError::Manifest(
            "compiled legacy MCP path has an unknown template",
        ));
    };
    Ok(format!("{home}{suffix}"))
}

/// Remove exact installer-owned Claude hook leaves, preserving all unrelated
/// entries, comments, key order, and array order. Other harnesses have no
/// fixture-backed legacy command registrations in this registry.
pub(crate) fn clean_hooks(
    bytes: &[u8],
    harness: Harness,
    _home: &Path,
    checkout: &Path,
) -> Result<(Vec<u8>, Vec<SelectorChange>), InstallError> {
    if harness != Harness::Claude {
        return Ok((bytes.to_vec(), Vec::new()));
    }
    let value: Value = decode_jsonc(Cursor::new(bytes), Limits::CONFIG)?;
    let Some(root) = value.as_object() else {
        return Err(InstallError::InvalidConfig(
            "Claude settings root must be an object",
        ));
    };
    let Some(hooks_value) = root.get("hooks") else {
        return Ok((bytes.to_vec(), Vec::new()));
    };
    let Some(hooks) = hooks_value.as_object() else {
        return Err(InstallError::InvalidConfig(
            "Claude hooks must be an object",
        ));
    };
    let registry = manifest()?;
    let entries: Vec<_> = registry
        .hooks
        .iter()
        .filter(|entry| entry.harness == harness)
        .collect();
    // No path/interpreter inference establishes ownership. An unrecorded
    // command form at this compiled event/matcher blocks cut-over unchanged.
    if registry.utility_hook_interpreters.is_empty()
        && let Some(groups) = hooks.get("PostToolUse").and_then(Value::as_array)
        && groups.iter().any(|g| {
            g.get("matcher").and_then(Value::as_str) == Some("Bash")
                && g.get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|h| !h.is_empty())
        })
    {
        return Err(InstallError::LegacyUnresolved(
            "PostToolUse/Bash requires a recorded utility-hook interpreter",
        ));
    }
    let mut operations = Vec::new();

    for event in entries
        .iter()
        .map(|entry| entry.event.as_str())
        .collect::<Vec<_>>()
    {
        if operations
            .iter()
            .any(|operation: &Removal| operation.event == event)
        {
            continue;
        }
        let Some(event_value) = hooks.get(event) else {
            continue;
        };
        let Some(groups) = event_value.as_array() else {
            return Err(InstallError::InvalidConfig(
                "Claude hook event value must be an array",
            ));
        };
        for (group_index, group) in groups.iter().enumerate() {
            let Some(group_object) = group.as_object() else {
                return Err(InstallError::InvalidConfig(
                    "Claude hook group must be an object",
                ));
            };
            let group_matcher = match group_object.get("matcher") {
                None => None,
                Some(Value::String(value)) => Some(value.as_str()),
                _ => {
                    return Err(InstallError::InvalidConfig(
                        "Claude hook matcher must be a string",
                    ));
                }
            };
            let expected_for_group: Vec<_> = entries
                .iter()
                .filter(|entry| entry.event == event && entry.matcher.as_deref() == group_matcher)
                .collect();
            if expected_for_group.is_empty() {
                continue;
            }
            let strict_group = (group_object.len() == 1 && group_object.contains_key("hooks"))
                || (group_object.len() == 2
                    && group_object.contains_key("hooks")
                    && group_object.contains_key("matcher"));
            let Some(group_hooks) = group_object.get("hooks").and_then(Value::as_array) else {
                return Err(InstallError::InvalidConfig(
                    "Claude hook group hooks value must be an array",
                ));
            };
            let mut matched = Vec::new();
            for (hook_index, hook_value) in group_hooks.iter().enumerate() {
                if hook_value.as_object().is_none() {
                    return Err(InstallError::InvalidConfig(
                        "Claude hook entry must be an object",
                    ));
                }
                for entry in &expected_for_group {
                    let mut found = false;
                    for command in
                        hook_commands(entry, checkout, &registry.utility_hook_interpreters)?
                    {
                        let expected = hook_value_for(entry, command);
                        if hook_value.get("command") == expected.get("command")
                            && hook_value != &expected
                        {
                            return Err(InstallError::LegacyUnresolved(
                                "compiled hook command has changed registration fields",
                            ));
                        }
                        if hook_value == &expected {
                            matched.push((hook_index, hook_value.clone(), entry.id.clone()));
                            found = true;
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
            }
            if matched.is_empty() {
                continue;
            }
            let only_owned = matched.len() == group_hooks.len() && strict_group;
            if only_owned {
                operations.push(Removal {
                    event: event.to_owned(),
                    path: vec!["hooks".into(), event.to_owned(), group_index.to_string()],
                    selector: format!("hooks.{event}[{group_index}]"),
                    before: group.clone(),
                });
            } else {
                for (hook_index, before, id) in matched {
                    operations.push(Removal {
                        event: event.to_owned(),
                        path: vec![
                            "hooks".into(),
                            event.to_owned(),
                            group_index.to_string(),
                            "hooks".into(),
                            hook_index.to_string(),
                        ],
                        selector: format!(
                            "hooks.{event}[{group_index}].hooks[{hook_index}] ({id})"
                        ),
                        before,
                    });
                }
            }
        }
    }

    operations.sort_by(|left, right| {
        left.event
            .cmp(&right.event)
            .then_with(|| {
                right
                    .path
                    .get(2)
                    .map_or(usize::MAX, |part| numeric_component(part))
                    .cmp(
                        &left
                            .path
                            .get(2)
                            .map_or(usize::MAX, |part| numeric_component(part)),
                    )
            })
            .then_with(|| {
                let left_nested = left
                    .path
                    .get(4)
                    .map_or(usize::MAX, |part| numeric_component(part));
                let right_nested = right
                    .path
                    .get(4)
                    .map_or(usize::MAX, |part| numeric_component(part));
                right_nested.cmp(&left_nested)
            })
    });
    let mut updated = bytes.to_vec();
    let mut changes = Vec::with_capacity(operations.len());
    for operation in operations {
        let path: Vec<_> = operation.path.iter().map(String::as_str).collect();
        let current = splice::get(&updated, &path)?;
        if current.as_ref() != Some(&operation.before) {
            return Err(InstallError::InvalidConfig(
                "legacy hook changed while its removal plan was applied",
            ));
        }
        updated = splice::set(&updated, &path, None)?;
        changes.push(SelectorChange {
            selector: operation.selector,
            before: Some(operation.before),
            after: None,
        });
    }
    if !changes.is_empty() {
        let _: Value = decode_jsonc(Cursor::new(&updated), Limits::CONFIG)?;
    }
    Ok((updated, changes))
}

struct Removal {
    event: String,
    path: Vec<String>,
    selector: String,
    before: Value,
}

fn numeric_component(value: &str) -> usize {
    value.parse::<usize>().unwrap_or(usize::MAX)
}

fn hook_commands(
    entry: &HookEntry,
    checkout: &Path,
    interpreters: &[String],
) -> Result<Vec<String>, InstallError> {
    let script = checkout.join(checked_relative(&entry.source)?);
    let script = script.to_str().ok_or(InstallError::InvalidConfig(
        "legacy hook path must be UTF-8",
    ))?;
    let base = match entry.command_form {
        CommandForm::DoubleQuotedPython => {
            format!("python3 \"{}\"", double_quote(script))
        }
        CommandForm::ShellPython => format!("python3 {}", shell_quote(script)),
        CommandForm::RecordedInterpreter => {
            return interpreters
                .iter()
                .map(|interpreter| {
                    if !Path::new(interpreter).is_absolute() {
                        return Err(InstallError::Manifest(
                            "compiled legacy Python interpreter is not absolute",
                        ));
                    }
                    Ok(format!(
                        "{} {}",
                        shell_quote(interpreter),
                        shell_quote(script)
                    ))
                })
                .collect();
        }
    };
    let mut command = base;
    for arg in &entry.args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    Ok(vec![command])
}

fn double_quote(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' | '"' | '$' | '`' => {
                result.push('\\');
                result.push(character);
            }
            other => result.push(other),
        }
    }
    result
}

/// POSIX shlex.quote's exact single-token rendering used by the former
/// installer; this is a renderer, not a shell parser.
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn hook_value_for(entry: &HookEntry, command: String) -> Value {
    let mut object = Map::from_iter([
        ("type".to_owned(), Value::String("command".to_owned())),
        ("command".to_owned(), Value::String(command)),
    ]);
    if let Some(timeout) = entry.timeout {
        object.insert("timeout".to_owned(), Value::Number(Number::from(timeout)));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_freezes_all_legacy_skill_names_and_dispositions() {
        let registry = manifest().expect("compiled registry parses");
        let expected = [
            "_shared",
            "autopilot",
            "code-review",
            "d2-diagrams",
            "deliver",
            "diagnose",
            "engineering-docs",
            "engineering-writing",
            "evaluate",
            "grill-me",
            "implement",
            "legal-writing",
            "natural-writing",
            "orchestrate",
            "prototype",
            "refactor",
            "release",
            "retrospect",
            "scope",
            "session",
            "setup-repo",
            "skill-craft",
            "tdd",
            "tracker",
            "ui-ux-design",
            "work-map",
            "playwright",
            "react-performance",
            "tanstack-query",
            "typescript-clean-code",
        ];
        let mut actual: Vec<_> = registry
            .skills
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        actual.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(actual, expected);
        assert!(registry.skills.iter().all(|entry| {
            if entry.name == "_shared" {
                matches!(entry.disposition, Disposition::Remove)
            } else {
                matches!(entry.disposition, Disposition::Replace)
            }
        }));
        assert_eq!(skill_names().expect("names are available").len(), 30);
    }

    fn test_target(harness: Harness) -> HarnessTarget {
        HarnessTarget {
            harness,
            skills_dir: PathBuf::from("/tmp/home/skills"),
            mcp_file: PathBuf::from("/tmp/home/config.json"),
            hook_file: None,
            instruction: None,
        }
    }

    #[test]
    fn manifest_projects_every_link_target_for_every_harness() {
        let registry = manifest().expect("compiled registry parses");
        let home = Path::new("/tmp/home");
        let checkout = Path::new("/tmp/legacy-checkout");
        for harness in [
            Harness::Claude,
            Harness::Codex,
            Harness::Cursor,
            Harness::Agy,
            Harness::Opencode,
        ] {
            let actual =
                legacy_links(home, checkout, &test_target(harness)).expect("legacy links compile");
            let mut expected = Vec::new();
            for entry in &registry.skills {
                if entry.harnesses.contains(&harness) {
                    expected.push(LegacyLink {
                        area: LegacyArea::Skills,
                        name: entry.name.clone(),
                        target: checkout.join(checked_relative(&entry.source).expect("skill path")),
                        retained: entry.disposition.retained(),
                    });
                }
            }
            for entry in &registry.directory_links {
                if entry.harness == harness {
                    expected.push(LegacyLink {
                        area: match entry.area {
                            DirectoryArea::Skills => LegacyArea::Skills,
                            DirectoryArea::Workflows => LegacyArea::Workflows,
                        },
                        name: String::new(),
                        target: checkout
                            .join(checked_relative(&entry.source).expect("directory path")),
                        retained: entry.disposition.retained(),
                    });
                }
            }
            for entry in &registry.workflows {
                if entry.harness == harness {
                    expected.push(LegacyLink {
                        area: LegacyArea::Workflows,
                        name: entry.name.clone(),
                        target: checkout
                            .join(checked_relative(&entry.source).expect("workflow path")),
                        retained: entry.disposition.retained(),
                    });
                }
            }
            for entry in &registry.instruction_links {
                if entry.harness != harness {
                    continue;
                }
                let (area, name, target) = match entry.kind {
                    InstructionKind::GlobalDoctrine => (
                        LegacyArea::GlobalInstruction,
                        "AGENTS.md",
                        PathBuf::from(expand_home(&entry.source, home).expect("global path")),
                    ),
                    InstructionKind::CheckoutDoctrine => (
                        LegacyArea::Instruction,
                        "AGENTS.md",
                        checkout.join(checked_relative(&entry.source).expect("doctrine path")),
                    ),
                    InstructionKind::Companion => (
                        LegacyArea::Companion,
                        "HARNESS.md",
                        checkout.join(checked_relative(&entry.source).expect("companion path")),
                    ),
                    InstructionKind::OpenCodeHomeDoctrine => (
                        LegacyArea::OpenCodeInstruction,
                        "home-doctrine",
                        PathBuf::from(
                            expand_home(&entry.source, home).expect("OpenCode home path"),
                        ),
                    ),
                    InstructionKind::OpenCodeCompanion => (
                        LegacyArea::OpenCodeInstruction,
                        "companion",
                        checkout.join(
                            checked_relative(&entry.source).expect("OpenCode companion path"),
                        ),
                    ),
                };
                expected.push(LegacyLink {
                    area,
                    name: name.to_owned(),
                    target,
                    retained: entry.disposition.retained(),
                });
            }
            for entry in &registry.git_hooks {
                expected.push(LegacyLink {
                    area: LegacyArea::GitHook,
                    name: entry.name.clone(),
                    target: checkout.join(checked_relative(&entry.source).expect("Git hook path")),
                    retained: entry.disposition.retained(),
                });
            }
            assert_eq!(actual.len(), expected.len(), "{harness:?} link count");
            for link in expected {
                assert!(actual.contains(&link), "{harness:?}: {link:?}");
            }
        }
    }

    #[test]
    fn mcp_fixture_shapes_are_exact_for_all_harnesses() {
        let home = Path::new("/tmp/legacy-home");
        let home_text = home.to_str().expect("test home is UTF-8");
        let command = format!("{home_text}/.local/bin/provenant");
        let make_env = |seat: &str| {
            json!({
                "AGENT_FABRIC_STATE_DIRECTORY": format!("{home_text}/.local/state/agent-harness/fabric"),
                "AGENT_FABRIC_SEAT": seat,
                "AGENT_FABRIC_CLIENT_LABEL": seat
            })
        };
        let fixtures = [
            (
                Harness::Claude,
                json!({"type":"stdio", "command":command, "args":[], "env":make_env("claude")}),
            ),
            (
                Harness::Codex,
                json!({"command":command, "env":make_env("codex")}),
            ),
            (
                Harness::Cursor,
                json!({"command":command, "env":make_env("cursor")}),
            ),
            (
                Harness::Agy,
                json!({"command":command, "env":make_env("agy")}),
            ),
            (
                Harness::Opencode,
                json!({"type":"local", "enabled":true, "command":[command], "environment":make_env("opencode")}),
            ),
        ];
        for (harness, fixture) in fixtures {
            assert!(
                mcp_matches(&fixture, harness, home, Path::new("/tmp/old")),
                "{harness:?}"
            );
            let mut near = fixture.clone();
            if harness == Harness::Opencode {
                near["command"][0] = json!(format!("{command}-wrapper"));
            } else {
                near["command"] = json!(format!("{command}-wrapper"));
            }
            assert!(
                !mcp_matches(&near, harness, home, Path::new("/tmp/old")),
                "{harness:?} command near miss"
            );
            let mut foreign = fixture.clone();
            let env_key = if harness == Harness::Opencode {
                "environment"
            } else {
                "env"
            };
            foreign[env_key]["FOREIGN"] = json!("preserve");
            assert!(
                !mcp_matches(&foreign, harness, home, Path::new("/tmp/old")),
                "{harness:?} foreign env"
            );
        }
    }

    #[test]
    fn hook_command_rows_match_recorded_forms_and_utility_has_no_guessed_interpreter() {
        let registry = manifest().expect("compiled registry parses");
        assert!(registry.utility_hook_interpreters.is_empty());
        assert_eq!(registry.hooks.len(), 6);
        let checkout = Path::new("/tmp/old checkout");
        let expected = [
            (
                "compact_reinject",
                Some("python3 \"/tmp/old checkout/skills/session/scripts/compact_reinject.py\""),
            ),
            (
                "state_check",
                Some("python3 \"/tmp/old checkout/skills/session/scripts/state_check.py\" --hook"),
            ),
            (
                "lane_safety_guard",
                Some("python3 '/tmp/old checkout/scripts/lane_safety_guard.py'"),
            ),
            (
                "routing_context_session",
                Some("python3 '/tmp/old checkout/scripts/routing_context_hook.py'"),
            ),
            (
                "routing_context_pre_tool",
                Some("python3 '/tmp/old checkout/scripts/routing_context_hook.py' --pre-tool"),
            ),
            ("utility_output", None),
        ];
        for (id, command) in expected {
            let entry = registry
                .hooks
                .iter()
                .find(|entry| entry.id == id)
                .expect("hook row");
            let actual = hook_commands(entry, checkout, &registry.utility_hook_interpreters)
                .expect("hook command is well formed");
            match command {
                Some(expected) => assert_eq!(actual, vec![expected.to_owned()], "{id}"),
                None => assert!(
                    actual.is_empty(),
                    "{id} has no captured interpreter fixture"
                ),
            }
        }
    }

    #[test]
    fn mcp_match_requires_exact_legacy_shape_and_preserves_near_misses() {
        let home = Path::new("/tmp/legacy-home");
        let checkout = Path::new("/tmp/old-checkout");
        let old = json!({
            "type": "stdio",
            "command": "/tmp/legacy-home/.local/bin/provenant",
            "args": [],
            "env": {
                "AGENT_FABRIC_STATE_DIRECTORY": "/tmp/legacy-home/.local/state/agent-harness/fabric",
                "AGENT_FABRIC_SEAT": "claude",
                "AGENT_FABRIC_CLIENT_LABEL": "claude"
            }
        });
        assert!(mcp_matches(&old, Harness::Claude, home, checkout));
        let mut near = old.clone();
        near["command"] = json!("/tmp/legacy-home/.local/bin/provenant-wrapper");
        assert!(!mcp_matches(&near, Harness::Claude, home, checkout));
        let mut secret = old.clone();
        secret["env"]["TOKEN"] = json!("redacted-test-value");
        assert!(!mcp_matches(&secret, Harness::Claude, home, checkout));
    }

    #[test]
    fn hooks_remove_only_exact_owned_leaf_and_preserve_siblings_and_comments() {
        let checkout = Path::new("/tmp/old-checkout");
        let command = format!(
            "python3 {}",
            shell_quote(
                &checkout
                    .join("scripts/lane_safety_guard.py")
                    .to_string_lossy()
            )
        );
        let source = format!(
            "{{\n  // keep this comment\n  \"hooks\": {{\n    \"PreToolUse\": [\n      {{\"matcher\":\"Bash\",\"hooks\":[{{\"type\":\"command\",\"command\":{}}},{{\"type\":\"command\",\"command\":\"foreign\"}}]}},\n      {{\"matcher\":\"Bash\",\"hooks\":[{{\"type\":\"command\",\"command\":\"python3 '{}'\"}}]}}\n    ]\n  }}\n}}\n",
            serde_json::to_string(&command).expect("command JSON"),
            checkout.join("scripts/lane_safety_guard.py").display()
        );
        let (updated, changes) = clean_hooks(
            source.as_bytes(),
            Harness::Claude,
            Path::new("/tmp/home"),
            checkout,
        )
        .expect("exact fixture is editable");
        let updated_text = String::from_utf8(updated).expect("UTF-8 JSONC");
        assert_eq!(changes.len(), 1);
        assert!(updated_text.contains("// keep this comment"));
        assert!(updated_text.contains("foreign"));
        assert!(updated_text.contains("lane_safety_guard.py"));
    }
}
