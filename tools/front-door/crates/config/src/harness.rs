//! Harness-owned selectors are edited in place, never whole-config ownership.
use crate::managed::{self, InstallError, SelectorChange, Snapshot, Targets, Value};
use crate::{Harness, Release};
use provenant_records::{
    AbsolutePath, InstallReceipt, Limits, OwnedEntry, OwnedValue, ResolvedHarnessTarget,
};
use provenant_supervisor::fs::SafeFs;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(crate) fn default_mcp(h: Harness, home: &Path, base: &Path) -> PathBuf {
    match h {
        Harness::Claude => home.join(".claude.json"),
        Harness::Codex => base.join("config.toml"),
        Harness::Cursor => base.join("mcp.json"),
        Harness::Agy => base.join("config/mcp_config.json"),
        Harness::Opencode => base.join("opencode.jsonc"),
    }
}
pub(crate) fn default_hooks(h: Harness, base: &Path) -> Option<PathBuf> {
    match h {
        Harness::Claude => Some(base.join("settings.json")),
        Harness::Codex => Some(base.join("hooks.json")),
        _ => None,
    }
}
pub(crate) fn name(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "claude",
        Harness::Codex => "codex",
        Harness::Cursor => "cursor",
        Harness::Agy => "agy",
        Harness::Opencode => "opencode",
    }
}
fn selector(h: Harness) -> &'static str {
    match h {
        Harness::Codex => "toml:mcp_servers.fabric",
        Harness::Opencode => "json:/mcp/fabric",
        _ => "json:/mcpServers/fabric",
    }
}
fn mcp(h: Harness, targets: &Targets) -> Result<Json, InstallError> {
    let binary = targets.bin_dir.join("provenant");
    let binary = binary
        .to_str()
        .ok_or(InstallError::InvalidConfig("MCP command requires UTF-8"))?;
    let state = targets
        .roots
        .state
        .as_path()
        .to_str()
        .ok_or(InstallError::InvalidConfig("MCP state path requires UTF-8"))?;
    let args = ["--state-dir", state, "fabric", "mcp", "--client", name(h)];
    Ok(if h == Harness::Opencode {
        let mut command = vec![binary];
        command.extend(args);
        json!({"type":"local","command":command,"environment":{},"enabled":true})
    } else if h == Harness::Claude {
        json!({"type":"stdio","command":binary,"args":args,"env":{}})
    } else {
        json!({"command":binary,"args":args,"env":{}})
    })
}
fn json_path(selector: &str) -> Result<Vec<&str>, InstallError> {
    let path = selector
        .strip_prefix("json:/")
        .ok_or(InstallError::InvalidConfig("unknown selector"))?;
    Ok(path.split('/').collect())
}
fn toml_document(bytes: &[u8]) -> Result<toml_edit::DocumentMut, InstallError> {
    if bytes.len() as u64 > Limits::CONFIG.max_bytes {
        return Err(InstallError::InvalidConfig("config byte bound"));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| InstallError::InvalidConfig("config is not UTF-8"))?;
    text.parse()
        .map_err(|_| InstallError::InvalidConfig("malformed TOML"))
}
fn get(bytes: &[u8], sel: &str) -> Result<Option<Json>, InstallError> {
    if let Some(expected) = sel.strip_prefix("array:") {
        let value = crate::splice::get(bytes, &["instructions"])?;
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
    if let Some(sel) = sel.strip_prefix("toml:") {
        let doc = toml_document(bytes)?;
        let mut item = doc.as_item();
        for key in sel.split('.') {
            match item.get(key) {
                Some(next) => item = next,
                None => return Ok(None),
            }
        }
        let mut table = toml_edit::Table::new();
        table.insert("value", item.clone());
        let parsed: Json = toml_edit::de::from_document(toml_edit::DocumentMut::from(table))
            .map_err(|_| InstallError::InvalidConfig("unsupported TOML selector"))?;
        Ok(parsed.get("value").cloned())
    } else {
        crate::splice::get(bytes, &json_path(sel)?)
    }
}
fn set(bytes: &[u8], sel: &str, value: Option<&Json>) -> Result<Vec<u8>, InstallError> {
    if let Some(expected) = sel.strip_prefix("array:") {
        let array = crate::splice::get(bytes, &["instructions"])?;
        if let Some(index) = array
            .as_ref()
            .and_then(Json::as_array)
            .and_then(|a| a.iter().position(|v| v.as_str() == Some(expected)))
        {
            return crate::splice::set(bytes, &["instructions", &index.to_string()], value);
        }
        return Ok(bytes.to_vec());
    }

    if let Some(sel) = sel.strip_prefix("toml:") {
        let mut doc = toml_document(bytes)?;
        let keys: Vec<_> = sel.split('.').collect();
        let (last, parents) = keys
            .split_last()
            .ok_or(InstallError::InvalidConfig("empty selector"))?;
        let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
        for key in parents {
            if !table.contains_key(key) {
                table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = table
                .get_mut(key)
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or(InstallError::InvalidConfig(
                    "selector parent is not a table",
                ))?;
        }
        if let Some(value) = value {
            let mut encoded = toml_edit::ser::to_document(&json!({"value":value}))
                .map_err(|_| InstallError::InvalidConfig("cannot encode TOML selector"))?;
            let item = encoded
                .remove("value")
                .ok_or(InstallError::InvalidConfig("cannot encode TOML selector"))?;
            table.insert(last, item);
        } else {
            table.remove(last);
        }
        let out = doc.to_string().into_bytes();
        toml_document(&out)?;
        Ok(out)
    } else {
        crate::splice::set(bytes, &json_path(sel)?, value)
    }
}
fn file(snapshot: &Option<Snapshot>, path: &Path) -> Result<(Vec<u8>, u64), InstallError> {
    match snapshot {
        None => Ok((b"{}\n".to_vec(), 0o600)),
        Some(Snapshot {
            value: Value::File { bytes, mode },
            ..
        }) if bytes.len() as u64 <= Limits::CONFIG.max_bytes => Ok((bytes.clone(), *mode)),
        _ => Err(InstallError::ConfigFile(path.into())),
    }
}
#[derive(Debug, Default)]
pub(crate) struct HarnessPlan {
    pub files: BTreeMap<PathBuf, Value>,
    pub removals: BTreeSet<PathBuf>,
    pub owned: Vec<OwnedEntry>,
    pub changes: BTreeMap<PathBuf, Vec<SelectorChange>>,
    pub authorized: BTreeSet<PathBuf>,
}
fn capture_one(
    path: &Path,
    entries: &mut BTreeMap<PathBuf, Option<Snapshot>>,
    fs: &dyn SafeFs,
) -> Result<Option<Snapshot>, InstallError> {
    if let Some(s) = entries.get(path) {
        return Ok(s.clone());
    }
    let s = managed::snapshot(path, fs)?;
    entries.insert(path.into(), s.clone());
    Ok(s)
}
fn expected_previous(
    receipt: Option<&InstallReceipt>,
    path: &AbsolutePath,
    sel: &str,
    current: &Json,
) -> bool {
    receipt.is_some_and(|r| {
        r.entries.iter().any(|e| {
            &e.path == path
                && e.selector == sel
                && e.expected
                    == OwnedValue::Selector {
                        value: current.clone(),
                    }
        })
    })
}
pub(crate) fn capture(
    release: &Release,
    targets: &Targets,
    entries: &mut BTreeMap<PathBuf, Option<Snapshot>>,
    previous: Option<&InstallReceipt>,
    fs: &dyn SafeFs,
) -> Result<HarnessPlan, InstallError> {
    let mut plan = HarnessPlan::default();
    let root = targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest().source_digest.to_string());
    for target in &targets.harnesses {
        let path = &target.mcp_file;
        let snap = capture_one(path, entries, fs)?;
        let (mut bytes, mode) = file(&snap, path)?;
        if snap.is_none() && target.harness == Harness::Codex {
            bytes.clear();
        }
        let sel = selector(target.harness);
        let before = get(&bytes, sel)?;
        let want = mcp(target.harness, targets)?;
        let wire = AbsolutePath::new(path)?;
        if before.as_ref() != Some(&want) {
            if let Some(current) = &before {
                let legacy = targets.legacy_checkout.as_ref().is_some_and(|checkout| {
                    crate::legacy::mcp_matches(current, target.harness, &targets.home, checkout)
                });
                if !legacy
                    && !expected_previous(previous, &wire, sel, current)
                    && (!targets.adopt_existing || !safe_adoption(current))
                {
                    return Err(InstallError::Conflict(path.clone()));
                }
            }
            bytes = set(&bytes, sel, Some(&want))?;
            plan.changes
                .entry(path.clone())
                .or_default()
                .push(SelectorChange {
                    selector: sel.into(),
                    before,
                    after: Some(want.clone()),
                });
        }
        plan.owned.push(OwnedEntry {
            path: wire,
            harness: Some(target.harness),
            selector: sel.into(),
            expected: OwnedValue::Selector { value: want },
        });
        if let Some(checkout) = &targets.legacy_checkout
            && let Some(hook) = &target.hook_file
        {
            let hook_snapshot = capture_one(hook, entries, fs)?;
            if hook_snapshot.is_some() {
                let (old, hook_mode) = file(&hook_snapshot, hook)?;
                let (updated, changes) =
                    crate::legacy::clean_hooks(&old, target.harness, &targets.home, checkout)?;
                if updated != old {
                    plan.files.insert(
                        hook.clone(),
                        Value::File {
                            bytes: updated,
                            mode: hook_mode,
                        },
                    );
                    plan.changes.insert(hook.clone(), changes);
                }
            }
        }
        if target.harness == Harness::Opencode {
            bytes = rewrite_instruction_array(&bytes, release, targets, previous, &mut plan, path)?;
            if let Some(array) = crate::splice::get(&bytes, &["instructions"])? {
                let array = array
                    .as_array()
                    .ok_or(InstallError::InvalidConfig("instructions must be an array"))?;
                for asset in ["instructions/AGENTS.md", "instructions/HARNESS.md"] {
                    let path_value = root.join(asset);
                    let Some(text) = path_value.to_str() else {
                        return Err(InstallError::InvalidConfig(
                            "instruction path requires UTF-8",
                        ));
                    };
                    if array.iter().filter(|v| v.as_str() == Some(text)).count() > 1 {
                        return Err(InstallError::InvalidConfig("duplicate owned instruction"));
                    }
                    if array.iter().any(|v| v.as_str() == Some(text)) {
                        plan.owned.push(OwnedEntry {
                            path: AbsolutePath::new(path)?,
                            harness: Some(target.harness),
                            selector: format!("array:{text}"),
                            expected: OwnedValue::Selector { value: json!(text) },
                        });
                    }
                }
            }
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
    if let Some(checkout) = &targets.legacy_checkout {
        legacy_links(release, targets, checkout, entries, &mut plan, fs)?;
        instruction_rewrite(release, targets, checkout, previous, entries, &mut plan, fs)?;
    }
    Ok(plan)
}
fn safe_adoption(value: &Json) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.keys().all(|key| {
        matches!(
            key.as_str(),
            "command" | "args" | "type" | "enabled" | "env" | "environment"
        )
    }) && ["env", "environment"].iter().all(|k| {
        obj.get(*k)
            .is_none_or(|v| v.as_object().is_some_and(|m| m.is_empty()))
    })
}
pub(crate) fn permitted_selector(
    entry: &OwnedEntry,
    target: &ResolvedHarnessTarget,
) -> Result<bool, InstallError> {
    if entry.path != target.mcp_file {
        return Ok(false);
    }
    if entry.selector == selector(target.harness) {
        return Ok(true);
    }
    Ok(target.harness == Harness::Opencode
        && matches!(&entry.expected,OwnedValue::Selector{value:Json::String(value)} if entry.selector==format!("array:{value}")))
}
pub(crate) fn entry_matches(value: &Value, entry: &OwnedEntry) -> Result<bool, InstallError> {
    if let OwnedValue::Selector { value: expected } = &entry.expected {
        let Value::File { bytes, .. } = value else {
            return Err(InstallError::ConfigFile(entry.path.to_path_buf()?));
        };
        Ok(get(bytes, &entry.selector)?.as_ref() == Some(expected))
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
    let before = get(bytes, &entry.selector)?;
    let out = set(bytes, &entry.selector, None)?;
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
// Further cut-over helpers below keep matching finite and never traverse old links.

// A whole skills/workflows link is compared before any child snapshot. Returning
// absence here is safe only for the exact compiled whole-directory target.
pub(crate) fn snapshot_target(
    path: &Path,
    targets: &Targets,
    fs: &dyn SafeFs,
) -> Result<Option<Snapshot>, InstallError> {
    for target in &targets.harnesses {
        for (directory, tail) in [
            (target.skills_dir.clone(), "skills"),
            (target.skills_dir.with_file_name("workflows"), "workflows"),
        ] {
            if path != directory
                && path.starts_with(&directory)
                && let Some(Snapshot {
                    value: Value::Link(link),
                    ..
                }) = managed::snapshot(&directory, fs)?
            {
                if targets
                    .legacy_checkout
                    .as_ref()
                    .is_some_and(|checkout| link == checkout.join(tail))
                {
                    return Ok(None);
                }
                return Err(InstallError::Conflict(directory));
            }
        }
    }
    managed::snapshot(path, fs)
}
fn rewrite_instruction_array(
    bytes: &[u8],
    release: &Release,
    targets: &Targets,
    previous: Option<&InstallReceipt>,
    plan: &mut HarnessPlan,
    path: &Path,
) -> Result<Vec<u8>, InstallError> {
    let root = targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest().source_digest.to_string());
    let Some(array) = crate::splice::get(bytes, &["instructions"])? else {
        return Ok(bytes.to_vec());
    };
    let array = array
        .as_array()
        .ok_or(InstallError::InvalidConfig("instructions must be an array"))?;
    let legacy = if let Some(checkout) = &targets.legacy_checkout {
        let target = targets
            .harnesses
            .iter()
            .find(|target| target.harness == Harness::Opencode)
            .ok_or(InstallError::HarnessSelection)?;
        crate::legacy::legacy_links(&targets.home, checkout, target)?
    } else {
        Vec::new()
    };
    let mut out = bytes.to_vec();
    // Reverse order keeps recorded array indices valid for removals.
    for (i, value) in array.iter().enumerate().rev() {
        let mut asset = None;
        if let Some(link) = legacy.iter().find(|link| {
            link.area == crate::legacy::LegacyArea::OpenCodeInstruction
                && value.as_str() == link.target.to_str()
        }) {
            if !link.retained {
                out = crate::splice::set(&out, &["instructions", &i.to_string()], None)?;
                plan.changes
                    .entry(path.into())
                    .or_default()
                    .push(SelectorChange {
                        selector: format!("json:/instructions/{i}"),
                        before: Some(value.clone()),
                        after: None,
                    });
                continue;
            }
            asset = Some(match link.name.as_str() {
                "home-doctrine" => "instructions/AGENTS.md",
                "companion" => "instructions/HARNESS.md",
                _ => return Err(InstallError::Manifest("unsupported legacy instruction")),
            });
        }
        if let Some(receipt) = previous {
            for entry in &receipt.entries {
                if entry.path.to_path_buf().is_ok_and(|p| p == path)
                    && entry.harness == Some(Harness::Opencode)
                    && entry.selector.starts_with("array:")
                    && entry.expected
                        == (OwnedValue::Selector {
                            value: value.clone(),
                        })
                    && let Some(text) = value.as_str()
                {
                    let old_root = targets
                        .roots
                        .state
                        .as_path()
                        .join("releases")
                        .join(receipt.source_digest.to_string());
                    for candidate in ["instructions/AGENTS.md", "instructions/HARNESS.md"] {
                        if Path::new(text) == old_root.join(candidate) {
                            asset = Some(candidate);
                        }
                    }
                }
            }
        }
        if let Some(asset) = asset {
            if !release.files.contains_key(Path::new(asset)) {
                return Err(InstallError::Manifest("missing instruction asset"));
            }
            let new = root.join(asset);
            let new = new.to_str().ok_or(InstallError::InvalidConfig(
                "instruction path requires UTF-8",
            ))?;
            let after = json!(new);
            let index = i.to_string();
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
    Ok(out)
}

fn instruction_rewrite(
    release: &Release,
    targets: &Targets,
    checkout: &Path,
    previous: Option<&InstallReceipt>,
    entries: &mut BTreeMap<PathBuf, Option<Snapshot>>,
    plan: &mut HarnessPlan,
    fs: &dyn SafeFs,
) -> Result<(), InstallError> {
    for (path, (bytes, _)) in &release.files {
        if path.starts_with("instructions")
            && bytes
                .windows(b"AGENT_FABRIC_LABEL".len())
                .any(|s| s == b"AGENT_FABRIC_LABEL")
        {
            return Err(InstallError::Manifest(
                "release instruction contains legacy variable",
            ));
        }
    }
    let global = targets.home.join(".agents/AGENTS.md");
    if let Some(s) = capture_one(&global, entries, fs)? {
        match s.value {
            Value::File { bytes, mode } => {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| InstallError::InvalidConfig("instruction is not UTF-8"))?;
                let updated = text
                    .replace("AGENT_FABRIC_LABEL", "PROVENANT_AGENT_LABEL")
                    .into_bytes();
                if updated != bytes {
                    plan.files.insert(
                        global.clone(),
                        Value::File {
                            bytes: updated,
                            mode,
                        },
                    );
                    plan.changes.insert(
                        global.clone(),
                        vec![SelectorChange {
                            selector: "instruction variable".into(),
                            before: Some(json!("AGENT_FABRIC_LABEL")),
                            after: Some(json!("PROVENANT_AGENT_LABEL")),
                        }],
                    );
                }
            }
            Value::Link(link) if link == checkout.join("AGENTS.md") => {
                let root = targets
                    .roots
                    .state
                    .as_path()
                    .join("releases")
                    .join(release.manifest().source_digest.to_string());
                if !release
                    .files
                    .contains_key(Path::new("instructions/AGENTS.md"))
                {
                    return Err(InstallError::Manifest("missing instruction asset"));
                }
                plan.files
                    .insert(global, Value::Link(root.join("instructions/AGENTS.md")));
            }
            Value::Link(link)
                if link
                    == targets
                        .roots
                        .state
                        .as_path()
                        .join("releases")
                        .join(release.manifest().source_digest.to_string())
                        .join("instructions/AGENTS.md")
                    || previous.is_some_and(|p| {
                        link == targets
                            .roots
                            .state
                            .as_path()
                            .join("releases")
                            .join(p.source_digest.to_string())
                            .join("instructions/AGENTS.md")
                    }) => {}
            _ => return Err(InstallError::Conflict(global)),
        }
    }
    Ok(())
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
        for target in &mut self.harnesses {
            let selected = match target.harness {
                Harness::Claude => claude,
                Harness::Codex => codex,
                _ => None,
            };
            if let Some(base) = selected {
                managed::normal_absolute(base)?;
                let default = Targets::new(self.roots.clone(), &self.home, &[target.harness]);
                if !default.harnesses.is_empty() {
                    if !self.configured_homes.contains(&target.harness) {
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
                        target.mcp_file = default_mcp(target.harness, &self.home, base);
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn apply_install_config(targets: &mut Targets, fs: &dyn SafeFs) -> Result<(), InstallError> {
    apply_install_config_with_domains(targets, fs, &crate::domains::NO_DOMAINS)
}

pub fn apply_install_config_with_domains<T: serde::de::DeserializeOwned + Serialize>(
    targets: &mut Targets,
    fs: &dyn SafeFs,
    sections: &crate::DomainSections<T>,
) -> Result<(), InstallError> {
    // `load` accepts a bounded optional reader and validates the entire user
    // configuration; no unrelated domain policy is interpreted by installation.
    let path = targets.roots.state.as_path().join("config.toml");
    let Some(Snapshot {
        value: Value::File { bytes, .. },
        ..
    }) = managed::snapshot(&path, fs)?
    else {
        if managed::snapshot(&path, fs)?.is_some() {
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
                    target.mcp_file = default_mcp(target.harness, &targets.home, base);
                }
                if let Some(config) = &settings.config {
                    targets.configured_configs.insert(target.harness);
                    target.mcp_file = config.clone();
                }
            }
        }
    }
    Ok(())
}

fn legacy_links(
    release: &Release,
    targets: &Targets,
    checkout: &Path,
    entries: &mut BTreeMap<PathBuf, Option<Snapshot>>,
    plan: &mut HarnessPlan,
    fs: &dyn SafeFs,
) -> Result<(), InstallError> {
    for target in &targets.harnesses {
        for link in crate::legacy::legacy_links(&targets.home, checkout, target)? {
            use crate::legacy::LegacyArea;
            let path = match link.area {
                LegacyArea::Skills => target.skills_dir.join(&link.name),
                LegacyArea::Workflows => target
                    .skills_dir
                    .with_file_name("workflows")
                    .join(&link.name),
                LegacyArea::Instruction | LegacyArea::GlobalInstruction => {
                    match &target.instruction {
                        Some(p) => p.clone(),
                        None => continue,
                    }
                }
                LegacyArea::Companion => match &target.instruction {
                    Some(p) => p.with_file_name("HARNESS.md"),
                    None => continue,
                },
                LegacyArea::GitHook => match &targets.git_hooks_dir {
                    Some(p) => p.join(&link.name),
                    None => continue,
                },
                LegacyArea::OpenCodeInstruction => continue,
            };
            let snap = if let Some(s) = entries.get(&path) {
                s.clone()
            } else {
                let s = snapshot_target(&path, targets, fs)?;
                entries.insert(path.clone(), s.clone());
                s
            };
            if let Some(Snapshot {
                value: Value::Link(actual),
                ..
            }) = snap
                && actual == link.target
            {
                plan.authorized.insert(path.clone());
                if !link.retained || matches!(link.area, LegacyArea::GitHook) {
                    // Hooks are off; retained handlers cannot register until
                    // CLI owns a fixture-backed HookSpec.
                    plan.removals.insert(path);
                } else {
                    let replacement = match link.area {
                        LegacyArea::Skills if link.name.is_empty() => {
                            // Replace the link itself with a managed directory.
                            if !release.files.keys().any(|p| {
                                (p.starts_with("skills") || p.starts_with("library"))
                                    && p.ends_with("SKILL.md")
                            }) {
                                return Err(InstallError::Manifest(
                                    "missing legacy replacement asset",
                                ));
                            }
                            plan.removals.insert(path);
                            None
                        }
                        LegacyArea::Skills => {
                            let skill = Path::new("skills").join(&link.name).join("SKILL.md");
                            let library = Path::new("library").join(&link.name).join("SKILL.md");
                            if release.files.contains_key(&skill) {
                                Some(skill)
                            } else {
                                Some(library)
                            }
                        }
                        LegacyArea::Instruction | LegacyArea::GlobalInstruction => {
                            Some(PathBuf::from("instructions/AGENTS.md"))
                        }
                        LegacyArea::Companion => Some(PathBuf::from("instructions/HARNESS.md")),
                        _ => return Err(InstallError::Manifest("unsupported legacy replacement")),
                    };
                    if replacement.is_some_and(|asset| !release.files.contains_key(&asset)) {
                        return Err(InstallError::Manifest("missing legacy replacement asset"));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn check_legacy(
    receipt: &InstallReceipt,
    legacy: &provenant_records::LegacyInstall,
    findings: &mut Vec<managed::InstallFinding>,
    basis: &mut Vec<u8>,
    fs: &dyn SafeFs,
) -> Result<(), InstallError> {
    if legacy.manifest_digest != crate::legacy::manifest_digest() {
        return Err(InstallError::Manifest("legacy manifest digest differs"));
    }
    let home = legacy.home.to_path_buf()?;
    let checkout = legacy.checkout.to_path_buf()?;
    for resolved in &receipt.harness_targets {
        let target = managed::HarnessTarget {
            harness: resolved.harness,
            skills_dir: resolved.skills_dir.to_path_buf()?,
            instruction: resolved
                .instruction
                .as_ref()
                .map(AbsolutePath::to_path_buf)
                .transpose()?,
            mcp_file: resolved.mcp_file.to_path_buf()?,
            hook_file: resolved
                .hook_file
                .as_ref()
                .map(AbsolutePath::to_path_buf)
                .transpose()?,
        };
        let mut blocked = Vec::new();
        let links = crate::legacy::legacy_links(&home, &checkout, &target)?;
        for link in links {
            use crate::legacy::LegacyArea;
            let path = match link.area {
                LegacyArea::Skills => target.skills_dir.join(&link.name),
                LegacyArea::Workflows => target
                    .skills_dir
                    .with_file_name("workflows")
                    .join(&link.name),
                LegacyArea::Instruction | LegacyArea::GlobalInstruction => {
                    match &target.instruction {
                        Some(p) => p.clone(),
                        None => continue,
                    }
                }
                LegacyArea::Companion => match &target.instruction {
                    Some(p) => p.with_file_name("HARNESS.md"),
                    None => continue,
                },
                _ => continue,
            };
            if blocked
                .iter()
                .any(|p: &PathBuf| path != *p && path.starts_with(p))
            {
                continue;
            }
            let current = managed::snapshot(&path, fs)?;
            managed::digest_snapshot(basis, &path, &current)?;
            if let Some(Snapshot {
                value: Value::Link(actual),
                ..
            }) = current
            {
                if link.name.is_empty() {
                    blocked.push(path.clone());
                }
                if actual == link.target {
                    findings.push(managed::InstallFinding {
                        rule: "legacy_entry".into(),
                        path: AbsolutePath::new(&path)?,
                    });
                }
            }
        }
        let config = managed::snapshot(&target.mcp_file, fs)?;
        managed::digest_snapshot(basis, &target.mcp_file, &config)?;
        if config.is_some() {
            let (bytes, _) = file(&config, &target.mcp_file)?;
            let old_mcp = get(&bytes, selector(target.harness))?
                .is_some_and(|v| crate::legacy::mcp_matches(&v, target.harness, &home, &checkout));
            let old_array = if target.harness == Harness::Opencode {
                crate::splice::get(&bytes, &["instructions"])?
                    .and_then(|v| v.as_array().cloned())
                    .is_some_and(|a| {
                        a.iter().any(|v| {
                            v.as_str() == home.join(".agents/AGENTS.md").to_str()
                                || v.as_str() == checkout.join("HARNESS.md").to_str()
                        })
                    })
            } else {
                false
            };
            if old_mcp || old_array {
                findings.push(managed::InstallFinding {
                    rule: "legacy_entry".into(),
                    path: AbsolutePath::new(&target.mcp_file)?,
                });
            }
        }
        for path in target.hook_file.iter() {
            let current = managed::snapshot(path, fs)?;
            managed::digest_snapshot(basis, path, &current)?;
            if current.is_some() {
                let (bytes, _) = file(&current, path)?;
                let (new, _) =
                    crate::legacy::clean_hooks(&bytes, target.harness, &home, &checkout)?;
                if new != bytes {
                    findings.push(managed::InstallFinding {
                        rule: "legacy_entry".into(),
                        path: AbsolutePath::new(path)?,
                    });
                }
            }
        }
    }
    if let Some(directory) = &legacy.git_hooks_dir {
        let directory = directory.to_path_buf()?;
        managed::normal_absolute(&directory)?;
        for (name, source) in [
            ("pre-push", "scripts/hooks/pre-push"),
            ("post-merge", "scripts/hooks/post-merge"),
        ] {
            let path = directory.join(name);
            let current = managed::snapshot(&path, fs)?;
            managed::digest_snapshot(basis, &path, &current)?;
            if matches!(current,Some(Snapshot{value:Value::Link(ref target),..}) if target==&checkout.join(source))
            {
                findings.push(managed::InstallFinding {
                    rule: "legacy_entry".into(),
                    path: AbsolutePath::new(&path)?,
                });
            }
        }
    }
    let path = home.join(".agents/AGENTS.md");
    let current = managed::snapshot(&path, fs)?;
    managed::digest_snapshot(basis, &path, &current)?;
    if matches!(&current,Some(Snapshot{value:Value::Link(link),..}) if link==&checkout.join("AGENTS.md"))
    {
        findings.push(managed::InstallFinding {
            rule: "legacy_entry".into(),
            path: AbsolutePath::new(&path)?,
        });
    }
    if let Some(Snapshot {
        value: Value::File { bytes, .. },
        ..
    }) = current
        && bytes
            .windows(b"AGENT_FABRIC_LABEL".len())
            .any(|s| s == b"AGENT_FABRIC_LABEL")
    {
        findings.push(managed::InstallFinding {
            rule: "legacy_instruction_variable".into(),
            path: AbsolutePath::new(&path)?,
        });
    }
    Ok(())
}
