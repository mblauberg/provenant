//! Fast manifest policy checks. No Cargo subprocess or metadata build required.
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, TableLike};

// ADR 0028 decision 3 and PORTING R6. tempfile is checked separately below.
const DIRECT: &[&str] = &[
    "clap",
    "jiff",
    "libc",
    "regex",
    "rusqlite",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
    "toml_edit",
];

fn read_manifest(path: &Path) -> Result<DocumentMut, Box<dyn Error>> {
    Ok(fs::read_to_string(path)?.parse()?)
}

fn member_manifests(root: &Path, workspace: &DocumentMut) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let members = workspace["workspace"]["members"]
        .as_array()
        .ok_or("workspace members must be an array")?;
    let mut paths = Vec::new();
    for member in members {
        let member = member.as_str().ok_or("member must be a path")?;
        if let Some(parent) = member.strip_suffix("/*") {
            for entry in fs::read_dir(root.join(parent))? {
                let path = entry?.path().join("Cargo.toml");
                if path.is_file() {
                    paths.push(path);
                }
            }
        } else {
            if member.contains(['*', '?', '[']) {
                return Err(format!("unsupported workspace member pattern: {member}").into());
            }
            paths.push(root.join(member).join("Cargo.toml"));
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn check_table(table: &dyn TableLike, kind: &str, workspace: &DocumentMut) -> Result<(), String> {
    for (alias, declared) in table.iter() {
        let dependency = if declared.get("workspace").and_then(Item::as_bool) == Some(true) {
            workspace["workspace"]["dependencies"]
                .get(alias)
                .ok_or_else(|| format!("missing workspace dependency: {alias}"))?
        } else {
            declared
        };
        if dependency.get("path").and_then(Item::as_str).is_some() {
            continue;
        }
        let name = dependency
            .get("package")
            .and_then(Item::as_str)
            .unwrap_or(alias);
        if !DIRECT.contains(&name) && !(name == "tempfile" && kind == "dev-dependencies") {
            return Err(format!(
                "{kind}: {alias} ({name}) is not permitted by ADR 0028/R6"
            ));
        }
    }
    Ok(())
}

fn check_dependencies(table: &dyn TableLike, workspace: &DocumentMut) -> Result<(), String> {
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = table.get(kind).and_then(Item::as_table_like) {
            check_table(dependencies, kind, workspace)?;
        }
    }
    Ok(())
}

#[test]
fn every_member_obeys_the_direct_dependency_policy() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = read_manifest(&root.join("Cargo.toml"))?;
    for path in member_manifests(root, &workspace)? {
        let manifest = read_manifest(&path)?;
        check_dependencies(manifest.as_table(), &workspace)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if let Some(targets) = manifest.get("target").and_then(Item::as_table_like) {
            for (target, tables) in targets.iter() {
                let tables = tables.as_table_like().ok_or("invalid target table")?;
                check_dependencies(tables, &workspace)
                    .map_err(|error| format!("{} [{target}]: {error}", path.display()))?;
            }
        }
    }
    Ok(())
}

#[test]
fn transitive_crates_and_renamed_dependencies_are_rejected() -> Result<(), Box<dyn Error>> {
    let workspace: DocumentMut = "[workspace.dependencies]\nlog = '0.4'".parse()?;
    for declaration in [
        "getrandom = '0.4'",
        "rng = { package = 'getrandom', version = '0.4' }",
        "log.workspace = true",
    ] {
        let manifest: DocumentMut = format!("[dependencies]\n{declaration}").parse()?;
        assert!(check_dependencies(manifest.as_table(), &workspace).is_err());
    }
    Ok(())
}

#[test]
fn tempfile_is_allowed_only_for_dev_dependencies() -> Result<(), Box<dyn Error>> {
    let workspace: DocumentMut = "[workspace.dependencies]\ntempfile = '3'".parse()?;
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        for declaration in [
            "tempfile = '3'",
            "scratch = { package = 'tempfile', version = '3' }",
            "tempfile.workspace = true",
        ] {
            let manifest: DocumentMut = format!("[{kind}]\n{declaration}").parse()?;
            assert_eq!(
                check_dependencies(manifest.as_table(), &workspace).is_ok(),
                kind == "dev-dependencies"
            );
        }
    }
    Ok(())
}
