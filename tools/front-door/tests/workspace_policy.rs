//! Fast manifest and source policy checks. No Cargo subprocess required.
use std::{
    collections::HashMap,
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
    ("test-support", &["core"]),
    ("records", &["core"]),
    ("config", &["core", "records", "os", "redact"]),
    // The typed warning facade lives in records::warning; every library routes
    // warnings through it (Spec: docs/specs/cli.md#warnings-are-typed-values). The edge is acyclic
    // because records depends only on core.
    ("os", &["core", "records"]),
    // Per-byte redaction, built optimised in dev and test profiles.
    ("redact", &["core", "records", "os"]),
    // Port-plan tests read the default lane settings from config (a sibling
    // layer), so the edge is acyclic.
    ("supervisor", &["core", "records", "os", "config", "redact"]),
    (
        "confinement",
        // Worktree lifecycle tests open a real host store; fabric-store sits
        // below confinement, so the edge stays acyclic.
        &[
            "core",
            "records",
            "config",
            "os",
            "redact",
            "supervisor",
            "fabric-store",
        ],
    ),
    (
        "providers",
        &[
            "core",
            "records",
            "config",
            "confinement",
            "quota",
            "os",
            "redact",
        ],
    ),
    ("quota", &["core", "records", "config"]),
    // Price overlays read host and project layers through config. The ingest
    // store merges usage through the accumulator owned by providers
    // (Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics); providers does not depend on telemetry.
    (
        "telemetry",
        &["core", "records", "config", "routing", "os", "providers"],
    ),
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
            "redact",
            "supervisor",
            "confinement",
            "sqlite",
        ],
    ),
    // Affected-crate selection reads only workspace metadata; no internal edges.
    ("select", &[]),
    ("sqlite", &["core", "os"]),
    ("fabric-store", &["core", "records", "os", "sqlite"]),
    (
        "fabric-runtime",
        // Graph patches use confinement scanners. Template tests resolve
        // smoke tiers against the provider registry routing already reads.
        &[
            "core",
            "records",
            "config",
            "os",
            "redact",
            "confinement",
            "fabric-store",
            "routing",
            "providers",
        ],
    ),
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
            "fabric-store",
            "workflow",
            "os",
            "redact",
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
            "redact",
            "supervisor",
            "confinement",
            "providers",
            "quota",
            "telemetry",
            // Preset selection rules and routine pool-effort diagnostics.
            "routing",
            "run",
            "select",
            "fabric-store",
            // The binary sets the process-wide store busy timeout at startup.
            "sqlite",
            "fabric-runtime",
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
        let raw_start = if bytes[i..].starts_with(b"br") || bytes[i..].starts_with(b"cr") {
            i + 1
        } else {
            i
        };
        if bytes[raw_start] == b'r' {
            let mut quote = raw_start + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) == Some(&b'"') {
                let hashes = quote - raw_start - 1;
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
                if bytes.get(end - 1) == Some(&b'u') && bytes.get(end) == Some(&b'{') {
                    end += 1;
                    while end < bytes.len() && bytes[end] != b'}' {
                        end += 1;
                    }
                    end = (end + 1).min(bytes.len());
                } else if bytes.get(end - 1) == Some(&b'x') {
                    end = (end + 2).min(bytes.len());
                }
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
                ")" | "]" => nesting = nesting.saturating_sub(1),
                ";" if depth + nesting == 0 => {
                    scopes.push(&source[offset(attribute[0])..=offset(token)]);
                    break;
                }
                "{" => depth += 1,
                "}" if depth <= 1 && nesting == 0 => {
                    scopes.push(&source[offset(attribute[0])..=offset(token)]);
                    break;
                }
                "}" => depth = depth.saturating_sub(1),
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
        // A fixture generator binary writes test data, so it is fixture source.
        || stem.ends_with("-fixtures")
}

// Method-level bans (including direct sleep) live in clippy.toml. The remaining
// scanners express contextual rules: Git arguments/environment, reserved OS
// namespaces, adapter identity comparisons and fixed waits in tests; Clippy
// cannot model those.
//
// Fixed waits in tests hide ordering bugs: they pass on an idle host and flake
// under load. This rule warns first, so existing fixtures are not broken by a
// policy change. `PROVENANT_STRICT_TEST_WAITS=1` turns the warning into a
// refusal once the named waits have moved to `wait_until` or an event. A scope
// that names `disallowed_methods` is an explicit real-time boundary and is
// exempt: the wait primitives themselves must still be allowed to sleep.
fn fixed_wait_violations(source: &str, test_file: bool) -> Vec<String> {
    test_scopes(source, test_file)
        .into_iter()
        .filter_map(|scope| {
            let tokens = rust_tokens(scope);
            // A crate-wide `#![allow]` is a blanket suppression, not a named
            // boundary; it cannot exempt every test in the file.
            let blanket = tokens.starts_with(&["#", "!", "["]);
            if !blanket && tokens.contains(&"disallowed_methods") {
                return None;
            }
            // A fixed `Duration` is a wait only when the statement also names
            // a sleep or timeout primitive. A deadline passed to a bounded
            // helper such as `wait_until` or `fixture_accept` is not a fixed
            // wait: its owner decides when it expires.
            let is_wait_statement = |index: usize| {
                let start = (0..index)
                    .rev()
                    .find(|position| matches!(tokens[*position], ";" | "{" | "}"))
                    .map_or(0, |position| position + 1);
                let end = (index..tokens.len())
                    .find(|position| matches!(tokens[*position], ";" | "{" | "}"))
                    .unwrap_or(tokens.len());
                tokens[start..end].iter().any(|token| {
                    matches!(
                        *token,
                        "sleep" | "park_timeout" | "recv_timeout" | "wait_timeout"
                    )
                })
            };
            let mut violations = Vec::new();
            // Track local bindings by lexical block, including aliases and reassignment.
            // A shadowing binding replaces the outer value until its block ends.
            let mut bindings: Vec<HashMap<&str, bool>> = vec![HashMap::new()];
            let is_fixed = |expression: &[&str], bindings: &[HashMap<&str, bool>]| {
                expression.windows(3).any(|window| {
                    window[0] == "Duration"
                        && window[1] == "::"
                        && matches!(
                            window[2],
                            "from_secs" | "from_millis" | "from_micros" | "from_nanos"
                        )
                }) || expression.iter().any(|name| {
                    bindings
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(name))
                        .copied()
                        .unwrap_or(false)
                })
            };
            for (index, token) in tokens.iter().enumerate() {
                match *token {
                    "{" => bindings.push(HashMap::new()),
                    "}" if bindings.len() > 1 => {
                        bindings.pop();
                    }
                    _ => {}
                }
                let declaration = *token == "let";
                let name_index = index + 1 + usize::from(tokens.get(index + 1) == Some(&"mut"));
                let assignment = !declaration
                    && tokens.get(index + 1) == Some(&"=")
                    && (index == 0 || matches!(tokens[index - 1], ";" | "{" | "}"));
                if declaration || assignment {
                    let name_index = if declaration { name_index } else { index };
                    if let Some(name) = tokens.get(name_index).copied() {
                        let end = (name_index + 1..tokens.len())
                            .find(|position| matches!(tokens[*position], ";" | "{" | "}"))
                            .unwrap_or(tokens.len());
                        if let Some(equal) =
                            (name_index + 1..end).find(|position| tokens[*position] == "=")
                        {
                            let fixed = is_fixed(&tokens[equal + 1..end], &bindings);
                            if declaration {
                                bindings.last_mut().expect("root scope").insert(name, fixed);
                            } else if let Some(scope) = bindings
                                .iter_mut()
                                .rev()
                                .find(|scope| scope.contains_key(name))
                            {
                                scope.insert(name, fixed);
                            }
                        }
                    }
                }
                if matches!(*token, "recv_timeout" | "wait_timeout")
                    && tokens.get(index + 1) == Some(&"(")
                {
                    let mut depth = 0usize;
                    let end = (index + 1..tokens.len()).find(|position| {
                        match tokens[*position] {
                            "(" => depth += 1,
                            ")" => depth = depth.saturating_sub(1),
                            _ => {}
                        }
                        depth == 0
                    });
                    if end.is_some_and(|end| is_fixed(&tokens[index + 2..end], &bindings)) {
                        violations.push(format!("{token}, fixed duration wait in a test"));
                    }
                }
                if matches!(*token, "sleep" | "park_timeout") && tokens.get(index + 1) == Some(&"(")
                {
                    violations.push(format!("{token}, fixed wait in a test"));
                }
                if *token == "Duration"
                    && tokens.get(index + 1) == Some(&"::")
                    && matches!(
                        tokens.get(index + 2).copied(),
                        Some("from_secs" | "from_millis" | "from_micros" | "from_nanos")
                    )
                    && is_wait_statement(index)
                {
                    violations.push(format!(
                        "Duration::{}, fixed wait in a test",
                        tokens[index + 2]
                    ));
                }
            }
            (!violations.is_empty()).then_some(violations.join(", "))
        })
        .collect()
}

