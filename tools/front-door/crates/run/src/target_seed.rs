//! Host-owned Cargo target seed store, shared by publication, cloning and GC.
#[cfg(any(unix, test))]
use crate::cargo_seed::codes;
use provenant_os::fs::{Directory, LockGuard, LockMode};
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub rustc_vv: String,
    pub host_triple: String,
    pub commit: String,
    pub profile: String,
    pub features: Vec<String>,
    pub flags: Vec<String>,
}

/// Select the project's Cargo build directory using the same workspace selector
/// as the verification tiers. Read only data here: lane preparation must never
/// invoke Cargo (or a writer-controlled compiler) outside confinement.
pub fn cargo_build_directory(project: &Path) -> io::Result<Option<PathBuf>> {
    #[derive(serde::Deserialize)]
    struct Selection {
        #[serde(default = "default_workspace")]
        workspace: PathBuf,
    }
    fn default_workspace() -> PathBuf {
        PathBuf::from(".")
    }
    let selection = project.join(".provenant/verify.json");
    let workspace = match std::fs::read(&selection) {
        Ok(bytes) => {
            serde_json::from_slice::<Selection>(&bytes)
                .map_err(io::Error::other)?
                .workspace
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => default_workspace(),
        Err(error) => return Err(error),
    };
    if workspace.is_absolute()
        || workspace.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(io::Error::other("Cargo workspace must be project-relative"));
    }
    let directory = project.join(workspace);
    match std::fs::metadata(directory.join("Cargo.toml")) {
        Ok(metadata) if metadata.is_file() => {
            let directory = directory.canonicalize()?;
            if !directory.starts_with(project.canonicalize()?) {
                return Err(io::Error::other("Cargo workspace escapes the project"));
            }
            Ok(Some(directory))
        }
        Ok(_) => Err(io::Error::other("Cargo manifest is not a file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn store_path(state: &Path) -> PathBuf {
    state.join("cargo-target-seeds")
}

/// The exclusive lock also protects readers for the duration of their clone.
pub fn lock_store(state: &Path) -> Result<LockGuard, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(state)?;
    let root = Directory::open_anchor(state)?;
    let name = OsStr::new("cargo-target-seeds");
    if let Err(error) = root.mkdir(name, 0o700)
        && !error.is_exists()
    {
        return Err(error.into());
    }
    let directory = root.open_verified_directory(name)?;
    directory.make_private()?;
    let generations = OsStr::new("generations");
    if let Err(error) = directory.mkdir(generations, 0o700)
        && !error.is_exists()
    {
        return Err(error.into());
    }
    directory
        .open_verified_directory(generations)?
        .make_private()?;
    Ok(directory.try_lock(OsStr::new("store.lock"), LockMode::Exclusive)?)
}

/// Serialises host builds and protects their staging trees from GC.
/// Acquire while holding the store lock when starting a build or collecting.
pub fn lock_build(state: &Path) -> Result<LockGuard, Box<dyn std::error::Error>> {
    let directory = Directory::open_anchor(&store_path(state))?;
    Ok(directory.try_lock(OsStr::new("build.lock"), LockMode::Exclusive)?)
}

pub fn rustc_metadata(worktree: &Path) -> io::Result<(String, String)> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let argv = ["rustc".into(), "-vV".into()];
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| key != "RUSTUP_AUTO_INSTALL")
        .collect();
    env.push(("RUSTUP_AUTO_INSTALL".into(), "0".into()));
    let mut options = ExecOptions::new(&argv, worktree, std::time::Duration::from_secs(30));
    options.env = Some(&env);
    let output = SystemExecutor::new(1)
        .and_then(|executor| executor.execute(options))
        .map_err(io::Error::other)?;
    if output.termination != Termination::Exit(0)
        || output.timed_out
        || output.stdout.truncated
        || output.stdout.read_failed
    {
        return Err(io::Error::other("rustc -vV failed"));
    }
    let version = String::from_utf8(output.stdout.bytes).map_err(io::Error::other)?;
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| io::Error::other("rustc -vV has no host triple"))?
        .to_owned();
    Ok((version, host))
}

