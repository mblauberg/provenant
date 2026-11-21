//! Host-owned Cargo target seed store, shared by publication, cloning and GC.
#[cfg(any(unix, test))]
use crate::cargo_seed::{Refusal, codes};
use provenant_os::fs::{Directory, LockGuard, LockMode};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    /// Hash of the canonical shared Git directory whose verify-base was built.
    pub project_key: String,
    pub rustc_vv: String,
    pub host_triple: String,
    pub commit: String,
    #[serde(default)]
    pub identity: Option<String>,
    pub profile: String,
    pub cargo_profile: provenant_config::CargoBuildProfile,
    pub features: Vec<String>,
    pub flags: Vec<String>,
    /// Allocated bytes of the built target, measured once at publication so a
    /// clone can reserve cache budget without walking the whole seed first.
    /// Older generations leave this unset; a clone then reserves nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_bytes: Option<u64>,
    /// What the generation's target holds. `"deps"` means the seed keeps only
    /// dependency units: every path package of the checkout was cleaned after
    /// the build. An older full generation leaves this unset (or records another
    /// value); it is not current and is collected as obsolete rather than kept.
    #[serde(default)]
    pub contents: String,
}

/// Dependency inputs used to decide whether a host generation can be reused.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SeedIdentity {
    pub schema: u32,
    pub inputs: serde_json::Value,
    pub hash: String,
}

/// File access is supplied by the caller; paths are checkout-relative.
pub trait SeedSource {
    fn list_manifests(&self) -> io::Result<Vec<PathBuf>>;
    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>>;
}

/// Git-backed source for a checkout or a pinned commit tree.
pub struct GitSeedSource<'a> {
    pub checkout: &'a Path,
    pub commit: Option<&'a str>,
}

impl GitSeedSource<'_> {
    fn git(&self, args: &[&str]) -> io::Result<Vec<u8>> {
        crate::git_context::seed_git(self.checkout, args)
    }

    fn tree_file(&self, commit: &str, path: &Path, depth: usize) -> io::Result<Option<Vec<u8>>> {
        if depth > 32 {
            return Err(io::Error::other("seed symlink depth exceeded"));
        }
        let name = path
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF-8 seed path"))?;
        let listing = self.git(&["--literal-pathspecs", "ls-tree", "-z", commit, "--", name])?;
        if listing.is_empty() {
            return Ok(None);
        }
        let bytes = self.git(&["show", &format!("{commit}:{name}")])?;
        if !listing.starts_with(b"120000 ") {
            return Ok(Some(bytes));
        }
        let target = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
        let mut resolved = path.parent().unwrap_or(Path::new("")).to_path_buf();
        for component in Path::new(target).components() {
            match component {
                std::path::Component::Normal(part) => resolved.push(part),
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir if resolved.pop() => {}
                _ => return Err(io::Error::other("seed symlink escapes checkout")),
            }
        }
        self.tree_file(commit, &resolved, depth + 1)
    }
}

impl SeedSource for GitSeedSource<'_> {
    fn list_manifests(&self) -> io::Result<Vec<PathBuf>> {
        let bytes = if let Some(commit) = self.commit {
            self.git(&[
                "--literal-pathspecs",
                "ls-tree",
                "-r",
                "--name-only",
                "-z",
                commit,
                "--",
            ])?
        } else {
            self.git(&["ls-files", "-z", "--"])?
        };
        bytes
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| {
                let name = std::str::from_utf8(p).map_err(io::Error::other)?;
                Ok(PathBuf::from(name))
            })
            .filter(|p: &io::Result<PathBuf>| {
                p.as_ref()
                    .map_or(true, |p| p.file_name().is_some_and(|n| n == "Cargo.toml"))
            })
            .collect()
    }
    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        if let Some(commit) = self.commit {
            self.tree_file(commit, path, 0)
        } else {
            match std::fs::read(self.checkout.join(path)) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
    }
}

