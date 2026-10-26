//! Exact hook-leaf reconciliation; sibling bytes and array positions survive.
use crate::{HookRegistration, InstallError, Targets, managed::SelectorChange, splice};
use provenant_records::{Harness, Limits, decode_jsonc};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Cursor, path::Path};

pub(crate) fn command(spec: &HookRegistration, targets: &Targets) -> Result<String, InstallError> {
    hook_command(
        spec,
        &targets.bin_dir.join("provenant"),
        targets.roots.state.as_path(),
    )
}
fn hook_command(
    spec: &HookRegistration,
    binary: &Path,
    state: &Path,
) -> Result<String, InstallError> {
    let binary = binary
        .to_str()
        .ok_or(InstallError::InvalidConfig("hook executable must be UTF-8"))?;
    let state = state
        .to_str()
        .ok_or(InstallError::InvalidConfig("hook state root must be UTF-8"))?;
    Ok(std::iter::once(binary)
        .chain(["--state-dir", state])
        .chain(spec.command.iter().copied())
        .map(quote)
        .collect::<Vec<_>>()
        .join(" "))
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub(crate) fn value(spec: &HookRegistration, targets: &Targets) -> Result<Value, InstallError> {
    hook_value(
        spec,
        &targets.bin_dir.join("provenant"),
        targets.roots.state.as_path(),
    )
}
fn hook_value(spec: &HookRegistration, binary: &Path, state: &Path) -> Result<Value, InstallError> {
    let mut value = json!({"type":"command", "command":hook_command(spec, binary, state)?});
    if let Some(timeout) = spec.timeout {
        value["timeout"] = timeout.into();
    }
    Ok(value)
}
pub(crate) fn selector(
    event: &str,
    matcher: Option<&str>,
    command: &str,
) -> Result<String, InstallError> {
    Ok(format!(
        "hook:{}",
        serde_json::to_string(&(event, matcher, command))
            .map_err(|_| InstallError::InvalidConfig("hook selector"))?
    ))
}
/// Locate by exact identity rather than a mutable array index. Duplicate owned
/// identities are ambiguous and never confer permission to edit either one.
pub(crate) fn path(bytes: &[u8], selector: &str) -> Result<Option<Vec<String>>, InstallError> {
    let identity = selector
        .strip_prefix("hook:")
        .ok_or(InstallError::InvalidConfig("hook selector"))?;
    let (event, matcher, command): (String, Option<String>, String) =
        serde_json::from_str(identity).map_err(|_| InstallError::InvalidConfig("hook selector"))?;
    let root: Value = decode_jsonc(Cursor::new(bytes), Limits::CONFIG)?;
    let groups = root
        .get("hooks")
        .and_then(|h| h.get(&event))
        .and_then(Value::as_array);
    let mut found = None;
    for (g, group) in groups.into_iter().flatten().enumerate() {
        if group.get("matcher").and_then(Value::as_str) != matcher.as_deref() {
            continue;
        }
        for (h, leaf) in group
            .get("hooks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if leaf.get("command").and_then(Value::as_str) == Some(&command) {
                if found.is_some() {
                    return Err(InstallError::InvalidConfig("duplicate owned hook"));
                }
                found = Some(vec![
                    "hooks".into(),
                    event.clone(),
                    g.to_string(),
                    "hooks".into(),
                    h.to_string(),
                ]);
            }
        }
    }
    Ok(found)
}
pub(crate) fn get(bytes: &[u8], selector: &str) -> Result<Option<Value>, InstallError> {
    match path(bytes, selector)? {
        Some(path) => splice::get(bytes, &path.iter().map(String::as_str).collect::<Vec<_>>()),
        None => Ok(None),
    }
}
pub(crate) fn set(
    bytes: &[u8],
    selector: &str,
    value: Option<&Value>,
) -> Result<Vec<u8>, InstallError> {
    let Some(path) = path(bytes, selector)? else {
        return Ok(bytes.to_vec());
    };
    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    let mut out = splice::set(bytes, &path, value)?;
    if value.is_none() {
        let group_path = &path[..3];
        if let Some(Value::Object(group)) = splice::get(&out, group_path)?
            && group
                .keys()
                .all(|key| matches!(key.as_str(), "matcher" | "hooks"))
            && group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        {
            out = splice::set(&out, group_path, None)?;
            let event_path = &path[..2];
            if let Some(Value::Array(groups)) = splice::get(&out, event_path)?
                && groups.is_empty()
            {
                out = splice::set(&out, event_path, None)?;
            }
        }
    }
    Ok(out)
}
/// Replace exact manifest leaves in place, then append only missing registry
/// registrations. Old `remove` entries are handled by the legacy cleaner.
pub(crate) struct Registration {
    pub bytes: Vec<u8>,
    pub changes: Vec<SelectorChange>,
    pub owned: Vec<(String, Value)>,
    pub relinquished_modified: bool,
}
pub(crate) fn register(
    bytes: &[u8],
    targets: &Targets,
    inherited: &BTreeSet<String>,
) -> Result<Registration, InstallError> {
    let mut out = bytes.to_vec();
    let mut changes = Vec::new();
    let mut owned = Vec::new();
    let mut relinquished_modified = false;
    for spec in targets
        .hook_specs
        .iter()
        .filter(|spec| spec.harness == Some(Harness::Claude))
    {
        let desired = value(spec, targets)?;
        let command = desired
            .get("command")
            .and_then(Value::as_str)
            .ok_or(InstallError::InvalidConfig("hook command"))?;
        let sel = selector(spec.event, spec.matcher, command)?;
        // Inheritance refreshes recorded registrations, including changes to
        // their command path or event. It must not recreate relinquished leaves.
        let was_owned = inherited.iter().any(|selector| {
            selector.strip_prefix("hook:").is_some_and(|identity| {
                serde_json::from_str::<(String, Option<String>, String)>(identity).is_ok_and(
                    |(event, matcher, old_command)| {
                        (event == spec.event && matcher.as_deref() == spec.matcher)
                            || old_command == command
                    },
                )
            })
        });
        if !targets.hooks && !was_owned {
            continue;
        }
        if let Some(checkout) = &targets.legacy_checkout {
            let root: Value = decode_jsonc(Cursor::new(&out), Limits::CONFIG)?;
            if let Some(groups) = root
                .get("hooks")
                .and_then(|h| h.get(spec.event))
                .and_then(Value::as_array)
            {
                for (g, group) in groups.iter().enumerate() {
                    if group.get("matcher").and_then(Value::as_str) != spec.matcher {
                        continue;
                    }
                    for (h, leaf) in group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .enumerate()
                    {
                        if crate::legacy::replace_hook_matches(leaf, spec, checkout, &targets.home)?
                        {
                            let path =
                                ["hooks", spec.event, &g.to_string(), "hooks", &h.to_string()];
                            out = splice::set(&out, &path, Some(&desired))?;
                            changes.push(SelectorChange {
                                selector: path.join("."),
                                before: Some(leaf.clone()),
                                after: Some(desired.clone()),
                            });
                        }
                    }
                }
            }
        }
        if let Some(existing) = get(&out, &sel)? {
            if existing != desired {
                if !targets.hooks {
                    relinquished_modified = true;
                    continue;
                }
                return Err(InstallError::InvalidConfig(
                    "managed hook registration changed; rerun install with --hooks off to relinquish edited hooks",
                ));
            }
        } else {
            let groups = splice::get(&out, &["hooks", spec.event])?;
            let index = match groups {
                Some(Value::Array(groups)) => groups.len(),
                None => 0,
                _ => return Err(InstallError::InvalidConfig("hook event must be array")),
            };
            let mut group = json!({"hooks":[desired.clone()]});
            if let Some(matcher) = spec.matcher {
                group["matcher"] = matcher.into();
            }
            if index == 0 && splice::get(&out, &["hooks", spec.event])?.is_none() {
                out = splice::set(&out, &["hooks", spec.event], Some(&json!([group])))?;
            } else {
                out = splice::set(
                    &out,
                    &["hooks", spec.event, &index.to_string()],
                    Some(&group),
                )?;
            }
            changes.push(SelectorChange {
                selector: sel.clone(),
                before: None,
                after: Some(desired.clone()),
            });
        }
        owned.push((sel, desired));
    }
    Ok(Registration {
        bytes: out,
        changes,
        owned,
        relinquished_modified,
    })
}

