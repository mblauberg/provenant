//! Repository-owned Claude workflows and their ownership receipt.
use std::{collections::BTreeMap, fs, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use crate::{invalid, io, pending, Result};

pub const MANIFEST_NAME: &str = ".agent-harness-workflows-installation.json";

pub fn valid_name(name: &str) -> bool {
    name.strip_suffix(".js").is_some_and(|stem| !stem.is_empty() && stem.as_bytes()[0].is_ascii_lowercase_or_digit() && stem.bytes().all(|c| c.is_ascii_lowercase_or_digit() || c == b'-'))
}

trait NameByte { fn is_ascii_lowercase_or_digit(self) -> bool; }
impl NameByte for u8 { fn is_ascii_lowercase_or_digit(self) -> bool { self.is_ascii_lowercase() || self.is_ascii_digit() } }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub owner: String,
    pub source_target: String,
    pub source_sha256: String,
    // The legacy validator deliberately treats this as opaque.
    pub installed_at: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: Value,
    pub owner: String,
    pub surface: String,
    pub target_root: String,
    pub managed: BTreeMap<String, Entry>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn manifest_path(target: &Path) -> PathBuf { target.parent().unwrap_or(Path::new(".")).join(MANIFEST_NAME) }
fn sha(path: &Path) -> Result<String> { Ok(pending::hex(&Sha256::digest(fs::read(path).map_err(|e| io(path, e))?))) }

pub(crate) fn valid_sha(value: &str) -> bool { value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) }

fn load(target: &Path, now: &str) -> Result<Manifest> {
    let path = manifest_path(target);
    if !path.exists() {
        return Ok(Manifest { schema_version: Value::from(1), owner: "agent-harness".into(), surface: "claude-workflows".into(), target_root: pending::resolve(target)?.to_string_lossy().into(), managed: BTreeMap::new(), extra: BTreeMap::from([("updated_at".into(), Value::from(now))]) });
    }
    let bytes = fs::read(&path).map_err(|e| invalid(format!("installation manifest is unreadable: {}", io(&path, e))))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("installation manifest is unreadable: {e}")))?;
    let valid_header = value.is_object() && pending::schema_one(&value["schema_version"]) && value["owner"] == "agent-harness" && value["surface"] == "claude-workflows" && value["target_root"].as_str() == Some(pending::resolve(target)?.to_string_lossy().as_ref()) && value["managed"].is_object();
    if !valid_header { return Err(invalid("workflow installation manifest is invalid")); }
    if let Some(entries) = value["managed"].as_object() {
        for (name, value) in entries {
            let entry: Entry = serde_json::from_value(value.clone()).map_err(|_| invalid(format!("workflow installation manifest entry is invalid: {name}")))?;
            if !valid_name(name) || entry.owner != "agent-harness" || !Path::new(&entry.source_target).is_absolute() || !valid_sha(&entry.source_sha256) { return Err(invalid(format!("workflow installation manifest entry is invalid: {name}"))); }
        }
    }
    serde_json::from_value(value).map_err(|_| invalid("workflow installation manifest is invalid"))
}

fn write(target: &Path, manifest: &mut Manifest, now: &str) -> Result<()> {
    manifest.extra.insert("updated_at".into(), now.into());
    pending::atomic_write(&manifest_path(target), ".workflows-installation.", &pending::json_bytes(manifest)?)
}

fn sources(source: &Path) -> Result<BTreeMap<String, PathBuf>> {
    if !source.is_dir() { return Err(invalid("source must be an existing workflow directory")); }
    let mut paths = BTreeMap::new();
    for entry in fs::read_dir(source).map_err(|e| io(source, e))? {
        let path = entry.map_err(|e| io(source, e))?.path();
        if path.extension().is_none_or(|ext| ext != "js") { continue; }
        let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| invalid("source contains an invalid workflow"))?;
        if !valid_name(name) || path.is_symlink() || !path.is_file() { return Err(invalid("source contains an invalid workflow")); }
        paths.insert(name.into(), pending::resolve(&path)?);
    }
    if paths.is_empty() { return Err(invalid("source contains an invalid workflow")); }
    Ok(paths)
}

