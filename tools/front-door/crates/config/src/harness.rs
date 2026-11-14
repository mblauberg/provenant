//! Harness-owned selectors are edited in place, never whole-config ownership.
use crate::managed::{self, InstallError, SelectorChange, Snapshot, Targets, Value};
use crate::{Harness, Release};
use provenant_records::{
    AbsolutePath, InstallReceipt, Limits, OwnedEntry, OwnedValue, ResolvedHarnessTarget,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const OPENCODE_INSTRUCTION_ASSETS: [&str; 1] = [managed::INSTRUCTION_ASSET];

pub(crate) fn default_config(h: Harness, home: &Path, base: &Path) -> PathBuf {
    match h {
        // Claude Code keeps `.claude.json` beside `CLAUDE_CONFIG_DIR` when set.
        Harness::Claude if base == home.join(".claude") => home.join(".claude.json"),
        Harness::Claude => base.join(".claude.json"),
        Harness::Codex => base.join("config.toml"),
        Harness::Cursor => base.join("mcp.json"),
        Harness::Agy => base.join("config/mcp_config.json"),
        Harness::Opencode => base.join("opencode.jsonc"),
        // An adapter without a managed configuration has no default path.
        _ => PathBuf::new(),
    }
}
pub(crate) fn default_hooks(h: Harness, base: &Path) -> Option<PathBuf> {
    match h {
        Harness::Claude => Some(base.join("settings.json")),
        Harness::Codex => Some(base.join("hooks.json")),
        _ => None,
    }
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
    Err(InstallError::InvalidConfig("unknown selector"))
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
    let root = managed::release_root(
        targets.roots.state.as_path(),
        release.manifest().source_digest,
    );
    for target in &targets.harnesses {
        let path = &target.config_file;
        // Hooks are one choice for every target of a harness. A target whose
        // account directory has none yet follows the targets that do.
        let inherited_hooks = !targets.remove_hooks
            && previous.is_some_and(|receipt| {
                receipt.entries.iter().any(|entry| {
                    entry.harness == Some(target.harness) && entry.selector.starts_with("hook:")
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
            && target.harness == Harness::Claude
        {
            let hook_snapshot = capture_one(hook, entries)?;
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
                            spec.harness == Some(target.harness)
                                && crate::hooks::value(spec, targets).is_ok_and(|value| {
                                    value.get("command").and_then(Json::as_str).is_some_and(
                                        |command| {
                                            crate::hooks::selector(
                                                spec.event,
                                                spec.matcher,
                                                command,
                                            )
                                            .is_ok_and(|selector| selector == entry.selector)
                                        },
                                    ) && entry.expected
                                        == OwnedValue::Selector {
                                            extra: Default::default(),
                                            value,
                                        }
                                })
                        });
                    if !still_desired {
                        if let Some((value, changes)) = remove_selector(&base, entry)? {
                            base = value;
                            removed_changes.extend(changes);
                        } else {
                            add_finding(&mut plan.findings, "relinquished_modified_entry", hook)?;
                        }
                    }
                }
            }
            let Value::File { bytes: base, .. } = base else {
                return Err(InstallError::ConfigFile(hook.clone()));
            };
            let (mut updated, mut changes, owned) = if hooks && target.harness == Harness::Claude {
                // A target with no hooks of its own yet takes the full set its
                // siblings own; an established target keeps what it relinquished.
                // One that removed them all (`hooks_relinquished`) stays without.
                let follows_siblings =
                    inherited_hooks && inherited.is_empty() && !had_own_hooks && !was_relinquished;
                let registration =
                    crate::hooks::register(&base, targets, &inherited, follows_siblings)?;
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
            let (mut bytes, mode) = file(&snap, path)?;
            let initial = bytes.clone();
            bytes = rewrite_instruction_array(&bytes, release, targets, previous, &mut plan, path)?;
            if let Some(array) = crate::splice::get(&bytes, &["instructions"])? {
                let array = array
                    .as_array()
                    .ok_or(InstallError::InvalidConfig("instructions must be an array"))?;
                for asset in OPENCODE_INSTRUCTION_ASSETS.map(PathBuf::from) {
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
        return Ok(
            target.harness == Harness::Claude && target.hook_file.as_ref() == Some(&entry.path)
        );
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
    let assets = OPENCODE_INSTRUCTION_ASSETS.map(PathBuf::from);
    let old_root = previous
        .map(|receipt| managed::release_root(targets.roots.state.as_path(), receipt.source_digest));
    let mut out = bytes.to_vec();
    // Reverse order keeps recorded array indices valid for removals.
    for (i, value) in array.iter().enumerate().rev() {
        let mut asset = None;
        if let (Some(receipt), Some(old_root), Some(text)) =
            (previous, old_root.as_ref(), value.as_str())
            && receipt.entries.iter().any(|entry| {
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
            })
        {
            if array.iter().filter(|candidate| *candidate == value).count() > 1 {
                return Err(InstallError::InvalidConfig("duplicate owned instruction"));
            }
            // Any owned instructions asset is recognised, including one a
            // later release retired, so an upgrade can remove it.
            asset = Path::new(text)
                .strip_prefix(old_root)
                .ok()
                .filter(|relative| relative.parent() == Some(Path::new("instructions")))
                .map(Path::to_path_buf);
        }
        if let Some(asset) = asset {
            let retired = !release.files.contains_key(&asset);
            if retired && assets.contains(&asset) {
                return Err(InstallError::Manifest("missing instruction asset"));
            }
            let new_path = root.join(&asset);
            let new_text = new_path.to_str().ok_or(InstallError::InvalidConfig(
                "instruction path requires UTF-8",
            ))?;
            let duplicate_new = array
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
