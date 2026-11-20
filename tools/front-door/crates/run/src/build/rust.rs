//! Rust adapter: eligibility, stable environment and path rewriting.
use super::{BuildContext, Exception, Job, JobKind, Priority};
use std::{
    ffi::{OsStr, OsString},
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
            schema: 1,
            project: "/project".into(),
            project_key: "p".into(),
            worktree: "/lane/a".into(),
            attempt: None,
            sandboxed: false,
            git: None,
            port: provenant_config::PortTable::builtin().build,
            strategy_source: provenant_config::PortSource::Builtin,
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
