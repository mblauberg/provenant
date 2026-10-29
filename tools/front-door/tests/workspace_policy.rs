//! Fast manifest and source policy checks. No Cargo subprocess required.
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};
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
    "serde-saphyr",
    "sha2",
    "thiserror",
    "toml_edit",
    "unicode-normalization",
];

// docs/specs/rust-workspace.md#crate-layering, without the provenant- prefix.
const LAYERS: &[(&str, &[&str])] = &[
    ("core", &[]),
    ("records", &["core"]),
    ("config", &["core", "records", "os"]),
    ("os", &["core"]),
    ("supervisor", &["core", "records", "os"]),
    (
        "confinement",
        &["core", "records", "config", "os", "supervisor"],
    ),
    (
        "providers",
        &["core", "records", "config", "confinement", "quota", "os"],
    ),
    ("quota", &["records", "config"]),
    ("telemetry", &["core", "records", "os"]),
    (
        "routing",
        &["core", "records", "config", "quota", "providers"],
    ),
    (
        "run",
        &[
            "core",
            "records",
            "config",
            "os",
            "supervisor",
            "confinement",
            "sqlite",
        ],
    ),
    ("sqlite", &["core", "os"]),
    ("fabric-store", &["core", "records", "os", "sqlite"]),
    (
        "fabric-runtime",
        &["core", "records", "config", "os", "fabric-store"],
    ),
    // The relay validates its socket directory against the effective UID.
    ("fabric-mcp", &["core", "records", "os", "fabric-runtime"]),
    // Chair logs use the lower-level SQLite helper for their durable receipt store.
    ("workflow", &["core", "records", "os", "sqlite"]),
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
            "fabric-mcp",
            "fabric-store",
            "workflow",
            "os",
            "supervisor",
        ],
    ),
    (
        "provenant",
        &[
            "core",
            "records",
            "config",
            "os",
            "supervisor",
            "confinement",
            "providers",
            "quota",
            "telemetry",
            // Preset selection rules and routine pool-effort diagnostics.
            "routing",
            "run",
            "fabric-store",
            // The binary sets the process-wide store busy timeout at startup.
            "sqlite",
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

// Ignore comments and literals so examples cannot satisfy or violate policy.
fn rust_tokens(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i] == b'r' {
            let mut quote = i + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) == Some(&b'"') {
                let hashes = quote - i - 1;
                i = quote + 1;
                while i < bytes.len() {
                    if bytes[i] == b'"'
                        && bytes
                            .get(i + 1..i + 1 + hashes)
                            .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
                    {
                        i += hashes + 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        if bytes[i] == b'\'' {
            let mut end = i + 1;
            if bytes.get(end) == Some(&b'\\') {
                end += 2;
            } else if let Some(character) = source[end..].chars().next() {
                end += character.len_utf8();
            }
            if bytes.get(end) == Some(&b'\'') {
                i = end + 1;
                continue;
            }
        }
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i = (i + 2).min(bytes.len()),
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            continue;
        }
        // Raw identifiers name the same Rust item as their plain spelling.
        if bytes[i..].starts_with(b"r#")
            && bytes
                .get(i + 2)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            i += 2;
        }
        let start = i;
        if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"::") {
            i += 2;
        } else {
            let Some(character) = source[i..].chars().next() else {
                break;
            };
            i += character.len_utf8();
            if character.is_whitespace() || !character.is_ascii() {
                continue;
            }
        }
        tokens.push(&source[start..i]);
    }
    tokens
}

fn check_os_path<'a>(
    tokens: &[&'a str],
    cursor: &mut usize,
    mut path: Vec<&'a str>,
) -> Result<(), String> {
    if tokens.get(*cursor) == Some(&"{") {
        *cursor += 1;
        while *cursor < tokens.len() && tokens[*cursor] != "}" {
            check_os_path(tokens, cursor, path.clone())?;
            if tokens.get(*cursor) != Some(&",") {
                break;
            }
            *cursor += 1;
        }
        if tokens.get(*cursor) == Some(&"}") {
            *cursor += 1;
        }
        return Ok(());
    }
    let Some(segment) = tokens.get(*cursor) else {
        return Ok(());
    };
    path.push(segment);
    *cursor += 1;
    if tokens.get(*cursor) == Some(&"::") {
        *cursor += 1;
        return check_os_path(tokens, cursor, path);
    }
    let tail = &path[1..];
    // Namespace imports, aliases and globs could hide a later restricted use.
    if tail.is_empty()
        || ["*", "self"].contains(&tail[0])
        || (tail[0] == "sys"
            && (tail.len() == 1
                || ["*", "self", "fs", "child", "raw_exec", "reaper"].contains(&tail[1])))
    {
        return Err(format!(
            "{} is reserved for provenant-supervisor",
            path.join("::")
        ));
    }
    if tokens.get(*cursor) == Some(&"as") {
        *cursor = (*cursor + 2).min(tokens.len());
    }
    Ok(())
}

