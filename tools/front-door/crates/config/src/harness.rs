//! Harness-owned selectors are edited in place, never whole-config ownership.
use crate::managed::{self, InstallError, SelectorChange, Snapshot, Targets, Value};
use crate::{Harness, Release};
use provenant_records::{
    AbsolutePath, CapabilitySupport, InstallReceipt, Limits, OwnedEntry, OwnedValue,
    ResolvedHarnessTarget,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(crate) fn default_config(h: Harness, home: &Path, base: &Path) -> PathBuf {
    let Some(install) = h.spec().install else {
        return PathBuf::new();
    };
    if let Some(default_home_config) = install.default_home_config
        && base == home.join(install.home)
    {
        return home.join(default_home_config);
    }
    base.join(install.config)
}
pub(crate) fn default_hooks(h: Harness, base: &Path) -> Option<PathBuf> {
    let install = h.spec().install?;
    (install.hooks.is_supported())
        .then_some(install.hook_file)
        .flatten()
        .map(|path| base.join(path))
}
fn get_selector(bytes: &[u8], sel: &str) -> Result<Option<Json>, InstallError> {
    let limits = Limits::CONFIG;
    if sel.starts_with("hook:") {
        return crate::hooks::get(bytes, sel);
    }
    if let Some(key) = sel.strip_prefix(crate::shared_settings::PREFIX) {
        return crate::splice::get_with_limits(bytes, &[key], limits);
    }
    if let Some(expected) = sel.strip_prefix("array:") {
        let value = crate::splice::get_with_limits(bytes, &["instructions"], limits)?;
        let Some(array) = value.as_ref().and_then(Json::as_array) else {
            return Ok(None);
        };
        let found: Vec<_> = array
            .iter()
            .filter(|v| v.as_str() == Some(expected))
            .collect();
        if found.len() > 1 {
            return Err(InstallError::InvalidConfig("duplicate owned instruction"));
        }
        return Ok(found.first().map(|v| (*v).clone()));
    }
    if let Some(path) = sel.strip_prefix("json:/") {
        let path: Vec<_> = path.split('/').collect();
        return crate::splice::get_with_limits(bytes, &path, limits);
    }
    if let Some(path) = sel.strip_prefix("toml:") {
        let Some((parent, key)) = path.split_once('.') else {
            return Err(InstallError::InvalidConfig("invalid TOML selector"));
        };
        if key.contains('.') {
            return Err(InstallError::InvalidConfig("invalid TOML selector"));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| InstallError::InvalidConfig("invalid TOML harness configuration"))?;
        let document = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| InstallError::InvalidConfig("invalid TOML harness configuration"))?;
        return Ok(document
            .get(parent)
            .and_then(toml_edit::Item::as_table_like)
            .filter(|table| table.contains_key(key))
            .map(|_| json!("present")));
    }
    Err(InstallError::InvalidConfig("unknown selector"))
}
fn set_selector(bytes: &[u8], sel: &str, value: Option<&Json>) -> Result<Vec<u8>, InstallError> {
    let limits = Limits::CONFIG;
    if sel.starts_with("hook:") {
        return crate::hooks::set(bytes, sel, value);
    }
    if let Some(key) = sel.strip_prefix(crate::shared_settings::PREFIX) {
        return crate::splice::set_with_limits(bytes, &[key], value, limits);
    }
    if let Some(expected) = sel.strip_prefix("array:") {
        let array = crate::splice::get_with_limits(bytes, &["instructions"], limits)?;
        if let Some(index) = array
            .as_ref()
            .and_then(Json::as_array)
            .and_then(|a| a.iter().position(|v| v.as_str() == Some(expected)))
        {
            return crate::splice::set_with_limits(
                bytes,
                &["instructions", &index.to_string()],
                value,
                limits,
            );
        }
        return Ok(bytes.to_vec());
    }
    if let Some(path) = sel.strip_prefix("json:/") {
        let path: Vec<_> = path.split('/').collect();
        return crate::splice::set_with_limits(bytes, &path, value, limits);
    }
    if value.is_none()
        && let Some(path) = sel.strip_prefix("toml:")
    {
        let Some((parent, key)) = path.split_once('.') else {
            return Err(InstallError::InvalidConfig("invalid TOML selector"));
        };
        if key.contains('.') {
            return Err(InstallError::InvalidConfig("invalid TOML selector"));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| InstallError::InvalidConfig("invalid TOML harness configuration"))?;
        let mut document = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| InstallError::InvalidConfig("invalid TOML harness configuration"))?;
        let removed = match document.get_mut(parent) {
            Some(item) => item
                .as_table_like_mut()
                .ok_or(InstallError::InvalidConfig(
                    "invalid TOML harness configuration",
                ))?
                .remove(key)
                .is_some(),
            None => false,
        };
        return Ok(if removed {
            document.to_string().into_bytes()
        } else {
            bytes.to_vec()
        });
    }
    Err(InstallError::InvalidConfig("unknown selector"))
}

