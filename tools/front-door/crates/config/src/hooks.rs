//! Exact hook-leaf reconciliation; sibling bytes and array positions survive.
use crate::{HookRegistration, InstallError, Targets, managed::SelectorChange, splice};
use provenant_records::{Harness, Limits, decode_jsonc};
use serde_json::{Value, json};
use std::io::Cursor;

pub(crate) fn command(spec: &HookRegistration, targets: &Targets) -> Result<String, InstallError> {
    let binary = targets.bin_dir.join("provenant");
    let binary = binary
        .to_str()
        .ok_or(InstallError::InvalidConfig("hook executable must be UTF-8"))?;
    let state = targets
        .roots
        .state
        .as_path()
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
    let mut value = json!({"type":"command", "command":command(spec, targets)?});
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
    match path(bytes, selector)? {
        Some(path) => splice::set(
            bytes,
            &path.iter().map(String::as_str).collect::<Vec<_>>(),
            value,
        ),
        None => Ok(bytes.to_vec()),
    }
}
/// Replace exact manifest leaves in place, then append only missing registry
/// registrations. Old `remove` entries are handled by the legacy cleaner.
type Registration = (Vec<u8>, Vec<SelectorChange>, Vec<(String, Value)>);
pub(crate) fn register(bytes: &[u8], targets: &Targets) -> Result<Registration, InstallError> {
    let mut out = bytes.to_vec();
    let mut changes = Vec::new();
    let mut owned = Vec::new();
    for spec in targets
        .hook_specs
        .iter()
        .filter(|spec| spec.harness == Some(Harness::Claude))
    {
        let desired = value(spec, targets)?;
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
        let command = desired
            .get("command")
            .and_then(Value::as_str)
            .ok_or(InstallError::InvalidConfig("hook command"))?;
        let sel = selector(spec.event, spec.matcher, command)?;
        if let Some(existing) = get(&out, &sel)? {
            if existing != desired {
                return Err(InstallError::InvalidConfig(
                    "managed hook registration changed",
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
    Ok((out, changes, owned))
}