/// Inspect the lane's selected build directory's pin as data, then run only the
/// corresponding host-owned compiler directly, outside the writer tree.
/// Rustup overrides and non-release/custom toolchains are deliberately refused.
/// A refusal is a stable snake_case code: it names the path taken and never
/// carries an environment value, so it is safe to record.
#[cfg(test)]
pub(crate) fn lane_rustc_metadata(
    worktree: &Path,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<(String, String), &'static str> {
    lane_rustc_metadata_cancellable(worktree, home, environment, None)
}

#[cfg(any(unix, test))]
pub(crate) fn lane_rustc_metadata_cancellable(
    worktree: &Path,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    cancel: Option<&provenant_core::CancelToken>,
) -> Result<(String, String), &'static str> {
    let rustup_home = home.join(".rustup");
    let mut saw_home = false;
    for (key, value) in environment {
        let key = key.to_string_lossy();
        if key == "RUSTUP_HOME" {
            if saw_home || Path::new(value) != rustup_home {
                return Err(codes::RUSTUP_HOME_UNEXPECTED);
            }
            saw_home = true;
        } else if key == "RUSTUP_AUTO_INSTALL" {
            if value != "0" {
                return Err(codes::RUSTUP_AUTO_INSTALL_UNEXPECTED);
            }
        } else if key.starts_with("RUSTUP_") || key == "RUSTC" || key == "CARGO_BUILD_RUSTC" {
            return Err(codes::TOOLCHAIN_ENV_OVERRIDE);
        }
    }
    if !saw_home {
        return Err(codes::RUSTUP_HOME_MISSING);
    }
    let channel = lane_release_pin(worktree, home)?;
    let toolchains = home.join(".rustup/toolchains");
    let mut candidates = std::fs::read_dir(&toolchains)
        .map_err(|_| codes::TOOLCHAIN_DIR_UNREADABLE)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{channel}-"))
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(codes::RELEASE_TOOLCHAIN_AMBIGUOUS_OR_MISSING);
    }
    let candidate = candidates.pop().ok_or(codes::RELEASE_TOOLCHAIN_MISSING)?;
    if !candidate.file_type().is_ok_and(|kind| kind.is_dir()) {
        return Err(codes::TOOLCHAIN_NOT_DIRECTORY);
    }
    let rustc = candidate.path().join("bin/rustc");
    let resolved = rustc
        .canonicalize()
        .map_err(|_| codes::COMPILER_UNRESOLVABLE)?;
    let candidate_path = candidate
        .path()
        .canonicalize()
        .map_err(|_| codes::TOOLCHAIN_UNRESOLVABLE)?;
    let worktree_path = worktree
        .canonicalize()
        .map_err(|_| codes::WORKTREE_UNRESOLVABLE)?;
    if !resolved.starts_with(&candidate_path)
        || candidate_path.starts_with(&worktree_path)
        || worktree_path.starts_with(&candidate_path)
    {
        return Err(codes::COMPILER_ESCAPES_HOST_TOOLCHAIN);
    }
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let argv = [resolved.into_os_string(), "-vV".into()];
    let env = [("HOME".into(), home.as_os_str().into())];
    let mut options = ExecOptions::new(&argv, home, std::time::Duration::from_secs(2));
    options.cancel = cancel;
    options.process_group = true;
    options.env = Some(&env);
    let output = SystemExecutor::new(1)
        .and_then(|e| e.execute(options))
        .map_err(|_| codes::COMPILER_PROBE_FAILED)?;
    if output.termination != Termination::Exit(0)
        || output.timed_out
        || output.stdout.truncated
        || output.stdout.read_failed
    {
        return Err(codes::COMPILER_PROBE_FAILED);
    }
    let version =
        String::from_utf8(output.stdout.bytes).map_err(|_| codes::COMPILER_PROBE_INVALID)?;
    if !version
        .lines()
        .any(|line| line == format!("release: {channel}"))
    {
        return Err(codes::COMPILER_PIN_MISMATCH);
    }
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or(codes::COMPILER_HOST_MISSING)?
        .to_owned();
    if candidate.file_name() != std::ffi::OsStr::new(&format!("{channel}-{host}")) {
        return Err(codes::COMPILER_HOST_MISMATCH);
    }
    Ok((version, host))
}