/// SHA-256 of compact canonical JSON `{schema: 1, inputs: ...}`. Inputs contain
/// the lock digest, sorted manifest dependency tables and root profiles, Cargo
/// config profiles, rustc text and host triple, `deps`, and the effective build
/// profile. Paths are checkout-relative and object keys are sorted. This can
/// over-invalidate when declarations change without changing units, but does not
/// omit any listed input. Cargo metadata is not run: lanes must identify seeds
/// before Cargo can run under confinement. See state.md "One digest preimage".
pub fn seed_identity(
    source: &impl SeedSource,
    workspace: &Path,
    rustc_vv: &str,
    host_triple: &str,
    cargo_profile: &provenant_config::CargoBuildProfile,
) -> io::Result<SeedIdentity> {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    if workspace.is_absolute()
        || workspace
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(io::Error::other("workspace outside checkout"));
    }
    let workspace: PathBuf = workspace
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let workspace = workspace.as_path();
    let required = |path: &Path| {
        source.read_file(path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("missing {}", path.display()),
            )
        })
    };
    let parse = |bytes: &[u8]| -> io::Result<serde_json::Value> {
        toml_edit::de::from_str(std::str::from_utf8(bytes).map_err(io::Error::other)?)
            .map_err(io::Error::other)
    };
    let lock = required(&workspace.join("Cargo.lock"))?;
    let mut manifests = BTreeMap::new();
    for path in source.list_manifests()? {
        let mut value = parse(&required(&path)?)?;
        let table = value
            .as_object_mut()
            .ok_or_else(|| io::Error::other("invalid manifest"))?;
        table.retain(|k, _| {
            matches!(
                k.as_str(),
                "features"
                    | "dependencies"
                    | "dev-dependencies"
                    | "build-dependencies"
                    | "target"
                    | "workspace"
                    | "patch"
                    | "replace"
            )
        });
        if let Some(w) = table
            .get_mut("workspace")
            .and_then(serde_json::Value::as_object_mut)
        {
            w.retain(|k, _| k == "dependencies");
        }
        if let Some(t) = table
            .get_mut("target")
            .and_then(serde_json::Value::as_object_mut)
        {
            for v in t.values_mut().filter_map(serde_json::Value::as_object_mut) {
                v.retain(|k, _| {
                    matches!(
                        k.as_str(),
                        "dependencies" | "dev-dependencies" | "build-dependencies"
                    )
                });
            }
            t.retain(|_, v| v.as_object().is_none_or(|v| !v.is_empty()));
        }
        manifests.insert(
            path.to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 manifest path"))?
                .to_owned(),
            value,
        );
    }
    let root = parse(&required(&workspace.join("Cargo.toml"))?)?;
    let profiles = root
        .get("profile")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut configs = BTreeMap::new();
    let mut dir = workspace;
    loop {
        let toml_path = dir.join(".cargo/config.toml");
        let legacy_path = dir.join(".cargo/config");
        let (path, bytes) = match source.read_file(&legacy_path)? {
            Some(bytes) => (legacy_path, Some(bytes)),
            None => (toml_path.clone(), source.read_file(&toml_path)?),
        };
        if let Some(bytes) = bytes {
            let config = parse(&bytes)?;
            if let Some(profile) = config.get("profile") {
                configs.insert(path.to_string_lossy().into_owned(), profile.clone());
            }
        }
        if dir.as_os_str().is_empty() {
            break;
        }
        dir = dir
            .parent()
            .ok_or_else(|| io::Error::other("workspace parent unavailable"))?;
    }
    let inputs = serde_json::json!({"lock_sha256": provenant_core::hex(&Sha256::digest(&lock)), "manifests": manifests, "profiles": profiles, "configs": configs, "rustc_vv": rustc_vv, "host_triple": host_triple, "contents": "deps", "cargo_profile": cargo_profile});
    let hash = identity_hash(&inputs)?;
    Ok(SeedIdentity {
        schema: 1,
        hash,
        inputs,
    })
}

fn identity_hash(inputs: &serde_json::Value) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let preimage =
        provenant_records::CanonicalV1::bytes(&serde_json::json!({"schema": 1, "inputs": inputs}))
            .map_err(io::Error::other)?;
    Ok(provenant_core::hex(&Sha256::digest(preimage.as_bytes())))
}

/// Allocated bytes of a seed target, measured at seed-build time. A clone of a
/// multi-gigabyte target cannot afford a second walk of the whole tree before it
/// starts, so the figure is recorded here and read from the manifest instead.
#[cfg(unix)]
pub fn allocated_tree_bytes(path: &Path) -> Option<u64> {
    use std::{fs, os::unix::fs::MetadataExt};
    const MAX_DEPTH: usize = 64;
    fn walk(path: &Path, depth: usize) -> Option<u64> {
        if depth > MAX_DEPTH {
            return None;
        }
        let metadata = fs::symlink_metadata(path).ok()?;
        let mut bytes = metadata.blocks().saturating_mul(512);
        if metadata.file_type().is_dir() {
            for entry in fs::read_dir(path).ok()? {
                bytes = bytes.saturating_add(walk(&entry.ok()?.path(), depth + 1)?);
            }
        }
        Some(bytes)
    }
    walk(path, 0)
}

#[cfg(not(unix))]
pub fn allocated_tree_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Hash a verified repository handle's canonical common Git directory for
/// stable host-seed ownership. Linked checkouts share that directory and key.
/// The definition now lives in config so the host-project configuration layer
/// and target-seed ownership key every checkout identically.
pub use provenant_config::project_key;

/// Select the project's Cargo build directory using the same workspace selector
/// as the verification tiers. Read only data here: lane preparation must never
/// invoke Cargo (or a writer-controlled compiler) outside confinement.
pub fn cargo_build_directory(project: &Path, host_state: &Path) -> io::Result<Option<PathBuf>> {
    let workspace = provenant_config::PortTable::read_for(Some(project), host_state)?
        .verify
        .workspace;
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

/// A lane's desired dependency identity, serviced by the host tick worker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeedRequest {
    pub schema: u32,
    pub project_key: String,
    pub commit: String,
    pub identity: String,
    pub requested_at: provenant_core::WallTime,
    pub attempt: u32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SeedRequestResult {
    pub schema: u32,
    pub request: SeedRequest,
    pub outcome: String,
    pub generation: Option<String>,
}

fn request_dir(state: &Path, key: &str) -> io::Result<PathBuf> {
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::other("invalid seed project key"));
    }
    Ok(store_path(state).join("requests").join(key))
}

