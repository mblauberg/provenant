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
    "serde_path_to_error",
    "serde-saphyr",
    "sha2",
    "thiserror",
    "toml_edit",
    "unicode-normalization",
];

// docs/specs/rust-workspace.md#crate-layering, without the provenant- prefix.
const LAYERS: &[(&str, &[&str])] = &[
    ("core", &[]),
    // Test-only helpers, reachable through dev-dependencies or optional fixture features.
    ("test-support", &[]),
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
    ("quota", &["core", "records", "config"]),
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
        // Graph patches use confinement scanners.
        &[
            "core",
            "records",
            "config",
            "os",
            "confinement",
            "fabric-store",
            "routing",
        ],
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
fn rust_sources(path: &Path, sources: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_symlink() {
                rust_sources(&entry.path(), sources)?;
            }
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        sources.push(path.to_path_buf());
    }
    Ok(())
}

// The directories whose Rust sources the Git guards scan.
fn source_roots() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    ["src", "tests", "crates", "build.rs"]
        .map(|path| root.join(path))
        .into()
}

fn check_fixture_git_sources(path: &Path, pattern: &regex::Regex) -> Result<(), Box<dyn Error>> {
    let mut sources = Vec::new();
    rust_sources(path, &mut sources)?;
    for path in sources {
        let source = fs::read_to_string(&path)?;
        let name = path.to_string_lossy();
        let test_file = is_test_source(&name);
        if fixture_git_violation(&source, test_file, pattern) {
            return Err(format!("{} spawns Git without the fixture helper", path.display()).into());
        }
    }
    Ok(())
}

// A test source in full, or the bodies of its test-only items.
fn test_scopes(source: &str, test_file: bool) -> Vec<&str> {
    selected_test_scopes(source, test_file, false)
}

// Fixture-only items include test-and-platform combinations and the fixture
// feature. `any(test, platform)` also applies in production, so leave it out.
fn selected_test_scopes(source: &str, test_file: bool, fixtures: bool) -> Vec<&str> {
    let tokens = rust_tokens(source);
    let offset = |token: &str| token.as_ptr() as usize - source.as_ptr() as usize;
    let mut scopes = vec![source; usize::from(test_file)];
    for index in 0..tokens.len() {
        if test_file || !tokens[index..].starts_with(&["#", "["]) {
            continue;
        }
        let Some(end) = tokens[index..]
            .iter()
            .position(|token| *token == "]")
            .map(|end| index + end)
        else {
            continue;
        };
        let attribute = &tokens[index..=end];
        let attribute_text = &source[offset(attribute[0])..=offset(attribute[attribute.len() - 1])];
        let test_only = attribute == ["#", "[", "test", "]"]
            || attribute == ["#", "[", "cfg", "(", "test", ")", "]"]
            || (fixtures
                && attribute.starts_with(&["#", "[", "cfg", "(", "all", "("])
                && attribute.contains(&"test")
                && !attribute.contains(&"not"))
            || (fixtures
                && attribute.starts_with(&["#", "[", "cfg", "("])
                && attribute_text.contains("\"fixture\"")
                && attribute.contains(&"feature")
                && !attribute.contains(&"not")
                && (!attribute.contains(&"any") || !attribute.contains(&"target_os")));
        if !test_only {
            continue;
        }
        let (mut nesting, mut depth, mut generics) = (0usize, 0usize, 0usize);
        for token in &tokens[end + 1..] {
            match *token {
                "(" | "[" => nesting += 1,
                "<" if depth == 0 => generics += 1,
                ">" if depth == 0 && generics > 0 => generics -= 1,
                // Leaving the enclosing group (a parameter or field attribute) ends the item.
                ")" | "]" | "," if depth + nesting + generics == 0 => break,
                ")" | "]" => nesting -= 1,
                ";" if depth + nesting == 0 => {
                    scopes.push(&source[offset(attribute[0])..=offset(token)]);
                    break;
                }
                "{" => depth += 1,
                "}" if depth <= 1 && nesting == 0 => {
                    scopes.push(&source[offset(attribute[0])..=offset(token)]);
                    break;
                }
                "}" => depth -= 1,
                _ => {}
            }
        }
    }
    scopes
}