#[cfg(any(unix, test))]
fn lane_release_pin(worktree: &Path, home: &Path) -> Result<String, &'static str> {
    fn read(path: &Path, unreadable: &'static str) -> Result<String, &'static str> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| unreadable)?
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| unreadable)?;
        if bytes.len() > 16384 {
            return Err(codes::TOOLCHAIN_CONFIG_TOO_LARGE);
        }
        String::from_utf8(bytes).map_err(|_| codes::TOOLCHAIN_CONFIG_INVALID)
    }
    fn parse(
        path: &Path,
        unreadable: &'static str,
    ) -> Result<toml_edit::DocumentMut, &'static str> {
        read(path, unreadable)?
            .parse()
            .map_err(|_| codes::TOOLCHAIN_CONFIG_INVALID)
    }
    let settings = parse(
        &home.join(".rustup/settings.toml"),
        "rustup_settings_unreadable",
    )?;
    if settings
        .get("overrides")
        .and_then(|v| v.as_table())
        .is_none_or(|t| !t.is_empty())
    {
        return Err(codes::RUSTUP_OVERRIDES_PRESENT);
    }
    let build = cargo_build_directory(worktree)
        .map_err(|_| codes::CARGO_WORKSPACE_UNREADABLE)?
        .ok_or(codes::CARGO_WORKSPACE_MISSING)?;
    // Rustup chooses the nearest pin, including pins above the build directory.
    let dir = build
        .ancestors()
        .find(|directory| {
            ["rust-toolchain", "rust-toolchain.toml"]
                .iter()
                .any(|name| {
                    !matches!(std::fs::symlink_metadata(directory.join(name)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound)
                })
        })
        .ok_or(codes::TOOLCHAIN_PIN_ABSENT_OR_AMBIGUOUS)?;
    let modern = dir.join("rust-toolchain.toml");
    // A second pin could select a different compiler, so do not seed in its presence.
    match std::fs::symlink_metadata(dir.join("rust-toolchain")) {
        Ok(_) => return Err(codes::TOOLCHAIN_PIN_ABSENT_OR_AMBIGUOUS),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(codes::TOOLCHAIN_PIN_UNREADABLE),
    }
    match std::fs::symlink_metadata(&modern) {
        Ok(metadata) if !metadata.is_file() => return Err(codes::TOOLCHAIN_PIN_NON_REGULAR),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(codes::TOOLCHAIN_PIN_ABSENT_OR_AMBIGUOUS);
        }
        Err(_) => return Err(codes::TOOLCHAIN_PIN_UNREADABLE),
    }
    let doc = parse(&modern, codes::TOOLCHAIN_PIN_UNREADABLE)?;
    let toolchain = doc
        .get("toolchain")
        .and_then(|v| v.as_table())
        .ok_or(codes::TOOLCHAIN_PIN_INVALID)?;
    if toolchain.contains_key("path") {
        return Err(codes::TOOLCHAIN_PIN_CUSTOM);
    }
    let channel = toolchain
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or(codes::TOOLCHAIN_PIN_CHANNEL_MISSING)?
        .to_owned();
    let parts: Vec<_> = channel.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(codes::TOOLCHAIN_NOT_FIXED_RELEASE);
    }
    Ok(channel)
}

#[cfg(test)]
mod lane_probe_tests {
    use super::*;

    #[test]
    fn cargo_selection_uses_project_configuration_and_skips_non_cargo() {
        let root = tempfile::tempdir().unwrap();
        assert!(cargo_build_directory(root.path()).unwrap().is_none());
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(
            cargo_build_directory(root.path()).unwrap(),
            Some(root.path().canonicalize().unwrap())
        );
        let nested = root.path().join("any/depth/workspace");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        std::fs::write(
            root.path().join(".provenant/verify.json"),
            r#"{"workspace":"any/depth/workspace"}"#,
        )
        .unwrap();
        assert_eq!(
            cargo_build_directory(root.path()).unwrap(),
            Some(nested.canonicalize().unwrap())
        );
        std::fs::remove_file(nested.join("Cargo.toml")).unwrap();
        assert!(cargo_build_directory(root.path()).unwrap().is_none());
    }

