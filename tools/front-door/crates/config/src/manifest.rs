//! Schema-v1 managed-skill receipts and the full skill-tree digest.
use crate::{invalid, io, pending, workflows::valid_sha, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const MANIFEST_NAME: &str = ".agent-harness-installation.json";

pub fn is_skill_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}
pub fn is_managed_name(name: &str) -> bool {
    name == "_shared" || is_skill_name(name)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub owner: String,
    pub source_target: String,
    pub source_sha256: String,
    pub installed_at: Value,
    /// Opaque compatibility data; never interpreted as rename intent.
    pub history: Vec<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomLink {
    pub source_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: Value,
    pub owner: String,
    pub target_root: String,
    pub managed: BTreeMap<String, Entry>,
    pub custom: BTreeMap<String, CustomLink>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn descendants(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root).map_err(|e| io(root, e))? {
        let path = entry.map_err(|e| io(root, e))?.path();
        paths.push(path.clone());
        if path.is_dir() && !path.is_symlink() {
            descendants(&path, paths)?;
        }
    }
    Ok(())
}

pub fn sha_skill(root: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    let mut paths = vec![];
    if root.is_dir() {
        descendants(root, &mut paths)?;
    }
    paths.sort();
    for child in paths {
        let relative = child
            .strip_prefix(root)
            .map_err(|e| invalid(e.to_string()))?;
        if relative
            .components()
            .any(|p| p.as_os_str() == "__pycache__" || p.as_os_str() == ".DS_Store")
            || child.extension().is_some_and(|s| s == "pyc")
        {
            continue;
        }
        if child.is_symlink() {
            return Err(invalid(format!(
                "skill source contains a symlink: {}",
                relative.display()
            )));
        }
        if !child.is_file() {
            continue;
        }
        let name = relative
            .to_str()
            .ok_or_else(|| invalid("skill source path is not UTF-8"))?;
        digest.update(name.as_bytes());
        digest.update(b"\0");
        digest.update(
            if fs::metadata(&child)
                .map_err(|e| io(&child, e))?
                .permissions()
                .mode()
                & 0o111
                != 0
            {
                b"x"
            } else {
                b"-"
            },
        );
        digest.update(b"\0");
        digest.update(fs::read(&child).map_err(|e| io(&child, e))?);
        digest.update(b"\0");
    }
    Ok(pending::hex(&digest.finalize()))
}

pub fn manifest_path(target: &Path) -> PathBuf {
    target
        .parent()
        .unwrap_or(Path::new("."))
        .join(MANIFEST_NAME)
}

pub fn empty_manifest(target: &Path, now: &str) -> Result<Manifest> {
    Ok(Manifest {
        schema_version: Value::from(1),
        owner: "agent-harness".into(),
        target_root: pending::path_text(&crate::resolve(target)?)?,
        managed: BTreeMap::new(),
        custom: BTreeMap::new(),
        extra: BTreeMap::from([("updated_at".into(), now.into())]),
    })
}

pub fn validate_manifest(mut value: Value, target: &Path) -> Result<Manifest> {
    if !value.is_object()
        || !pending::schema_one(&value["schema_version"])
        || value["owner"] != "agent-harness"
        || !value["managed"].is_object()
    {
        return Err(invalid(
            "installation manifest is invalid or not owned by agent-harness",
        ));
    }
    if value["target_root"].as_str() != Some(pending::path_text(&crate::resolve(target)?)?.as_str())
    {
        return Err(invalid(
            "installation manifest belongs to a different target root",
        ));
    }
    if value.get("custom").is_none() {
        value["custom"] = serde_json::json!({});
    }
    let custom = value["custom"]
        .as_object()
        .ok_or_else(|| invalid("installation manifest custom links are invalid"))?;
    if let Some(managed) = value["managed"].as_object() {
        for (name, item) in managed {
            if !is_managed_name(name) {
                return Err(invalid(
                    "installation manifest contains an invalid skill name",
                ));
            }
            if item.as_object().is_none_or(|m| {
                m.len() != 5
                    || ![
                        "owner",
                        "source_target",
                        "source_sha256",
                        "installed_at",
                        "history",
                    ]
                    .iter()
                    .all(|k| m.contains_key(*k))
            }) || item["owner"] != "agent-harness"
            {
                return Err(invalid(format!(
                    "installation manifest entry is invalid: {name}"
                )));
            }
            if item["source_target"]
                .as_str()
                .is_none_or(|s| !Path::new(s).is_absolute())
            {
                return Err(invalid(format!(
                    "installation manifest source target is invalid: {name}"
                )));
            }
            if item["source_sha256"].as_str().is_none_or(|s| !valid_sha(s)) {
                return Err(invalid(format!(
                    "installation manifest digest is invalid: {name}"
                )));
            }
            if !item["history"].is_array() {
                return Err(invalid(format!(
                    "installation manifest history is invalid: {name}"
                )));
            }
        }
    }
    for (name, item) in custom {
        if !is_skill_name(name) {
            return Err(invalid(
                "installation manifest contains an invalid custom skill name",
            ));
        }
        if item
            .as_object()
            .is_none_or(|m| m.len() != 1 || !m.contains_key("source_target"))
            || item["source_target"]
                .as_str()
                .is_none_or(|s| !Path::new(s).is_absolute())
        {
            return Err(invalid(format!(
                "installation manifest custom link is invalid: {name}"
            )));
        }
    }
    if let Some(managed) = value["managed"].as_object() {
        let overlap = managed
            .keys()
            .filter(|k| custom.contains_key(*k))
            .cloned()
            .collect::<Vec<_>>();
        if !overlap.is_empty() {
            return Err(invalid(format!(
                "installation manifest contains overlapping managed and custom skill names: {}",
                overlap.join(", ")
            )));
        }
    }
    serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
}

pub fn load_manifest(target: &Path, now: &str) -> Result<Manifest> {
    let path = manifest_path(target);
    if !path.exists() {
        return empty_manifest(target, now);
    }
    let value = pending::read_json(&path, "installation manifest is unreadable")?;
    validate_manifest(value, target)
}

pub fn write_manifest(target: &Path, manifest: &mut Manifest, now: &str) -> Result<()> {
    manifest.extra.insert("updated_at".into(), now.into());
    crate::filesystem::atomic_write(
        &manifest_path(target),
        ".installation.",
        &pending::json_bytes(manifest)?,
    )
}

pub fn entry(source: &Path, history: Vec<Value>, now: &str) -> Result<Entry> {
    Ok(Entry {
        owner: "agent-harness".into(),
        source_target: pending::path_text(source)?,
        source_sha256: sha_skill(source)?,
        installed_at: now.into(),
        history,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: &str = "2026-10-23T00:00:00Z";
    #[test]
    fn managed_names_include_shared_but_custom_names_do_not() {
        assert!(is_managed_name("_shared"));
        assert!(!is_skill_name("_shared"));
        for name in ["../victim", ".", "Bad", "", "a/b", "-name", "a\n"] {
            assert!(!is_managed_name(name), "{name}");
        }
    }
    #[test]
    fn validates_root_ownership_disjoint_names_and_opaque_history() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("skills");
        let mut manifest = empty_manifest(&target, NOW).unwrap();
        manifest.managed.insert(
            "skill".into(),
            entry(
                dir.path(),
                vec![serde_json::json!({"unknown": [1,null]}), Value::Bool(true)],
                NOW,
            )
            .unwrap(),
        );
        let mut value = serde_json::to_value(manifest).unwrap();
        value["future"] = serde_json::json!(["é", "😀"]);
        let parsed = validate_manifest(value.clone(), &target).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        assert!(validate_manifest(value.clone(), &dir.path().join("other")).is_err());
        value["custom"]["skill"] = serde_json::json!({"source_target":"/custom"});
        assert!(validate_manifest(value, &target)
            .unwrap_err()
            .to_string()
            .contains("overlapping"));
    }
    #[test]
    fn missing_custom_defaults_but_bad_history_and_entry_keys_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("skills");
        let mut value = serde_json::to_value(empty_manifest(&target, NOW).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("custom");
        assert!(validate_manifest(value.clone(), &target)
            .unwrap()
            .custom
            .is_empty());
        value["managed"]["skill"] =
            serde_json::to_value(entry(dir.path(), vec![], NOW).unwrap()).unwrap();
        value["managed"]["skill"]["history"] = Value::Null;
        assert!(validate_manifest(value.clone(), &target)
            .unwrap_err()
            .to_string()
            .contains("history"));
        value["managed"]["skill"]["extra"] = 1.into();
        assert!(validate_manifest(value, &target)
            .unwrap_err()
            .to_string()
            .contains("entry is invalid"));
    }
    #[test]
    fn digest_covers_reference_content_and_executable_mode() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), "skill").unwrap();
        fs::create_dir(dir.path().join("references")).unwrap();
        let reference = dir.path().join("references/a");
        fs::write(&reference, "one").unwrap();
        let original = sha_skill(dir.path()).unwrap();
        fs::write(&reference, "two").unwrap();
        assert_ne!(sha_skill(dir.path()).unwrap(), original);
        fs::write(&reference, "one").unwrap();
        assert_eq!(sha_skill(dir.path()).unwrap(), original);
        let original_mode = fs::metadata(&reference).unwrap().permissions().mode();
        fs::set_permissions(
            &reference,
            fs::Permissions::from_mode(original_mode | 0o111),
        )
        .unwrap();
        assert_ne!(sha_skill(dir.path()).unwrap(), original);
    }
    #[test]
    fn ignored_files_do_not_hash_but_symlinks_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let base = sha_skill(dir.path()).unwrap();
        fs::create_dir(dir.path().join("__pycache__")).unwrap();
        fs::write(dir.path().join("__pycache__/compiled"), "cache").unwrap();
        fs::write(dir.path().join(".DS_Store"), "mac").unwrap();
        fs::write(dir.path().join("compiled.pyc"), "cache").unwrap();
        assert_eq!(sha_skill(dir.path()).unwrap(), base);
        std::os::unix::fs::symlink(dir.path().join("missing"), dir.path().join("reference"))
            .unwrap();
        assert!(sha_skill(dir.path())
            .unwrap_err()
            .to_string()
            .contains("symlink: reference"));
    }
    #[test]
    fn writes_sorted_ascii_json_with_newline_and_explicit_time() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("skills");
        let mut manifest = empty_manifest(&target, "before").unwrap();
        manifest.extra.insert("unicode".into(), "é😀".into());
        write_manifest(&target, &mut manifest, NOW).unwrap();
        let bytes = fs::read(manifest_path(&target)).unwrap();
        assert!(bytes.is_ascii());
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(
            load_manifest(&target, NOW).unwrap().extra["updated_at"],
            NOW
        );
        let Some(old) = crate::oracle::python("scripts/managed_installation_manifest.py", "import json\nd=json.loads(m.manifest_path(p).read_text()); print(json.dumps(d,indent=2,sort_keys=True))", &target) else { return; };
        assert_eq!(bytes, old);
    }
    #[test]
    fn python_digest_and_validation_differential() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        for name in ["SKILL.md", "a.txt", "a/z", "é😀"] {
            fs::write(dir.path().join(name), name).unwrap();
        }
        let Some(old) = crate::oracle::python(
            "scripts/managed_installation_manifest.py",
            "print(m.sha_skill(p))",
            dir.path(),
        ) else {
            return;
        };
        assert_eq!(
            sha_skill(dir.path()).unwrap(),
            String::from_utf8(old).unwrap().trim()
        );
        let target = dir.path().join("skills");
        let mut manifest = empty_manifest(&target, NOW).unwrap();
        manifest.managed.insert(
            "_shared".into(),
            entry(dir.path(), vec![Value::Bool(false)], NOW).unwrap(),
        );
        write_manifest(&target, &mut manifest, NOW).unwrap();
        let Some(old) = crate::oracle::python(
            "scripts/managed_installation_manifest.py",
            "import json; print(json.dumps(m.load_manifest(p),sort_keys=True))",
            &target,
        ) else {
            return;
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&old).unwrap(),
            serde_json::to_value(load_manifest(&target, NOW).unwrap()).unwrap()
        );
    }
}
