//! Read-only discovery of commands a consumer repository already supports.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DetectedStack {
    pub stack: String,
    pub cwd: PathBuf,
}

use crate::ports::TierCommand;

#[derive(Debug, Default, Serialize)]
pub struct Detection {
    pub stacks: Vec<DetectedStack>,
    pub tiers: BTreeMap<String, Vec<TierCommand>>,
    pub default_tier: Option<String>,
    pub warnings: Vec<String>,
}

impl Detection {
    fn add(&mut self, tier: &str, stack: &str, cwd: &Path, argv: &[&str]) {
        self.tiers
            .entry(tier.into())
            .or_default()
            .push(TierCommand {
                stack: stack.into(),
                cwd: cwd.into(),
                argv: argv.iter().map(|s| (*s).into()).collect(),
            });
    }
}

/// Discover without invoking project commands or changing the project tree.
/// Detected paths are relative to the project root, assuming the default verify workspace.
pub fn detect(project: &Path) -> Detection {
    let mut result = Detection::default();
    let mut dirs = vec![PathBuf::from(".")];
    for depth in 0..=2 {
        let mut candidates = Vec::new();
        for dir in &dirs {
            if let Ok(text) = std::fs::read_to_string(project.join(dir).join("Cargo.toml"))
                && let Ok(doc) = text.parse::<toml_edit::DocumentMut>()
                && (doc.get("workspace").is_some_and(toml_edit::Item::is_table)
                    || doc.get("package").is_some_and(toml_edit::Item::is_table))
            {
                candidates.push(dir.clone());
            }
        }
        if candidates.len() > 1 {
            result.warnings.push(format!(
                "multiple Rust manifests at depth {depth}; Rust detection skipped"
            ));
            break;
        }
        if let Some(cwd) = candidates.first() {
            result.stacks.push(DetectedStack {
                stack: "rust".into(),
                cwd: cwd.clone(),
            });
            for (tier, argv) in [
                ("0", vec!["cargo", "fmt", "{packages}", "--", "--check"]),
                ("0", vec!["cargo", "check", "--locked", "{packages}"]),
                (
                    "0",
                    vec![
                        "cargo",
                        "nextest",
                        "run",
                        "--profile",
                        "lane",
                        "--locked",
                        "{packages}",
                        "--lib",
                        "--bins",
                    ],
                ),
                (
                    "1",
                    vec![
                        "cargo",
                        "nextest",
                        "run",
                        "--profile",
                        "lane",
                        "--locked",
                        "{integration_args}",
                    ],
                ),
                (
                    "2",
                    vec![
                        "cargo",
                        "nextest",
                        "run",
                        "--profile",
                        "ci",
                        "--workspace",
                        "--locked",
                    ],
                ),
                (
                    "3",
                    vec![
                        "cargo",
                        "nextest",
                        "run",
                        "--profile",
                        "ci",
                        "--workspace",
                        "--locked",
                        "--stress-count",
                        "20",
                    ],
                ),
            ] {
                result.add(tier, "rust", cwd, &argv);
            }
            break;
        }
        if depth < 2 {
            let mut next = Vec::new();
            for dir in dirs {
                if let Ok(entries) = std::fs::read_dir(project.join(&dir)) {
                    for entry in entries.flatten() {
                        if entry.file_type().is_ok_and(|t| t.is_dir()) {
                            next.push(dir.join(entry.file_name()));
                        }
                    }
                }
            }
            next.sort();
            dirs = next;
        }
    }
    let package = project.join("package.json");
    if let Ok(text) = std::fs::read_to_string(package) {
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) if value.is_object() => {
                let manager = value
                    .get("packageManager")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.split('@').next())
                    .filter(|s| matches!(*s, "npm" | "pnpm" | "yarn" | "bun"))
                    .map(str::to_owned)
                    .or_else(|| {
                        [
                            ("pnpm-lock.yaml", "pnpm"),
                            ("yarn.lock", "yarn"),
                            ("package-lock.json", "npm"),
                            ("bun.lock", "bun"),
                            ("bun.lockb", "bun"),
                        ]
                        .iter()
                        .find(|(file, _)| project.join(file).is_file())
                        .map(|(_, manager)| (*manager).to_owned())
                    })
                    .unwrap_or_else(|| {
                        result
                            .warnings
                            .push("Node package manager not found; using npm".into());
                        "npm".into()
                    });
                let cwd = Path::new(".");
                result.stacks.push(DetectedStack {
                    stack: "node".into(),
                    cwd: cwd.into(),
                });
                for (tier, script) in [
                    ("0", "lint"),
                    ("0", "typecheck"),
                    ("1", "test"),
                    ("2", "build"),
                ] {
                    if value
                        .get("scripts")
                        .and_then(|s| s.get(script))
                        .is_some_and(|s| s.is_string())
                    {
                        result.add(tier, "node", cwd, &[&manager, "run", script]);
                    }
                }
            }
            _ => result
                .warnings
                .push("invalid root package.json; Node detection skipped".into()),
        }
    }
    result.default_tier = result.tiers.keys().next().cloned();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    fn write(root: &Path, name: &str, data: &str) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    #[test]
    fn nested_rust_found_ties_refused() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "a/b/Cargo.toml", "[workspace]\n");
        assert_eq!(detect(root.path()).stacks[0].cwd, Path::new("./a/b"));
        write(root.path(), "c/d/Cargo.toml", "[package]\nname='other'\n");
        let result = detect(root.path());
        assert!(result.stacks.is_empty());
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn node_manager_field_then_lockfile() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "package.json",
            r#"{"packageManager":"yarn@4.0.0","scripts":{"lint":"ok"}}"#,
        );
        write(root.path(), "pnpm-lock.yaml", "");
        let result = detect(root.path());
        assert_eq!(result.tiers["0"][0].argv, ["yarn", "run", "lint"]);
        assert!(!result.tiers.contains_key("1"));
        write(root.path(), "package.json", r#"{"scripts":{"test":"ok"}}"#);
        assert_eq!(
            detect(root.path()).tiers["1"][0].argv,
            ["pnpm", "run", "test"]
        );
    }

    #[test]
    fn root_pnpm_with_nested_rust() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "nested/Cargo.toml", "[workspace]\n");
        write(
            root.path(),
            "package.json",
            r#"{"scripts":{"lint":"ok","test":"ok","build":"ok"}}"#,
        );
        write(root.path(), "pnpm-lock.yaml", "");
        let state = tempfile::tempdir().unwrap();
        let table = crate::PortTable::read_for(Some(root.path()), state.path()).unwrap();
        for (name, script) in [("0", "lint"), ("1", "test"), ("2", "build")] {
            let tier = &table.verify.tiers[name];
            assert_eq!(tier.selection, crate::VerifySelection::All);
            let (node, rust) = tier.commands.split_last().unwrap();
            assert!(!rust.is_empty());
            for command in rust {
                assert_eq!(
                    (&*command.stack, command.cwd.as_path()),
                    ("rust", Path::new("./nested"))
                );
            }
            assert_eq!((&*node.stack, node.cwd.as_path()), ("node", Path::new(".")));
            assert_eq!(node.argv, ["pnpm", "run", script]);
        }
    }

    fn snapshot(root: &Path, dir: &Path, entries: &mut Vec<(PathBuf, u64, SystemTime)>) {
        for entry in fs::read_dir(root.join(dir)).unwrap() {
            let entry = entry.unwrap();
            let path = dir.join(entry.file_name());
            let metadata = entry.metadata().unwrap();
            entries.push((path.clone(), metadata.len(), metadata.modified().unwrap()));
            if metadata.is_dir() {
                snapshot(root, &path, entries);
            }
        }
    }

    #[test]
    fn detection_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "nested/Cargo.toml", "[workspace]\n");
        write(root.path(), "package.json", r#"{"scripts":{"test":"ok"}}"#);
        let mut before = Vec::new();
        snapshot(root.path(), Path::new(""), &mut before);
        detect(root.path());
        let mut after = Vec::new();
        snapshot(root.path(), Path::new(""), &mut after);
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn detected_never_overrides_configured() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write(root.path(), "package.json", r#"{"scripts":{"test":"ok"}}"#);
        let table = crate::PortTable::read_for(Some(root.path()), state.path()).unwrap();
        assert_eq!(table.verify.default_tier, "1");
        assert_eq!(
            table.provenance()["verify.default_tier"],
            crate::PortSource::Detected
        );
        write(
            state.path(),
            "ports.json",
            r#"{"verify":{"default_tier":"2","tiers":{"1":{"commands":[{"argv":["custom"]}]}}}}"#,
        );
        let table = crate::PortTable::read_for(Some(root.path()), state.path()).unwrap();
        assert_eq!(table.verify.default_tier, "2");
        assert_eq!(table.verify.tiers["1"].commands[0].argv, ["custom"]);
        assert_eq!(
            table.provenance()["verify.default_tier"],
            crate::PortSource::Host
        );
    }

    #[test]
    fn this_repo_resolves_unchanged() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let state = tempfile::tempdir().unwrap();
        let table = crate::PortTable::read_for(Some(&project), state.path()).unwrap();
        let mut actual = serde_json::to_value(&table.verify).unwrap();
        for (name, tier) in &table.verify.tiers {
            actual["tiers"][name]["commands"] = serde_json::json!(
                tier.commands
                    .iter()
                    .map(|command| command.argv.join(" "))
                    .collect::<Vec<_>>()
            );
        }
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/verify.json")).unwrap();
        assert_eq!(actual, expected);
    }
}