#[test]
fn fixed_wait_rule_distinguishes_explicit_boundaries_from_test_waits() {
    let direct = r#"
        #[test]
        fn flaky() {
            std::thread::sleep(Duration::from_millis(50));
        }
    "#;
    let found = fixed_wait_violations(direct, false);
    assert!(found.iter().any(|item| item.contains("sleep")), "{found:?}");
    assert!(
        found
            .iter()
            .any(|item| item.contains("Duration::from_millis")),
        "{found:?}"
    );
    let allowed = r#"
        #[allow(clippy::disallowed_methods)]
        #[test]
        fn bounded() {
            std::thread::park_timeout(Duration::from_millis(5));
        }
    "#;
    assert!(fixed_wait_violations(allowed, true).is_empty());
    let blanket = r#"
        #![allow(clippy::disallowed_methods)]
        #[test]
        fn flaky() {
            std::thread::park_timeout(Duration::from_millis(5));
        }
    "#;
    assert!(!fixed_wait_violations(blanket, true).is_empty());
    let bounded = r#"
        #[test]
        fn patient() {
            wait_until("ready", deadline, observe, reached);
            wait_for("ready", Duration::from_secs(1), observe, reached);
        }
    "#;
    assert!(fixed_wait_violations(bounded, false).is_empty());
    let production = "fn busy() { std::thread::sleep(Duration::from_millis(50)); }";
    assert!(fixed_wait_violations(production, false).is_empty());
}

#[test]
fn fixed_duration_bindings_to_timeouts_warn_and_refuse_in_strict_mode() {
    let split = r#"
        #[test]
        fn flaky() {
            let timeout = Duration::from_millis(50);
            receiver.recv_timeout(timeout);
        }
    "#;
    let found = fixed_wait_violations(split, false);
    assert!(!found.is_empty(), "split fixed duration must be reported");
    assert!(!fixed_waits_refused(&found, false));
    assert!(fixed_waits_refused(&found, true));
    let aliases_and_shadowing = r#"
        #[test]
        fn waits() {
            let mut timeout = deadline;
            timeout = Duration::from_secs(1);
            let alias = timeout;
            { let alias = deadline; receiver.recv_timeout(alias); }
            receiver.recv_timeout(alias);
        }
    "#;
    assert!(!fixed_wait_violations(aliases_and_shadowing, false).is_empty());
    let shadowed = r#"
        #[test]
        fn bounded() {
            let timeout = Duration::from_secs(1);
            { let timeout = deadline; receiver.recv_timeout(timeout); }
            wait_for("ready", timeout, observe, reached);
        }
    "#;
    assert!(fixed_wait_violations(shadowed, false).is_empty());
}

fn fixed_waits_refused(violations: &[String], strict: bool) -> bool {
    strict && !violations.is_empty()
}

#[test]
#[allow(
    clippy::print_stderr,
    reason = "warn-first policy reports fixed waits on stderr"
)]
fn test_waits_warn_before_refusal() -> Result<(), Box<dyn Error>> {
    let strict = std::env::var("PROVENANT_STRICT_TEST_WAITS").is_ok_and(|value| value == "1");
    let mut violations = Vec::new();
    for path in source_roots() {
        let mut sources = Vec::new();
        rust_sources(&path, &mut sources)?;
        for source_path in sources {
            let source = fs::read_to_string(&source_path)?;
            let test_file = is_test_source(&source_path.to_string_lossy());
            for violation in fixed_wait_violations(&source, test_file) {
                violations.push(format!("{}: {violation}", source_path.display()));
            }
        }
    }
    if !violations.is_empty() {
        eprintln!(
            "fixed waits in tests (warn first; set PROVENANT_STRICT_TEST_WAITS=1 to refuse):"
        );
        for violation in violations.iter().take(20) {
            eprintln!("  {violation}");
        }
        if violations.len() > 20 {
            eprintln!("  ... and {} more", violations.len() - 20);
        }
    }
    assert!(
        !fixed_waits_refused(&violations, strict),
        "fixed waits in tests under PROVENANT_STRICT_TEST_WAITS=1:\n{}",
        violations.join("\n")
    );
    Ok(())
}

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

// Git config argv belongs to the core's typed read/write constructors. These
// patterns catch the bypass shapes at process/git-helper call sites and a raw
// write tail; `ConfigMutation` is the only supported mutation input. Strings
// and test-only scopes are excluded so a fixture or a literal label cannot
// satisfy or trip the rule.
fn git_config_violations(source: &str, test_file: bool) -> Vec<String> {
    const PATTERNS: &[(&str, &str)] = &[
        (
            "process command config argv",
            r#"(?s)Command\s*::\s*new\s*\([^;]*?\)\s*\.args?\s*\([^;]*?"config""#,
        ),
        (
            "executor config argv",
            r#"(?s)\.execute\s*\(\s*ExecOptions\s*::\s*new\s*\([^;]*?"config""#,
        ),
        (
            "Git method config argv",
            r#"(?s)\b(?:git|self)\s*\.\s*(?:full|run)\s*\([^;]*?"config""#,
        ),
        (
            "Git helper config argv",
            r#"(?s)\b(?:git|host_git\w*|execute_for|local_git)\s*\([^;]*?"config""#,
        ),
        (
            "Git helper closure config argv",
            r#"(?s)\brun\s*\(\s*&\[\s*"config""#,
        ),
        (
            "raw git_config::write argv",
            r#"git_config\s*::\s*write\s*\(\s*&"#,
        ),
        (
            "raw config mutation argv",
            r#"(?s)"config"\s*,\s*"--(?:local|file|replace-all|remove-section|unset|unset-all|add|set)""#,
        ),
    ];
    let tests = selected_test_scopes(source, test_file, false);
    let mut findings = Vec::new();
    for (label, pattern) in PATTERNS {
        let Ok(regex) = regex::Regex::new(pattern) else {
            continue;
        };
        for found in regex.find_iter(source) {
            let offset = source.as_ptr() as usize + found.start();
            if tests.iter().any(|scope| {
                let start = scope.as_ptr() as usize;
                (start..start + scope.len()).contains(&offset)
            }) {
                continue;
            }
            findings.push((*label).to_owned());
            break;
        }
    }
    findings
}

