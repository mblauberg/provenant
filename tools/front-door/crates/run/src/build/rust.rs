//! Rust adapter: eligibility, stable environment and path rewriting.
use super::process::Command;
use super::{BuildContext, Exception, Job, JobKind, Priority};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RustClass {
    OffloadEligible { priority: Priority },
    Local { exception: Exception },
}

/// Classify intercepted commands without changing the caller's argv.
pub fn classify(job: &Job) -> RustClass {
    let eligible = match &job.kind {
        JobKind::Intercepted { .. } => job.steps.len() == 1 && eligible_step(job),
        _ => true,
    };
    if eligible {
        RustClass::OffloadEligible {
            priority: job.priority,
        }
    } else {
        RustClass::Local {
            exception: Exception::UnsupportedArgv,
        }
    }
}

fn eligible_step(job: &Job) -> bool {
    let step = &job.steps[0];
    let root = super::job_root(job);
    let cwd = if step.cwd_rel.is_absolute() {
        step.cwd_rel.clone()
    } else {
        root.join(&step.cwd_rel)
    };
    let (Ok(root), Ok(cwd)) = (root.canonicalize(), cwd.canonicalize()) else {
        return false;
    };
    if !cwd.starts_with(&root) {
        return false;
    }
    let args = &step.argv;
    let Some(program) = args
        .first()
        .and_then(|p| Path::new(p).file_name())
        .and_then(OsStr::to_str)
    else {
        return false;
    };
    let mut i = 1;
    if args
        .get(i)
        .and_then(|a| a.to_str())
        .is_some_and(|a| a.starts_with('+'))
    {
        i += 1;
    }
    let command = if program == "nextest" || program == "cargo-nextest" {
        "nextest"
    } else if program == "cargo" {
        let Some(c) = args.get(i).and_then(|a| a.to_str()) else {
            return false;
        };
        i += 1;
        c
    } else {
        return false;
    };
    if command == "nextest" {
        if program == "cargo-nextest" && args.get(i).and_then(|a| a.to_str()) == Some("nextest") {
            i += 1;
        }
        if !matches!(args.get(i).and_then(|a| a.to_str()), Some("run" | "list")) {
            return false;
        }
        i += 1;
    } else if !matches!(command, "check" | "clippy" | "test") {
        return false;
    }
    while i < args.len() {
        let Some(arg) = args[i].to_str() else {
            return false;
        };
        if ["--no-run", "--fix", "--target-dir", "--config"]
            .iter()
            .any(|x| arg == *x || arg.starts_with(&format!("{x}=")))
        {
            return false;
        }
        let manifest = if arg == "--manifest-path" {
            i += 1;
            args.get(i).and_then(|a| a.to_str())
        } else {
            arg.strip_prefix("--manifest-path=")
        };
        if arg == "--manifest-path" || arg.starts_with("--manifest-path=") {
            let Some(manifest) = manifest else {
                return false;
            };
            let path = Path::new(manifest);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            };
            let Ok(path) = path.canonicalize() else {
                return false;
            };
            if !path.starts_with(&root) {
                return false;
            }
        }
        i += 1;
    }
    true
}

/// Remove lane-specific environment and install the slot's stable inputs.
pub fn job_env(
    env: &[(OsString, OsString)],
    context: &BuildContext,
    slot: &super::slots::SlotPaths,
) -> Vec<(OsString, OsString)> {
    let mut result: Vec<_> = env
        .iter()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !k.starts_with("PROVENANT_")
                && !k.starts_with("GIT_")
                && !matches!(
                    k.as_ref(),
                    "PWD"
                        | "OLDPWD"
                        | "CARGO_TARGET_DIR"
                        | "CARGO_BUILD_TARGET_DIR"
                        | "CARGO_BUILD_BUILD_DIR"
                        | "TMPDIR"
                        | "TMP"
                        | "TEMP"
                        | "HOME"
                        | "CARGO_HOME"
                        | "PATH"
                )
        })
        .cloned()
        .collect();
    for (k, v) in [
        ("CARGO_TARGET_DIR", slot.target.as_os_str()),
        ("TMPDIR", slot.tmp.as_os_str()),
        ("HOME", context.host_home.as_os_str()),
        ("CARGO_HOME", context.cargo_home.as_os_str()),
        ("PATH", context.host_path.as_os_str()),
    ] {
        result.push((k.into(), v.into()));
    }
    if let Some(home) = &context.rustup_home {
        result.retain(|(key, _)| key != "RUSTUP_HOME");
        result.push(("RUSTUP_HOME".into(), home.as_os_str().into()));
    }
    result
}