fn same_link(destination: &Path, source: &Path) -> bool {
    destination.is_symlink() && pending::resolve(destination).ok().zip(pending::resolve(source).ok()).is_some_and(|(a,b)| a == b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State { Missing, Unmanaged, Managed, Stale, Conflicting }

fn state(destination: &Path, source: &Path, entry: Option<&Entry>) -> Result<State> {
    let Some(entry) = entry else { return Ok(if destination.exists() || destination.is_symlink() { State::Unmanaged } else { State::Missing }); };
    if same_link(destination, source) {
        return Ok(if pending::resolve(Path::new(&entry.source_target))? == pending::resolve(source)? && entry.source_sha256 == sha(source)? { State::Managed } else { State::Stale });
    }
    Ok(if same_link(destination, Path::new(&entry.source_target)) || (!destination.exists() && !destination.is_symlink()) { State::Stale } else { State::Conflicting })
}

fn entry(source: &Path, now: &str) -> Result<Entry> {
    Ok(Entry { owner: "agent-harness".into(), source_target: source.to_string_lossy().into(), source_sha256: sha(source)?, installed_at: now.into() })
}

pub fn canonical_directory_link(source: &Path, target: &Path) -> bool {
    target.is_symlink() && fs::canonicalize(target).ok().zip(fs::canonicalize(source).ok()).is_some_and(|(a,b)| a == b)
}

pub fn preflight(source: &Path, target: &Path, reject_unmanaged: bool, now: &str) -> Result<()> {
    if target.is_symlink() {
        if canonical_directory_link(source, target) { return Ok(()); }
        return Err(invalid("target must not be a non-canonical symlink"));
    }
    let target = pending::resolve(target)?;
    let sources = sources(source)?;
    let manifest = load(&target, now)?;
    let mut conflicts = Vec::new();
    for (name, source) in &sources {
        let state = state(&target.join(name), source, manifest.managed.get(name))?;
        if state == State::Conflicting || (reject_unmanaged && state == State::Unmanaged) { conflicts.push(name.clone()); }
    }
    for (name, entry) in &manifest.managed {
        let destination = target.join(name);
        if !sources.contains_key(name) && (destination.exists() || destination.is_symlink()) && !same_link(&destination, Path::new(&entry.source_target)) { conflicts.push(name.clone()); }
    }
    if !conflicts.is_empty() { return Err(invalid(format!("conflicting managed targets: {}", conflicts.join(", ")))); }
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed { pub linked: usize, pub existing: i64, pub failures: Vec<String> }

/// Caller handles the canonical-directory-link fast path before invoking this.
pub fn install(source: &Path, target: &Path, now: &str) -> Result<Installed> {
    preflight(source, target, false, now)?;
    let target = pending::resolve(target)?;
    let sources = sources(source)?;
    let mut manifest = load(&target, now)?;
    let states = sources.iter().map(|(name, source)| Ok((name.clone(), state(&target.join(name), source, manifest.managed.get(name))?))).collect::<Result<BTreeMap<_,_>>>()?;
    fs::create_dir_all(&target).map_err(|e| io(&target, e))?;
    let mut newly_managed = false;
    for (name, state) in &states {
        if *state == State::Missing { manifest.managed.insert(name.clone(), entry(&sources[name], now)?); newly_managed = true; }
    }
    // Ownership is durable before publishing any new link: retry repairs gaps.
    if newly_managed { write(&target, &mut manifest, now)?; }
    let mut linked = 0;
    for (name, source) in &sources {
        if matches!(states[name], State::Missing | State::Stale) {
            pending::replace_link(&target.join(name), source)?;
            manifest.managed.insert(name.clone(), entry(source, now)?);
            linked += 1;
        }
    }
    let retired = manifest.managed.keys().filter(|name| !sources.contains_key(*name)).cloned().collect::<Vec<_>>();
    for name in retired {
        let destination = target.join(&name);
        if destination.is_symlink() { fs::remove_file(&destination).map_err(|e| io(&destination, e))?; }
        manifest.managed.remove(&name); linked += 1;
    }
    write(&target, &mut manifest, now)?;
    let mut failures = vec![];
    for (name, source) in &sources {
        let dest = target.join(name);
        let integrity = if states[name] == State::Unmanaged { "unmanaged" }
        else if same_link(&dest, source) { "present" }
        else if !dest.exists() && !dest.is_symlink() { "missing" }
        else if dest.is_symlink() { "foreign" }
        else if sha(&dest).ok() == Some(sha(source)?) { "present" } else { "noncanonical" };
        if integrity != "present" { failures.push(format!("{name}={integrity}")); }
    }
    Ok(Installed { linked, existing: sources.len() as i64 - linked as i64, failures })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    const NOW: &str = "2026-10-23T00:00:00Z";
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap(); let source = dir.path().join("workflows"); fs::create_dir(&source).unwrap(); fs::write(source.join("one.js"), "one").unwrap(); fs::write(source.join("two.js"), "two").unwrap(); let target = dir.path().join("claude/workflows"); (dir, source, target)
    }
    #[test]
    fn installs_repairs_and_retires_owned_links() {
        let (_dir, source, target) = fixture(); assert_eq!(install(&source, &target, NOW).unwrap().linked, 2); assert_eq!(install(&source, &target, NOW).unwrap().existing, 2);
        fs::remove_file(target.join("one.js")).unwrap(); assert_eq!(install(&source, &target, NOW).unwrap().linked, 1);
        fs::remove_file(source.join("two.js")).unwrap(); assert_eq!(install(&source, &target, NOW).unwrap().linked, 1); assert!(!target.join("two.js").is_symlink());
    }
    #[test]
    fn unmanaged_equivalent_files_and_links_are_not_adopted() {
        for link in [false, true] {
            let (_dir, source, target) = fixture(); fs::create_dir_all(&target).unwrap();
            if link { symlink(source.join("one.js"), target.join("one.js")).unwrap(); } else { fs::write(target.join("one.js"), "one").unwrap(); }
            assert!(preflight(&source, &target, true, NOW).is_err()); assert!(!manifest_path(&target).exists());
            let result = install(&source, &target, NOW).unwrap(); assert_eq!(result.failures, ["one.js=unmanaged"]); assert!(!load(&target, NOW).unwrap().managed.contains_key("one.js"));
        }
    }
    #[test]
    fn foreign_broken_managed_link_refuses_all_mutation() {
        let (dir, source, target) = fixture(); install(&source, &target, NOW).unwrap(); let before = fs::read(manifest_path(&target)).unwrap(); fs::remove_file(target.join("one.js")).unwrap(); let foreign = dir.path().join("missing"); symlink(&foreign, target.join("one.js")).unwrap();
        assert!(install(&source, &target, NOW).is_err()); assert_eq!(fs::read_link(target.join("one.js")).unwrap(), foreign); assert_eq!(fs::read(manifest_path(&target)).unwrap(), before);
    }
    #[test]
    fn durable_ownership_repairs_interrupted_publication() {
        let (_dir, source, target) = fixture(); fs::create_dir_all(&target).unwrap(); let mut manifest = load(&target, NOW).unwrap(); for (name,path) in sources(&source).unwrap() { manifest.managed.insert(name, entry(&path, NOW).unwrap()); } write(&target, &mut manifest, NOW).unwrap();
        symlink(source.join("one.js"), target.join("one.js")).unwrap(); assert_eq!(install(&source, &target, NOW).unwrap().linked, 1);
    }
    #[test]
    fn source_validation_and_canonical_directory_link() {
        let (dir, source, target) = fixture(); fs::create_dir_all(target.parent().unwrap()).unwrap(); symlink(&source, &target).unwrap(); assert!(canonical_directory_link(&source, &target)); preflight(&source, &target, true, NOW).unwrap(); assert!(!manifest_path(&target).exists());
        symlink(dir.path().join("missing"), source.join("bad.js")).unwrap(); assert!(sources(&source).is_err());
    }
    #[test]
    fn relocation_updates_only_the_recorded_link() {
        let (dir, source, target) = fixture(); install(&source, &target, NOW).unwrap(); let next = dir.path().join("next"); fs::rename(&source, &next).unwrap(); assert_eq!(install(&next, &target, NOW).unwrap().linked, 2); assert_eq!(fs::read_link(target.join("one.js")).unwrap(), next.join("one.js"));
    }
    #[test]
    fn python_differential_install_and_receipt() {
        let (_dir, source, target) = fixture(); let result = install(&source, &target, NOW).unwrap();
        let Some(output) = crate::oracle::python("scripts/install-workflows", "import json\nm.SOURCE=p.parent/'workflows'\nm._now=lambda: '2026-10-23T00:00:00Z'\nt=p.parent/'python'/'workflows'\nr=m.install(t)\nd=m._load_manifest(t)\nd['target_root']='TARGET'\nprint(json.dumps([r,d]))", &source) else { return; };
        let old: Value = serde_json::from_slice(&output).unwrap(); let mut new = serde_json::to_value(load(&target, NOW).unwrap()).unwrap(); new["target_root"] = "TARGET".into();
        assert_eq!(old, serde_json::json!([[result.linked,result.existing,result.failures],new]));
    }
}
