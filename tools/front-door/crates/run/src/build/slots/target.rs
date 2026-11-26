//! Toolchain and worktree identity for a slot's Cargo target.

use super::super::{BuildContext, Job, RunContext, SnapshotSource};
use super::SlotPaths;
use std::{fs, path::Path, path::PathBuf};

/// The derived Cargo target key for one job. Toolchain is resolved from
/// `rustc -vV` in the job's own environment; workspace members are isolated
/// with a worktree-specific rustc wrapper while dependencies stay shared.
pub(super) struct TargetResolution {
    pub(super) key: Vec<String>,
    pub(super) workspace_wrapper: Option<PathBuf>,
    pub(super) toolchain: Option<super::super::rust::ToolchainInfo>,
    pub(super) warnings: Vec<String>,
}

pub(super) fn resolve_target(
    job: &Job,
    context: &RunContext,
    paths: &SlotPaths,
) -> TargetResolution {
    use provenant_config::BuildTargetKey;

    let layout = context.strategy.port.target_layout();
    let mut warnings = layout.warnings;
    if layout.value.is_empty() {
        return TargetResolution {
            key: Vec::new(),
            workspace_wrapper: None,
            toolchain: None,
            warnings,
        };
    }

    let build_context = super::build_context(job, context);
    let env = super::super::rust::job_env(&job.env, &build_context, paths);
    let wants_toolchain = layout.value.contains(&BuildTargetKey::Toolchain);
    let toolchain = if wants_toolchain {
        let argv = job
            .steps
            .first()
            .map(|step| step.argv.as_slice())
            .unwrap_or(&[]);
        let cwd = target_probe_cwd(job, &paths.repo);
        super::super::rust::probe_job_toolchain(&env, &cwd, argv)
    } else {
        None
    };
    if wants_toolchain && toolchain.is_none() {
        warnings
            .push("could not resolve rustc -vV for the slot target; toolchain key omitted".into());
    }

    let checkout = match &job.source {
        SnapshotSource::WorkingTree { worktree, .. } => worktree.as_path(),
        SnapshotSource::Commit {
            repo_common_dir, ..
        } => repo_common_dir.as_path(),
    };
    let mut key = Vec::new();
    let mut wrapper_path = None;
    for component in &layout.value {
        match component {
            BuildTargetKey::Toolchain => {
                if let Some(info) = &toolchain {
                    key.push(info.key());
                }
            }
            BuildTargetKey::Worktree => match workspace_wrapper(paths, &build_context, checkout) {
                Ok(wrapper) => wrapper_path = Some(wrapper),
                Err(error) => {
                    warnings.push(format!(
                        "could not create per-worktree rustc wrapper; isolating the full Cargo target instead: {error}"
                    ));
                    key.push(super::super::rust::worktree_key(checkout));
                }
            },
        }
    }

    TargetResolution {
        key,
        workspace_wrapper: wrapper_path,
        toolchain,
        warnings,
    }
}

/// Resolve the first build step's working directory in the slot snapshot,
/// matching the path mapping used when the command itself is executed.
fn target_probe_cwd(job: &Job, slot_repo: &Path) -> PathBuf {
    let cwd_rel = job
        .steps
        .first()
        .map(|step| step.cwd_rel.as_path())
        .unwrap_or_else(|| Path::new(""));
    let source_root = super::super::job_root(job);
    let original = if cwd_rel.is_absolute() {
        cwd_rel.to_path_buf()
    } else {
        source_root.join(cwd_rel)
    };
    super::super::rust::rewrite_argv(&[original.into_os_string()], source_root, slot_repo)
        .into_iter()
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| slot_repo.join(cwd_rel))
}