/// Registrations shared by the installer, lane settings and CLI handlers.
pub const CLAUDE_HOOKS: &[HookRegistration] = &[
    HookRegistration {
        harness: Some(Harness::Claude),
        event: "PreToolUse",
        matcher: Some("Bash"),
        timeout: None,
        command: &["hook", "claude", "pre-tool-use"],
    },
    HookRegistration {
        harness: Some(Harness::Claude),
        event: "PreCompact",
        matcher: None,
        timeout: Some(5),
        command: &["hook", "claude", "pre-compact"],
    },
    HookRegistration {
        harness: Some(Harness::Claude),
        event: "SessionStart",
        matcher: Some("compact"),
        timeout: Some(5),
        command: &["hook", "claude", "compact"],
    },
];

/// An isolated settings document, containing only Provenant's own hooks.
pub fn claude_lane_settings(binary: &Path, state: &Path) -> Result<Value, InstallError> {
    let mut settings = json!({"hooks": {}});
    for spec in CLAUDE_HOOKS {
        let mut group = json!({"hooks": [hook_value(spec, binary, state)?]});
        if let Some(matcher) = spec.matcher {
            group["matcher"] = matcher.into();
        }
        settings["hooks"][spec.event] = json!([group]);
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_prunes_only_the_group_and_event_it_emptied() {
        const OWNED: &str = "'/x/provenant' 'hook' 'claude' 'pre-compact'";
        let sel = selector("PreCompact", None, OWNED).expect("selector");
        let owned = format!(r#"{{"type":"command","command":"{OWNED}"}}"#);
        let source = format!(
            r#"{{"hooks":{{"PreCompact":[{{"hooks":[]}},{{"hooks":[{owned}]}},{{"matcher":"x","hooks":[]}}],"Other":[]}}}}"#
        );
        let out = set(source.as_bytes(), &sel, None).expect("removal");
        assert_eq!(
            std::str::from_utf8(&out).expect("UTF-8"),
            r#"{"hooks":{"PreCompact":[{"hooks":[]},{"matcher":"x","hooks":[]}],"Other":[]}}"#
        );

        let source = format!(r#"{{"hooks":{{"PreCompact":[{{"hooks":[{owned}]}}],"Other":[]}}}}"#);
        let out = set(source.as_bytes(), &sel, None).expect("removal");
        assert_eq!(
            std::str::from_utf8(&out).expect("UTF-8"),
            r#"{"hooks":{"Other":[]}}"#
        );
    }
}
