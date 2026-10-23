//! Skill source catalogues used by managed installation planning.
use crate::{invalid, io, Result};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

pub const SHARED_NAMES: [&str; 1] = ["_shared"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalogues {
    /// Python dictionary insertion order: skills, shared entries, then library skills.
    pub managed: Vec<(String, PathBuf)>,
    pub custom: Vec<(String, PathBuf)>,
}

fn descendants(root: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let mut children = fs::read_dir(root)
        .map_err(|e| io(root, e))?
        .map(|entry| entry.map(|entry| entry.path()).map_err(|e| io(root, e)))
        .collect::<Result<Vec<_>>>()?;
    children.sort();
    for path in children {
        output.push(path.clone());
        if path.is_dir() && !path.is_symlink() {
            descendants(&path, output)?;
        }
    }
    Ok(())
}

pub fn skills(
    source: &Path,
    optional: bool,
    validate_content: bool,
) -> Result<Vec<(String, PathBuf)>> {
    if !source.is_dir() {
        if optional && !source.exists() && !source.is_symlink() {
            return Ok(vec![]);
        }
        return Err(invalid("source must be an existing skill directory"));
    }
    if validate_content {
        let mut paths = vec![];
        descendants(source, &mut paths)?;
        paths.sort_by_cached_key(|p| {
            p.components()
                .map(|c| crate::pending::path_sort_key(c.as_os_str()))
                .collect::<Vec<_>>()
        });
        for path in paths {
            if path.is_symlink() {
                let relative = path
                    .strip_prefix(source)
                    .map_err(|e| invalid(e.to_string()))?;
                return Err(invalid(format!(
                    "skill source contains a symlink: {}",
                    relative.display()
                )));
            }
        }
    }
    let mut entries = fs::read_dir(source)
        .map_err(|e| io(source, e))?
        .map(|entry| entry.map(|entry| entry.path()).map_err(|e| io(source, e)))
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by_cached_key(|p| crate::pending::path_sort_key(p.as_os_str()));
    let mut result = vec![];
    for child in entries {
        let marker = child.join("SKILL.md");
        // pathlib glob matches a named entry of any type, including a directory.
        // The wildcard step includes hidden directories and follows directory links.
        if !child.is_dir() || fs::symlink_metadata(&marker).is_err() {
            continue;
        }
        let Some(name) = child.file_name().and_then(OsStr::to_str) else {
            return Err(invalid("source contains an invalid skill name"));
        };
        if !crate::manifest::is_skill_name(name) {
            return Err(invalid("source contains an invalid skill name"));
        }
        result.push((name.into(), crate::resolve(&child)?));
    }
    Ok(result)
}

fn shared(source: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut result = vec![];
    for name in SHARED_NAMES {
        let path = source.join(name);
        if path.is_dir() && !path.is_symlink() {
            result.push((name.into(), crate::resolve(&path)?));
        }
    }
    Ok(result)
}

fn names(entries: &[(String, PathBuf)]) -> BTreeSet<&str> {
    entries.iter().map(|(name, _)| name.as_str()).collect()
}

pub fn catalogues(
    source: &Path,
    custom_source: Option<&Path>,
    allowed_collisions: &[(String, PathBuf)],
) -> Result<Catalogues> {
    let mut managed = skills(source, false, true)?;
    for (name, path) in shared(source)? {
        if let Some((_, existing)) = managed
            .iter_mut()
            .find(|(current, _)| current.as_str() == name)
        {
            *existing = path;
        } else {
            managed.push((name, path));
        }
    }
    let library = if source.file_name() == Some(OsStr::new("skills")) {
        skills(
            &source.parent().unwrap_or(Path::new(".")).join("library"),
            true,
            true,
        )?
    } else {
        vec![]
    };
    let managed_names = names(&managed);
    let library_collisions = library
        .iter()
        .filter(|(name, _)| managed_names.contains(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    if !library_collisions.is_empty() {
        return Err(invalid(format!(
            "library skill name collides with a workflow skill: {}",
            library_collisions.join(", ")
        )));
    }
    managed.extend(library);

    let mut custom = match custom_source {
        Some(path) => skills(path, true, false)?,
        None => vec![],
    };
    let managed_names = names(&managed);
    let mut collisions = custom
        .iter()
        .filter(|(name, _)| managed_names.contains(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    collisions.sort();
    for name in &collisions {
        if let Some((_, custom_path)) = custom.iter().find(|(candidate, _)| candidate == name) {
            let allowed = allowed_collisions
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, path)| crate::resolve(path))
                .transpose()?;
            if allowed.as_ref() == Some(custom_path) {
                custom.retain(|(candidate, _)| candidate != name);
            }
        }
    }
    let managed_names = names(&managed);
    collisions = custom
        .iter()
        .filter(|(name, _)| managed_names.contains(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect();
    collisions.sort();
    if !collisions.is_empty() {
        let details = collisions
            .iter()
            .filter_map(|name| {
                let managed_path = managed
                    .iter()
                    .find(|(candidate, _)| candidate == name)?
                    .1
                    .display();
                let custom_path = custom
                    .iter()
                    .find(|(candidate, _)| candidate == name)?
                    .1
                    .display();
                Some(format!("{managed_path} conflicts with {custom_path}"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(invalid(format!("skill source name collision: {details}")));
    }
    Ok(Catalogues { managed, custom })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_matches_python_order_and_custom_collision_policy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("skills");
        let custom = dir.path().join("custom");
        let library = dir.path().join("library");
        for name in ["zeta", "alpha"] {
            fs::create_dir_all(source.join(name)).unwrap();
            fs::write(source.join(name).join("SKILL.md"), "skill").unwrap();
        }
        fs::create_dir_all(source.join("_shared")).unwrap();
        fs::create_dir_all(library.join("lib")).unwrap();
        fs::write(library.join("lib/SKILL.md"), "library").unwrap();
        fs::create_dir_all(custom.join("alpha")).unwrap();
        fs::write(custom.join("alpha/SKILL.md"), "custom").unwrap();
        fs::create_dir_all(custom.join("personal")).unwrap();
        fs::write(custom.join("personal/SKILL.md"), "custom").unwrap();

        let result = catalogues(&source, Some(&custom), &[]).unwrap_err();
        assert!(result.to_string().contains("skill source name collision"));
        let allowed = vec![("alpha".into(), custom.join("alpha"))];
        let result = catalogues(&source, Some(&custom), &allowed).unwrap();
        assert_eq!(
            result
                .managed
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta", "_shared", "lib"]
        );
        assert_eq!(
            result
                .custom
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["personal"]
        );

        let Some(output) = crate::oracle::python(
            "scripts/manage_installation.py",
            "import json\nc=m._catalogues(p,p.parent/'custom',allowed_collisions={'alpha':p.parent/'custom'/'alpha'})\nprint(json.dumps([[ [n,str(v) ] for n,v in c[0].items() ],[[n,str(v)] for n,v in c[1].items()]]))",
            &source,
        ) else {
            return;
        };
        let old: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let actual = serde_json::json!([result.managed, result.custom]);
        assert_eq!(old, actual);
    }

    #[cfg(unix)]
    #[test]
    fn managed_sources_refuse_nested_symlinks_before_cataloguing() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("skills");
        fs::create_dir_all(source.join("one")).unwrap();
        fs::write(source.join("one/SKILL.md"), "skill").unwrap();
        std::os::unix::fs::symlink(dir.path(), source.join("one/link")).unwrap();
        assert!(skills(&source, false, true)
            .unwrap_err()
            .to_string()
            .contains("skill source contains a symlink: one/link"));
    }
}

#[cfg(test)]
mod glob_tests {
    use super::*;
    #[test]
    fn skill_marker_glob_matches_directories_symlinked_dirs_hidden_dirs_and_sorting() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("custom");
        fs::create_dir(&custom).unwrap();
        for name in ["zeta", "alpha", "directory-marker"] {
            fs::create_dir_all(custom.join(name)).unwrap();
        }
        fs::write(custom.join("zeta/SKILL.md"), "z").unwrap();
        fs::write(custom.join("alpha/SKILL.md"), "a").unwrap();
        fs::create_dir(custom.join("directory-marker/SKILL.md")).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("SKILL.md"), "link").unwrap();
        std::os::unix::fs::symlink(&elsewhere, custom.join("linked")).unwrap();
        let Some(old)=crate::oracle::python("scripts/manage_installation.py","import json\nprint(json.dumps([[k,str(v)] for k,v in m._skills(p,validate_content=False).items()]))",&custom)else{return;};
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&old).unwrap(),
            serde_json::json!(skills(&custom, false, false).unwrap())
        );
        fs::create_dir(custom.join(".hidden")).unwrap();
        fs::write(custom.join(".hidden/SKILL.md"), "h").unwrap();
        let Some(old) = crate::oracle::python(
            "scripts/manage_installation.py",
            "try: m._skills(p,validate_content=False)\nexcept m.InstallError as e: print(str(e))",
            &custom,
        ) else {
            return;
        };
        assert_eq!(
            skills(&custom, false, false)
                .unwrap_err()
                .to_string()
                .as_bytes(),
            old.strip_suffix(b"\n").unwrap()
        );
    }
}
