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

pub(crate) const CLAUDE_CONFIG_LIMIT: u64 = 16 * 1024 * 1024;

fn limits(harness: Harness) -> Limits {
    Limits {
        max_bytes: if harness == Harness::Claude {
            CLAUDE_CONFIG_LIMIT
        } else {
            Limits::CONFIG.max_bytes
        },
        ..Limits::CONFIG
    }
}

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
fn toml_document(bytes: &[u8], limits: Limits) -> Result<toml_edit::DocumentMut, InstallError> {
    if bytes.len() as u64 > limits.max_bytes {
        return Err(InstallError::InvalidConfig("config byte bound"));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| InstallError::InvalidConfig("config is not UTF-8"))?;
    text.parse()
        .map_err(|_| InstallError::InvalidConfig("malformed TOML"))
}
fn get_for(bytes: &[u8], sel: &str, harness: Harness) -> Result<Option<Json>, InstallError> {
    get_with_limits(bytes, sel, limits(harness))
}
fn get_with_limits(bytes: &[u8], sel: &str, limits: Limits) -> Result<Option<Json>, InstallError> {
    if sel.starts_with("hook:") {
        return crate::hooks::get(bytes, sel);
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
    if let Some(sel) = sel.strip_prefix("toml:") {
        let doc = toml_document(bytes, limits)?;
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
        crate::splice::get_with_limits(bytes, &json_path(sel)?, limits)
    }
}
fn set_for(
    bytes: &[u8],
    sel: &str,
    value: Option<&Json>,
    harness: Harness,
) -> Result<Vec<u8>, InstallError> {
    set_with_limits(bytes, sel, value, limits(harness))
}
fn set_with_limits(
    bytes: &[u8],
    sel: &str,
    value: Option<&Json>,
    limits: Limits,
) -> Result<Vec<u8>, InstallError> {
    if sel.starts_with("hook:") {
        return crate::hooks::set(bytes, sel, value);
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

    if let Some(sel) = sel.strip_prefix("toml:") {
        let mut doc = toml_document(bytes, limits)?;
        let keys: Vec<_> = sel.split('.').collect();
        let (last, parents) = keys
            .split_last()
            .ok_or(InstallError::InvalidConfig("empty selector"))?;
        let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
        for key in parents {
            if !table.contains_key(key) {
                let mut parent = toml_edit::Table::new();
                parent.set_implicit(true);
                table.insert(key, toml_edit::Item::Table(parent));
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
            let mut item = encoded
                .remove("value")
                .ok_or(InstallError::InvalidConfig("cannot encode TOML selector"))?;
            if matches!(table.get(last), Some(toml_edit::Item::Table(_))) {
                item = toml_edit::Item::Table(
                    item.into_table()
                        .map_err(|_| InstallError::InvalidConfig("cannot encode TOML table"))?,
                );
            }
            if let Some(previous) = table.get(last).and_then(item_decor).cloned() {
                set_item_decor(&mut item, previous);
            }
            table.insert(last, item);
        } else {
            let source = doc.to_string();
            let mut comments = Vec::new();
            remove_toml_path(doc.as_table_mut(), &keys, &source, &mut comments)?;
            if !comments.is_empty() {
                append_root_comments(doc.as_table_mut(), &comments, &source);
            }
        }
        let out = doc.to_string().into_bytes();
        toml_document(&out, limits)?;
        Ok(out)
    } else {
        crate::splice::set_with_limits(bytes, &json_path(sel)?, value, limits)
    }
}
fn item_decor(item: &toml_edit::Item) -> Option<&toml_edit::Decor> {
    match item {
        toml_edit::Item::Table(table) => Some(table.decor()),
        toml_edit::Item::Value(toml_edit::Value::InlineTable(table)) => Some(table.decor()),
        _ => None,
    }
}
fn set_item_decor(item: &mut toml_edit::Item, decor: toml_edit::Decor) {
    match item {
        toml_edit::Item::Table(table) => *table.decor_mut() = decor,
        toml_edit::Item::Value(toml_edit::Value::InlineTable(table)) => {
            *table.decor_mut() = decor;
        }
        _ => {}
    }
}
fn decor_text(decor: Option<&toml_edit::Decor>, source: &str) -> Option<String> {
    let raw = decor?.prefix()?;
    raw.as_str().map(str::to_owned).or_else(|| {
        raw.span()
            .and_then(|span| source.get(span).map(str::to_owned))
    })
}
fn item_prefix(item: &toml_edit::Item, source: &str) -> Option<String> {
    match item {
        toml_edit::Item::Table(table) => decor_text(Some(table.decor()), source),
        toml_edit::Item::Value(toml_edit::Value::InlineTable(table)) => {
            decor_text(Some(table.decor()), source)
        }
        _ => None,
    }
}
fn remove_toml_path(
    table: &mut dyn toml_edit::TableLike,
    keys: &[&str],
    source: &str,
    comments: &mut Vec<String>,
) -> Result<(), InstallError> {
    let (key, tail) = keys
        .split_first()
        .ok_or(InstallError::InvalidConfig("empty selector"))?;
    if tail.is_empty() {
        if let Some(item) = table.get(key)
            && let Some(prefix) = item_prefix(item, source)
            && prefix.contains('#')
        {
            comments.push(prefix);
        }
        table.remove(key);
        return Ok(());
    }
    let empty = {
        let Some(child) = table.get_mut(key) else {
            return Ok(());
        };
        let child = child
            .as_table_like_mut()
            .ok_or(InstallError::InvalidConfig(
                "selector parent is not a table",
            ))?;
        remove_toml_path(child, tail, source, comments)?;
        child.iter().next().is_none()
    };
    if empty {
        if let Some(item) = table.get(key)
            && let Some(prefix) = item_prefix(item, source)
            && prefix.contains('#')
        {
            comments.push(prefix);
        }
        table.remove(key);
    }
    Ok(())
}
fn append_root_comments(root: &mut toml_edit::Table, comments: &[String], source: &str) {
    let mut suffix = root
        .decor()
        .suffix()
        .and_then(|raw| {
            raw.as_str().map(str::to_owned).or_else(|| {
                raw.span()
                    .and_then(|span| source.get(span).map(str::to_owned))
            })
        })
        .unwrap_or_default();
    for comment in comments {
        suffix.push_str(comment);
    }
    root.decor_mut().set_suffix(suffix);
}
fn file(snapshot: &Option<Snapshot>, path: &Path) -> Result<(Vec<u8>, u64), InstallError> {
    file_with_limits(snapshot, path, Limits::CONFIG)
}
fn file_for(
    snapshot: &Option<Snapshot>,
    path: &Path,
    harness: Harness,
) -> Result<(Vec<u8>, u64), InstallError> {
    file_with_limits(snapshot, path, limits(harness))
}
fn file_with_limits(
    snapshot: &Option<Snapshot>,
    path: &Path,
    limits: Limits,
) -> Result<(Vec<u8>, u64), InstallError> {
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
    pub removals: BTreeSet<PathBuf>,
    pub owned: Vec<OwnedEntry>,
    pub changes: BTreeMap<PathBuf, Vec<SelectorChange>>,
    pub findings: Vec<managed::InstallFinding>,
    pub authorised: BTreeSet<PathBuf>,
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
    let checkout = &targets.legacy_checkout;
    let root = managed::release_root(
        targets.roots.state.as_path(),
        release.manifest().source_digest,
    );
    for target in &targets.harnesses {
        let path = &target.mcp_file;
        let snap = capture_one(path, entries, fs)?;
        let (mut bytes, mode) = file_for(&snap, path, target.harness)?;
        if snap.is_none() && target.harness == Harness::Codex {
            bytes.clear();
        }
        let sel = selector(target.harness);
        let before = get_for(&bytes, sel, target.harness)?;
        let want = mcp(target.harness, targets)?;
        let wire = AbsolutePath::new(path)?;
        if before.as_ref() != Some(&want) {
            if let Some(current) = &before {
                let legacy = checkout.as_ref().is_some_and(|_| {
                    crate::legacy::mcp_matches(current, target.harness, &targets.home)
                });
                if !legacy
                    && !expected_previous(previous, &wire, sel, current)
                    && (!targets.adopt_existing || !safe_adoption(current))
                {
                    return Err(InstallError::Conflict(path.clone()));
                }
            }
            bytes = set_for(&bytes, sel, Some(&want), target.harness)?;
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
        if let Some(hook) = &target.hook_file
            && (checkout.is_some() || (targets.hooks && target.harness == Harness::Claude))
        {
            let hook_snapshot = capture_one(hook, entries, fs)?;
            let (old, hook_mode) = file(&hook_snapshot, hook)?;
            let (mut updated, mut changes, owned) =
                if targets.hooks && target.harness == Harness::Claude {
                    crate::hooks::register(&old, targets)?
                } else {
                    (old.clone(), Vec::new(), Vec::new())
                };
            if let Some(checkout) = checkout.as_deref() {
                let (cleaned, removals, unresolved) = crate::legacy::clean_hooks_at_home(
                    &updated,
                    target.harness,
                    checkout,
                    Some(&targets.home),
                )?;
                updated = cleaned;
                changes.extend(removals);
                if !unresolved.is_empty() {
                    add_finding(&mut plan.findings, "legacy_entry", hook)?;
                }
            }
            for (selector, value) in owned {
                plan.owned.push(OwnedEntry {
                    path: AbsolutePath::new(hook)?,
                    harness: Some(target.harness),
                    selector,
                    expected: OwnedValue::Selector { value },
                });
            }
            if updated != old {
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
        if target.harness == Harness::Opencode {
            bytes = rewrite_instruction_array(&bytes, release, targets, previous, &mut plan, path)?;
            if let Some(array) = crate::splice::get(&bytes, &["instructions"])? {
                let array = array
                    .as_array()
                    .ok_or(InstallError::InvalidConfig("instructions must be an array"))?;
                for asset in crate::legacy::instruction_assets(target.harness)? {
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
    if let Some(checkout) = checkout.as_deref() {
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
    if entry.selector.starts_with("hook:") {
        return Ok(
            target.harness == Harness::Claude && target.hook_file.as_ref() == Some(&entry.path)
        );
    }
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
        Ok(get_for(
            bytes,
            &entry.selector,
            entry.harness.unwrap_or(Harness::Codex),
        )?
        .as_ref()
            == Some(expected))
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
    let harness = entry.harness.unwrap_or(Harness::Codex);
    let before = get_for(bytes, &entry.selector, harness)?;
    let out = set_for(bytes, &entry.selector, None, harness)?;
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
    if let Some(checkout) = &targets.legacy_checkout {
        for target in &targets.harnesses {
            for link in crate::legacy::legacy_links(&targets.home, checkout, target)? {
                use crate::legacy::LegacyArea;
                let directory = match (link.area, link.name.is_empty()) {
                    (LegacyArea::Skills, true) => Some(target.skills_dir.clone()),
                    (LegacyArea::Workflows, true) => {
                        Some(target.skills_dir.with_file_name("workflows"))
                    }
                    _ => None,
                };
                if let Some(directory) = directory
                    && path != directory
                    && path.starts_with(&directory)
                    && let Some(Snapshot {
                        value: Value::Link(actual),
                        ..
                    }) = managed::snapshot(&directory, fs)?
                {
                    if actual == link.target {
                        return Ok(None);
                    }
                    return Err(InstallError::Conflict(directory));
                }
            }
        }
    }
    managed::snapshot_scoped(path, targets.roots.state.as_path(), fs)
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
    let target = targets
        .harnesses
        .iter()
        .find(|target| target.harness == Harness::Opencode)
        .ok_or(InstallError::HarnessSelection)?;
    let legacy = if let Some(checkout) = targets.legacy_checkout.as_deref() {
        crate::legacy::legacy_links(&targets.home, checkout, target)?
    } else {
        Vec::new()
    };
    // The receipt can identify prior managed release paths without authorising
    // writes to a legacy checkout. Asset names come from the manifest itself.
    let assets = crate::legacy::instruction_assets(Harness::Opencode)?;
    let old_root = previous
        .map(|receipt| managed::release_root(targets.roots.state.as_path(), receipt.source_digest));
    let mut out = bytes.to_vec();
    // Reverse order keeps recorded array indices valid for removals.
    for (i, value) in array.iter().enumerate().rev() {
        let mut asset = None;
        let mut matched_legacy = false;
        let mut matched_receipt = false;
        if let Some(link) = legacy.iter().find(|link| {
            link.area == crate::legacy::LegacyArea::OpenCodeInstruction
                && value.as_str() == link.target.to_str()
        }) {
            matched_legacy = true;
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
            asset = link.replacement_asset.clone();
        }
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
                            value: value.clone(),
                        })
            })
        {
            if array.iter().filter(|candidate| *candidate == value).count() > 1 {
                return Err(InstallError::InvalidConfig("duplicate owned instruction"));
            }
            for candidate in &assets {
                if Path::new(text) == old_root.join(candidate) {
                    asset = Some(candidate.clone());
                    matched_receipt = true;
                }
            }
        }
        if let Some(asset) = asset {
            if !release.files.contains_key(&asset) {
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
            if duplicate_new && (matched_receipt || matched_legacy) {
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

fn rewrite_legacy_variable_lines(text: &str) -> (String, Vec<SelectorChange>) {
    let mut updated = String::with_capacity(text.len());
    let mut changes = Vec::new();
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let rewritten = line.replace("AGENT_FABRIC_LABEL", "PROVENANT_AGENT_LABEL");
        if rewritten != line {
            changes.push(SelectorChange {
                selector: format!("line {}", index + 1),
                before: Some(json!(line)),
                after: Some(json!(rewritten)),
            });
        }
        updated.push_str(&rewritten);
    }
    (updated, changes)
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
                let (updated, changes) = rewrite_legacy_variable_lines(text);
                if !changes.is_empty() {
                    plan.files.insert(
                        global.clone(),
                        Value::File {
                            bytes: updated.into_bytes(),
                            mode,
                        },
                    );
                    plan.changes
                        .entry(global.clone())
                        .or_default()
                        .extend(changes);
                }
            }
            Value::Link(link) if link == checkout.join("AGENTS.md") => {
                let root = managed::release_root(
                    targets.roots.state.as_path(),
                    release.manifest().source_digest,
                );
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
                    == managed::release_root(
                        targets.roots.state.as_path(),
                        release.manifest().source_digest,
                    )
                    .join("instructions/AGENTS.md")
                    || previous.is_some_and(|p| {
                        link == managed::release_root(
                            targets.roots.state.as_path(),
                            p.source_digest,
                        )
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
pub fn apply_install_config_with_domains<T: serde::de::DeserializeOwned + Serialize>(
    targets: &mut Targets,
    fs: &dyn SafeFs,
    sections: &crate::DomainSections<T>,
) -> Result<(), InstallError> {
    // `load` accepts a bounded optional reader and validates the entire user
    // configuration; no unrelated domain policy is interpreted by installation.
    let path = targets.roots.state.as_path().join("config.toml");
    let snapshot = managed::snapshot_scoped(&path, targets.roots.state.as_path(), fs)?;
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

fn legacy_destination(
    target: &managed::HarnessTarget,
    link: &crate::legacy::LegacyLink,
    git_hooks_dir: Option<&Path>,
) -> Option<PathBuf> {
    use crate::legacy::LegacyArea;
    match link.area {
        LegacyArea::Skills => Some(target.skills_dir.join(&link.name)),
        LegacyArea::Workflows => Some(
            target
                .skills_dir
                .with_file_name("workflows")
                .join(&link.name),
        ),
        LegacyArea::Instruction | LegacyArea::GlobalInstruction => target.instruction.clone(),
        LegacyArea::Companion => target
            .instruction
            .as_ref()
            .map(|path| path.with_file_name(&link.name)),
        LegacyArea::GitHook => git_hooks_dir.map(|path| path.join(&link.name)),
        LegacyArea::OpenCodeInstruction => None,
    }
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
            path,
        });
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
            let Some(path) = legacy_destination(target, &link, targets.git_hooks_dir.as_deref())
            else {
                continue;
            };
            let snap = if let Some(s) = entries.get(&path) {
                s.clone()
            } else {
                let s = snapshot_target(&path, targets, fs)?;
                entries.insert(path.clone(), s.clone());
                s
            };
            if link.area == LegacyArea::GitHook {
                match snap {
                    Some(Snapshot {
                        value: Value::Link(actual),
                        ..
                    }) if actual == link.target => {
                        if link.retained {
                            if targets.hooks {
                                if let Some(spec) = targets
                                    .hook_specs
                                    .iter()
                                    .find(|spec| spec.harness.is_none() && spec.event == link.name)
                                {
                                    if !plan.files.contains_key(&path) {
                                        let bytes = format!(
                                            "#!/bin/sh\nexec {} \"$@\"\n",
                                            crate::hooks::command(spec, targets)?
                                        )
                                        .into_bytes();
                                        let value = Value::File { bytes, mode: 0o755 };
                                        plan.owned.push(OwnedEntry {
                                            path: AbsolutePath::new(&path)?,
                                            harness: None,
                                            selector: format!("git-hook:{}", link.name),
                                            expected: managed::owned(&value)?,
                                        });
                                        plan.authorised.insert(path.clone());
                                        plan.files.insert(path.clone(), value);
                                    }
                                } else {
                                    return Err(InstallError::HooksUnavailable);
                                }
                            } else {
                                add_finding(&mut plan.findings, "legacy_entry", &path)?;
                            }
                        } else {
                            plan.authorised.insert(path.clone());
                            plan.removals.insert(path);
                        }
                    }
                    Some(snapshot) => {
                        if targets.hooks && link.retained {
                            let spec = targets
                                .hook_specs
                                .iter()
                                .find(|spec| spec.harness.is_none() && spec.event == link.name)
                                .ok_or(InstallError::HooksUnavailable)?;
                            let desired = Value::File {
                                bytes: format!(
                                    "#!/bin/sh\nexec {} \"$@\"\n",
                                    crate::hooks::command(spec, targets)?
                                )
                                .into_bytes(),
                                mode: 0o755,
                            };
                            if snapshot.value == desired {
                                plan.owned.push(OwnedEntry {
                                    path: AbsolutePath::new(&path)?,
                                    harness: None,
                                    selector: format!("git-hook:{}", link.name),
                                    expected: managed::owned(&desired)?,
                                });
                            } else {
                                add_finding(&mut plan.findings, "legacy_hook_conflict", &path)?;
                            }
                        } else {
                            add_finding(&mut plan.findings, "legacy_hook_conflict", &path)?;
                        }
                    }
                    None => {}
                }
                continue;
            }
            if let Some(Snapshot {
                value: Value::Link(actual),
                ..
            }) = snap
                && actual == link.target
            {
                plan.authorised.insert(path.clone());
                if !link.retained {
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
                        LegacyArea::Instruction
                        | LegacyArea::GlobalInstruction
                        | LegacyArea::Companion => link.replacement_asset.clone(),
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
    evidence: &mut crate::inspection::InspectionEvidence,
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
        for link in &links {
            use crate::legacy::LegacyArea;
            if matches!(
                link.area,
                LegacyArea::GitHook | LegacyArea::OpenCodeInstruction
            ) {
                continue;
            }
            let Some(path) = legacy_destination(&target, link, None) else {
                continue;
            };
            if blocked
                .iter()
                .any(|p: &PathBuf| path != *p && path.starts_with(p))
            {
                continue;
            }
            let current = managed::snapshot_inspected(&path, fs, Some(evidence))?;
            if let Some(Snapshot {
                value: Value::Link(actual),
                ..
            }) = current
            {
                if link.name.is_empty() {
                    blocked.push(path.clone());
                }
                if actual == link.target {
                    add_finding(findings, "legacy_entry", &path)?;
                }
            }
        }
        let config = managed::snapshot_inspected(&target.mcp_file, fs, Some(evidence))?;
        if config.is_some() {
            let (bytes, _) = file_for(&config, &target.mcp_file, target.harness)?;
            let old_mcp = get_for(&bytes, selector(target.harness), target.harness)?
                .is_some_and(|v| crate::legacy::mcp_matches(&v, target.harness, &home));
            let old_array = if target.harness == Harness::Opencode {
                crate::splice::get(&bytes, &["instructions"])?
                    .and_then(|v| v.as_array().cloned())
                    .is_some_and(|array| {
                        array.iter().any(|value| {
                            links.iter().any(|link| {
                                link.area == crate::legacy::LegacyArea::OpenCodeInstruction
                                    && value.as_str() == link.target.to_str()
                            })
                        })
                    })
            } else {
                false
            };
            if old_mcp || old_array {
                add_finding(findings, "legacy_entry", &target.mcp_file)?;
            }
        }
        for path in target.hook_file.iter() {
            let current = managed::snapshot_inspected(path, fs, Some(evidence))?;
            if current.is_some() {
                let (bytes, _) = file(&current, path)?;
                match crate::legacy::clean_hooks_at_home(
                    &bytes,
                    target.harness,
                    &checkout,
                    Some(&home),
                ) {
                    Ok((new, _, unresolved)) if new != bytes || !unresolved.is_empty() => {
                        add_finding(findings, "legacy_entry", path)?;
                    }
                    Ok(_) => {}
                    Err(InstallError::LegacyUnresolved(_)) => {
                        add_finding(findings, "legacy_entry", path)?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    if let Some(directory) = &legacy.git_hooks_dir {
        let directory = directory.to_path_buf()?;
        managed::normal_absolute(&directory)?;
        for link in crate::legacy::git_hook_links(&checkout)? {
            let path = directory.join(&link.name);
            let current = managed::snapshot_inspected(&path, fs, Some(evidence))?;
            match current {
                Some(Snapshot {
                    value: Value::Link(actual),
                    ..
                }) if actual == link.target => {
                    add_finding(findings, "legacy_entry", &path)?;
                }
                Some(current)
                    if !receipt.entries.iter().any(|entry| {
                        entry.path.to_path_buf().is_ok_and(|p| p == path)
                            && managed::matches_owned(&current.value, &entry.expected)
                    }) =>
                {
                    add_finding(findings, "legacy_hook_conflict", &path)?;
                }
                _ => {}
            }
        }
    }
    let path = home.join(".agents/AGENTS.md");
    let current = managed::snapshot_inspected(&path, fs, Some(evidence))?;
    if matches!(&current,Some(Snapshot{value:Value::Link(link),..}) if link==&checkout.join("AGENTS.md"))
    {
        add_finding(findings, "legacy_entry", &path)?;
    }
    if let Some(Snapshot {
        value: Value::File { bytes, .. },
        ..
    }) = current
        && bytes
            .windows(b"AGENT_FABRIC_LABEL".len())
            .any(|s| s == b"AGENT_FABRIC_LABEL")
    {
        add_finding(findings, "legacy_instruction_variable", &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{rewrite_legacy_variable_lines, set_with_limits};
    use crate::managed::SelectorChange;
    use provenant_records::Limits;
    use serde_json::json;

    #[test]
    fn fabric_table_comments_survive_replacement_and_removal_without_empty_tables() {
        let before = b"# fabric table note\n[mcp_servers.fabric]\ncommand = 'old'\n[mcp_servers.fabric.env]\n[mcp_servers.other]\ncommand = 'keep'\n";
        let replacement = json!({"command": "/new/provenant", "env": {}});
        let replaced = set_with_limits(
            before,
            "toml:mcp_servers.fabric",
            Some(&replacement),
            Limits::CONFIG,
        )
        .expect("selector replacement should preserve valid TOML");
        let replaced = String::from_utf8(replaced).expect("TOML remains UTF-8");
        assert!(replaced.contains("# fabric table note"));
        assert!(replaced.contains("[mcp_servers.fabric]"));
        assert!(replaced.contains("[mcp_servers.other]"));
        assert!(!replaced.contains("[mcp_servers.fabric.env]"));

        let removed = set_with_limits(before, "toml:mcp_servers.fabric", None, Limits::CONFIG)
            .expect("selector removal should preserve valid TOML");
        let removed = String::from_utf8(removed).expect("TOML remains UTF-8");
        assert!(removed.contains("# fabric table note"));
        assert!(removed.contains("[mcp_servers.other]"));
        assert!(!removed.contains("[mcp_servers.fabric"));
        assert!(!removed.contains("[mcp_servers]"));
    }

    #[test]
    fn instruction_preview_records_each_changed_line_exactly() {
        let before = "first AGENT_FABRIC_LABEL\r\nAGENT_FABRIC_LABEL and again\nunchanged";
        let (after, changes) = rewrite_legacy_variable_lines(before);
        assert_eq!(
            after,
            "first PROVENANT_AGENT_LABEL\r\nPROVENANT_AGENT_LABEL and again\nunchanged"
        );
        assert_eq!(
            changes,
            vec![
                SelectorChange {
                    selector: "line 1".into(),
                    before: Some(json!("first AGENT_FABRIC_LABEL\r\n")),
                    after: Some(json!("first PROVENANT_AGENT_LABEL\r\n")),
                },
                SelectorChange {
                    selector: "line 2".into(),
                    before: Some(json!("AGENT_FABRIC_LABEL and again\n")),
                    after: Some(json!("PROVENANT_AGENT_LABEL and again\n")),
                },
            ]
        );
    }
}
