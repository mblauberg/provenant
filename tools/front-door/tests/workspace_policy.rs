//! Fast manifest policy checks. No Cargo subprocess or metadata build required.
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, TableLike};

// ADR 0028 decision 3 and docs/specs/rust-workspace.md#dependencies. tempfile is checked separately below.
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

// docs/specs/rust-workspace.md#crate-layering, without the provenant- prefix.
const LAYERS: &[(&str, &[&str])] = &[
    ("core", &[]),
    ("records", &["core"]),
    ("config", &["core", "records", "supervisor"]),
    ("supervisor", &["core", "records"]),
    ("confinement", &["core", "records", "config", "supervisor"]),
    (
        "providers",
        &[
            "core",
            "records",
            "config",
            "confinement",
            "quota",
            "supervisor",
        ],
    ),
    ("quota", &["records", "config"]),
    ("telemetry", &["core", "records"]),
    (
        "routing",
        &["core", "records", "config", "quota", "providers"],
    ),
    (
        "run",
        &["core", "records", "config", "supervisor", "confinement"],
    ),
    ("fabric-store", &["core", "records", "supervisor"]),
    (
        "fabric-runtime",
        &["core", "records", "config", "supervisor", "fabric-store"],
    ),
    ("fabric-mcp", &["core", "records", "fabric-runtime"]),
    ("workflow", &["core", "records", "supervisor"]),
    (
        "cli",
        &[
            "core",
            "records",
            "config",
            "confinement",
            "providers",
            "quota",
            "telemetry",
            "routing",
            "run",
            "fabric-runtime",
            "workflow",
            "supervisor",
        ],
    ),
    (
        "provenant",
        &[
            "core",
            "records",
            "config",
            "supervisor",
            "confinement",
            "providers",
            "quota",
            "run",
            "fabric-store",
            "fabric-runtime",
            "fabric-mcp",
            "workflow",
            "cli",
        ],
    ),
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

fn check_internal_dependencies(
    table: &dyn TableLike,
    name: &str,
    allowed: &[&str],
    workspace: &DocumentMut,
) -> Result<(), String> {
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(dependencies) = table.get(kind).and_then(Item::as_table_like) else {
            continue;
        };
        for (alias, declared) in dependencies.iter() {
            let dependency = if declared.get("workspace").and_then(Item::as_bool) == Some(true) {
                workspace["workspace"]["dependencies"]
                    .get(alias)
                    .ok_or_else(|| format!("missing workspace dependency: {alias}"))?
            } else {
                declared
            };
            let package = dependency
                .get("package")
                .and_then(Item::as_str)
                .unwrap_or(alias);
            let Some(internal) = package.strip_prefix("provenant-") else {
                continue;
            };
            let dev_exception = kind == "dev-dependencies"
                && (internal == name || (name == "fabric-mcp" && internal == "fabric-store"));
            if (internal == "cli" && name != "provenant")
                || !(allowed.contains(&internal) || dev_exception)
            {
                return Err(format!(
                    "{name} {kind}: {alias} ({package}) is not permitted by the crate layering table"
                ));
            }
        }
    }
    Ok(())
}

fn check_internal_edges(manifest: &DocumentMut, workspace: &DocumentMut) -> Result<(), String> {
    let package = manifest["package"]["name"]
        .as_str()
        .ok_or("package name must be a string")?;
    let name = package.strip_prefix("provenant-").unwrap_or(package);
    let (_, allowed) = LAYERS
        .iter()
        .find(|(member, _)| *member == name)
        .ok_or_else(|| format!("missing crate layering row for {package}"))?;
    check_internal_dependencies(manifest.as_table(), name, allowed, workspace)?;
    if let Some(targets) = manifest.get("target").and_then(Item::as_table_like) {
        for (target, tables) in targets.iter() {
            let tables = tables.as_table_like().ok_or("invalid target table")?;
            check_internal_dependencies(tables, name, allowed, workspace)
                .map_err(|error| format!("[{target}]: {error}"))?;
        }
    }
    Ok(())
}

#[test]
fn internal_edges_obey_the_layering_table() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = read_manifest(&root.join("Cargo.toml"))?;
    for path in member_manifests(root, &workspace)? {
        let manifest = read_manifest(&path)?;
        check_internal_edges(&manifest, &workspace)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

#[test]
fn routing_cannot_depend_on_run() -> Result<(), Box<dyn Error>> {
    let workspace: DocumentMut = "[workspace.dependencies]\nprovenant-run = { path = 'crates/run' }\nrunner = { package = 'provenant-run', path = 'crates/run' }".parse()?;
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        for scope in [String::new(), "target.'cfg(unix)'.".to_owned()] {
            for declaration in [
                "provenant-run = { path = '../run' }",
                "runner = { package = 'provenant-run', path = '../run' }",
                "provenant-run.workspace = true",
                "runner.workspace = true",
            ] {
                let manifest: DocumentMut = format!(
                    "[package]\nname = 'provenant-routing'\n[{scope}{kind}]\n{declaration}"
                )
                .parse()?;
                let error = check_internal_edges(&manifest, &workspace)
                    .err()
                    .ok_or("routing must not import run")?;
                assert!(error.contains("provenant-run"), "{error}");
            }
        }
    }
    Ok(())
}

#[test]
fn internal_dev_exceptions_do_not_allow_production_edges() -> Result<(), Box<dyn Error>> {
    let workspace: DocumentMut = "".parse()?;
    for (member, dependency) in [("supervisor", "supervisor"), ("fabric-mcp", "fabric-store")] {
        for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
            for scope in [String::new(), "target.'cfg(unix)'.".to_owned()] {
                let manifest: DocumentMut = format!(
                    "[package]\nname = 'provenant-{member}'\n[{scope}{kind}]\nprovenant-{dependency} = {{ path = '../{dependency}' }}"
                )
                .parse()?;
                assert_eq!(
                    check_internal_edges(&manifest, &workspace).is_ok(),
                    kind == "dev-dependencies",
                    "{member} -> {dependency} in {scope}{kind}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn libraries_cannot_depend_on_cli() -> Result<(), Box<dyn Error>> {
    let workspace: DocumentMut = "".parse()?;
    for (member, _) in LAYERS.iter().filter(|(name, _)| *name != "provenant") {
        for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let manifest: DocumentMut = format!(
                "[package]\nname = 'provenant-{member}'\n[{kind}]\nprovenant-cli = {{ path = '../cli' }}"
            )
            .parse()?;
            assert!(check_internal_edges(&manifest, &workspace).is_err());
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

#[test]
fn supervisor_mirrors_workspace_lints_except_for_its_unsafe_boundary() -> Result<(), Box<dyn Error>>
{
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = read_manifest(&root.join("Cargo.toml"))?;
    let supervisor = read_manifest(&root.join("crates/supervisor/Cargo.toml"))?;
    let mut expected = workspace["workspace"]["lints"].clone();
    assert_eq!(expected["rust"]["unsafe_code"].as_str(), Some("forbid"));
    expected["rust"]["unsafe_code"] = toml_edit::value("deny");
    // Compare parsed values, so key order, comments and whitespace do not matter.
    let expected: serde_json::Value = toml_edit::de::from_str(&expected.to_string())?;
    let actual: serde_json::Value = toml_edit::de::from_str(&supervisor["lints"].to_string())?;
    assert_eq!(
        actual, expected,
        "supervisor must mirror the workspace lint policy"
    );
    Ok(())
}