#[test]
fn git_config_rule_detects_raw_argv_and_spares_test_scopes() {
    // Split the process-command constructor across literals so the
    // fixture-isolation scanner does not read these synthetic inputs as a real
    // Git spawn in this test file; the asserted string is unchanged.
    for source in [
        concat!(
            r#"fn f(git: &str) { std::process::Command::new("#,
            r#"git).args(["config", "--list"]).output(); }"#
        ),
        concat!(
            r#"fn f(git: &str) { std::process::Command::new("#,
            r#"git).arg("config").output(); }"#
        ),
        r#"fn f(executor: &dyn Executor) { executor.execute(ExecOptions::new(&["git", "config"], cwd, timeout)); }"#,
        r#"fn f() { host_git(repo, executor, &["config", "--list"], timeout); }"#,
        r#"fn f() { git(repo, home, &["config", "user.name", "Probe"]); }"#,
        r#"fn f(repo: &Path, git: &Git) { git.full(repo, &["config", "--get", "core.worktree"]); }"#,
        r#"fn f(repo: &Path, git: &Git) { git.run(repo, &["config", "--list"]); }"#,
        r#"fn f(dir: &Path) { self.full(dir, &["config", "--get", "core.worktree"]); }"#,
        r#"fn f(dir: &Path) { self.run(dir, &["config", "user.name", "Probe"]); }"#,
        r#"fn f() { mutate(&["config", "--file", "cfg", "user.name", "Probe"]); }"#,
        r#"fn f() { execute_for(repo, &["git", "config"], timeout, false); }"#,
        r#"fn f() { local_git(git, cwd, &["config", "--get", "branch.lane.remote"]); }"#,
        r#"fn f() { let run = |args| host_git_with_cancel(repo, executor, args, timeout, false, None); run(&["config", "--get", "remote.origin.url"]); }"#,
        r#"fn f(tail: &[OsString]) { provenant_os::git_config::write(&tail, |args| run(args)); }"#,
        concat!(
            r#"fn f(git: &str) { std::process::Command::new("#,
            r#"git).args(["config", "--local", "branch.lane.remote", "origin"]).output(); }"#
        ),
    ] {
        assert!(
            !git_config_violations(source, false).is_empty(),
            "missed: {source}"
        );
    }
    for source in [
        r#"fn f() { provenant_os::git_config::write(context, ConfigMutation::SetStore { setting, value }, |args| run(args)); }"#,
        r#"const LABEL: &str = "config"; fn f() { run(&["status"]); }"#,
        concat!(
            r#"#[cfg(test)] fn fixture(git: &str) { std::process::Command::new("#,
            r#"git).args(["config"]).output(); }"#
        ),
    ] {
        assert!(
            git_config_violations(source, false).is_empty(),
            "false positive: {source}"
        );
    }
    assert!(
        git_config_violations(
            concat!(
                r#"fn f(git: &str) { std::process::Command::new("#,
                r#"git).args(["config"]).output(); }"#
            ),
            true
        )
        .is_empty()
    );
}