pub fn read_request(state: &Path, key: &str) -> io::Result<Option<SeedRequest>> {
    match std::fs::read(request_dir(state, key)?.join("seed-request.json")) {
        Ok(bytes) => {
            let request: SeedRequest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if request.schema != 1 || request.project_key != key {
                return Err(io::Error::other("invalid seed request"));
            }
            Ok(Some(request))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_request_file(
    state: &Path,
    key: &str,
    name: &str,
    value: &impl serde::Serialize,
) -> io::Result<()> {
    let path = request_dir(state, key)?;
    let directory = provenant_config::private_directory(&path, true).map_err(io::Error::other)?;
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    directory
        .atomic_write(
            name.as_ref(),
            "seed-request",
            &bytes,
            provenant_os::fs::PublishMode::Replace,
        )
        .map(|_| ())
        .map_err(io::Error::other)
}

/// Serialize replacement and acknowledgement under the store lock.
pub fn queue_request(state: &Path, request: &SeedRequest) -> io::Result<()> {
    let _lock = lock_store_for_publication(state, 10)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if read_request(state, &request.project_key)?
        .is_none_or(|old| old.requested_at < request.requested_at)
    {
        write_request_file(state, &request.project_key, "seed-request.json", request)?;
    }
    Ok(())
}

pub fn acknowledge_request(
    state: &Path,
    result: &SeedRequestResult,
    retry: bool,
) -> io::Result<()> {
    let _lock = lock_store_for_publication(state, 10)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let request = &result.request;
    write_request_file(state, &request.project_key, "seed-result.json", result)?;
    if let Some(mut current) = read_request(state, &request.project_key)?
        && current.identity == request.identity
        && current.requested_at == request.requested_at
    {
        if retry {
            current.attempt = current.attempt.saturating_add(1);
            write_request_file(state, &request.project_key, "seed-request.json", &current)?;
        } else {
            std::fs::remove_file(
                request_dir(state, &request.project_key)?.join("seed-request.json"),
            )?;
        }
    }
    Ok(())
}

pub fn pending_request_keys(state: &Path) -> io::Result<Vec<String>> {
    let root = store_path(state).join("requests");
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut keys = Vec::new();
    for entry in entries {
        let entry = entry?;
        let key = entry.file_name().to_string_lossy().into_owned();
        if read_request(state, &key).ok().flatten().is_some() {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// Resolve host Cargo tools from the same Cargo home used by lanes, independent
/// of the daemon's PATH. Errors are stable diagnostics and disclose the path
/// searched without consulting a shell or guessing a home directory.
pub fn cargo_tool(environment: &[(OsString, OsString)], tool: &str) -> io::Result<PathBuf> {
    let home = environment
        .iter()
        .find(|(key, _)| key == "HOME")
        .map(|(_, value)| PathBuf::from(value))
        .ok_or_else(|| io::Error::other(format!("{tool} unresolved: HOME not set")))?;
    let cargo_home = provenant_confinement::cargo::CargoHome::resolve(&home, |name| {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    })?;
    let candidate = cargo_home.path.join("bin").join(tool);
    if !std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_file()) {
        return Err(io::Error::other(format!(
            "{tool} unresolved: searched {}",
            candidate.display()
        )));
    }
    // Keep the shim name: rustup selects the invoked proxy from argv[0].
    Ok(candidate)
}

/// Cargo environment settings that change which compiler, target, or untracked
/// build flags a shared seed represents. Values stay in the native Cargo
/// environment; callers use this only to decide whether caching is eligible.
pub fn unsupported_seed_build_environment_key(key: &OsStr) -> Option<&'static str> {
    let key = key.to_str()?;
    match key {
        "RUSTC" => Some("RUSTC"),
        "CARGO_BUILD_RUSTC" => Some("CARGO_BUILD_RUSTC"),
        "RUSTC_WRAPPER" => Some("RUSTC_WRAPPER"),
        "RUSTC_WORKSPACE_WRAPPER" => Some("RUSTC_WORKSPACE_WRAPPER"),
        "CARGO_BUILD_RUSTC_WRAPPER" => Some("CARGO_BUILD_RUSTC_WRAPPER"),
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER" => Some("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"),
        "CARGO_BUILD_TARGET" => Some("CARGO_BUILD_TARGET"),
        "RUSTFLAGS" => Some("RUSTFLAGS"),
        "CARGO_ENCODED_RUSTFLAGS" => Some("CARGO_ENCODED_RUSTFLAGS"),
        "CARGO_BUILD_RUSTFLAGS" => Some("CARGO_BUILD_RUSTFLAGS"),
        "CARGO_PROFILE_DEV_DEBUG" | "CARGO_INCREMENTAL" | "CARGO_TARGET_DIR" => None,
        "RUSTUP_AUTO_INSTALL" => None,
        _ if key.starts_with("RUSTUP_") => Some("RUSTUP_*"),
        _ if key.starts_with("CARGO_TARGET_") => Some("CARGO_TARGET_*"),
        _ if key.starts_with("CARGO_PROFILE_") => Some("CARGO_PROFILE_*"),
        _ => None,
    }
}

/// Find an unsupported build override without exposing any environment value.
pub fn unsupported_seed_build_environment(
    environment: &[(OsString, OsString)],
) -> Option<&'static str> {
    environment.iter().find_map(|(key, value)| {
        let key_text = key.to_str()?;
        if key_text == "RUSTUP_AUTO_INSTALL" && value != "0" {
            return Some("RUSTUP_AUTO_INSTALL");
        }
        unsupported_seed_build_environment_key(key)
    })
}

/// Names of inherited compiler-selection keys a seed build removes from its
/// child environment. Only names are returned, never values; the result is
/// sorted and deduplicated so the build's warning and the tick's seed record
/// cannot disagree. `RUSTUP_AUTO_INSTALL=0` is the exception Provenant sets
/// itself; a different inherited value is an override to remove. Profile keys
/// (`CARGO_INCREMENTAL`, `CARGO_PROFILE_*`, `CARGO_TARGET_DIR`) are not listed:
/// they are set by the caller's Cargo profile and are part of the seed identity.
pub fn seed_build_cleared_keys(environment: &[(OsString, OsString)]) -> Vec<String> {
    let mut keys: Vec<String> = environment
        .iter()
        .filter_map(|(key, value)| {
            let name = key.to_str()?;
            if name == "RUSTUP_AUTO_INSTALL" {
                return (value != "0").then(|| name.to_owned());
            }
            unsupported_seed_build_environment_key(key).map(|_| name.to_owned())
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Apply the same bounded TOML policy to live Cargo config files and config
/// blobs read from a pinned verify-base tree. Input bytes are never retained.
pub fn cargo_config_seed_eligibility(bytes: &[u8]) -> Result<(), &'static str> {
    crate::cargo_seed::cargo_config_seed_eligibility(bytes)
}

fn open_store_directory(state: &Path) -> Result<Directory, Box<dyn std::error::Error>> {
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
    Ok(directory)
}

/// The exclusive lock also protects readers for the duration of their clone.
pub fn lock_store(state: &Path) -> Result<LockGuard, Box<dyn std::error::Error>> {
    let directory = open_store_directory(state)?;
    Ok(directory.try_lock(OsStr::new("store.lock"), LockMode::Exclusive)?)
}

/// Reacquire the store for publication after a build without dropping its
/// completed stage on a short reader or collector lock. The bounded wait is
/// supplied by the caller and is safe while holding the build lock: collectors
/// and refresh probes take the store lock first and only try the build lock
/// without waiting.
pub fn lock_store_for_publication(
    state: &Path,
    timeout_seconds: u64,
) -> Result<LockGuard, Box<dyn std::error::Error>> {
    use provenant_core::{CancelToken, Clock, WaitBudget};
    use std::ffi::OsStr;

    let directory = open_store_directory(state)?;
    let clock = provenant_os::clock::SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), timeout_seconds)?;
    Ok(directory.lock(
        OsStr::new("store.lock"),
        LockMode::Exclusive,
        budget,
        &CancelToken::new(),
        &clock,
    )?)
}

/// Serialises host builds and protects their staging trees from GC.
/// Acquire while holding the store lock when starting a build or collecting.
pub fn lock_build(state: &Path) -> Result<LockGuard, Box<dyn std::error::Error>> {
    let directory = Directory::open_anchor(&store_path(state))?;
    Ok(directory.try_lock(OsStr::new("build.lock"), LockMode::Exclusive)?)
}

pub fn rustc_metadata(worktree: &Path) -> io::Result<(String, String)> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let inherited: Vec<_> = std::env::vars_os().collect();
    let rustc = cargo_tool(&inherited, "rustc")?;
    let argv = [rustc.into_os_string(), "-vV".into()];
    // A seed build is not a lane: wrappers and rustup overrides come from the
    // launcher's shell, not this repository. Probe with the compiler the pinned
    // verify base selects, not the one the caller happened to export.
    let cleared = seed_build_cleared_keys(&inherited);
    let mut env: Vec<_> = inherited
        .into_iter()
        .filter(|(key, _)| !cleared.iter().any(|name| OsStr::new(name) == key))
        .collect();
    env.retain(|(key, _)| key != "RUSTUP_AUTO_INSTALL");
    // The seed-origin marker belongs only to seed-clone prepare children; an
    // ambient value must never reach this verify-base compiler probe.
    env.retain(|(key, _)| key != provenant_core::consumer_env::SEED_ORIGIN);
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
/// A refusal is a stable snake_case code, with what was expected and found when
/// it is a mismatch. It names the path taken and never carries a path or an
/// environment value, so it is safe to record.
#[cfg(test)]
pub(crate) fn lane_rustc_metadata(
    worktree: &Path,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<(String, String), Refusal> {
    lane_rustc_metadata_cancellable(worktree, home, home, environment, None)
}

#[cfg(any(unix, test))]
pub(crate) fn lane_rustc_metadata_cancellable(
    worktree: &Path,
    home: &Path,
    host_state: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    cancel: Option<&provenant_core::CancelToken>,
) -> Result<(String, String), Refusal> {
    let rustup_home = home.join(".rustup");
    let mut saw_home = false;
    for (key, value) in environment {
        if key == "RUSTUP_HOME" {
            if saw_home || Path::new(value) != rustup_home {
                return Err(Refusal::differs(
                    codes::RUSTUP_HOME_UNEXPECTED,
                    "RUSTUP_HOME unset or $HOME/.rustup",
                    if saw_home {
                        "RUSTUP_HOME set more than once"
                    } else {
                        "RUSTUP_HOME set to another directory"
                    },
                ));
            }
            saw_home = true;
        } else if key == "RUSTUP_AUTO_INSTALL" {
            if value != "0" {
                return Err(Refusal::differs(
                    codes::RUSTUP_AUTO_INSTALL_UNEXPECTED,
                    "RUSTUP_AUTO_INSTALL unset or 0",
                    "RUSTUP_AUTO_INSTALL set to another value",
                ));
            }
        } else if let Some(class) = unsupported_seed_build_environment_key(key) {
            return Err(Refusal::differs(
                codes::TOOLCHAIN_ENV_OVERRIDE,
                "no compiler, target or build-flag override",
                format!("{class} is set"),
            ));
        }
    }
    if !saw_home {
        return Err(Refusal::differs(
            codes::RUSTUP_HOME_MISSING,
            "RUSTUP_HOME=$HOME/.rustup",
            "RUSTUP_HOME unset",
        ));
    }
    let channel = lane_release_pin(worktree, home, host_state)?;
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
        let names: Vec<_> = candidates
            .iter()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        return Err(Refusal::differs(
            codes::RELEASE_TOOLCHAIN_AMBIGUOUS_OR_MISSING,
            format!("one installed toolchain named {channel}-<host>"),
            if names.is_empty() {
                "none installed".to_owned()
            } else {
                names.join(", ")
            },
        ));
    }
    let candidate = candidates.pop().ok_or(codes::RELEASE_TOOLCHAIN_MISSING)?;
    if !candidate.file_type().is_ok_and(|kind| kind.is_dir()) {
        return Err(codes::TOOLCHAIN_NOT_DIRECTORY.into());
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
        return Err(codes::COMPILER_ESCAPES_HOST_TOOLCHAIN.into());
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
        return Err(codes::COMPILER_PROBE_FAILED.into());
    }
    let version =
        String::from_utf8(output.stdout.bytes).map_err(|_| codes::COMPILER_PROBE_INVALID)?;
    if !version
        .lines()
        .any(|line| line == format!("release: {channel}"))
    {
        return Err(Refusal::differs(
            codes::COMPILER_PIN_MISMATCH,
            format!("release: {channel}"),
            version
                .lines()
                .find(|line| line.starts_with("release: "))
                .unwrap_or("no release line"),
        ));
    }
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or(codes::COMPILER_HOST_MISSING)?
        .to_owned();
    let expected = format!("{channel}-{host}");
    if candidate.file_name() != std::ffi::OsStr::new(&expected) {
        return Err(Refusal::differs(
            codes::COMPILER_HOST_MISMATCH,
            expected,
            candidate.file_name().to_string_lossy(),
        ));
    }
    Ok((version, host))
}

#[cfg(any(unix, test))]
fn lane_release_pin(worktree: &Path, home: &Path, host_state: &Path) -> Result<String, Refusal> {
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
        return Err(codes::RUSTUP_OVERRIDES_PRESENT.into());
    }
    let build = cargo_build_directory(worktree, host_state)
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
        Ok(_) => return Err(codes::TOOLCHAIN_PIN_ABSENT_OR_AMBIGUOUS.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(codes::TOOLCHAIN_PIN_UNREADABLE.into()),
    }
    match std::fs::symlink_metadata(&modern) {
        Ok(metadata) if !metadata.is_file() => return Err(codes::TOOLCHAIN_PIN_NON_REGULAR.into()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(codes::TOOLCHAIN_PIN_ABSENT_OR_AMBIGUOUS.into());
        }
        Err(_) => return Err(codes::TOOLCHAIN_PIN_UNREADABLE.into()),
    }
    let doc = parse(&modern, codes::TOOLCHAIN_PIN_UNREADABLE)?;
    let toolchain = doc
        .get("toolchain")
        .and_then(|v| v.as_table())
        .ok_or(codes::TOOLCHAIN_PIN_INVALID)?;
    if toolchain.contains_key("path") {
        return Err(codes::TOOLCHAIN_PIN_CUSTOM.into());
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
        return Err(Refusal::differs(
            codes::TOOLCHAIN_NOT_FIXED_RELEASE,
            "a fixed release such as 1.100.0",
            format!("channel {channel}"),
        ));
    }
    Ok(channel)
}

#[cfg(test)]
mod lane_probe_tests {
    use super::*;

    #[derive(Default)]
    struct MemorySource(std::collections::BTreeMap<PathBuf, Vec<u8>>);
    impl SeedSource for MemorySource {
        fn list_manifests(&self) -> io::Result<Vec<PathBuf>> {
            Ok(self
                .0
                .keys()
                .filter(|p| p.file_name().is_some_and(|n| n == "Cargo.toml"))
                .cloned()
                .collect())
        }
        fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
            Ok(self.0.get(path).cloned())
        }
    }

    #[test]
    fn identity_changes_with_each_input() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".cargo")).unwrap();
        fs::write(root.join("Cargo.lock"), "lock one").unwrap();
        let manifest = "[package]\nname = \"sample\"\nversion = \"1.0.0\"\ndescription = \"first\"\n[dependencies]\nexample = \"1\"\n[profile.dev]\nopt-level = 0\n";
        fs::write(root.join("Cargo.toml"), manifest).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[profile.dev]\nopt-level = 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            fs::create_dir(root.join(".hidden")).unwrap();
            fs::write(
                root.join(".hidden/declarations"),
                "[dependencies]\nhidden = '1'\n",
            )
            .unwrap();
            std::os::unix::fs::symlink("declarations", root.join(".hidden/Cargo.toml")).unwrap();
        }
        provenant_os::fixture::run_git(root, &["init", "-q"]).unwrap();
        provenant_os::fixture::run_git(root, &["add", "-A"]).unwrap();
        provenant_os::fixture::run_git(
            root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "inputs",
            ],
        )
        .unwrap();
        let source = GitSeedSource {
            checkout: root,
            commit: None,
        };
        let tree = GitSeedSource {
            checkout: root,
            commit: Some("HEAD"),
        };
        let mut memory = MemorySource::default();
        for path in ["Cargo.toml", "Cargo.lock", ".cargo/config.toml"] {
            memory
                .0
                .insert(path.into(), fs::read(root.join(path)).unwrap());
        }
        #[cfg(unix)]
        memory.0.insert(
            ".hidden/Cargo.toml".into(),
            fs::read(root.join(".hidden/Cargo.toml")).unwrap(),
        );
        let profile = provenant_config::CargoBuildProfile::default();
        let identity =
            |version: &str, host: &str, profile: &provenant_config::CargoBuildProfile| {
                seed_identity(&source, Path::new(""), version, host, profile)
                    .unwrap()
                    .hash
            };
        fs::write(root.join("source.rs"), "source one").unwrap();
        let original = identity("rustc one", "host one", &profile);
        let tree_hash =
            seed_identity(&tree, Path::new(""), "rustc one", "host one", &profile).unwrap();
        let memory_hash =
            seed_identity(&memory, Path::new(""), "rustc one", "host one", &profile).unwrap();
        assert_eq!(original, tree_hash.hash);
        assert_eq!(original, memory_hash.hash);
        assert_eq!(memory_hash.inputs["contents"], "deps");
        let mut other_contents = memory_hash.inputs.clone();
        other_contents["contents"] = serde_json::json!("full");
        assert_ne!(original, identity_hash(&other_contents).unwrap());
        fs::write(root.join("source.rs"), "source two").unwrap();
        assert_eq!(original, identity("rustc one", "host one", &profile));
        fs::write(root.join("Cargo.toml"), manifest.replace("1.0.0", "2.0.0")).unwrap();
        assert_eq!(original, identity("rustc one", "host one", &profile));
        fs::write(root.join("Cargo.toml"), manifest.replace("first", "second")).unwrap();
        assert_eq!(original, identity("rustc one", "host one", &profile));
        fs::write(root.join("Cargo.lock"), "lock two").unwrap();
        assert_ne!(original, identity("rustc one", "host one", &profile));
        fs::write(root.join("Cargo.lock"), "lock one").unwrap();
        fs::write(
            root.join("Cargo.toml"),
            manifest.replace("example = \"1\"", "example = \"2\""),
        )
        .unwrap();
        assert_ne!(original, identity("rustc one", "host one", &profile));
        fs::write(
            root.join("Cargo.toml"),
            manifest.replace("opt-level = 0", "opt-level = 1"),
        )
        .unwrap();
        assert_ne!(original, identity("rustc one", "host one", &profile));
        fs::write(root.join("Cargo.toml"), manifest).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[profile.dev]\nopt-level = 2\n",
        )
        .unwrap();
        assert_ne!(original, identity("rustc one", "host one", &profile));
        fs::write(
            root.join(".cargo/config.toml"),
            "[profile.dev]\nopt-level = 1\n",
        )
        .unwrap();
        fs::remove_file(root.join(".cargo/config.toml")).unwrap();
        fs::write(root.join(".cargo/config"), "[profile.dev]\nopt-level = 1\n").unwrap();
        let legacy = identity("rustc one", "host one", &profile);
        fs::write(root.join(".cargo/config"), "[profile.dev]\nopt-level = 2\n").unwrap();
        assert_ne!(legacy, identity("rustc one", "host one", &profile));
        fs::write(
            root.join(".cargo/config.toml"),
            "[profile.dev]\nopt-level = 1\n",
        )
        .unwrap();
        let preferred = identity("rustc one", "host one", &profile);
        assert_ne!(original, preferred);
        fs::write(
            root.join(".cargo/config.toml"),
            "[profile.dev]\nopt-level = 3\n",
        )
        .unwrap();
        assert_eq!(preferred, identity("rustc one", "host one", &profile));
        fs::remove_file(root.join(".cargo/config")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[profile.dev]\nopt-level = 1\n",
        )
        .unwrap();
        assert_ne!(original, identity("rustc two", "host one", &profile));
        assert_ne!(original, identity("rustc one", "host two", &profile));
        let mut other = profile.clone();
        other.incremental.push('x');
        assert_ne!(original, identity("rustc one", "host one", &other));
        assert!(
            original
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    #[test]
    fn project_key_reexport_matches_config() {
        let path = Path::new("/repo/project/.git");
        assert_eq!(project_key(path), provenant_config::project_key(path));
    }

    #[test]
    fn seed_cargo_resolves_from_fixture_cargo_home_without_path() {
        let root = tempfile::tempdir().unwrap();
        let cargo = root.path().join("custom cargo").join("bin").join("cargo");
        std::fs::create_dir_all(cargo.parent().unwrap()).unwrap();
        std::fs::write(&cargo, "fixture").unwrap();
        let environment = [
            (OsString::from("HOME"), root.path().as_os_str().to_owned()),
            (
                OsString::from("CARGO_HOME"),
                root.path().join("custom cargo").into_os_string(),
            ),
        ];

        assert_eq!(cargo_tool(&environment, "cargo").unwrap(), cargo);
    }

    #[test]
    fn seed_eligibility_rejects_unrecorded_compiler_and_target_overrides() {
        for (key, expected) in [
            ("RUSTC", "RUSTC"),
            ("CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTC"),
            ("RUSTC_WRAPPER", "RUSTC_WRAPPER"),
            ("RUSTC_WORKSPACE_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"),
            ("CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"),
            (
                "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
                "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            ),
            ("CARGO_BUILD_TARGET", "CARGO_BUILD_TARGET"),
            (
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
                "CARGO_TARGET_*",
            ),
            ("RUSTFLAGS", "RUSTFLAGS"),
            ("CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"),
            ("CARGO_BUILD_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS"),
            ("CARGO_PROFILE_TEST_DEBUG", "CARGO_PROFILE_*"),
            ("RUSTUP_TOOLCHAIN", "RUSTUP_*"),
        ] {
            assert_eq!(
                unsupported_seed_build_environment_key(OsStr::new(key)),
                Some(expected),
                "{key}"
            );
        }
        assert_eq!(
            unsupported_seed_build_environment_key(OsStr::new("CARGO_TARGET_DIR")),
            None
        );
        assert_eq!(
            unsupported_seed_build_environment_key(OsStr::new("CARGO_PROFILE_DEV_DEBUG")),
            None
        );
        let env = [
            (OsString::from("RUSTC"), OsString::from("/secret/compiler")),
            (OsString::from("CARGO_INCREMENTAL"), OsString::from("1")),
        ];
        assert_eq!(unsupported_seed_build_environment(&env), Some("RUSTC"));
        assert!(
            !format!("{:?}", unsupported_seed_build_environment(&env)).contains("/secret/compiler")
        );
        assert_eq!(
            unsupported_seed_build_environment(&[(
                OsString::from("RUSTUP_AUTO_INSTALL"),
                OsString::from("1")
            )]),
            Some("RUSTUP_AUTO_INSTALL")
        );
        assert_eq!(
            unsupported_seed_build_environment(&[(
                OsString::from("RUSTUP_AUTO_INSTALL"),
                OsString::from("0")
            )]),
            None
        );
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn cleared_keys_helper_names_only_and_keeps_provenant_set_keys() {
        let env = vec![
            (
                OsString::from("RUSTC_WRAPPER"),
                OsString::from("/secret/wrap"),
            ),
            (
                OsString::from("RUSTUP_TOOLCHAIN"),
                OsString::from("nightly"),
            ),
            (
                OsString::from("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER"),
                OsString::from("secret-ld"),
            ),
            (OsString::from("RUSTFLAGS"), OsString::from("-C secretflag")),
            // Provenant sets these itself; they are not inherited overrides.
            (OsString::from("RUSTUP_AUTO_INSTALL"), OsString::from("0")),
            (OsString::from("CARGO_INCREMENTAL"), OsString::from("0")),
            (
                OsString::from("CARGO_PROFILE_DEV_DEBUG"),
                OsString::from("line-tables-only"),
            ),
            (
                OsString::from("CARGO_TARGET_DIR"),
                OsString::from("/secret/target"),
            ),
            // A repeated key must not be recorded twice.
            (
                OsString::from("RUSTC_WRAPPER"),
                OsString::from("/secret/wrap2"),
            ),
        ];
        assert_eq!(
            seed_build_cleared_keys(&env),
            vec![
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
                "RUSTC_WRAPPER",
                "RUSTFLAGS",
                "RUSTUP_TOOLCHAIN",
            ]
        );
        let rendered = format!("{:?}", seed_build_cleared_keys(&env));
        assert!(
            !rendered.contains("secret"),
            "names only, never values: {rendered}"
        );

        // A non-zero auto-install value is an inherited override and is named;
        // the Provenant-set zero value never is.
        let bad_auto_install = [(OsString::from("RUSTUP_AUTO_INSTALL"), OsString::from("1"))];
        assert_eq!(
            seed_build_cleared_keys(&bad_auto_install),
            vec!["RUSTUP_AUTO_INSTALL"]
        );
        assert!(seed_build_cleared_keys(&[]).is_empty());
    }

    #[test]
    fn seed_cargo_config_predicate_rejects_compiler_and_target_selection() {
        for (config, expected) in [
            (
                "[build]\nrustc = 'secret compiler'\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            (
                "[build]\nrustc-wrapper = 'secret wrapper'\n",
                codes::CARGO_CONFIG_SELECTS_COMPILER,
            ),
            (
                "[build]\ntarget = 'x86_64-unknown-linux-gnu'\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
            ),
            (
                "[target.x86_64-unknown-linux-gnu]\nlinker = 'secret linker'\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
            ),
            (
                "[target.aarch64-apple-darwin]\nrustflags = ['-C', 'link-arg=-s']\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
            ),
            (
                "[env]\nCARGO_BUILD_TARGET = 'x86_64-unknown-linux-gnu'\n",
                codes::CARGO_CONFIG_CHANGES_BUILD_TARGET,
            ),
        ] {
            assert_eq!(
                cargo_config_seed_eligibility(config.as_bytes()),
                Err(expected),
                "{config:?}"
            );
        }
        assert_eq!(
            cargo_config_seed_eligibility(b"[env]\nRUST_MIN_STACK = '8388608'\n"),
            Ok(())
        );
    }

    #[test]
    fn cargo_selection_uses_project_configuration_and_skips_non_cargo() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            cargo_build_directory(root.path(), root.path())
                .unwrap()
                .is_none()
        );
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(
            cargo_build_directory(root.path(), root.path()).unwrap(),
            Some(root.path().canonicalize().unwrap())
        );
        let nested = root.path().join("any/depth/workspace");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        let mut ports = provenant_config::PortTable::builtin();
        ports.verify.workspace = PathBuf::from("any/depth/workspace");
        std::fs::write(
            root.path().join(provenant_config::PORTS_CONFIG),
            serde_json::to_vec(&ports).unwrap(),
        )
        .unwrap();
        assert_eq!(
            cargo_build_directory(root.path(), root.path()).unwrap(),
            Some(nested.canonicalize().unwrap())
        );
        std::fs::remove_file(nested.join("Cargo.toml")).unwrap();
        assert!(
            cargo_build_directory(root.path(), root.path())
                .unwrap()
                .is_none()
        );
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
        assert_eq!(lane_release_pin(&worktree, &home, &home).unwrap(), "1.99.1");
        let plain = front.join("rust-toolchain");
        std::fs::remove_file(&modern).unwrap();
        std::fs::write(&plain, "1.99.1\n").unwrap();
        assert!(lane_release_pin(&worktree, &home, &home).is_err());
        std::fs::write(&modern, "[toolchain]\nchannel = '1.99.1'\n").unwrap();
        assert!(lane_release_pin(&worktree, &home, &home).is_err());
        std::fs::remove_file(&plain).unwrap();
        std::fs::write(&settings, "[overrides]\n'/writer' = 'custom'\n").unwrap();
        assert!(lane_release_pin(&worktree, &home, &home).is_err());
        std::fs::write(&settings, "[overrides]\n").unwrap();
        std::fs::write(
            &modern,
            "[toolchain]\nchannel = '1.99.1'\npath = '/writer/evil'\n",
        )
        .unwrap();
        let env = [("RUSTUP_HOME".into(), home.join(".rustup").into_os_string())];
        assert!(lane_rustc_metadata(&worktree, &home, &env).is_err());
        std::fs::remove_file(&modern).unwrap();
        assert!(lane_release_pin(&worktree, &home, &home).is_err());
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
        for key in [
            "RUSTC",
            "CARGO_BUILD_RUSTC",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_TARGET",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_BUILD_RUSTFLAGS",
            "CARGO_PROFILE_TEST_DEBUG",
            "RUSTUP_TOOLCHAIN",
        ] {
            let mut overridden = env.to_vec();
            overridden.push((key.into(), "custom override".into()));
            assert!(
                lane_rustc_metadata(&worktree, &home, &overridden).is_err(),
                "{key}"
            );
        }
    }

    /// A refusal that is a mismatch says what was expected and what was found,
    /// without a path or an environment value.
    #[test]
    fn mismatched_toolchains_name_expected_and_found() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let worktree = root.path().join("writer");
        let bin = home.join(".rustup/toolchains/1.99.1-test-host/bin");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(worktree.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(home.join(".rustup/settings.toml"), "[overrides]\n").unwrap();
        let pin = worktree.join("rust-toolchain.toml");
        std::fs::write(&pin, "[toolchain]\nchannel = '1.99.1'\n").unwrap();
        let rustc = bin.join("rustc");
        // The directory is named for the pin but its compiler is another release.
        std::fs::write(
            &rustc,
            "#!/bin/sh\nprintf 'rustc 1.98.0\\nhost: test-host\\nrelease: 1.98.0\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = [("RUSTUP_HOME".into(), home.join(".rustup").into_os_string())];
        let refusal =
            |env: &[(OsString, OsString)]| lane_rustc_metadata(&worktree, &home, env).unwrap_err();
        assert_eq!(
            refusal(&env),
            Refusal::differs(
                codes::COMPILER_PIN_MISMATCH,
                "release: 1.99.1",
                "release: 1.98.0"
            )
        );
        std::fs::write(&pin, "[toolchain]\nchannel = '1.97.0'\n").unwrap();
        assert_eq!(
            refusal(&env),
            Refusal::differs(
                codes::RELEASE_TOOLCHAIN_AMBIGUOUS_OR_MISSING,
                "one installed toolchain named 1.97.0-<host>",
                "none installed"
            )
        );
        std::fs::write(&pin, "[toolchain]\nchannel = 'stable'\n").unwrap();
        assert_eq!(
            refusal(&env),
            Refusal::differs(
                codes::TOOLCHAIN_NOT_FIXED_RELEASE,
                "a fixed release such as 1.100.0",
                "channel stable"
            )
        );
        std::fs::write(&pin, "[toolchain]\nchannel = '1.99.1'\n").unwrap();
        let mut overridden = env.to_vec();
        overridden.push(("RUSTFLAGS".into(), "-C secret".into()));
        let refused = refusal(&overridden);
        assert_eq!(refused.code, codes::TOOLCHAIN_ENV_OVERRIDE);
        let named = format!("{:?}", refused.differs);
        assert!(named.contains("RUSTFLAGS is set") && !named.contains("secret"));
        assert_eq!(
            refusal(&[]),
            Refusal::differs(
                codes::RUSTUP_HOME_MISSING,
                "RUSTUP_HOME=$HOME/.rustup",
                "RUSTUP_HOME unset"
            )
        );
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;

    fn request(at: i64, identity: &str) -> SeedRequest {
        SeedRequest {
            schema: 1,
            project_key: "abcd".into(),
            commit: "1234".into(),
            identity: identity.into(),
            requested_at: provenant_core::WallTime::from_micros(at).unwrap(),
            attempt: 0,
        }
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn newer_request_replaces_older() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let old = request(1, "old");
        let new = request(2, "new");
        queue_request(&state, &new).unwrap();
        queue_request(&state, &old).unwrap();
        assert_eq!(read_request(&state, "abcd").unwrap(), Some(new));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn result_acknowledges_request() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let old = request(1, "old");
        let new = request(2, "new");
        queue_request(&state, &old).unwrap();
        queue_request(&state, &new).unwrap();
        let result = SeedRequestResult {
            schema: 1,
            request: old,
            outcome: "built".into(),
            generation: Some("generation".into()),
        };
        acknowledge_request(&state, &result, false).unwrap();
        assert_eq!(read_request(&state, "abcd").unwrap(), Some(new.clone()));
        let saved: SeedRequestResult = serde_json::from_slice(
            &std::fs::read(
                request_dir(&state, "abcd")
                    .unwrap()
                    .join("seed-result.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(saved.request, result.request);
        let result = SeedRequestResult {
            request: new,
            ..result
        };
        acknowledge_request(&state, &result, true).unwrap();
        assert_eq!(read_request(&state, "abcd").unwrap().unwrap().attempt, 1);
        acknowledge_request(&state, &result, false).unwrap();
        assert!(read_request(&state, "abcd").unwrap().is_none());
    }
}