fn json_legacy_selector(harness: Harness, selector: &str) -> bool {
    harness != Harness::Codex && selector.starts_with("json:/")
}
fn toml_legacy_selector(harness: Harness, selector: &str) -> bool {
    harness == Harness::Codex && selector.starts_with("toml:")
}

fn remove_legacy_selectors(
    harness: Harness,
    bytes: &[u8],
) -> Result<(Vec<u8>, Vec<SelectorChange>), InstallError> {
    let mut updated = bytes.to_vec();
    let mut changes = Vec::new();
    for artifact in crate::LEGACY_1X_ARTIFACTS {
        let crate::LegacyArtifact::HarnessSelector {
            selector,
            disposition: crate::ArtifactDisposition::Remove,
        } = artifact
        else {
            continue;
        };
        if !json_legacy_selector(harness, selector) && !toml_legacy_selector(harness, selector) {
            continue;
        }
        let Some(before) = get_selector(&updated, selector)? else {
            continue;
        };
        updated = set_selector(&updated, selector, None)?;
        changes.push(SelectorChange {
            selector: (*selector).into(),
            before: Some(before),
            after: None,
        });
    }
    Ok((updated, changes))
}

/// Inspect declared legacy MCP names in all default adapter configs and
/// registered account configs. No receipt or 1.x state is opened.
pub fn legacy_harness_selectors(
    roots: &provenant_core::Roots,
    home: &Path,
    accounts: &[crate::AccountHome],
) -> Result<Vec<(PathBuf, &'static str)>, InstallError> {
    let harnesses: Vec<_> = provenant_records::ADAPTER_SPECS
        .iter()
        .map(|spec| spec.id)
        .filter(|harness| harness.spec().install.is_some())
        .collect();
    let mut targets = Targets::new(roots.clone(), home, &harnesses);
    targets.accounts = accounts.to_vec();
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    for target in targets
        .harnesses
        .iter()
        .cloned()
        .chain(targets.accounts.iter().map(|account| account.target(home)))
    {
        if target.config_file.as_os_str().is_empty() || !seen.insert(target.config_file.clone()) {
            continue;
        }
        let Some(Snapshot {
            value: Value::File { bytes, .. },
            ..
        }) = managed::snapshot(&target.config_file)?
        else {
            continue;
        };
        for artifact in crate::LEGACY_1X_ARTIFACTS {
            let crate::LegacyArtifact::HarnessSelector {
                selector,
                disposition: crate::ArtifactDisposition::Remove,
            } = artifact
            else {
                continue;
            };
            if (json_legacy_selector(target.harness, selector)
                || toml_legacy_selector(target.harness, selector))
                && get_selector(&bytes, selector)?.is_some()
            {
                found.push((target.config_file.clone(), *selector));
            }
        }
    }
    Ok(found)
}
fn file(snapshot: &Option<Snapshot>, path: &Path) -> Result<(Vec<u8>, u64), InstallError> {
    let limits = Limits::CONFIG;
    match snapshot {
        None => Ok((b"{}\n".to_vec(), 0o600)),
        Some(Snapshot {
            value: Value::File { bytes, mode },
            ..
        }) if bytes.len() as u64 <= limits.max_bytes => Ok((bytes.clone(), *mode)),
        _ => Err(InstallError::ConfigFile(path.into())),
    }
}
#[derive(Debug, Default)]
pub(crate) struct HarnessPlan {
    pub files: BTreeMap<PathBuf, Value>,
    pub owned: Vec<OwnedEntry>,
    pub changes: BTreeMap<PathBuf, Vec<SelectorChange>>,
    pub findings: Vec<managed::InstallFinding>,
    /// Targets whose account deliberately holds none of the managed hooks.
    pub relinquished: BTreeSet<managed::TargetId>,
}
fn capture_one(
    path: &Path,
    entries: &mut BTreeMap<PathBuf, Option<Snapshot>>,
) -> Result<Option<Snapshot>, InstallError> {
    if let Some(s) = entries.get(path) {
        return Ok(s.clone());
    }
    let s = managed::snapshot(path)?;
    entries.insert(path.into(), s.clone());
    Ok(s)
}