    #[test]
    fn accepts_only_explicit_fixed_pins_without_overrides() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let worktree = root.path().join("writer");
        let front = worktree.clone();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        std::fs::create_dir_all(&front).unwrap();
        std::fs::write(front.join("Cargo.toml"), "[workspace]\n").unwrap();
        let settings = home.join(".rustup/settings.toml");
        std::fs::write(&settings, "[overrides]\n").unwrap();
        let modern = front.join("rust-toolchain.toml");
        std::fs::write(&modern, "[toolchain]\nchannel = '1.99.1'\n").unwrap();
        assert_eq!(lane_release_pin(&worktree, &home).unwrap(), "1.99.1");
        let plain = front.join("rust-toolchain");
        std::fs::remove_file(&modern).unwrap();
        std::fs::write(&plain, "1.99.1\n").unwrap();
        assert!(lane_release_pin(&worktree, &home).is_err());
        std::fs::write(&modern, "[toolchain]\nchannel = '1.99.1'\n").unwrap();
        assert!(lane_release_pin(&worktree, &home).is_err());
        std::fs::remove_file(&plain).unwrap();
        std::fs::write(&settings, "[overrides]\n'/writer' = 'custom'\n").unwrap();
        assert!(lane_release_pin(&worktree, &home).is_err());
        std::fs::write(&settings, "[overrides]\n").unwrap();
        std::fs::write(
            &modern,
            "[toolchain]\nchannel = '1.99.1'\npath = '/writer/evil'\n",
        )
        .unwrap();
        let env = [("RUSTUP_HOME".into(), home.join(".rustup").into_os_string())];
        assert!(lane_rustc_metadata(&worktree, &home, &env).is_err());
        std::fs::remove_file(&modern).unwrap();
        assert!(lane_release_pin(&worktree, &home).is_err());
        std::fs::write(&modern, "[toolchain]\nchannel = 'nightly'\n").unwrap();
        assert!(lane_rustc_metadata(&worktree, &home, &env).is_err());
    }

    #[test]
    fn never_executes_a_compiler_reachable_from_the_writer() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let writer = root.path().join("writer");
        let front = writer.clone();
        let rustup = writer.join("evil-rustup");
        let bin = rustup.join("toolchains/1.99.1-test-host/bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&front).unwrap();
        std::fs::write(front.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            front.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = '1.99.1'\n",
        )
        .unwrap();
        std::fs::write(rustup.join("settings.toml"), "[overrides]\n").unwrap();
        let marker = root.path().join("executed");
        let executable = bin.join("rustc");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&rustup, home.join(".rustup")).unwrap();
        let env = [("RUSTUP_HOME".into(), home.join(".rustup").into_os_string())];
        assert!(lane_rustc_metadata(&writer, &home, &env).is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn probe_uses_lane_environment_and_ignores_host_override() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let worktree = root.path().join("writer");
        let front = worktree.clone();
        let bin = home.join(".rustup/toolchains/1.99.1-test-host/bin");
        std::fs::create_dir_all(&front).unwrap();
        std::fs::write(front.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(home.join(".rustup/settings.toml"), "[overrides]\n").unwrap();
        std::fs::write(
            front.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = '1.99.1'\n",
        )
        .unwrap();
        let rustc = bin.join("rustc");
        std::fs::write(
            &rustc,
            "#!/bin/sh\nprintf 'rustc 1.99.1\\nhost: test-host\\nrelease: 1.99.1\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = [("RUSTUP_HOME".into(), home.join(".rustup").into_os_string())];
        assert_eq!(
            lane_rustc_metadata(&worktree, &home, &env).unwrap().1,
            "test-host"
        );
        let mut overridden = env.to_vec();
        overridden.push(("RUSTUP_TOOLCHAIN".into(), "nightly".into()));
        assert!(lane_rustc_metadata(&worktree, &home, &overridden).is_err());
    }
}