fn fixture_git_violation(source: &str, test_file: bool, pattern: &regex::Regex) -> bool {
    test_scopes(source, test_file)
        .iter()
        .any(|scope| pattern.is_match(scope))
}

// Whether a path names a test source, including split test modules and files
// beneath a test module directory.
fn is_test_source(name: &str) -> bool {
    let path = Path::new(name);
    let path = if path.is_absolute() {
        let Ok(relative) = path.strip_prefix(env!("CARGO_MANIFEST_DIR")) else {
            return false;
        };
        relative
    } else {
        path
    };
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    path.parent().is_some_and(|parent| {
        parent.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| name == "tests" || name.ends_with("_tests"))
        })
    }) || stem == "tests"
        || stem.starts_with("tests_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
}

// Method-level bans (including direct sleep) live in clippy.toml. The remaining
// scanners express contextual rules: Git arguments/environment, reserved OS
// namespaces, and adapter identity comparisons; Clippy cannot model those.

// Counts for the tokens owned by one function body (innermost function wins).
#[derive(Default)]
struct ExecutorUse {
    git_executable: bool,
    git_env: bool,
    option_sites: usize,
    explicit_env: usize,
}

// Test code that runs the resolved Git through `ExecOptions` must use the fixture
// helper. With `env` unset the child inherits the lane's GIT_DIR and GIT_WORK_TREE,
// so a fixture commits into the lane. An explicit `env` does not inherit them.
// Judged per function body: a function that names the Git executable must give
// every `ExecOptions::new` an explicit `env`, or call `git_env`, within itself.
// Works on tokens, so comments and string literals cannot trigger or hide a use.
fn executor_git_violation(source: &str, test_file: bool) -> bool {
    let tokens: Vec<&str> = test_scopes(source, test_file)
        .into_iter()
        .flat_map(rust_tokens)
        .collect();
    // Owner of each open brace: Some(function id) for a function body, else the parent's.
    let mut frames: Vec<Option<usize>> = Vec::new();
    let mut functions: Vec<ExecutorUse> = vec![ExecutorUse::default()];
    let mut pending: Option<(usize, usize)> = None; // (function id, signature bracket depth)
    for (index, token) in tokens.iter().enumerate() {
        let owner = frames.last().copied().flatten().unwrap_or(0);
        let next = tokens.get(index + 1).copied();
        let after = tokens.get(index + 2).copied();
        match *token {
            "fn" if next
                .is_some_and(|name| name.starts_with(|c: char| c.is_alphabetic() || c == '_')) =>
            {
                functions.push(ExecutorUse::default());
                pending = Some((functions.len() - 1, 0));
            }
            "(" | "[" => {
                if let Some((_, depth)) = pending.as_mut() {
                    *depth += 1;
                }
            }
            ")" | "]" => {
                if let Some((_, depth)) = pending.as_mut() {
                    *depth = depth.saturating_sub(1);
                }
            }
            ";" if pending.is_some_and(|(_, depth)| depth == 0) => pending = None,
            "{" => frames.push(match pending.take() {
                Some((id, _)) => Some(id),
                None => frames.last().copied().flatten(),
            }),
            "}" => {
                frames.pop();
            }
            "git_executable" => functions[owner].git_executable = true,
            "git_env" if next == Some("(") => functions[owner].git_env = true,
            "ExecOptions" if next == Some("::") && after == Some("new") => {
                functions[owner].option_sites += 1;
            }
            "env" if matches!(next, Some("=") | Some(":")) && after == Some("Some") => {
                functions[owner].explicit_env += 1;
            }
            _ => {}
        }
    }
    functions.iter().any(|function| {
        function.git_executable
            && !function.git_env
            && function.option_sites > function.explicit_env
    })
}

