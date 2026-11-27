//! B8 invariants: the batch's new public seams have no dead-code allows, and
//! `LaunchTimings` has one owner. Spec: docs/specs/agent-native.md §3.4 and §4.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const LAUNCH: &str = include_str!("../crates/run/src/launch.rs");
const RUN_MODEL: &str = include_str!("../crates/fabric-runtime/src/graph/model.rs");
const RUN_LIB: &str = include_str!("../crates/run/src/lib.rs");

fn rust_sources(path: &Path, paths: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_symlink() {
                rust_sources(&entry.path(), paths)?;
            }
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        paths.push(path.to_owned());
    }
    Ok(())
}

#[test]
fn production_dead_code_allows_are_explicitly_justified() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pattern = regex::Regex::new(r"#\s*!?\s*\[[^\]]*?\ballow\s*\([^)]*\bdead_code\b[^\]]*\]")?;
    let mut allowed = BTreeMap::<(String, String), usize>::new();
    for line in include_str!("dead_code_allowlist.tsv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 3, "invalid allowlist entry: {line}");
        assert!(
            !fields[2].trim().is_empty(),
            "allowance needs a reason: {line}"
        );
        *allowed
            .entry((fields[0].to_owned(), fields[1].to_owned()))
            .or_default() += 1;
    }
    let mut paths = Vec::new();
    rust_sources(&root.join("src"), &mut paths)?;
    for entry in fs::read_dir(root.join("crates"))? {
        rust_sources(&entry?.path().join("src"), &mut paths)?;
    }
    for path in paths {
        let relative = path.strip_prefix(root)?;
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if relative
            .components()
            .any(|part| part.as_os_str() == "tests" || part.as_os_str() == "test")
            || stem.ends_with("_tests")
            || stem.ends_with("_test")
            || stem.ends_with("_invariants")
            || stem.starts_with("tests_")
        {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        for found in pattern.find_iter(&source) {
            let attribute = found
                .as_str()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let key = (relative.to_string_lossy().into_owned(), attribute);
            match allowed.get_mut(&key) {
                Some(count) if *count > 0 => *count -= 1,
                _ => {
                    return Err(
                        format!("unlisted dead-code allowance: {}: {}", key.0, key.1).into(),
                    );
                }
            }
        }
    }
    for ((path, attribute), count) in allowed {
        assert_eq!(count, 0, "stale dead-code allowance: {path}: {attribute}");
    }
    Ok(())
}

#[test]
fn every_top_level_test_file_is_reachable() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        fs::read_to_string(root.join("Cargo.toml"))?.parse::<toml_edit::DocumentMut>()?;
    let tests = manifest["test"]
        .as_array_of_tables()
        .ok_or("no registered test targets")?;
    let mut registered = Vec::new();
    for test in tests {
        registered.push(root.join(test["path"].as_str().ok_or("test target has no path")?));
    }
    let mut sources = registered.clone();
    // Unit-test modules may be registered with a path attribute in the library.
    sources.push(root.join("src/lib.rs"));
    let module = regex::Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+([a-zA-Z_][a-zA-Z_0-9]*)\s*;")?;
    let path_attr = regex::Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#)?;
    let mut cursor = 0;
    while cursor < sources.len() {
        let path = sources[cursor].clone();
        cursor += 1;
        let source = fs::read_to_string(&path)?;
        let parent = path.parent().ok_or("source has no parent")?;
        for capture in module.captures_iter(&source) {
            let candidate = parent.join(format!("{}.rs", &capture[1]));
            if candidate.is_file() && candidate.starts_with(root.join("tests")) {
                let candidate = candidate.canonicalize()?;
                if !sources
                    .iter()
                    .any(|path| path.canonicalize().ok().as_ref() == Some(&candidate))
                {
                    sources.push(candidate);
                }
            }
        }
        for capture in path_attr.captures_iter(&source) {
            let candidate = parent.join(&capture[1]);
            if candidate.is_file() {
                let candidate = candidate.canonicalize()?;
                if !candidate.starts_with(root.join("tests")) {
                    continue;
                }
                if !sources
                    .iter()
                    .any(|path| path.canonicalize().ok().as_ref() == Some(&candidate))
                {
                    sources.push(candidate);
                }
            }
        }
    }
    let reachable: Vec<_> = sources
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    for entry in fs::read_dir(root.join("tests"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            assert!(
                reachable.contains(&path.canonicalize()?),
                "unregistered test file: {}",
                path.display()
            );
        }
    }
    Ok(())
}

#[test]
fn launch_timings_has_one_owner() {
    assert!(
        !LAUNCH.contains("struct LaunchTimings"),
        "the run crate must not redeclare LaunchTimings"
    );
    assert!(
        RUN_MODEL.contains("pub struct LaunchTimings"),
        "fabric-runtime owns the single LaunchTimings"
    );
    assert!(
        !RUN_LIB.contains("LaunchTimings"),
        "the run crate must not re-export a type it does not own"
    );
}

#[test]
fn command_errors_keep_the_wire_shape_and_fit_in_a_small_result() -> Result<(), Box<dyn Error>> {
    use provenant_records::CommandError;
    assert!(std::mem::size_of::<CommandError>() <= 2 * std::mem::size_of::<usize>());
    let error = CommandError::new("EXAMPLE", "example message", Some("cause"), "retry");
    let encoded = serde_json::to_value(&error)?;
    assert_eq!(encoded["code"], "EXAMPLE");
    assert_eq!(encoded["message"], "example message");
    assert_eq!(encoded["cause"], "cause");
    let decoded: CommandError = serde_json::from_value(encoded)?;
    assert_eq!(decoded.code, "EXAMPLE");
    Ok(())
}
