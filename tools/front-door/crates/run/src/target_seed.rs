//! Host-owned Cargo target seed store, shared by publication, cloning and GC.
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

/// Inspect the lane's explicit front-door pin as data, then run only the
/// corresponding host-owned compiler directly, outside the writer tree.
/// Rustup overrides and non-release/custom toolchains are deliberately refused.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn lane_rustc_metadata(
    worktree: &Path,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> io::Result<(String, String)> {
    let rustup_home = home.join(".rustup");
    let mut saw_home = false;
    for (key, value) in environment {
        let key = key.to_string_lossy();
        if key == "RUSTUP_HOME" {
            if saw_home || Path::new(value) != rustup_home {
                return Err(io::Error::other("unexpected rustup home"));
            }
            saw_home = true;
        } else if key == "RUSTUP_AUTO_INSTALL" {
            if value != "0" {
                return Err(io::Error::other("unexpected rustup environment"));
            }
        } else if key.starts_with("RUSTUP_") || key == "RUSTC" || key == "CARGO_BUILD_RUSTC" {
            return Err(io::Error::other("toolchain environment is not verifiable"));
        }
    }
    if !saw_home {
        return Err(io::Error::other("missing rustup home"));
    }
    let channel = lane_release_pin(worktree, home)?;
    let toolchains = home.join(".rustup/toolchains");
    let mut candidates = std::fs::read_dir(&toolchains)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{channel}-"))
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(io::Error::other(
            "release toolchain is ambiguous or unavailable",
        ));
    }
    let candidate = candidates
        .pop()
        .ok_or_else(|| io::Error::other("missing release toolchain"))?;
    if !candidate.file_type()?.is_dir() {
        return Err(io::Error::other("toolchain is not a directory"));
    }
    let rustc = candidate.path().join("bin/rustc");
    let resolved = rustc.canonicalize()?;
    let candidate_path = candidate.path().canonicalize()?;
    let worktree_path = worktree.canonicalize()?;
    if !resolved.starts_with(&candidate_path)
        || candidate_path.starts_with(&worktree_path)
        || worktree_path.starts_with(&candidate_path)
    {
        return Err(io::Error::other("compiler escapes host toolchain"));
    }
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let argv = [resolved.into_os_string(), "-vV".into()];
    let env = [("HOME".into(), home.as_os_str().into())];
    let mut options = ExecOptions::new(&argv, home, std::time::Duration::from_secs(30));
    options.env = Some(&env);
    let output = SystemExecutor::new(1)
        .and_then(|e| e.execute(options))
        .map_err(io::Error::other)?;
    if output.termination != Termination::Exit(0)
        || output.timed_out
        || output.stdout.truncated
        || output.stdout.read_failed
    {
        return Err(io::Error::other("trusted compiler probe failed"));
    }
    let version = String::from_utf8(output.stdout.bytes).map_err(io::Error::other)?;
    if !version
        .lines()
        .any(|line| line == format!("release: {channel}"))
    {
        return Err(io::Error::other("compiler does not match pin"));
    }
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| io::Error::other("compiler has no host triple"))?
        .to_owned();
    if candidate.file_name() != std::ffi::OsStr::new(&format!("{channel}-{host}")) {
        return Err(io::Error::other(
            "compiler host does not match installation",
        ));
    }
    Ok((version, host))
}

#[cfg(any(target_os = "macos", test))]
fn lane_release_pin(worktree: &Path, home: &Path) -> io::Result<String> {
    fn read(path: &Path) -> io::Result<String> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(16385)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16384 {
            return Err(io::Error::other("toolchain configuration too large"));
        }
        String::from_utf8(bytes).map_err(io::Error::other)
    }
    fn parse(path: &Path) -> io::Result<toml_edit::DocumentMut> {
        read(path)?.parse().map_err(io::Error::other)
    }
    let settings = parse(&home.join(".rustup/settings.toml"))?;
    if settings
        .get("overrides")
        .and_then(|v| v.as_table())
        .is_none_or(|t| !t.is_empty())
    {
        return Err(io::Error::other("rustup overrides cannot be verified"));
    }
    let dir = worktree.join("tools/front-door");
    let modern = dir.join("rust-toolchain.toml");
    let legacy = dir.join("rust-toolchain");
    let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
    for path in [&modern, &legacy] {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(io::Error::other("non-regular toolchain pin"));
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    let channel = match (regular(&modern), regular(&legacy)) {
        (true, false) => {
            let doc = parse(&modern)?;
            let toolchain = doc
                .get("toolchain")
                .and_then(|v| v.as_table())
                .ok_or_else(|| io::Error::other("invalid pin"))?;
            if toolchain.contains_key("path") {
                return Err(io::Error::other("custom toolchain"));
            }
            toolchain
                .get("channel")
                .and_then(|v| v.as_str())
                .ok_or_else(|| io::Error::other("missing channel"))?
                .to_owned()
        }
        (false, true) => read(&legacy)?.trim().to_owned(),
        _ => return Err(io::Error::other("toolchain pin missing or ambiguous")),
    };
    let parts: Vec<_> = channel.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(io::Error::other("toolchain is not a fixed release"));
    }
    Ok(channel)
}

#[cfg(test)]
mod lane_probe_tests {
    use super::*;

    #[test]
    fn accepts_only_explicit_fixed_pins_without_overrides() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let worktree = root.path().join("writer");
        let front = worktree.join("tools/front-door");
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        std::fs::create_dir_all(&front).unwrap();
        let settings = home.join(".rustup/settings.toml");
        std::fs::write(&settings, "[overrides]\n").unwrap();
        let modern = front.join("rust-toolchain.toml");
        std::fs::write(&modern, "[toolchain]\nchannel = '1.99.1'\n").unwrap();
        assert_eq!(lane_release_pin(&worktree, &home).unwrap(), "1.99.1");
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
        std::fs::write(front.join("rust-toolchain"), "1.99.1\n").unwrap();
        assert_eq!(lane_release_pin(&worktree, &home).unwrap(), "1.99.1");
        std::fs::write(front.join("rust-toolchain"), "nightly\n").unwrap();
        assert!(lane_rustc_metadata(&worktree, &home, &env).is_err());
    }

    #[test]
    fn never_executes_a_compiler_reachable_from_the_writer() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let writer = root.path().join("writer");
        let front = writer.join("tools/front-door");
        let rustup = writer.join("evil-rustup");
        let bin = rustup.join("toolchains/1.99.1-test-host/bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&front).unwrap();
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
        let front = worktree.join("tools/front-door");
        let bin = home.join(".rustup/toolchains/1.99.1-test-host/bin");
        std::fs::create_dir_all(&front).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(home.join(".rustup/settings.toml"), "[overrides]\n").unwrap();
        std::fs::write(front.join("rust-toolchain"), "1.99.1\n").unwrap();
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