#[test]
fn executor_git_policy_flags_only_inherited_env() {
    let offending = concat!(
        "let git = provenant_os::git::git_executable().ok_or(\"git\")?;\n",
        "let options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));"
    );
    assert!(executor_git_violation(offending, true));
    assert!(executor_git_violation(
        concat!(
            "#[cfg(test)]\n#[allow(dead_code)]\nmod t { ",
            "fn f() { ",
            "git_executable(); ",
            "ExecOptions::new(); } }"
        ),
        false
    ));
    // A Git site without env is still flagged beside an unrelated function that sets env.
    assert!(executor_git_violation(
        concat!(
            "fn git_site() { let git = git_executable(); let o = ExecOptions::new(a, c, d); }\n",
            "fn other() { let mut o = ExecOptions::new(a, c, d); o.env = Some(&env); }"
        ),
        true
    ));
    // So is the unscrubbed site of two in one function, and one inside a closure.
    assert!(executor_git_violation(
        concat!(
            "fn f() { let git = git_executable(); ",
            "let mut a = ExecOptions::new(a, c, d); a.env = Some(&env); ",
            "let run = || ExecOptions::new(a, c, d); }"
        ),
        true
    ));
    // Split test files and nested test modules are scanned in full too.
    for name in [
        "crates/run/src/tests.rs",
        "crates/cli/src/run/provider/confinement_test.rs",
        "crates/x/src/foo_tests.rs",
        "tools/front-door/tests/graph_cli.rs",
        "crates/cli/src/run/provider/tests_prompt_and_registry.rs",
        "crates/cli/src/route/tests_3.rs",
        "src/worktree_tests/lane_cases.rs",
        "src/worktree_tests/nested/support.rs",
    ] {
        assert!(is_test_source(name), "{name}");
        assert!(executor_git_violation(offending, is_test_source(name)));
    }
    for name in [
        "crates/run/src/controls.rs",
        "crates/run/src/contests.rs",
        "src/worktree_tests_support/production.rs",
        "src/test_support.rs",
        "/tmp/g11_tests/provenant/src/route/config.rs",
        "/tmp/tests/provenant/src/route/config.rs",
    ] {
        assert!(!is_test_source(name), "{name}");
    }
    for source in [
        "fn f() { let git = git_executable()?; run_git(cwd, &[\"init\"])?; }",
        "fn f() { let git = git_executable()?; let mut o = ExecOptions::new(a, c, d); o.env = Some(&env); }",
        "fn f() { let git = git_executable()?; let o = ExecOptions { env: Some(&env), ..x }; }",
        "fn f() { let git = git_executable()?; let o = ExecOptions::new(a, c, d); let e = git_env(); }",
        "fn f(a: [u8; 4]) { let git = git_executable()?; let o = ExecOptions::new(a, c, d); let e = git_env(); }",
        "fn git_site() { git_executable(); run_git(c, a); }\nfn other() { ExecOptions::new(a, c, d); }",
        "// git_executable() ExecOptions::new\nlet s = \"git_executable() ExecOptions::new\";",
    ] {
        assert!(!executor_git_violation(source, true), "{source}");
    }
    // Production code outside a test module is not in scope, even between two of them.
    assert!(!executor_git_violation(
        concat!(
            "#[cfg(test)] mod a {}\n",
            "fn production() { let git = git_executable(); let o = ExecOptions::new(a, c, d); }\n",
            "#[cfg(test)] mod b {}"
        ),
        false
    ));
}

#[test]
fn executor_git_commands_use_the_fixture_helper() -> Result<(), Box<dyn Error>> {
    let mut sources = Vec::new();
    for path in source_roots() {
        rust_sources(&path, &mut sources)?;
    }
    for path in sources {
        let source = fs::read_to_string(&path)?;
        let name = path.to_string_lossy();
        if executor_git_violation(&source, is_test_source(&name)) {
            return Err(format!(
                "{} runs Git through ExecOptions with the inherited env; use provenant_os::fixture::run_git",
                path.display()
            )
            .into());
        }
    }
    // These two files must not build Git argv or filter GIT_ variables inline.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["crates/run/src/controls.rs", "crates/run/src/cargo_seed.rs"] {
        let source = fs::read_to_string(root.join(file))?;
        for forbidden in ["starts_with(\"GIT_\")", "git_executable()", "once(\"git\")"] {
            assert!(!source.contains(forbidden), "{file} contains {forbidden}");
        }
    }
    Ok(())
}