/// Rewrite absolute path arguments only at a path-component boundary.
pub fn rewrite_argv(argv: &[OsString], worktree: &Path, repo: &Path) -> Vec<OsString> {
    argv.iter()
        .map(|arg| {
            if let Some(value) = arg
                .to_str()
                .and_then(|a| a.strip_prefix("--manifest-path="))
                && let Ok(rest) = Path::new(value).strip_prefix(worktree)
            {
                let mut mapped = OsString::from("--manifest-path=");
                mapped.push(repo.join(rest));
                return mapped;
            }
            let path = Path::new(arg);
            if path.is_absolute()
                && let Ok(rest) = path.strip_prefix(worktree)
            {
                return repo.join(rest).into_os_string();
            }
            arg.clone()
        })
        .collect()
}

/// The prewarm command is intentionally independent of the caller's argv.
pub fn maintenance_argv() -> Vec<OsString> {
    ["cargo", "check", "--workspace", "--all-targets", "--locked"]
        .into_iter()
        .map(Into::into)
        .collect()
}

/// What `rustc -vV` reports about the compiler a job actually uses. A channel
/// string alone does not identify a compiler: two toolchains can share a
/// channel while producing incompatible artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolchainInfo {
    pub release: String,
    pub host: String,
    pub commit: String,
}

impl ToolchainInfo {
    /// A stable, filesystem-safe key from the full `rustc -vV` identity. This
    /// is what keys the shared target directory, never the channel string.
    pub fn key(&self) -> String {
        digest_key(
            &[&self.release, &self.host, &self.commit],
            &[&self.release, &self.host],
        )
    }
}

/// Keep the human-readable parts of a key legible and bound them to a digest
/// of every distinguishing field, so no two identities can collide.
fn digest_key(hashed: &[&str], labelled: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in hashed {
        digest.update(part.as_bytes());
        digest.update([0_u8]);
    }
    let hash: String = digest
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let labels: Vec<String> = labelled.iter().map(|part| key_label(part)).collect();
    format!("{}--{hash}", labels.join("-"))
}

fn key_label(raw: &str) -> String {
    let label: String = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let label = label.trim_matches('-');
    if label.is_empty() {
        "unknown".to_owned()
    } else {
        label.to_owned()
    }
}