fn check_os_source(source: &str) -> Result<(), String> {
    let tokens = rust_tokens(source);
    for (index, token) in tokens.iter().enumerate() {
        if ["handoff_file", "from_handoff"].contains(token) {
            return Err(format!("{token} is reserved for provenant-supervisor"));
        }
        if *token == "provenant_os" {
            let mut cursor = index;
            check_os_path(&tokens, &mut cursor, Vec::new())?;
        }
    }
    Ok(())
}

fn check_os_sources(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_symlink() {
                check_os_sources(&entry.path())?;
            }
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        check_os_source(&fs::read_to_string(path)?)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

// Cover literal Git commands and the common names used for fixture Git paths.
// Deliberately bound tests also go through the helper before adding their env.
fn check_fixture_git_sources(path: &Path, pattern: &regex::Regex) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_symlink() {
                check_fixture_git_sources(&entry.path(), pattern)?;
            }
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        let source = fs::read_to_string(path)?;
        let name = path.to_string_lossy();
        let test_file = name.contains("/tests/") || name.ends_with("_tests.rs");
        if fixture_git_violation(&source, test_file, pattern) {
            return Err(format!("{} spawns Git without the fixture helper", path.display()).into());
        }
    }
    Ok(())
}

// Source before the first test configuration is production. Scan the remainder
// conservatively, including all test modules and attributes between them.
fn fixture_git_violation(source: &str, test_file: bool, pattern: &regex::Regex) -> bool {
    let test_source = if test_file {
        source
    } else {
        source
            .find("#[cfg(test)]")
            .map_or("", |start| &source[start..])
    };
    pattern.is_match(test_source)
}

#[test]
fn fixture_git_policy_rejects_named_and_multiple_test_modules() {
    let pattern = regex::Regex::new(
        r#"Command\s*::\s*new\s*\(\s*(?:"(?:/usr/bin/)?git"|&?(?:(?:self|fixture)\.)?git\b|executable\b)"#,
    ).unwrap();
    let source = concat!(
        "#[cfg(test)]\n#[allow(dead_code)]\nmod git_tests { ",
        "std::process::Command::new(",
        "git); }\n#[cfg(test)]\nmod tests {} "
    );
    assert!(fixture_git_violation(source, false, &pattern));
    assert!(fixture_git_violation(
        concat!("Command::new(", "git)"),
        true,
        &pattern
    ));
    assert!(!fixture_git_violation(
        concat!("Command::new(", "git)"),
        false,
        &pattern
    ));
}