#[test]
fn git_config_commands_are_built_only_in_the_core() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let core = root.join("crates/os/src/git_config.rs");
    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources)?;
    if root.join("examples").is_dir() {
        rust_sources(&root.join("examples"), &mut sources)?;
    }
    for entry in fs::read_dir(root.join("crates"))? {
        let directory = entry?.path();
        for tree in ["src", "examples"] {
            let source_root = directory.join(tree);
            if source_root.is_dir() {
                rust_sources(&source_root, &mut sources)?;
            }
        }
    }
    let mut violations = Vec::new();
    for path in sources {
        if path == core {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        for finding in git_config_violations(&source, is_test_source(&path.to_string_lossy())) {
            violations.push(format!("{}: {finding}", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "Git config commands must be built in the core:\n{}",
        violations.join("\n")
    );
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
        // tempfile stays test-only: either a dev-dependency or an optional
        // dependency behind the fixture feature for the linked test harness.
        let optional_tempfile = name == "tempfile"
            && kind == "dependencies"
            && declared.get("optional").and_then(Item::as_bool) == Some(true);
        if !DIRECT.contains(&name)
            && !(name == "tempfile" && kind == "dev-dependencies")
            && !optional_tempfile
        {
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
            let dev_exception =
                kind == "dev-dependencies" && (internal == name || internal == "test-support");
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
fn internal_dependency_boundaries_cover_declaration_forms_and_scopes() -> Result<(), Box<dyn Error>>
{
    let scopes = ["", "target.'cfg(unix)'."];
    let kinds = ["dependencies", "dev-dependencies", "build-dependencies"];

    // Two forbidden cross-layer edges cover direct, renamed and workspace
    // declarations in both ordinary and target-specific tables.
    for (member, dependency) in [("os", "supervisor"), ("routing", "run")] {
        let package = format!("provenant-{dependency}");
        let renamed = format!("renamed-{dependency}");
        let workspace: DocumentMut = format!(
            "[workspace.dependencies]\n{package} = {{ path = 'crates/{dependency}' }}\n{renamed} = {{ package = '{package}', path = 'crates/{dependency}' }}"
        )
        .parse()?;
        let declarations = [
            format!("{package} = {{ path = '../{dependency}' }}"),
            format!("{renamed} = {{ package = '{package}', path = '../{dependency}' }}"),
            format!("{package}.workspace = true"),
            format!("{renamed}.workspace = true"),
        ];
        for kind in kinds {
            for scope in scopes {
                for declaration in &declarations {
                    let manifest: DocumentMut = format!(
                        "[package]\nname = 'provenant-{member}'\n[{scope}{kind}]\n{declaration}"
                    )
                    .parse()?;
                    assert!(
                        check_internal_edges(&manifest, &workspace).is_err(),
                        "{member} -> {package} must be refused in {scope}{kind}: {declaration}"
                    );
                }
            }
        }
    }

    // A crate may name itself only in its development dependencies.
    let workspace: DocumentMut = "".parse()?;
    for member in ["os", "supervisor"] {
        for kind in kinds {
            for scope in scopes {
                let manifest: DocumentMut = format!(
                    "[package]\nname = 'provenant-{member}'\n[{scope}{kind}]\nprovenant-{member} = {{ path = '../{member}' }}"
                )
                .parse()?;
                assert_eq!(
                    check_internal_edges(&manifest, &workspace).is_ok(),
                    kind == "dev-dependencies",
                    "{member} self-edge in {scope}{kind}"
                );
            }
        }
    }

    // Libraries never depend on the CLI, for any Cargo dependency kind.
    for (member, _) in LAYERS.iter().filter(|(name, _)| *name != "provenant") {
        for kind in kinds {
            let manifest: DocumentMut = format!(
                "[package]\nname = 'provenant-{member}'\n[{kind}]\nprovenant-cli = {{ path = '../cli' }}"
            )
            .parse()?;
            assert!(
                check_internal_edges(&manifest, &workspace).is_err(),
                "{member} -> cli must be refused in {kind}"
            );
        }
    }

    // This crate has one approved checked alias for its direct OS dependency.
    for (alias, allowed) in [("provenant-os", true), ("native", false)] {
        let manifest: DocumentMut = format!(
            "[package]\nname = 'provenant-config'\n[dependencies]\n{alias} = {{ package = 'provenant-os', path = '../os' }}"
        )
        .parse()?;
        assert_eq!(
            check_internal_edges(&manifest, &workspace).is_ok(),
            allowed,
            "config -> os alias {alias}"
        );
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
fn tempfile_stays_test_only_in_dev_and_optional_fixture_dependencies() -> Result<(), Box<dyn Error>>
{
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
                kind == "dev-dependencies",
                "[{kind}] {declaration}"
            );
        }
    }
    // #3486 links the fixture-gated worktree harness from the library, so
    // tempfile is also an optional regular dependency there.
    let manifest: DocumentMut =
        "[dependencies]\ntempfile = { workspace = true, optional = true }".parse()?;
    assert!(check_dependencies(manifest.as_table(), &workspace).is_ok());
    let manifest: DocumentMut =
        "[build-dependencies]\ntempfile = { workspace = true, optional = true }".parse()?;
    assert!(check_dependencies(manifest.as_table(), &workspace).is_err());
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
fn test_scopes_preserve_policy_after_rust_literals() {
    let source = r###"
        #[test]
        fn fixture<'a>(value: &'a str) {
            let bytes = br#"{"paths":["}", "[", "\\\"nested\\\""]}"#;
            let text = cr##"{ ] ) }"##;
            let chars = [')', '\u{1b}', '\x5d'];
            git_executable();
            ExecOptions::new();
            fixture_command_here;
        }
        fn production() { production_marker(); }
    "###
    .replace("fixture_command_here", concat!("Command::new(", "\"git\")"));
    let scopes = test_scopes(&source, false);
    assert_eq!(scopes.len(), 1);
    assert!(scopes[0].contains("ExecOptions::new"));
    assert!(!scopes[0].contains("production_marker"));
    assert!(executor_git_violation(&source, false));
    let git_command = regex::Regex::new(r#"Command\s*::\s*new\s*\(\s*"git""#).unwrap();
    assert!(fixture_git_violation(&source, false, &git_command));
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
        .map(|spec| regex::escape(spec.identity.name))
        .collect::<Vec<_>>()
        .join("|");
    let literal = format!(r#""(?:{names})""#);
    let harness =
        r"Harness::[A-Za-z_][A-Za-z_0-9]*\s*\.\s*spec\s*\(\s*\)\s*\.\s*identity\s*\.\s*name";
    let typed_harness = r"Harness::[A-Za-z_][A-Za-z_0-9]*";
    regex::Regex::new(&format!(
        r"(?:(?:==|!=)\s*(?:Some\s*\(\s*)?(?:{literal}|{harness}|{typed_harness})|(?:{literal}|{harness}|{typed_harness})\s*(?:\))?\s*(?:==|!=)|{literal}\s*(?:\||=>|if\b)|matches!\s*\([^,]+,[^;{{}}\n]*(?:{literal}|{harness})|\.\s*(?:eq|ne|eq_ignore_ascii_case)\s*\(\s*(?:{literal}|{harness}|{typed_harness})|{typed_harness}[^,;{{}}]*=>|\blet\s+[^=;{{}}]*{typed_harness}[^=;{{}}]*=|matches!\s*\([^;{{}}]*{typed_harness})"
    ))
}

#[test]
fn adapter_policy_detects_registry_names_and_harness_comparisons() -> Result<(), Box<dyn Error>> {
    let comparison = adapter_identity_comparison()?;
    for spec in provenant_records::ADAPTER_SPECS {
        assert!(comparison.is_match(&format!("adapter == \"{}\"", spec.identity.name)));
        assert!(comparison.is_match(&format!("\"{}\" => action()", spec.identity.name)));
    }
    assert!(comparison.is_match("adapter == Harness::Codex.spec().identity.name"));
    assert!(comparison.is_match("matches!(adapter, \"codex\")"));
    assert!(comparison.is_match("matches!(adapter, Some(\"codex\"))"));
    assert!(comparison.is_match("matches!(adapter, None | Some(\"codex\"))"));
    assert!(comparison.is_match("\"codex\" | other => action()"));
    assert!(comparison.is_match("adapter.eq(\"codex\")"));
    assert!(comparison.is_match("adapter.eq_ignore_ascii_case(\"codex\")"));
    assert!(comparison.is_match("\"codex\" if ready => action()"));
    assert!(comparison.is_match("Harness::Agy.spec().identity.name != adapter"));
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
    assert!(!comparison.is_match("default_value = Harness::Claude.spec().identity.name"));
    assert!(!comparison.is_match("true => Some(Harness::Codex),\nfalse => None"));
    Ok(())
}

/// Provider and config sources are scanned for policy comparisons even though
/// their own binding literals are declarative. A new adapter-identity branch in
/// either crate must still come from the registry.
#[test]
fn adapter_policy_detects_provider_and_config_identity_branches() -> Result<(), Box<dyn Error>> {
    let comparison = adapter_identity_comparison()?;
    for source in [
        "if adapter == \"agy\" { detect(); }",
        "if target.harness == Harness::Claude { sync(); }",
        "match adapter { \"codex\" => update(), _ => {} }",
        "matches!(adapter, Harness::Opencode)",
    ] {
        assert!(comparison.is_match(source), "missed: {source}");
    }
    for source in [
        "crate::adapter!(Agy, \"agy\", prepare);",
        "adapter: \"agy\",",
        "provenant_records::Harness::Agy.spec().identity.name",
    ] {
        assert!(!comparison.is_match(source), "false positive: {source}");
    }
    Ok(())
}

/// Adapter policy belongs to the records registry; hooks are declarative
/// provider command data. Test fixtures may still name their provider.
#[test]
fn adapter_policy_uses_registry_data() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let names = provenant_records::ADAPTER_SPECS
        .iter()
        .map(|spec| regex::escape(spec.identity.name))
        .collect::<Vec<_>>()
        .join("|");
    let literal = regex::Regex::new(&format!(r#""(?:{names})""#))?;
    let comparison = adapter_identity_comparison()?;
    let mut sources = Vec::new();
    // Every crate can dispatch on adapter identity, so every crate is in scope.
    // Provider, config and registry sources declare their own adapter bindings
    // and name adapters in declarative data and diagnostics, so only identity
    // comparisons are policy there; elsewhere a bare adapter literal is policy.
    for entry in fs::read_dir(root.join("crates"))? {
        let entry = entry?;
        if !entry.file_type()?.is_symlink() {
            rust_sources(&entry.path().join("src"), &mut sources)?;
        }
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
        let declarative = path.starts_with(root.join("src"))
            || path == root.join("crates/cli/src/hooks.rs")
            || path.starts_with(root.join("crates/providers/src"))
            || path.starts_with(root.join("crates/config/src"))
            || path == root.join("crates/records/src/adapters.rs");
        let checks: Vec<_> = if declarative {
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
                // Declared under #[cfg(test)] in lib.rs; the source scanner cannot see that.
                || path.ends_with("seatbelt_snapshots.rs")
                // A build strategy name, not an adapter identity.
                || (path.ends_with("lane.rs") && line_text.contains("build_port.strategy"))
                || (path.ends_with("run/provider/command.rs") && line_text.contains(".join("))
                || (path.ends_with("run/status.rs")
                    && source[found.end()..].starts_with(": cursor"))
                || (path.ends_with("run/provider/preparation.rs")
                    && source[found.end()..].starts_with(':'))
                // Chair telemetry reads the Claude transcript format only.
                || (path.ends_with("telemetry/src/ingest.rs")
                    && line_text.contains("PROVIDER"))
                // Host credential declarations are declarative adapter data.
                || path.ends_with("confinement/src/permissions/policy/host.rs")
                // Built-in credential inventory is declarative adapter data.
                || path.ends_with("core/src/credential_declarations.rs")
                // A name-kind example in documentation, not an adapter identity.
                || (path.ends_with("core/src/names.rs") && line_text.contains("///"))
                // Fixture document data, not an adapter policy branch.
                || path.ends_with("fabric-runtime/src/graph/cutover_followup_invariants.rs")
                // A declared host-native constant, not an adapter policy branch.
                || (path.ends_with("fabric-runtime/src/graph/next_step.rs")
                    && line_text.contains("HOST_NATIVE"))
                // Result-envelope schema field names, not adapter identities.
                || path.ends_with("records/src/wire/mod.rs")
                // CLI event JSON field names, not adapter identities.
                || path.ends_with("telemetry/src/signals.rs")
                // A declared cache inventory, not an adapter policy branch.
                || (path.ends_with("run/src/retention.rs")
                    && source[..found.start()].contains("PROVIDER_CACHES"));
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

// One implementation per shared primitive. The owner path is relative to
// tools/front-door; a free function with the same name anywhere else would
// restore the duplication these helpers were collapsed from.
const OWNED_HELPERS: &[(&str, &str)] = &[
    ("quote", "crates/os/src/shell.rs"),
    ("quote_if_needed", "crates/os/src/shell.rs"),
    ("quote_portable", "crates/os/src/shell.rs"),
    ("quote_os", "crates/os/src/shell.rs"),
    ("shell_quote", "crates/os/src/shell.rs"),
    ("shell_quote_text", "crates/os/src/shell.rs"),
    ("shell_quote_os", "crates/os/src/shell.rs"),
    ("now_unix_seconds", "crates/os/src/clock.rs"),
    ("now_unix_millis", "crates/os/src/clock.rs"),
    ("now_unix_ms", "crates/os/src/clock.rs"),
    ("hex", "crates/core/src/bytes.rs"),
    ("atomic_write", "crates/os/src/fs.rs"),
];

// Different behaviours with their own owners: SQL identifier quoting and a
// byte predicate used by diagnostic redaction. Neither is shell quoting.
const HELPER_EXEMPTIONS: &[&str] = &[
    "crates/sqlite/src/schema.rs",
    "crates/confinement/src/publication.rs",
];

fn free_fn_declaration() -> Result<regex::Regex, regex::Error> {
    regex::Regex::new(
        r"^(?:pub(?:\([^)]*\))?\s+|async\s+|const\s+|unsafe\s+)*fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
    )
}

fn local_helper_violations(source: &str, relative: &str, pattern: &regex::Regex) -> Vec<String> {
    if HELPER_EXEMPTIONS.contains(&relative) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (index, line) in source.lines().enumerate() {
        // Methods sit inside an impl and are indented; only free definitions
        // create a second owner for a primitive.
        if line.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        let Some(name) = pattern.captures(line).and_then(|found| found.get(1)) else {
            continue;
        };
        let owner = OWNED_HELPERS
            .iter()
            .find(|(owned, _)| *owned == name.as_str())
            .map(|(_, owner)| *owner);
        if let Some(owner) = owner
            && owner != relative
        {
            findings.push(format!(
                "{}:{}: {} must be defined in {owner}",
                relative,
                index + 1,
                name.as_str()
            ));
        }
    }
    findings
}

#[test]
fn local_helper_rule_distinguishes_owners_from_duplicates() -> Result<(), Box<dyn Error>> {
    let pattern = free_fn_declaration()?;
    assert_eq!(
        local_helper_violations(
            "fn shell_quote(x: &str) -> String { x.into() }",
            "src/pr/sync.rs",
            &pattern
        )
        .len(),
        1
    );
    assert!(
        local_helper_violations(
            "pub fn quote(text: &str) -> String {\n    format!(\"'{text}'\")\n}",
            "crates/os/src/shell.rs",
            &pattern
        )
        .is_empty()
    );
    assert!(
        local_helper_violations(
            "fn quote(identifier: &str) -> String { identifier.into() }",
            "crates/sqlite/src/schema.rs",
            &pattern
        )
        .is_empty()
    );
    // A method is not a second owner, and a line inside a literal is not code.
    assert!(local_helper_violations(
        "impl Store {\n    fn atomic_write(&self) {}\n}\nlet text = \"fn hex(bytes: &[u8]) -> String\";",
        "crates/workflow/src/store.rs",
        &pattern
    )
    .is_empty());
    assert_eq!(
        local_helper_violations(
            "pub(crate) fn now_unix_ms() -> u64 { 0 }",
            "crates/run/src/build/ledger.rs",
            &pattern
        )
        .len(),
        1
    );
    Ok(())
}

#[test]
fn owned_helpers_have_one_definition() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pattern = free_fn_declaration()?;
    let mut sources = Vec::new();
    for path in source_roots() {
        rust_sources(&path, &mut sources)?;
    }
    let mut violations = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(&path)?;
        violations.extend(local_helper_violations(&source, &relative, &pattern));
    }
    assert!(
        violations.is_empty(),
        "shared helpers must have one owner:\n{}",
        violations.join("\n")
    );
    Ok(())
}

// Spec: docs/specs/rust-workspace.md#one-owner-per-command-domain
#[test]
fn every_grammar_leaf_has_exactly_one_registry_verb() -> Result<(), Box<dyn Error>> {
    provenant::commands::install();
    let grammar: std::collections::BTreeSet<String> =
        provenant_cli::command_verbs().into_iter().collect();
    let mut registered = Vec::new();
    let mut domain_names = std::collections::BTreeSet::new();
    for domain in provenant::commands::DOMAINS {
        assert!(
            domain_names.insert(domain.name),
            "domain `{}` is declared twice",
            domain.name
        );
        for verb in domain.verbs {
            assert!(!verb.path.is_empty(), "a verb has an empty path");
            assert!(
                verb.path
                    .iter()
                    .all(|part| !part.is_empty() && !part.contains(' ')),
                "a verb path segment must be one grammar word: {:?}",
                verb.path
            );
            registered.push(verb.path.join(" "));
        }
    }
    let unique: std::collections::BTreeSet<String> = registered.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        registered.len(),
        "two registry entries share a path: {registered:?}"
    );
    assert_eq!(
        unique, grammar,
        "every grammar leaf needs exactly one registry verb and every verb a grammar leaf"
    );
    Ok(())
}

// Spec: docs/specs/rust-workspace.md#one-owner-per-command-domain
// The usage verb strings are persisted telemetry units, so their set is a
// contract and may not change without a migration.
#[test]
fn usage_verbs_keep_their_persisted_spelling() {
    provenant::commands::install();
    let expected: std::collections::BTreeSet<&str> = [
        "accounts list",
        "accounts register",
        "accounts set",
        "accounts show",
        "accounts sync",
        "adapter capabilities",
        "admit",
        "adopt",
        "answer",
        "ask",
        "build maintain",
        "build report",
        "build reset",
        "build status",
        "chair log append",
        "chair log list",
        "chair message",
        "chair summary diff",
        "chair summary show",
        "chair summary update",
        "chair switch",
        "check config",
        "check install",
        "check providers",
        "check release",
        "check writing",
        "ci failures",
        "ci failures pr",
        "ci failures run",
        "cloud collect",
        "cloud launch",
        "config apply",
        "config check",
        "config detect",
        "config history",
        "config path",
        "config ports",
        "config schema",
        "config set",
        "config show",
        "config unset",
        "console board",
        "console changes",
        "console detail",
        "console open",
        "console overview",
        "console projects",
        "console search",
        "console sessions",
        "console tree",
        // #4020 replaces the handoff prose channel with a typed declaration.
        // `declare` is the new persisted top-level verb: the unit string is a
        // contract, so record its spelling alongside the other verbs.
        "declare",
        "disk report",
        // #4028 adds a real top-level doctor beside `hosts doctor`; the unit
        // string is part of the persisted verb set, so declare the new spelling.
        "doctor",
        "edit",
        "events",
        "fabric claim acquire",
        "fabric claim list",
        "fabric claim release",
        "fabric claim renew",
        "fabric claim verify",
        "fabric events note",
        "fabric events read",
        "fabric identity",
        "fabric mail mark-read",
        "fabric mail read",
        "fabric mail send",
        "fabric peer",
        "fabric peer-agent",
        "gc",
        "help",
        "hook git pre-push",
        "hosts",
        "hosts doctor",
        "inbox",
        "inbox acknowledge",
        "inbox close",
        "init",
        "install",
        "internal event-listen",
        "internal lane-cleanup",
        "internal launch",
        "internal provider-updates",
        "internal settle",
        "internal supervise",
        "internal tick-gc",
        "land",
        "lane deliver",
        "native end",
        "native start",
        "notes add",
        "notes read",
        "output",
        "parse",
        "pr contributions",
        "pr open",
        "pr status",
        "pr sync",
        "process-tree",
        "quota",
        "quota clear",
        "quota display",
        "quota history",
        "quota show",
        "quota tick",
        "reconcile",
        "release",
        "result",
        "retry",
        "root",
        "route exclude add",
        "route exclude list",
        "route exclude remove",
        "route list",
        "route override clear",
        "route override list",
        "route override set",
        "route pacing",
        "route remove",
        "route reset",
        "route resolve",
        "route set",
        "run",
        "status",
        "steer",
        "stop",
        "telemetry cost",
        "telemetry experiments show",
        "telemetry ingest",
        "telemetry mileage",
        "telemetry prices refresh",
        "telemetry stats",
        "telemetry trace",
        "telemetry value",
        "templates",
        "templates show",
        "tick",
        "trace",
        "tracker lint",
        "uninstall",
        "upgrade",
        "verify",
        "version",
        "wait",
        "workflow receipt save",
        "workflow receipt show",
        "workflow review",
        "workflow session diff",
        "workflow session handoff",
        "workflow session save",
        "workflow session show",
        "worktree create",
        "worktree doctor",
        "worktree list",
        "worktree locate",
        "worktree remove",
        "worktree session-list",
    ]
    .into_iter()
    .collect();
    let actual: std::collections::BTreeSet<String> =
        provenant_cli::usage_verbs().into_iter().collect();
    let expected: std::collections::BTreeSet<String> =
        expected.into_iter().map(str::to_owned).collect();
    let added: Vec<_> = actual.difference(&expected).cloned().collect();
    let removed: Vec<_> = expected.difference(&actual).cloned().collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "persisted usage verbs changed; added {added:?}, removed {removed:?}. The unit strings are a \
         persisted contract: keep the spelling, or migrate recorded usage in the same PR"
    );
}

// Spec: docs/specs/rust-workspace.md#one-owner-per-command-domain
// A ceiling is an overridable default: raise a file's entry in the same PR that
// makes the growth deliberate, and split rather than raise where a module has
// become an owner of unrelated mechanisms. Entries sit on 250-line bands with at
// least 100 lines of headroom, so ordinary growth does not edit this shared list
// and concurrent pull requests do not conflict on it; a raise moves an entry to
// the next band above its size plus 100.
#[test]
fn production_files_stay_within_their_line_ceilings() -> Result<(), Box<dyn Error>> {
    const DEFAULT_CEILING: usize = 2_000;
    const BAND: usize = 250;
    const CEILINGS: &[(&str, usize)] = &[
        ("crates/cli/src/fabric/graph_relay.rs", 2750),
        ("crates/cli/src/lib.rs", 4000),
        ("crates/cli/src/quota.rs", 7250),
        ("crates/cli/src/run.rs", 2750),
        // Compact output leads with settlement facts and bounded tails (#3807).
        ("crates/cli/src/run/output.rs", 2250),
        ("crates/cli/src/run/provider/confinement.rs", 3000),
        // Offer-mismatch refusals and portable fixture staging (#3892, #3979).
        ("crates/cli/src/run/provider/preparation.rs", 8250),
        ("crates/config/src/loader.rs", 3000),
        ("crates/config/src/managed.rs", 3250),
        ("crates/config/src/ports.rs", 2500),
        ("crates/config/src/project_binding.rs", 2500),
        ("crates/confinement/src/cache.rs", 4250),
        ("crates/confinement/src/repo.rs", 2250),
        ("crates/confinement/src/utility.rs", 6750),
        ("crates/confinement/src/worktrees/census.rs", 2750),
        ("crates/confinement/src/worktrees/gc.rs", 3500),
        ("crates/confinement/src/worktrees/lifecycle.rs", 3000),
        ("crates/fabric-runtime/src/coordination/facade.rs", 2000),
        ("crates/fabric-runtime/src/graph.rs", 4500),
        ("crates/fabric-runtime/src/graph/journal.rs", 2750),
        ("crates/fabric-runtime/src/graph/model.rs", 3000),
        ("crates/fabric-runtime/src/graph/outputs.rs", 2500),
        ("crates/fabric-runtime/src/graph/patch.rs", 2500),
        ("crates/fabric-runtime/src/graph/plans.rs", 2500),
        ("crates/fabric-runtime/src/graph/template.rs", 3250),
        ("crates/fabric-runtime/src/graph/transitions.rs", 2750),
        ("crates/fabric-runtime/src/hosts.rs", 4000),
        ("crates/os/src/sys/pid.rs", 2750),
        ("crates/providers/src/lane.rs", 3750),
        ("crates/providers/src/launch.rs", 3000),
        ("crates/providers/src/opencode.rs", 1500),
        ("crates/providers/src/package_cache.rs", 2500),
        ("crates/quota/src/admission.rs", 2500),
        ("crates/quota/src/schedule.rs", 3000),
        ("crates/records/src/adapters.rs", 2500),
        // The persisted offer-mismatch fact and its owner-facing detail (#3892).
        ("crates/records/src/run.rs", 4500),
        ("crates/redact/src/lib.rs", 2500),
        ("crates/routing/src/catalog.rs", 2500),
        // One owner for offer evidence, pinned-account judgement and the
        // same-reason refusal detail (#3892, #3933).
        ("crates/routing/src/resolve.rs", 4750),
        ("crates/routing/src/review.rs", 3000),
        ("crates/run/src/build_dir_gc.rs", 6250),
        // Static-check launch guidance and receipt provenance (#3652).
        ("crates/run/src/completion.rs", 2750),
        // Stall detection and its opt-in policy (#3903); unreadable-record
        // writer-lease reclaim under writer exclusion (#3829).
        ("crates/run/src/controls.rs", 5500),
        ("crates/run/src/delivery.rs", 4750),
        ("crates/run/src/git_context.rs", 3000),
        // One host phase and one sealed settlement assembly (#4020, #4024).
        ("crates/run/src/holder.rs", 5750),
        ("crates/run/src/launch.rs", 4250),
        // Scratch-root pruning and its pinned-Git invariants (#3947).
        ("crates/run/src/retention.rs", 5250),
        // The #3906 abandoned-lane registration prune and its pinned test.
        ("crates/run/src/settlement.rs", 4750),
        ("crates/run/src/store.rs", 2750),
        // The pushed tip sealed at the cut joined the writer terminal record
        // (#4020): 4255 lines. Next 250-band above size + 100.
        ("crates/run/src/writer.rs", 4250),
        ("crates/supervisor/src/custody.rs", 4000),
        ("crates/supervisor/src/guardian.rs", 3250),
        ("crates/workflow/src/session.rs", 3500),
        ("src/ci.rs", 2500),
        ("src/fabric/graph_execution.rs", 2750),
        // The #4064 reused-worktree review-base binding, typed writer-base
        // report and node-sourced panel helpers, plus #4066's authored repair
        // writer resolution and per-node route preview, grew this owner. #4062
        // then added capability-gap reporting, taking it to 4504 lines; 4750 is
        // the next 250-line band above size + 100.
        ("src/fabric/interface.rs", 4750),
        // Peer fixture transport and writer boundary coverage (#3979).
        ("src/fabric/owner.rs", 6000),
        ("src/fabric/peer.rs", 2500),
        ("src/fabric/run_owner.rs", 3000),
        // #4104's terminal remote-attempt recovery export grew this owner to
        // 4503 lines; 4750 is the next 250-line band above size + 100.
        ("src/fabric/unified.rs", 4750),
        ("src/session_handoff.rs", 1750),
        // The #4028 hook budget, overrun warning and diagnostic reader grew this
        // owner deliberately: 2770 lines on the CI merge base. Next 250-band
        // above size + 100, with headroom for routine growth.
        ("src/hooks.rs", 3000),
        ("src/install.rs", 3250),
        ("src/lane_cleanup.rs", 3250),
        ("src/pr/landing/adapters.rs", 3250),
        ("src/pr/landing/driver.rs", 4250),
        ("src/pr/observer.rs", 2750),
        ("src/status_wait.rs", 3500),
        // The #3966 one-pass dweller census and settled-candidate progress
        // reporting moved here from the per-project census helper.
        ("src/worktrees.rs", 6000),
    ];
    let off_band: Vec<_> = CEILINGS
        .iter()
        .filter(|(_, ceiling)| ceiling % BAND != 0)
        .map(|(file, ceiling)| format!("{file}: {ceiling}"))
        .collect();
    assert!(
        off_band.is_empty(),
        "line ceilings must sit on {BAND}-line bands so routine growth does not edit them:\n{}",
        off_band.join("\n")
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for path in source_roots() {
        rust_sources(&path, &mut sources)?;
    }
    let mut violations = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_source(&relative) {
            continue;
        }
        let lines = fs::read_to_string(&path)?.lines().count();
        let ceiling = CEILINGS
            .iter()
            .find(|(file, _)| *file == relative.as_str())
            .map_or(DEFAULT_CEILING, |(_, ceiling)| *ceiling);
        if lines > ceiling {
            violations.push(format!(
                "{relative} is {lines} lines, over its ceiling of {ceiling}; split the file, or raise \
                 its ceiling to the next {BAND}-line band above {lines} + 100 in the same PR that \
                 justifies the growth"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "production files grew past their line ceilings:\n{}",
        violations.join("\n")
    );
    Ok(())
}

// The `warning: ` prefix belongs only to the binary edge's emitter. A library
// that writes its own prefix bypasses dedup and JSON delivery, so the source
// check scans records too. Spec: docs/specs/cli.md#warnings-are-typed-values
const WARNING_EMITTERS: &[&str] = &["src/output.rs"];

/// Byte offsets of string literals whose text starts with `warning: `.
/// Comments and character literals are skipped so examples cannot trip the
/// rule; raw and byte strings are handled like plain ones.
fn warning_prefix_literals(source: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
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
        let raw_start = if bytes[i..].starts_with(b"br") || bytes[i..].starts_with(b"cr") {
            i + 1
        } else {
            i
        };
        if bytes.get(raw_start) == Some(&b'r') {
            let mut quote = raw_start + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) == Some(&b'"') {
                let hashes = quote - raw_start - 1;
                let start = quote + 1;
                i = start;
                while i < bytes.len() {
                    if bytes[i] == b'"'
                        && bytes
                            .get(i + 1..i + 1 + hashes)
                            .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
                    {
                        break;
                    }
                    i += 1;
                }
                if source[start..i.min(source.len())].starts_with("warning: ") {
                    found.push(raw_start);
                }
                i = (i + hashes + 1).min(bytes.len());
                continue;
            }
        }
        if bytes[i] == b'\'' {
            let mut end = i + 1;
            if bytes.get(end) == Some(&b'\\') {
                end = (end + 2).min(bytes.len());
            } else if let Some(character) = source[end..].chars().next() {
                end += character.len_utf8();
            }
            if bytes.get(end) == Some(&b'\'') {
                i = end + 1;
                continue;
            }
        }
        if bytes[i] == b'"' {
            let start = i;
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
            let text = &source[start + 1..i.saturating_sub(1).max(start + 1)];
            if text.starts_with("warning: ") {
                found.push(start);
            }
            continue;
        }
        i += 1;
    }
    found
}

/// Whether the literal at `at` is an argument of a write/print macro that
/// targets a standard stream. `format!` and other string builders are how the
/// emitter renders a human view; they are not direct writes.
fn warning_prefix_stream_write(source: &str, at: usize) -> bool {
    const MACROS: &[&str] = &["eprintln", "eprint", "println", "print", "writeln", "write"];
    let before = &source[..at];
    let window = before.len().saturating_sub(240);
    let mut nearest: Option<(usize, &str)> = None;
    for name in MACROS {
        let needle = format!("{name}!");
        let Some(relative) = before[window..].rfind(&needle) else {
            continue;
        };
        let position = window + relative;
        let boundary = position == 0
            || !before.as_bytes()[position - 1].is_ascii_alphanumeric()
                && before.as_bytes()[position - 1] != b'_';
        if boundary && nearest.is_none_or(|(found, _)| position > found) {
            nearest = Some((position, name));
        }
    }
    let Some((position, name)) = nearest else {
        return false;
    };
    let Some(open) = source[position..at].find('(') else {
        return false;
    };
    // The literal must be inside this invocation, not merely nearby code.
    let mut depth = 0_i32;
    for byte in source[position + open + 1..at].bytes() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    let invocation = &source[position..at];
    match name {
        "eprintln" | "eprint" | "println" | "print" => true,
        _ => invocation.contains("stderr") || invocation.contains("stdout"),
    }
}

fn warning_prefix_write_violations(source: &str, test_file: bool) -> bool {
    let offset = |part: &str| part.as_ptr() as usize - source.as_ptr() as usize;
    let tests = selected_test_scopes(source, test_file, false);
    warning_prefix_literals(source).into_iter().any(|at| {
        let in_test = tests.iter().any(|scope| {
            let start = offset(scope);
            (start..start + scope.len()).contains(&at)
        });
        !in_test && warning_prefix_stream_write(source, at)
    })
}

#[test]
fn warning_prefix_rule_distinguishes_emission_from_prose() {
    for source in [
        "fn f() { eprintln!(\"warning: x\"); }",
        "fn f() { println!(\"warning: {}\", x); }",
        "fn f() { writeln!(std::io::stderr().lock(), \"warning: {}\", x); }",
        "fn f() { write!(io::stdout(), \"warning: {}\", x); }",
        "fn f() { eprintln!(r#\"warning: x\"#); }",
        "fn f() { let before = 1; eprintln!(\"warning: x\"); }",
    ] {
        assert!(
            warning_prefix_write_violations(source, false),
            "missed: {source}"
        );
    }
    for source in [
        "/// prints `warning: <message>` to stderr",
        "// eprintln!(\"warning: x\");",
        "fn f() { let text = \"warning: x\"; }",
        "fn f() { let text = format!(\"warning: {x}\"); }",
        "fn f() { writeln!(human, \"warning: {}\", x); }",
        "fn f() { let x = 1; let text = \"warning: x\"; }",
        "#[cfg(test)] fn f() { eprintln!(\"warning: x\"); }",
        "#[cfg(test)] mod tests { fn f() { eprintln!(\"warning: x\"); } }",
    ] {
        assert!(
            !warning_prefix_write_violations(source, false),
            "false positive: {source}"
        );
    }
    assert!(!warning_prefix_write_violations(
        "fn f() { eprintln!(\"warning: x\"); }",
        true
    ));
}

#[test]
fn warning_prefix_is_written_only_by_the_emitter() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for path in source_roots() {
        rust_sources(&path, &mut sources)?;
    }
    let mut violations = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if WARNING_EMITTERS.contains(&relative.as_str()) {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        if warning_prefix_write_violations(&source, is_test_source(&path.to_string_lossy())) {
            violations.push(relative);
        }
    }
    assert!(
        violations.is_empty(),
        "route warnings through provenant_records::warning::warn; only the edge writes the prefix:\n{}",
        violations.join("\n")
    );
    Ok(())
}

// Spec: docs/specs/run-settlement.md#outcome-before-release
// One pure `settle` is the only writer of a launched attempt's outcome fact. The
// patterns use the slice 12 spellings (`CauseKind`, `SettlementClass`); old names
// are refused so a compatibility alias cannot silently reintroduce a decider.
#[test]
#[allow(
    clippy::print_stderr,
    reason = "Preserve the warn-only architecture growth advisory in this developer test."
)]
fn settlement_outcome_has_one_writer() -> Result<(), Box<dyn Error>> {
    const SETTLE_OWNER: &str = "crates/records/src/settlement/classify.rs";
    const EVIDENCE_OWNER: &str = "crates/records/src/settlement/mod.rs";
    const HOLDER: &str = "crates/run/src/holder.rs";
    const TRANSITIONS: &str = "crates/fabric-runtime/src/graph/transitions.rs";
    const HOLDER_CEILING: usize = 5750;

    // Blank test-only items without moving line numbers, so an assertion can
    // name both the file and the line. Whole test files are blanked as well:
    // stale parser call sites in tests are not production deciders.
    let production = |source: &str, test_file: bool| -> String {
        let base = source.as_ptr() as usize;
        let mut spans: Vec<(usize, usize)> = test_scopes(source, test_file)
            .iter()
            .map(|scope| {
                let start = scope.as_ptr() as usize - base;
                (start, start + scope.len())
            })
            .collect();
        spans.sort_unstable();
        let mut out = String::with_capacity(source.len());
        let mut cursor = 0;
        for (start, end) in spans {
            let start = start.max(cursor).min(source.len());
            let end = end.min(source.len());
            // A nested test item can lie wholly inside an enclosing `#[cfg(test)]`
            // module scope: after the outer span the cursor is already past the
            // inner end. That span is blanked already; skip it rather than slice
            // a reversed byte range.
            if start >= end {
                continue;
            }
            out.push_str(&source[cursor..start]);
            for ch in source[start..end].chars() {
                out.push(if ch == '\n' { '\n' } else { ' ' });
            }
            cursor = end;
        }
        if cursor < source.len() {
            out.push_str(&source[cursor..]);
        }
        out
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for path in source_roots() {
        rust_sources(&path, &mut sources)?;
    }
    let mut violations = Vec::new();
    let mut holder_lines = 0usize;

    for path in sources {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(&path)?;
        if relative == HOLDER {
            holder_lines = source.lines().count();
        }
        // `*_invariants.rs` modules are registered under `#[cfg(test)]` in this
        // workspace, so they are test sources too even though their stem does
        // not carry a test suffix.
        let test_file = is_test_source(&relative) || relative.ends_with("_invariants.rs");
        let scanned = production(&source, test_file);
        let lines: Vec<&str> = scanned.lines().collect();

        for (index, line) in lines.iter().enumerate() {
            let where_ = format!("{relative}:{}", index + 1);
            if line.contains("OutcomeFact {")
                && !line.contains("struct OutcomeFact")
                && !line.trim_start().starts_with("->")
                && relative != SETTLE_OWNER
            {
                violations.push(format!("{where_}: outcome fact built outside settle"));
            }
            if line.contains("AppliedPolicy {")
                && !line.contains("struct AppliedPolicy")
                && relative != SETTLE_OWNER
            {
                violations.push(format!("{where_}: applied policy built outside settle"));
            }
            // The holder builds a launched lane's evidence; every host-decided
            // terminal goes through `SettlementEvidence::host_decided`.
            if line.contains("SettlementEvidence {")
                && !line.contains("struct SettlementEvidence")
                && !line.contains("enum SettlementEvidence")
                && !line.contains("impl SettlementEvidence")
                && relative != EVIDENCE_OWNER
                && relative != HOLDER
            {
                violations.push(format!(
                    "{where_}: settlement evidence built outside host_decided and the holder"
                ));
            }
            if line.contains("settlement_class = Some(SettlementClass::") {
                violations.push(format!("{where_}: settlement class written without settle"));
            }
            if relative == SETTLE_OWNER
                && (line.contains(".pushed_head") || line.contains(".uncommitted_paths"))
            {
                violations.push(format!("{where_}: settle reads display-only Git evidence"));
            }
            if relative != TRANSITIONS
                && line.contains("advance_code = Some(")
                && line.contains("\"stopped_no_work\"")
            {
                violations.push(format!(
                    "{where_}: no-work advance code written outside transitions"
                ));
            }
            for retired in [
                "from_turns",
                "json_fence_fallback",
                "SettlementReason::derive",
                "DECLARED_STOP_CODES",
                "TerminalCause",
                "PartialSignal",
                "SettlementReason",
            ] {
                if line.contains(retired) {
                    violations.push(format!("{where_}: retired settlement name `{retired}`"));
                }
            }
        }

        let settle_line = if relative == HOLDER {
            lines
                .iter()
                .position(|line| line.contains("classify::settle("))
        } else {
            None
        };
        if let Some(settle_line) = settle_line {
            for (index, line) in lines.iter().enumerate().skip(settle_line + 1) {
                if line.contains("decoded.outcome =") || line.contains("decoded.cause =") {
                    violations.push(format!(
                        "{relative}:{}: host outcome mutated after settle",
                        index + 1
                    ));
                }
                // The settled decision is one record: a later field write
                // would leave the class, release and delivery disagreeing.
                for field in [
                    "fact.outcome = ",
                    "fact.cause = ",
                    "fact.complete = ",
                    "fact.settlement_class = ",
                    "fact.release = ",
                    "fact.delivery_eligible = ",
                    "fact.signals.push(",
                ] {
                    if line.contains(field) {
                        violations.push(format!(
                            "{relative}:{}: settled fact mutated after settle (`{field}`)",
                            index + 1
                        ));
                    }
                }
            }
        }
    }

    if holder_lines > HOLDER_CEILING {
        eprintln!(
            "settlement host phase grew to {holder_lines} lines (ceiling {HOLDER_CEILING}); \
             split the host-phase record from the sealed assembly step before raising the ceiling"
        );
    }

    assert!(
        violations.is_empty(),
        "settlement outcome has more than one writer:\n{}",
        violations.join("\n")
    );
    Ok(())
}

/// True when the declaration at `index` is guarded by `gate` in its own
/// attribute stack. Other attributes may be stacked above it (including a
/// multi-line clippy allow) and function bodies indent them; neither changes
/// which cfg governs the declaration.
fn declaration_is_gated(lines: &[&str], index: usize, gate: &str) -> bool {
    let mut cursor = index;
    let mut depth = 0usize;
    while cursor > 0 {
        cursor -= 1;
        let line = lines[cursor].trim();
        if line == gate {
            return true;
        }
        // Walking up, a closing bracket opens an attribute body and an
        // opening bracket closes it; a one-line attribute nets to zero.
        let opens = line.chars().filter(|character| *character == '[').count();
        let closes = line.chars().filter(|character| *character == ']').count();
        depth = depth.saturating_add(closes).saturating_sub(opens);
        if depth > 0 || line.starts_with("#[") {
            continue;
        }
        return false;
    }
    false
}

#[test]
fn worktree_driver_links_the_library_instead_of_including_source() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_gate = "#[cfg(any(test, feature = \"fixture\"))]";
    let driver = fs::read_to_string(root.join("tests/worktree_driver.rs"))?;
    assert!(
        !driver.contains("include!"),
        "tests/worktree_driver.rs compiles source inline; link the library instead"
    );
    assert!(
        driver.contains("use provenant::worktree_tests;"),
        "tests/worktree_driver.rs must dispatch through the library's worktree harness"
    );
    for entry in fs::read_dir(root.join("tests"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            let source = fs::read_to_string(&path)?;
            assert!(
                !source.contains("include!(\"../src/"),
                "{} compiles crate source inline; link the library instead",
                path.display()
            );
        }
    }
    let lib = fs::read_to_string(root.join("src/lib.rs"))?;
    let lib_lines = lib.lines().collect::<Vec<_>>();
    let worktree_module = lib_lines
        .iter()
        .position(|line| line.trim() == "pub mod worktree_tests;");
    assert!(
        worktree_module.is_some_and(|index| declaration_is_gated(&lib_lines, index, fixture_gate)),
        "worktree_tests must stay fixture-gated so native worker observations never reach release builds"
    );
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    let document = manifest.parse::<DocumentMut>()?;
    let target = document["test"]
        .as_array_of_tables()
        .ok_or("Cargo.toml has no [[test]] targets")?
        .iter()
        .find(|table| table["name"].as_str() == Some("worktrees"))
        .ok_or("Cargo.toml has no worktrees test target")?;
    let required = target["required-features"]
        .as_array()
        .ok_or("worktrees target has no required-features")?
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        required.as_slice(),
        &["fixture"],
        "worktrees must build the fixture-gated library"
    );
    Ok(())
}

#[test]
fn linked_harness_keeps_its_native_observation_seams_under_fixture() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_gate = "#[cfg(any(test, feature = \"fixture\"))]";
    let production_gate = "#[cfg(not(any(test, feature = \"fixture\")))]";

    let gc = fs::read_to_string(root.join("src/fabric/gc_cleanup.rs"))?;
    let gc_lines = gc.lines().collect::<Vec<_>>();
    assert!(
        gc_lines.windows(2).any(|pair| {
            pair[0].trim() == fixture_gate && pair[1].trim_start() == "thread_local! {"
        }),
        "SPAWNED must exist in fixture builds so the harness can inject collection failures"
    );
    let spawned = gc_lines.iter().position(|line| {
        line.trim_start()
            .starts_with("return SPAWNED.with_borrow_mut")
    });
    assert!(
        spawned.is_some_and(|index| declaration_is_gated(&gc_lines, index, fixture_gate)),
        "detached collection must record its request in the fixture-linked library"
    );
    assert!(
        gc_lines
            .windows(2)
            .any(|pair| pair[0].trim() == production_gate && pair[1].trim() == "{"),
        "real detached collection must stay disabled whenever the harness is linked"
    );

    let land = fs::read_to_string(root.join("src/pr/land.rs"))?;
    let land_lines = land.lines().collect::<Vec<_>>();
    let delete = land_lines
        .iter()
        .position(|line| line.trim() == "pub(crate) fn delete_after_check(");
    assert!(
        delete.is_some_and(|index| declaration_is_gated(&land_lines, index, fixture_gate)),
        "delete_after_check must compile into the fixture-linked library"
    );
    Ok(())
}

#[test]
fn linked_harness_receives_the_production_binary_from_its_driver() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = fs::read_to_string(root.join("tests/worktree_driver.rs"))?;
    assert!(
        driver.contains("option_env!(\"CARGO_BIN_EXE_provenant\")")
            && driver.contains("worktree_tests::main("),
        "the linked driver must pass Cargo's production binary path to the harness"
    );
    let harness = fs::read_to_string(root.join("src/worktree_tests.rs"))?;
    assert!(
        harness.contains("set_production_binary(production_binary)"),
        "the harness entry must install the supplied path before dispatch"
    );
    let support = fs::read_to_string(root.join("tests/support/mod.rs"))?;
    assert!(
        support.contains("pub fn production_binary()") && support.contains("OnceLock<PathBuf>"),
        "fixtures must read the explicit path before Cargo's integration-only default"
    );
    let compose = fs::read_to_string(root.join("src/compose_tests.rs"))?;
    assert!(
        compose.contains("production_binary()?")
            && !compose.contains("option_env!(\"CARGO_BIN_EXE_provenant\")"),
        "fixture-linked dispatch must use the handed-off binary path"
    );
    Ok(())
}