/// The channel pinned by a checkout, from `rust-toolchain.toml` or the plain
/// `rust-toolchain` file. Aliases such as `stable` are returned verbatim and
/// are not compared as concrete versions.
pub fn pinned_toolchain(root: &Path) -> Option<String> {
    let toml = fs::read_to_string(root.join("rust-toolchain.toml")).ok();
    if let Some(text) = toml {
        for line in text.lines() {
            let line = line.trim();
            let Some(value) = line.strip_prefix("channel") else {
                continue;
            };
            let value = value.trim().trim_start_matches('=').trim();
            let value = value.trim_matches(['"', '\'']);
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    let plain = fs::read_to_string(root.join("rust-toolchain")).ok()?;
    plain
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
}

/// A concrete pin that differs from the toolchain that warmed the cache. A
/// channel alias cannot be compared to a `rustc -vV` release, so it never
/// produces a drift report.
pub fn toolchain_drift(pin: &str, warm: Option<&str>) -> Option<(String, String)> {
    let warm = warm?;
    let concrete = pin.chars().next().is_some_and(|c| c.is_ascii_digit());
    (concrete && pin != warm).then(|| (pin.to_owned(), warm.to_owned()))
}

/// Resolve the compiler a job will actually run from the same stable
/// environment the job uses. Nothing here reads the caller's toolchain pin:
/// the pin is only compared against this result when reporting drift.
pub fn probe_toolchain(env: &[(OsString, OsString)], cwd: &Path) -> Option<ToolchainInfo> {
    let output = Command::new("rustc")
        .arg("-vV")
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().cloned())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let field = |name: &str| {
        let prefix = format!("{name}: ");
        text.lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    Some(ToolchainInfo {
        release: field("release")?.to_owned(),
        host: field("host")?.to_owned(),
        commit: field("commit").unwrap_or_default().to_owned(),
    })
}

/// Stable identity for a source checkout. Workspace members are keyed by this
/// so a worktree never reuses another worktree's member artifacts, while
/// commit-sourced maintenance jobs share one identity across their runs.
pub fn worktree_key(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    digest
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{JobKind, Origin, Output, SnapshotSource, Stack, Step};
    fn job(root: &Path, argv: &[&str]) -> Job {
        Job {
            id: "id".into(),
            project_key: "p".into(),
            project: root.into(),
            origin: Origin::Chair,
            owner: "chair".into(),
            kind: JobKind::Intercepted {
                stack: Stack::Rust,
                subcommand: "check".into(),
            },
            priority: Priority::Interactive,
            source: SnapshotSource::Commit {
                repo_common_dir: root.into(),
                commit: "head".into(),
            },
            verify_base: None,
            steps: vec![Step {
                cwd_rel: "".into(),
                argv: argv.iter().map(|arg| OsString::from(*arg)).collect(),
            }],
            stop_on_failure: true,
            env: vec![],
            output: Output::Stream,
            timeout: None,
            sandboxed: false,
            prelude: None,
        }
    }
    #[test]
    fn toolchain_key_binds_the_full_rustc_identity() {
        let pinned = ToolchainInfo {
            release: "1.100.0".into(),
            host: "x86_64-apple-darwin".into(),
            commit: "aaaaaaaa".into(),
        };
        let same_channel = ToolchainInfo {
            release: "1.100.0".into(),
            host: "x86_64-apple-darwin".into(),
            commit: "bbbbbbbb".into(),
        };
        let older = ToolchainInfo {
            release: "1.99.1".into(),
            host: "x86_64-apple-darwin".into(),
            commit: "aaaaaaaa".into(),
        };
        assert_eq!(pinned.key(), pinned.key());
        assert_ne!(pinned.key(), same_channel.key());
        assert_ne!(pinned.key(), older.key());
        assert!(pinned.key().starts_with("1.100.0-x86_64-apple-darwin--"));
        assert!(
            pinned
                .key()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        );
    }

    #[test]
    fn pinned_toolchain_reads_a_concrete_channel() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(pinned_toolchain(tmp.path()), None);
        std::fs::write(
            tmp.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.100.0\"\n",
        )
        .unwrap();
        assert_eq!(pinned_toolchain(tmp.path()).as_deref(), Some("1.100.0"));
    }

    #[test]
    fn toolchain_drift_ignores_aliases_and_matching_pins() {
        assert_eq!(
            toolchain_drift("1.100.0", Some("1.99.1")),
            Some(("1.100.0".into(), "1.99.1".into()))
        );
        assert_eq!(toolchain_drift("1.100.0", Some("1.100.0")), None);
        assert_eq!(toolchain_drift("stable", Some("1.99.1")), None);
        assert_eq!(toolchain_drift("1.100.0", None), None);
    }

    #[test]
    fn worktree_key_is_stable_per_checkout() {
        assert_eq!(
            worktree_key(Path::new("/lane/a")),
            worktree_key(Path::new("/lane/a"))
        );
        assert_ne!(
            worktree_key(Path::new("/lane/a")),
            worktree_key(Path::new("/lane/b"))
        );
    }

    #[test]
    fn eligibility_table() {
        let tmp = tempfile::tempdir().unwrap();
        for (argv, expected) in [
            (vec!["cargo", "+stable", "check"], true),
            (vec!["cargo", "clippy"], true),
            (vec!["cargo", "test"], true),
            (vec!["cargo", "nextest", "run"], true),
            (vec!["cargo-nextest", "nextest", "list"], true),
            (vec!["cargo", "build"], false),
            (vec!["cargo", "test", "--no-run"], false),
            (vec!["cargo", "clippy", "--fix"], false),
            (vec!["cargo", "check", "--target-dir=foo"], false),
            (vec!["cargo", "check", "--config", "x"], false),
            (
                vec!["cargo", "check", "--manifest-path", "/outside/Cargo.toml"],
                false,
            ),
        ] {
            assert_eq!(
                matches!(
                    classify(&job(tmp.path(), &argv)),
                    RustClass::OffloadEligible { .. }
                ),
                expected,
                "{argv:?}"
            );
        }
        let mut outside = job(tmp.path(), &["cargo", "check"]);
        outside.steps[0].cwd_rel = "../other".into();
        assert!(matches!(classify(&outside), RustClass::Local { .. }));
        let mut inside = job(tmp.path(), &["cargo", "check"]);
        inside.steps[0].cwd_rel = tmp.path().to_path_buf();
        assert!(matches!(
            classify(&inside),
            RustClass::OffloadEligible { .. }
        ));
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        assert!(matches!(
            classify(&job(
                tmp.path(),
                &["cargo", "check", "--manifest-path", "Cargo.toml"]
            )),
            RustClass::OffloadEligible { .. }
        ));
    }
    #[test]
    fn rewrite_only_component_prefixes() {
        let args = ["/lane/tree/file", "/lane/trees/file", "relative"].map(OsString::from);
        assert_eq!(
            rewrite_argv(&args, Path::new("/lane/tree"), Path::new("/slot/repo")),
            ["/slot/repo/file", "/lane/trees/file", "relative"].map(OsString::from)
        );
    }
    #[test]
    fn lane_varying_environment_is_removed() {
        let context = BuildContext {
            project: "/project".into(),
            project_key: "p".into(),
            worktree: "/lane/a".into(),
            attempt: None,
            sandboxed: false,
            git: None,
            port: provenant_config::PortTable::builtin().build,
            strategy_source: provenant_records::Layer::Default,
            host_home: "/home".into(),
            rustup_home: Some("/rustup".into()),
            cargo_home: "/cargo".into(),
            host_path: "/bin".into(),
            verify_base: None,
        };
        let slot = super::super::slots::SlotPaths::new(Path::new("/home"), "p", 0);
        let env = |lane: &str| {
            vec![
                ("PROVENANT_LANE".into(), lane.into()),
                ("GIT_INDEX_FILE".into(), lane.into()),
                ("PWD".into(), lane.into()),
                ("CARGO_TARGET_DIR".into(), lane.into()),
                ("TMPDIR".into(), lane.into()),
                ("RUSTFLAGS".into(), "-Dwarnings".into()),
                ("RUSTC_WRAPPER".into(), "/wrapper".into()),
            ]
        };
        let a = job_env(&env("a"), &context, &slot);
        assert_eq!(a, job_env(&env("b"), &context, &slot));
        assert!(a.contains(&("RUSTFLAGS".into(), "-Dwarnings".into())));
        assert!(a.contains(&("RUSTC_WRAPPER".into(), "/wrapper".into())));
        assert!(
            !a.iter()
                .any(|(k, _)| k == "GIT_INDEX_FILE" || k == "PWD" || k == "PROVENANT_LANE")
        );
    }
}
