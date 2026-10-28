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