#[test]
fn fixture_git_commands_use_the_scrubbed_helper() -> Result<(), Box<dyn Error>> {
    let pattern = regex::Regex::new(
        r#"Command\s*::\s*new\s*\(\s*(?:"(?:/usr/bin/)?git"|&?(?:(?:self|fixture)\.)?git\b|executable\b)"#,
    )?;
    assert!(pattern.is_match(concat!("Command::new(", "\"git\")")));
    assert!(pattern.is_match(concat!("Command::new(", "&fixture.git)")));
    assert!(!pattern.is_match("provenant_os::fixture::git_command()"));
    let helper = concat!("fn helper() { Command::new(", "\"git\"); }");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in [
        "crates/cli/src/run/provider/tests_prompt_and_registry.rs",
        "crates/cli/src/route/tests_3.rs",
        "src/worktree_tests/lane_cases.rs",
    ] {
        let path = root.join(name);
        assert!(is_test_source(&path.to_string_lossy()), "{name}");
        assert!(fixture_git_violation(
            helper,
            is_test_source(&path.to_string_lossy()),
            &pattern,
        ));
    }
    assert!(!is_test_source(
        &root.join("src/route/config.rs").to_string_lossy()
    ));
    assert!(!fixture_git_violation(helper, false, &pattern));
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
        if !name.starts_with("test-") {
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
                && (internal == name
                    || internal == "test-support"
                    || (name == "fabric-mcp" && internal == "fabric-store"));
            let fixture_exception = internal == "test-support"
                && kind == "dependencies"
                && declared.get("optional").and_then(Item::as_bool) == Some(true)
                && table
                    .get("features")
                    .and_then(|features| features.get("fixture"))
                    .and_then(Item::as_array)
                    .is_some_and(|features| {
                        features
                            .iter()
                            .any(|feature| feature.as_str() == Some("dep:provenant-test-support"))
                    });
            if (internal == "cli" && name != "provenant")
                || !(allowed.contains(&internal) || dev_exception || fixture_exception)
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

#[test]
fn adapter_policy_excludes_complete_test_const_arrays() {
    let source = "#[cfg(test)] const ROWS: &[Row] = &[Row { adapter: \"claude\" }, Row { adapter: \"agy\" }];\nfn production() {}";
    let scopes = test_scopes(source, false);
    assert_eq!(scopes.len(), 1);
    assert!(scopes[0].contains("agy"));
    assert!(!scopes[0].contains("production"));
}

#[test]
fn adapter_policy_excludes_fixture_items_but_scans_production_neighbours() {
    for attribute in ["#[cfg(feature = \"fixture\")]", "#[test]"] {
        let source = format!(
            "{attribute} fn fixture() -> Result<(), Error> {{ adapter == \"codex\"; }}\nfn production() {{ adapter == \"claude\"; }}"
        );
        let scopes = selected_test_scopes(&source, false, true);
        assert_eq!(scopes.len(), 1);
        assert!(scopes[0].contains("codex"));
        assert!(!scopes[0].contains("claude"));
    }
}

fn adapter_identity_comparison() -> Result<regex::Regex, regex::Error> {
    let names = provenant_records::ADAPTER_SPECS
        .iter()
        .map(|spec| regex::escape(spec.name))
        .collect::<Vec<_>>()
        .join("|");
    let literal = format!(r#""(?:{names})""#);
    let harness = r"Harness::[A-Za-z_][A-Za-z_0-9]*\s*\.\s*spec\s*\(\s*\)\s*\.\s*name";
    let typed_harness = r"Harness::[A-Za-z_][A-Za-z_0-9]*";
    regex::Regex::new(&format!(
        r"(?:(?:==|!=)\s*(?:Some\s*\(\s*)?(?:{literal}|{harness}|{typed_harness})|(?:{literal}|{harness}|{typed_harness})\s*(?:\))?\s*(?:==|!=)|{literal}\s*(?:\||=>|if\b)|matches!\s*\([^,]+,[^;{{}}\n]*(?:{literal}|{harness})|\.\s*(?:eq|ne|eq_ignore_ascii_case)\s*\(\s*(?:{literal}|{harness}|{typed_harness})|{typed_harness}[^,;{{}}]*=>|\blet\s+[^=;{{}}]*{typed_harness}[^=;{{}}]*=|matches!\s*\([^;{{}}]*{typed_harness})"
    ))
}

#[test]
fn adapter_policy_detects_registry_names_and_harness_comparisons() -> Result<(), Box<dyn Error>> {
    let comparison = adapter_identity_comparison()?;
    for spec in provenant_records::ADAPTER_SPECS {
        assert!(comparison.is_match(&format!("adapter == \"{}\"", spec.name)));
        assert!(comparison.is_match(&format!("\"{}\" => action()", spec.name)));
    }
    assert!(comparison.is_match("adapter == Harness::Codex.spec().name"));
    assert!(comparison.is_match("matches!(adapter, \"codex\")"));
    assert!(comparison.is_match("matches!(adapter, Some(\"codex\"))"));
    assert!(comparison.is_match("matches!(adapter, None | Some(\"codex\"))"));
    assert!(comparison.is_match("\"codex\" | other => action()"));
    assert!(comparison.is_match("adapter.eq(\"codex\")"));
    assert!(comparison.is_match("adapter.eq_ignore_ascii_case(\"codex\")"));
    assert!(comparison.is_match("\"codex\" if ready => action()"));
    assert!(comparison.is_match("Harness::Agy.spec().name != adapter"));
    assert!(comparison.is_match("provider == Harness::Claude"));
    for pattern in [
        "Harness::Claude => action()",
        "Harness::Claude | Harness::Codex => action()",
        "if let Some(\n    Harness::Claude\n) = client {}",
        "if let Some(Harness::Claude) = client {}",
        "let Some(client @ Harness::Claude) = client else {}",
        "matches!(client, Some(Harness::Claude))",
    ] {
        assert!(comparison.is_match(pattern), "{pattern}");
    }
    assert!(!comparison.is_match("default_value = Harness::Claude.spec().name"));
    assert!(!comparison.is_match("true => Some(Harness::Codex),\nfalse => None"));
    Ok(())
}

/// Adapter policy belongs to the records registry; hooks are declarative
/// provider command data. Test fixtures may still name their provider.
#[test]
fn adapter_policy_uses_registry_data() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let names = provenant_records::ADAPTER_SPECS
        .iter()
        .map(|spec| regex::escape(spec.name))
        .collect::<Vec<_>>()
        .join("|");
    let literal = regex::Regex::new(&format!(r#""(?:{names})""#))?;
    let comparison = adapter_identity_comparison()?;
    let mut sources = Vec::new();
    for name in ["cli", "routing", "quota"] {
        rust_sources(&root.join("crates").join(name).join("src"), &mut sources)?;
    }
    rust_sources(&root.join("src"), &mut sources)?;
    let mut violations = Vec::new();
    for path in sources {
        let source = fs::read_to_string(&path)?;
        // The fixture module also contains production code. Exclude only its
        // fixture-gated items rather than exempting the whole file.
        let tests = selected_test_scopes(&source, is_test_source(&path.to_string_lossy()), true);
        // Hook command literals are declarative data; comparisons still
        // represent policy and must be checked.
        let checks: Vec<_> =
            if path.starts_with(root.join("src")) || path == root.join("crates/cli/src/hooks.rs") {
                vec![&comparison]
            } else {
                vec![&literal, &comparison]
            };
        for found in checks
            .into_iter()
            .flat_map(|check| check.find_iter(&source))
        {
            let line_text = source[..found.start()].rsplit('\n').next().unwrap_or("");
            let non_adapter_literal = (path.ends_with("grammar.rs")
                && line_text.contains("conflicts_with_all"))
                || (path.ends_with("fabric.rs") && line_text.contains("InvalidField("))
                || (path.ends_with("run/provider/command.rs") && line_text.contains(".join("))
                || (path.ends_with("run/status.rs")
                    && source[found.end()..].starts_with(": cursor"))
                || (path.ends_with("run/provider/preparation.rs")
                    && source[found.end()..].starts_with(':'));
            if non_adapter_literal {
                continue;
            }
            let offset = source.as_ptr() as usize + found.start();
            if tests.iter().any(|scope| {
                let start = scope.as_ptr() as usize;
                (start..start + scope.len()).contains(&offset)
            }) {
                continue;
            }
            let line = source[..found.start()]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            violations.push(format!("{}:{line}: {}", path.display(), found.as_str()));
        }
    }
    assert!(
        violations.is_empty(),
        "adapter policy must use AdapterSpec data:\n{}",
        violations.join("\n")
    );
    Ok(())
}