pub(crate) fn capture(
    release: &Release,
    targets: &Targets,
    entries: &mut BTreeMap<PathBuf, Option<Snapshot>>,
    previous: Option<&InstallReceipt>,
) -> Result<HarnessPlan, InstallError> {
    let mut plan = HarnessPlan::default();
    plan.findings.extend(capability_findings(targets)?);
    let root = managed::release_root(
        targets.roots.state.as_path(),
        release.manifest().source_digest,
    );
    for target in &targets.harnesses {
        let path = &target.config_file;
        if !path.as_os_str().is_empty() {
            let snap = capture_one(path, entries)?;
            if let Some(Snapshot {
                value: Value::File { bytes, mode },
                ..
            }) = &snap
            {
                let (updated, changes) = remove_legacy_selectors(target.harness, bytes)?;
                if updated != *bytes {
                    plan.files.insert(
                        path.clone(),
                        Value::File {
                            bytes: updated,
                            mode: *mode,
                        },
                    );
                    plan.changes
                        .entry(path.clone())
                        .or_default()
                        .extend(changes);
                }
            }
        }
        // Hooks are one choice for every target of a harness. A target whose
        // account directory has none yet follows the targets that do.
        let inherited_hooks = !targets.remove_hooks
            && previous.is_some_and(|receipt| {
                receipt.entries.iter().any(|entry| {
                    entry.harness == Some(target.harness)
                        && (entry.selector.starts_with("hook:") || entry.selector == "hook-plugin")
                })
            });
        let hooks = targets.hooks || inherited_hooks;
        let was_relinquished = previous.is_some_and(|receipt| {
            receipt.harness_targets.iter().any(|recorded| {
                recorded.hooks_relinquished
                    && managed::recorded_id(recorded).is_ok_and(|id| id == target.id())
            })
        });
        // An explicit `--hooks on` or off ends a relinquishment; otherwise it persists.
        if was_relinquished && !targets.hooks && !targets.remove_hooks {
            plan.relinquished.insert(target.id());
        }
        if let Some(hook) = &target.hook_file
            && (hooks || targets.remove_hooks)
        {
            let hook_snapshot = capture_one(hook, entries)?;
            let protocol = target
                .harness
                .spec()
                .install
                .and_then(|install| install.hook_protocol)
                .ok_or(InstallError::Manifest("hook protocol missing"))?;
            if matches!(protocol, provenant_records::HookProtocol::OpenCodePlugin) {
                let old_owned = previous
                    .into_iter()
                    .flat_map(|receipt| &receipt.entries)
                    .find(|entry| {
                        entry.harness == Some(target.harness)
                            && entry.selector == "hook-plugin"
                            && entry.path.to_path_buf().is_ok_and(|path| path == *hook)
                    });
                if hooks {
                    let (old, mode) = file(&hook_snapshot, hook)?;
                    let generated = crate::hooks::hook_plugin_source(
                        &targets.bin_dir.join("provenant"),
                        targets.roots.state.as_path(),
                    )?;
                    let write_allowed = if let Some(entry) = old_owned {
                        if !managed::matches_owned(
                            &Value::File {
                                bytes: old.clone(),
                                mode,
                            },
                            &entry.expected,
                        ) {
                            add_finding(&mut plan.findings, "relinquished_modified_entry", hook)?;
                            plan.relinquished.insert(target.id());
                            false
                        } else {
                            true
                        }
                    } else if hook_snapshot.is_some() {
                        return Err(InstallError::Conflict(hook.clone()));
                    } else {
                        true
                    };
                    if write_allowed {
                        let value = Value::File {
                            bytes: generated,
                            mode: if hook_snapshot.is_some() { mode } else { 0o600 },
                        };
                        plan.owned.push(OwnedEntry {
                            extra: Default::default(),
                            path: AbsolutePath::new(hook)?,
                            harness: Some(target.harness),
                            selector: "hook-plugin".into(),
                            expected: managed::owned(&value)?,
                        });
                        if hook_snapshot
                            .as_ref()
                            .is_none_or(|snapshot| snapshot.value != value)
                        {
                            plan.files.insert(hook.clone(), value);
                        }
                    }
                } else if targets.remove_hooks && old_owned.is_some() {
                    plan.relinquished.insert(target.id());
                }
            } else {
                let (old, hook_mode) = file(&hook_snapshot, hook)?;
                let mut base = Value::File {
                    bytes: old.clone(),
                    mode: hook_mode,
                };
                let mut removed_changes = Vec::new();
                let mut inherited = BTreeSet::new();
                let mut had_own_hooks = false;
                if let Some(previous) = previous {
                    for entry in &previous.entries {
                        if entry.harness != Some(target.harness)
                            || !entry.path.to_path_buf().is_ok_and(|path| &path == hook)
                            || !entry.selector.starts_with("hook:")
                        {
                            continue;
                        }
                        had_own_hooks = true;
                        if hooks && !targets.hooks && !entry_matches(&base, entry)? {
                            add_finding(&mut plan.findings, "relinquished_modified_entry", hook)?;
                            continue;
                        }
                        inherited.insert(entry.selector.clone());
                        // Keep current registrations in place so re-install is a no-op.
                        let still_desired = hooks
                            && targets.hook_specs.iter().any(|spec| {
                                if spec.harness != target.harness {
                                    return false;
                                }
                                let Ok(value) = crate::hooks::value(spec, targets) else {
                                    return false;
                                };
                                let Some(command) = value.get("command").and_then(Json::as_str)
                                else {
                                    return false;
                                };
                                crate::hooks::selector(spec, command)
                                    .is_ok_and(|selector| selector == entry.selector)
                                    && entry.expected
                                        == (OwnedValue::Selector {
                                            extra: Default::default(),
                                            value,
                                        })
                            });
                        if !still_desired {
                            if let Some((value, changes)) = remove_selector(&base, entry)? {
                                base = value;
                                removed_changes.extend(changes);
                            } else {
                                add_finding(
                                    &mut plan.findings,
                                    "relinquished_modified_entry",
                                    hook,
                                )?;
                            }
                        }
                    }
                }
                let Value::File { bytes: base, .. } = base else {
                    return Err(InstallError::ConfigFile(hook.clone()));
                };
                let (mut updated, mut changes, owned) = if hooks {
                    // A target with no hooks of its own yet takes the full set its
                    // siblings own; an established target keeps what it relinquished.
                    // One that removed them all (`hooks_relinquished`) stays without.
                    let follows_siblings = inherited_hooks
                        && inherited.is_empty()
                        && !had_own_hooks
                        && !was_relinquished;
                    let registration = crate::hooks::register(
                        target.harness,
                        &base,
                        targets,
                        &inherited,
                        follows_siblings,
                    )?;
                    if registration.relinquished_modified {
                        add_finding(&mut plan.findings, "relinquished_modified_entry", hook)?;
                    }
                    if !targets.hooks
                        && registration.owned.is_empty()
                        && (had_own_hooks || was_relinquished)
                    {
                        plan.relinquished.insert(target.id());
                    }
                    (registration.bytes, registration.changes, registration.owned)
                } else {
                    (base, Vec::new(), Vec::new())
                };
                changes.extend(removed_changes);
                for (selector, value) in owned {
                    plan.owned.push(OwnedEntry {
                        extra: Default::default(),
                        path: AbsolutePath::new(hook)?,
                        harness: Some(target.harness),
                        selector,
                        expected: OwnedValue::Selector {
                            extra: Default::default(),
                            value,
                        },
                    });
                }
                if updated != old {
                    updated = crate::splice::pretty_json(&updated)?;
                    plan.files.insert(
                        hook.clone(),
                        Value::File {
                            bytes: updated,
                            mode: hook_mode,
                        },
                    );
                    plan.changes
                        .entry(hook.clone())
                        .or_default()
                        .extend(changes);
                }
            }
        }
        if let Some(settings) = &targets.shared_settings
            && target.harness == Harness::Claude
            && let Some(hook) = &target.hook_file
            && !same_file(hook, &settings.source_file)
        {
            let (old, mode) = match plan.files.get(hook) {
                Some(Value::File { bytes, mode }) => (bytes.clone(), *mode),
                _ => file(&capture_one(hook, entries)?, hook)?,
            };
            let wire = AbsolutePath::new(hook)?;
            let previous_values: BTreeMap<String, Json> = previous
                .into_iter()
                .flat_map(|receipt| &receipt.entries)
                .filter(|entry| entry.path == wire)
                .filter_map(|entry| {
                    let key = entry
                        .selector
                        .strip_prefix(crate::shared_settings::PREFIX)?;
                    match &entry.expected {
                        OwnedValue::Selector { value, .. } => Some((key.to_owned(), value.clone())),
                        _ => None,
                    }
                })
                .collect();
            let home = hook.parent().map(crate::shared_settings::canonical);
            let desired = settings.desired(home.as_deref());
            let synced = crate::shared_settings::plan(
                &old,
                &settings.keys,
                &desired,
                &previous_values,
                targets.force_settings,
            )?;
            for key in &synced.conflicts {
                plan.findings.push(managed::InstallFinding {
                    rule: "settings_conflict".into(),
                    detail: Some(crate::shared_settings::detail(
                        &targets.accounts,
                        hook.parent(),
                        key,
                    )),
                    path: wire.clone(),
                });
            }
            for (key, value) in synced.owned {
                plan.owned.push(OwnedEntry {
                    extra: Default::default(),
                    path: wire.clone(),
                    harness: Some(target.harness),
                    selector: format!("{}{key}", crate::shared_settings::PREFIX),
                    expected: OwnedValue::Selector {
                        value,
                        extra: Default::default(),
                    },
                });
            }
            if synced.bytes != old {
                // Existing bytes stay as they are; only a new file is laid out.
                let updated = if old == b"{}\n" {
                    crate::splice::pretty_json(&synced.bytes)?
                } else {
                    synced.bytes
                };
                plan.files.insert(
                    hook.clone(),
                    Value::File {
                        bytes: updated,
                        mode,
                    },
                );
                plan.changes
                    .entry(hook.clone())
                    .or_default()
                    .extend(synced.changes);
            }
        }
        if target.harness == Harness::Opencode {
            let snap = capture_one(path, entries)?;
            let (mut bytes, mode) = match plan.files.get(path) {
                Some(Value::File { bytes, mode }) => (bytes.clone(), *mode),
                _ => file(&snap, path)?,
            };
            let initial = bytes.clone();
            bytes = rewrite_instruction_array(&bytes, release, targets, previous, &mut plan, path)?;
            if let Some(array) = crate::splice::get(&bytes, &["instructions"])? {
                let array = array
                    .as_array()
                    .ok_or(InstallError::InvalidConfig("instructions must be an array"))?;
                for asset in [PathBuf::from(managed::INSTRUCTION_ASSET)] {
                    let path_value = root.join(&asset);
                    let text = path_value.to_str().ok_or(InstallError::InvalidConfig(
                        "instruction path requires UTF-8",
                    ))?;
                    let count = array.iter().filter(|v| v.as_str() == Some(text)).count();
                    if count > 1 {
                        return Err(InstallError::InvalidConfig("duplicate owned instruction"));
                    }
                    if count == 1 {
                        if !release.files.contains_key(&asset) {
                            return Err(InstallError::Manifest("missing instruction asset"));
                        }
                        plan.owned.push(OwnedEntry {
                            extra: Default::default(),
                            path: AbsolutePath::new(path)?,
                            harness: Some(target.harness),
                            selector: format!("array:{text}"),
                            expected: OwnedValue::Selector {
                                extra: Default::default(),
                                value: json!(text),
                            },
                        });
                    }
                }
            }
            if snap.is_none() && bytes == initial {
                continue;
            }
            if snap.as_ref().is_none_or(|s| {
                s.value
                    != Value::File {
                        bytes: bytes.clone(),
                        mode,
                    }
            }) {
                plan.files.insert(path.clone(), Value::File { bytes, mode });
            }
        }
    }
    Ok(plan)
}
fn same_file(a: &Path, b: &Path) -> bool {
    crate::shared_settings::canonical(a) == crate::shared_settings::canonical(b)
}
pub(crate) fn permitted_selector(
    entry: &OwnedEntry,
    target: &ResolvedHarnessTarget,
) -> Result<bool, InstallError> {
    if entry.selector.starts_with("hook:") {
        return Ok(target.hook_file.as_ref() == Some(&entry.path)
            && crate::hooks::belongs_to(&entry.selector, target.harness)?
            && target.harness.spec().install.is_some_and(|install| {
                install.hooks.is_supported()
                    && !matches!(
                        install.hook_protocol,
                        Some(provenant_records::HookProtocol::OpenCodePlugin)
                    )
            }));
    }
    if let Some(key) = entry.selector.strip_prefix(crate::shared_settings::PREFIX) {
        return Ok(target.harness == Harness::Claude
            && target.hook_file.as_ref() == Some(&entry.path)
            && crate::shared_settings::check_key(key).is_ok());
    }
    if entry.path != target.config_file {
        return Ok(false);
    }
    Ok(target.harness == Harness::Opencode
        && matches!(&entry.expected,OwnedValue::Selector{value:Json::String(value), .. } if entry.selector==format!("array:{value}")))
}
pub(crate) fn entry_matches(value: &Value, entry: &OwnedEntry) -> Result<bool, InstallError> {
    if let OwnedValue::Selector {
        value: expected, ..
    } = &entry.expected
    {
        let Value::File { bytes, .. } = value else {
            return Err(InstallError::ConfigFile(entry.path.to_path_buf()?));
        };
        Ok(get_selector(bytes, &entry.selector)?.as_ref() == Some(expected))
    } else {
        Ok(managed::matches_owned(value, &entry.expected))
    }
}
pub(crate) fn remove_selector(
    value: &Value,
    entry: &OwnedEntry,
) -> Result<Option<(Value, Vec<SelectorChange>)>, InstallError> {
    if !entry_matches(value, entry)? {
        return Ok(None);
    }
    let Value::File { bytes, mode } = value else {
        return Ok(None);
    };
    let before = get_selector(bytes, &entry.selector)?;
    let out = set_selector(bytes, &entry.selector, None)?;
    Ok(Some((
        Value::File {
            bytes: out,
            mode: *mode,
        },
        vec![SelectorChange {
            selector: entry.selector.clone(),
            before,
            after: None,
        }],
    )))
}
fn rewrite_instruction_array(
    bytes: &[u8],
    release: &Release,
    targets: &Targets,
    previous: Option<&InstallReceipt>,
    plan: &mut HarnessPlan,
    path: &Path,
) -> Result<Vec<u8>, InstallError> {
    let root = managed::release_root(
        targets.roots.state.as_path(),
        release.manifest().source_digest,
    );
    let Some(array) = crate::splice::get(bytes, &["instructions"])? else {
        return Ok(bytes.to_vec());
    };
    let array = array
        .as_array()
        .ok_or(InstallError::InvalidConfig("instructions must be an array"))?;
    let old_root = previous
        .map(|receipt| managed::release_root(targets.roots.state.as_path(), receipt.source_digest));
    let mut out = bytes.to_vec();
    // Reverse order keeps recorded array indices valid for removals.
    for (i, value) in array.iter().enumerate().rev() {
        let text = value.as_str();
        let name_owned = text.and_then(|text| managed::release_instruction_asset(Path::new(text)));
        let receipt_owned = text.and_then(|text| {
            let receipt = previous?;
            let old_root = old_root.as_ref()?;
            if !receipt.entries.iter().any(|entry| {
                entry
                    .path
                    .to_path_buf()
                    .is_ok_and(|owned_path| owned_path == path)
                    && entry.harness == Some(Harness::Opencode)
                    && entry.selector.starts_with("array:")
                    && entry.expected
                        == (OwnedValue::Selector {
                            extra: Default::default(),
                            value: value.clone(),
                        })
            }) {
                return None;
            }
            Path::new(text)
                .strip_prefix(old_root)
                .ok()
                .filter(|relative| relative.parent() == Some(Path::new("instructions")))
                .map(Path::to_path_buf)
        });
        if name_owned.is_some() || receipt_owned.is_some() {
            if array.iter().filter(|candidate| *candidate == value).count() > 1 {
                return Err(InstallError::InvalidConfig("duplicate owned instruction"));
            }
        }
        if let Some(asset) = name_owned.or(receipt_owned) {
            let retired = !release.files.contains_key(&asset);
            let new_path = root.join(&asset);
            let new_text = new_path.to_str().ok_or(InstallError::InvalidConfig(
                "instruction path requires UTF-8",
            ))?;
            let current_array = crate::splice::get(&out, &["instructions"])?
                .and_then(|array| array.as_array().cloned())
                .ok_or(InstallError::InvalidConfig("instructions must be an array"))?;
            let duplicate_new = current_array
                .iter()
                .enumerate()
                .any(|(index, candidate)| index != i && candidate.as_str() == Some(new_text));
            let index = i.to_string();
            if duplicate_new || retired {
                out = crate::splice::set(&out, &["instructions", &index], None)?;
                plan.changes
                    .entry(path.into())
                    .or_default()
                    .push(SelectorChange {
                        selector: format!("json:/instructions/{i}"),
                        before: Some(value.clone()),
                        after: None,
                    });
            } else {
                let after = json!(new_text);
                if &after != value {
                    out = crate::splice::set(&out, &["instructions", &index], Some(&after))?;
                    plan.changes
                        .entry(path.into())
                        .or_default()
                        .push(SelectorChange {
                            selector: format!("json:/instructions/{i}"),
                            before: Some(value.clone()),
                            after: Some(after),
                        });
                }
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstallSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bin_dir: Option<PathBuf>,
    pub targets: BTreeMap<Harness, TargetSettings>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
}
impl InstallSettings {
    pub(crate) fn validate(&self) -> Result<(), InstallError> {
        for path in self.bin_dir.iter().chain(
            self.targets
                .values()
                .flat_map(|s| s.home.iter().chain(s.config.iter())),
        ) {
            managed::normal_absolute(path)?;
        }
        Ok(())
    }
}
impl Targets {
    pub fn apply_environment(
        &mut self,
        claude: Option<&Path>,
        codex: Option<&Path>,
    ) -> Result<(), InstallError> {
        for (harness, base) in [(Harness::Claude, claude), (Harness::Codex, codex)] {
            if let Some(base) = base {
                self.apply_account_environment(harness, base)?;
            }
        }
        Ok(())
    }

    /// Apply a captured account directory to one selected harness.
    pub fn apply_account_environment(
        &mut self,
        harness: Harness,
        base: &Path,
    ) -> Result<(), InstallError> {
        if !self
            .harnesses
            .iter()
            .any(|target| target.harness == harness)
        {
            return Ok(());
        }
        managed::normal_absolute(base)?;
        for target in self
            .harnesses
            .iter_mut()
            .filter(|target| target.harness == harness)
        {
            let default = Targets::new(self.roots.clone(), &self.home, &[target.harness]);
            if !default.harnesses.is_empty() {
                if !self.configured_homes.contains(&target.harness) {
                    self.environment_homes.insert(target.harness);
                    let instruction = target
                        .instruction
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| base.join(n));
                    target.skills_dir = base.join("skills");
                    target.instruction = instruction;
                    target.hook_file = default_hooks(target.harness, base);
                }
                if !self.configured_homes.contains(&target.harness)
                    && !self.configured_configs.contains(&target.harness)
                {
                    target.config_file = default_config(target.harness, &self.home, base);
                }
            }
        }
        Ok(())
    }
}
pub fn apply_install_config_with_domains<T: serde::de::DeserializeOwned + Serialize>(
    targets: &mut Targets,
    sections: &crate::DomainSections<T>,
) -> Result<(), InstallError> {
    // `load` accepts a bounded optional reader and validates the entire user
    // configuration; no unrelated domain policy is interpreted by installation.
    let path = targets.roots.state.as_path().join("config.toml");
    let snapshot = managed::snapshot_scoped(&path, targets.roots.state.as_path())?;
    let Some(Snapshot {
        value: Value::File { bytes, .. },
        ..
    }) = snapshot
    else {
        if snapshot.is_some() {
            return Err(InstallError::ConfigFile(path));
        }
        return Ok(());
    };
    let settings = crate::load_with_domains(&targets.roots, Some(bytes.as_slice()), sections)?;
    {
        settings.install.validate()?;
        if let Some(bin) = settings.install.bin_dir {
            targets.bin_dir = bin;
        }
        for target in &mut targets.harnesses {
            if let Some(settings) = settings.install.targets.get(&target.harness) {
                if let Some(base) = &settings.home {
                    targets.configured_homes.insert(target.harness);
                    target.skills_dir = base.join("skills");
                    target.hook_file = default_hooks(target.harness, base);
                    target.instruction = target
                        .instruction
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| base.join(n));
                    target.config_file = default_config(target.harness, &targets.home, base);
                }
                if let Some(config) = &settings.config {
                    targets.configured_configs.insert(target.harness);
                    target.config_file = config.clone();
                }
            }
        }
    }
    Ok(())
}

fn add_finding(
    findings: &mut Vec<managed::InstallFinding>,
    rule: &str,
    path: &Path,
) -> Result<(), InstallError> {
    let path = AbsolutePath::new(path)?;
    if !findings
        .iter()
        .any(|finding| finding.rule == rule && finding.path == path)
    {
        findings.push(managed::InstallFinding {
            rule: rule.into(),
            detail: None,
            path,
        });
    }
    Ok(())
}

pub(crate) fn capability_findings(
    targets: &Targets,
) -> Result<Vec<managed::InstallFinding>, InstallError> {
    fn unsupported(capability: CapabilitySupport) -> Option<(&'static str, &'static str)> {
        match capability {
            CapabilitySupport::Supported => None,
            CapabilitySupport::Unsupported { reason, source } => Some((reason, source)),
        }
    }

    let mut findings = Vec::new();
    for target in &targets.harnesses {
        let Some(install) = target.harness.spec().install else {
            continue;
        };
        let path = target.hook_file.as_deref().unwrap_or(&target.config_file);
        for (name, support) in [
            ("instructions", install.instructions),
            ("answer delivery", install.answer_delivery),
        ] {
            if let Some((reason, source)) = unsupported(support) {
                findings.push(managed::InstallFinding {
                    rule: crate::NATIVE_CAPABILITY_UNAVAILABLE.into(),
                    path: AbsolutePath::new(path)?,
                    detail: Some(format!("{name}: {reason} (source: {source})")),
                });
            }
        }
        let has_precompact_event = install.hook_events.iter().any(|event| {
            matches!(
                event,
                provenant_records::HookEvent::Supported {
                    action: provenant_records::HookAction::PreCompact,
                    ..
                }
            )
        });
        if has_precompact_event
            && let Some((reason, source)) = unsupported(install.precompact_context)
        {
            findings.push(managed::InstallFinding {
                rule: crate::NATIVE_CAPABILITY_UNAVAILABLE.into(),
                path: AbsolutePath::new(path)?,
                detail: Some(format!(
                    "pre-compaction model context: {reason} (source: {source})"
                )),
            });
        }
        if !install.hooks.is_supported() {
            if let Some((reason, source)) = unsupported(install.hooks) {
                findings.push(managed::InstallFinding {
                    rule: crate::NATIVE_CAPABILITY_UNAVAILABLE.into(),
                    path: AbsolutePath::new(path)?,
                    detail: Some(format!("hooks: {reason} (source: {source})")),
                });
            }
        } else {
            let unsupported_actions = install
                .hook_events
                .iter()
                .filter_map(|event| match event {
                    provenant_records::HookEvent::Unsupported {
                        action,
                        reason,
                        source,
                    } if !matches!(
                        action,
                        provenant_records::HookAction::ChairPrompt
                            | provenant_records::HookAction::ChairStart
                            | provenant_records::HookAction::ChairStop
                            | provenant_records::HookAction::PreToolUse
                    ) =>
                    {
                        Some(format!(
                            "{}: {} (source: {})",
                            action.command(),
                            reason,
                            source
                        ))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !unsupported_actions.is_empty() {
                findings.push(managed::InstallFinding {
                    rule: crate::NATIVE_CAPABILITY_UNAVAILABLE.into(),
                    path: AbsolutePath::new(path)?,
                    detail: Some(format!(
                        "native hooks unavailable: {}",
                        unsupported_actions.join("; ")
                    )),
                });
            }
        }
    }
    Ok(findings)
}