#[test]
fn fixture_git_commands_use_the_scrubbed_helper() -> Result<(), Box<dyn Error>> {
    let pattern = regex::Regex::new(
        r#"Command\s*::\s*new\s*\(\s*(?:"(?:/usr/bin/)?git"|&?(?:(?:self|fixture)\.)?git\b|executable\b)"#,
    )?;
    assert!(pattern.is_match(concat!("Command::new(", "\"git\")")));
    assert!(pattern.is_match(concat!("Command::new(", "&fixture.git)")));
    assert!(!pattern.is_match("provenant_os::fixture::git_command()"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in [
        root.join("src"),
        root.join("tests"),
        root.join("crates"),
        root.join("build.rs"),
    ] {
        check_fixture_git_sources(&path, &pattern)?;
    }
    let script_pattern =
        regex::Regex::new(r#"(?m)(?:^\s*git\s|\$\(\s*git\s|subprocess\.\w+\(\s*\[\s*"git")"#)?;
    for entry in fs::read_dir(root.join("../../scripts"))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("test-") || name == "test-check-adr-index" {
            // This one only reads the checkout containing the test itself.
            continue;
        }
        let source = fs::read_to_string(entry.path())?;
        if script_pattern.is_match(&source)
            && !source.contains("test-git-env.sh")
            && !source.contains("test_git_env.isolate()")
        {
            return Err(format!(
                "{} spawns Git without fixture isolation",
                entry.path().display()
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn os_internals_are_reserved_for_supervisor() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = read_manifest(&root.join("Cargo.toml"))?;
    for manifest in member_manifests(root, &workspace)? {
        let document = read_manifest(&manifest)?;
        if matches!(
            document["package"]["name"].as_str(),
            Some("provenant-os" | "provenant-supervisor")
        ) {
            continue;
        }
        let directory = manifest.parent().ok_or("manifest has no parent")?;
        for source in ["src", "tests", "examples", "benches", "build.rs"] {
            let path = directory.join(source);
            if path.exists() {
                check_os_sources(&path)?;
            }
        }
    }
    Ok(())
}

#[test]
fn os_internal_policy_handles_grouped_imports_and_namespace_escapes() {
    for source in [
        "provenant_os::sys::child::configure_command();",
        "provenant_os::r#sys::fs::unlink();",
        "use provenant_os::sys::{r#fs as raw_fs};",
        "use provenant_os::sys::{resources, fs as raw_fs};",
        "use provenant_os::{fs::Directory, sys::{raw_exec::OwnedPid}};",
        "use provenant_os::sys::reaper::*;",
        "use provenant_os as os;",
        "extern crate provenant_os as os;",
        "use provenant_os::*;",
        "use provenant_os::{self as os, fs::Directory};",
        "use provenant_os::sys as sys;",
        "use provenant_os::{sys::{self as sys, resources}};",
        "root.handoff_file();",
        "Directory::from_handoff(file);",
        "let quote = '\"'; provenant_os::sys::fs::unlink();",
    ] {
        assert!(check_os_source(source).is_err(), "{source}");
    }
    for source in [
        "use provenant_os::{fs::Directory, sys::{resources::MemorySample}};",
        "use provenant_os::sys::resources as memory;",
        "provenant_os::sys::account(provenant_os::sys::Account::Current);",
        "let state_handoff_file = None;",
        "// provenant_os::sys::child\nlet x = \"handoff_file\";",
        "/* nested /* handoff_file */ provenant_os::sys::fs */",
        "let x = r##\"provenant_os::sys::child\"##;",
    ] {
        assert!(check_os_source(source).is_ok(), "{source}");
    }
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
            if internal == "os" && alias != "provenant-os" {
                return Err("provenant-os must keep its name for source policy checks".into());
            }
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
    let mut checked = Vec::new();
    for path in member_manifests(root, &workspace)? {
        let manifest = read_manifest(&path)?;
        check_internal_edges(&manifest, &workspace)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let package = manifest["package"]["name"].as_str().unwrap_or_default();
        checked.push(
            package
                .strip_prefix("provenant-")
                .unwrap_or(package)
                .to_owned(),
        );
    }
    // Every row names a real member, so a missed crate cannot pass unchecked.
    checked.sort();
    let mut rows: Vec<_> = LAYERS.iter().map(|(name, _)| (*name).to_owned()).collect();
    rows.sort();
    assert_eq!(checked, rows, "layering rows and workspace members differ");
    Ok(())
}

#[test]
fn os_cannot_depend_on_supervisor() -> Result<(), Box<dyn Error>> {
    let workspace: DocumentMut = "[workspace.dependencies]\nprovenant-supervisor = { path = 'crates/supervisor' }\nprocesses = { package = 'provenant-supervisor', path = 'crates/supervisor' }".parse()?;
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        for scope in [String::new(), "target.'cfg(unix)'.".to_owned()] {
            for declaration in [
                "provenant-supervisor = { path = '../supervisor' }",
                "processes = { package = 'provenant-supervisor', path = '../supervisor' }",
                "provenant-supervisor.workspace = true",
                "processes.workspace = true",
            ] {
                let manifest: DocumentMut =
                    format!("[package]\nname = 'provenant-os'\n[{scope}{kind}]\n{declaration}")
                        .parse()?;
                let error = check_internal_edges(&manifest, &workspace)
                    .err()
                    .ok_or("os must not import supervisor")?;
                assert!(error.contains("provenant-supervisor"), "{error}");
            }
        }
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
    for (member, dependency) in [
        ("os", "os"),
        ("supervisor", "supervisor"),
        ("fabric-mcp", "fabric-store"),
    ] {
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
fn os_dependency_keeps_its_policy_checked_name() -> Result<(), Box<dyn Error>> {
    let workspace: DocumentMut = "".parse()?;
    for alias in ["provenant-os", "native"] {
        let manifest: DocumentMut = format!(
            "[package]\nname = 'provenant-config'\n[dependencies]\n{alias} = {{ package = 'provenant-os', path = '../os' }}"
        ).parse()?;
        assert_eq!(
            check_internal_edges(&manifest, &workspace).is_ok(),
            alias == "provenant-os"
        );
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
fn native_boundaries_mirror_workspace_lints_except_for_unsafe_code() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = read_manifest(&root.join("Cargo.toml"))?;

    let mut expected = workspace["workspace"]["lints"].clone();
    assert_eq!(expected["rust"]["unsafe_code"].as_str(), Some("forbid"));
    expected["rust"]["unsafe_code"] = toml_edit::value("deny");
    // Compare parsed values, so key order, comments and whitespace do not matter.
    let expected: serde_json::Value = toml_edit::de::from_str(&expected.to_string())?;
    for member in ["os", "supervisor", "sqlite"] {
        let manifest = read_manifest(&root.join(format!("crates/{member}/Cargo.toml")))?;
        let actual: serde_json::Value = toml_edit::de::from_str(&manifest["lints"].to_string())?;
        assert_eq!(
            actual, expected,
            "{member} must mirror workspace lint policy"
        );
    }
    Ok(())
}