#[cfg(unix)]
fn workspace_wrapper(
    paths: &SlotPaths,
    context: &BuildContext,
    checkout: &Path,
) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::symlink;

    let env = std::env::split_paths(&context.host_path)
        .map(|directory| directory.join("env"))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "env executable missing")
        })?;
    let directory = paths.root.join("workspace-wrappers");
    fs::create_dir_all(&directory)?;
    let wrapper = directory.join(super::super::rust::worktree_key(checkout));
    let recreate = match fs::symlink_metadata(&wrapper) {
        Ok(_) => fs::read_link(&wrapper).is_ok_and(|target| target != env),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error),
    };
    if recreate {
        if fs::symlink_metadata(&wrapper).is_ok() {
            fs::remove_file(&wrapper)?;
        }
        match symlink(&env, &wrapper) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Ok(wrapper)
}

#[cfg(not(unix))]
fn workspace_wrapper(
    _paths: &SlotPaths,
    _context: &BuildContext,
    _checkout: &Path,
) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "per-worktree rustc wrapper is not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    use super::super::super::{
        GitBinding, JobKind, Origin, Output, Priority, SnapshotSource, Stack, Step,
    };

    #[cfg(unix)]
    #[test]
    fn target_resolution_shares_dependencies_and_separates_workspace_members_by_worktree() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let first_source = tmp.path().join("first-source");
        let second_source = tmp.path().join("second-source");
        let slot_repo = tmp.path().join("slot-repo");
        let bin = tmp.path().join("bin");
        for source in [&first_source, &second_source] {
            fs::create_dir_all(source.join("nested")).unwrap();
        }
        fs::create_dir_all(slot_repo.join("nested")).unwrap();
        fs::create_dir_all(&bin).unwrap();

        let rustc = bin.join("rustc");
        fs::write(
            &rustc,
            "#!/bin/sh\nprintf 'release: %s\\nhost: %s\\ncommit: fixture\\n' \"$RUSTUP_TOOLCHAIN\" \"${PWD##*/}\"\n",
        )
        .unwrap();
        fs::set_permissions(&rustc, fs::Permissions::from_mode(0o700)).unwrap();
        let env = bin.join("env");
        fs::write(&env, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&env, fs::Permissions::from_mode(0o700)).unwrap();

        let mut port = provenant_config::PortTable::builtin().build;
        port.target = "toolchain,worktree".into();
        let context = RunContext {
            state: tmp.path().join("state"),
            home: tmp.path().join("home"),
            strategy: super::super::super::strategy(&port).value,
            cancel: provenant_core::CancelToken::new(),
        };
        let job_for = |source: &Path| Job {
            id: "fixture".into(),
            project_key: "project".into(),
            project: source.to_path_buf(),
            origin: Origin::Chair,
            owner: "chair".into(),
            kind: JobKind::Intercepted {
                stack: Stack::Rust,
                subcommand: "check".into(),
            },
            priority: Priority::Interactive,
            source: SnapshotSource::WorkingTree {
                worktree: source.to_path_buf(),
                git: GitBinding {
                    git: "git".into(),
                    git_dir: tmp.path().join("git"),
                    common_dir: tmp.path().join("common"),
                    object_dir: None,
                    index_file: tmp.path().join("index"),
                    config_global: None,
                },
            },
            verify_base: None,
            steps: vec![Step {
                cwd_rel: PathBuf::from("nested"),
                argv: ["cargo", "+1.99.1", "check"].map(OsString::from).to_vec(),
            }],
            stop_on_failure: true,
            env: vec![
                ("PATH".into(), bin.as_os_str().into()),
                ("RUSTUP_TOOLCHAIN".into(), "1.100.0".into()),
            ],
            output: Output::Capture { limit_bytes: 1024 },
            timeout: None,
            sandboxed: false,
            prelude: None,
        };
        let paths = SlotPaths {
            repo: slot_repo,
            ..SlotPaths::new(&context.home, "project", 0)
        };

        let first = resolve_target(&job_for(&first_source), &context, &paths);
        let second = resolve_target(&job_for(&second_source), &context, &paths);

        assert_eq!(first.toolchain.as_ref().unwrap().release, "1.99.1");
        assert_eq!(first.toolchain.as_ref().unwrap().host, "nested");
        assert_eq!(
            paths.keyed_target(&first.key),
            paths.keyed_target(&second.key)
        );
        assert_ne!(first.workspace_wrapper, second.workspace_wrapper);
    }
}
