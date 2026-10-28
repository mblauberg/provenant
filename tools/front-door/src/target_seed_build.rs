//! Build from a pinned remote-tracking commit in a disposable host checkout.
use provenant_core::Clock;
use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use provenant_run::target_seed::{self, Manifest};
use std::{
    error::Error,
    ffi::OsString,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = make_directories_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn make_directories_writable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
        )?;
        for entry in fs::read_dir(path)? {
            make_directories_writable(&entry?.path())?;
        }
    }
    Ok(())
}

fn run(cwd: &Path, argv: &[OsString], overrides: &[(OsString, OsString)]) -> Result<Vec<u8>> {
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            !key.to_string_lossy().starts_with("GIT_")
                && !overrides.iter().any(|(name, _)| name == key)
        })
        .collect();
    env.extend_from_slice(overrides);
    let mut options = ExecOptions::new(argv, cwd, Duration::from_secs(4 * 60 * 60));
    options.env = Some(&env);
    options.process_group = true;
    let result = SystemExecutor::new(1)?.execute(options)?;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.stdout.truncated
        || result.stdout.read_failed
    {
        return Err(io::Error::other(format!(
            "command failed: {argv:?}: {}",
            String::from_utf8_lossy(&result.stderr.bytes)
        ))
        .into());
    }
    Ok(result.stdout.bytes)
}
fn git(cwd: &Path, args: &[OsString]) -> Result<Vec<u8>> {
    let executable =
        provenant_os::git::git_executable().ok_or_else(|| io::Error::other("git unavailable"))?;
    let mut argv: Vec<OsString> = vec![
        executable.into_os_string(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
    ];
    argv.extend_from_slice(args);
    run(
        cwd,
        &argv,
        &[
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ],
    )
}

pub fn build(state: &Path, project: &Path) -> Result<String> {
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() {
        return Err(io::Error::other("target seeds can only be built on the host").into());
    }
    let output = git(
        project,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "refs/remotes/origin/main^{commit}".into(),
        ],
    )?;
    let commit = String::from_utf8(output)?.trim().to_owned();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::other("invalid origin/main commit").into());
    }
    publish(state, |stage| {
        let checkout = stage.join("checkout");
        git(
            project,
            &[
                "clone".into(),
                "--no-local".into(),
                "--no-hardlinks".into(),
                "--no-checkout".into(),
                "--".into(),
                project.as_os_str().into(),
                checkout.as_os_str().into(),
            ],
        )?;
        git(
            &checkout,
            &["checkout".into(), "--detach".into(), commit.clone().into()],
        )?;
        let worktree = checkout.join("tools/front-door");
        let target = stage.join("target");
        fs::create_dir(&target)?;
        let (rustc_vv, host_triple) = target_seed::rustc_metadata(&worktree)?;
        let mut flags = vec![
            "cargo build --locked --all-targets".to_owned(),
            "cargo test --locked --no-run".to_owned(),
        ];
        flags.extend(
            [
                "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "CARGO_BUILD_RUSTFLAGS",
                "CARGO_BUILD_TARGET",
            ]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|v| format!("{key}={v}"))),
        );
        for args in [
            &["build", "--locked", "--all-targets"][..],
            &["test", "--locked", "--no-run"][..],
        ] {
            let argv: Vec<OsString> = std::iter::once(OsString::from("cargo"))
                .chain(args.iter().map(OsString::from))
                .collect();
            run(
                &worktree,
                &argv,
                &[
                    ("CARGO_TARGET_DIR".into(), target.as_os_str().into()),
                    ("CARGO_BUILD_BUILD_DIR".into(), target.as_os_str().into()),
                ],
            )?;
        }
        fs::remove_dir_all(checkout)?;
        Ok(Manifest {
            rustc_vv,
            host_triple,
            commit,
            profile: "dev".into(),
            features: vec!["default".into()],
            flags,
        })
    })
}

fn publish(state: &Path, build: impl FnOnce(&Path) -> Result<Manifest>) -> Result<String> {
    drop(target_seed::lock_store(state)?);
    let store = target_seed::store_path(state);
    let random = provenant_os::fs::temporary_name()?;
    let stage = Staging(store.join(format!(".staging-{}", random.to_string_lossy())));
    fs::create_dir(&stage.0)?;
    let manifest = build(&stage.0)?;
    fs::write(
        stage.0.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    make_read_only(&stage.0.join("target"))
        .map_err(|error| io::Error::new(error.kind(), format!("freeze generation: {error}")))?;
    make_read_only(&stage.0.join("manifest.json"))?;
    let id = format!(
        "{:020}-{}",
        provenant_os::clock::SystemClock.wall_now()?.as_micros(),
        random.to_string_lossy()
    );
    let _lock = target_seed::lock_store(state)
        .map_err(|error| io::Error::other(format!("lock publication: {error}")))?;
    fs::rename(&stage.0, store.join("generations").join(&id))
        .map_err(|error| io::Error::new(error.kind(), format!("publish generation: {error}")))?;
    let generation = store.join("generations").join(&id);
    if let Err(error) = make_read_only(&generation) {
        let _ = make_directories_writable(&generation);
        let _ = fs::remove_dir_all(&generation);
        return Err(error.into());
    }
    Ok(id)
}

fn make_read_only(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            make_read_only(&entry?.path())?;
        }
    }
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(metadata.permissions().mode() & !0o222),
    )
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    // This module is also included by a harness-free integration target.
    use super::*;
    #[test]
    fn failure_never_publishes() {
        let state = tempfile::tempdir().unwrap();
        assert!(
            publish(state.path(), |_| Err(
                io::Error::other("build failed").into()
            ))
            .is_err()
        );
        assert_eq!(
            fs::read_dir(target_seed::store_path(state.path()).join("generations"))
                .unwrap()
                .count(),
            0
        );
    }
    #[test]
    fn success_publishes_read_only_generation() {
        let state = tempfile::tempdir().unwrap();
        let id = publish(state.path(), |stage| {
            fs::create_dir(stage.join("target"))?;
            fs::write(stage.join("target/artifact"), b"built")?;
            Ok(Manifest {
                rustc_vv: "version".into(),
                host_triple: "host".into(),
                commit: "commit".into(),
                profile: "dev".into(),
                features: vec![],
                flags: vec![],
            })
        })
        .unwrap();
        let generation = target_seed::store_path(state.path())
            .join("generations")
            .join(id);
        for path in [
            generation.clone(),
            generation.join("manifest.json"),
            generation.join("target/artifact"),
        ] {
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o222, 0);
        }
        make_directories_writable(&generation).unwrap();
    }
}
