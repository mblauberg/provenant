//! Prepared launches: exact argv, environment allow-list, stdin, cwd and the
//! confinement profile. Nothing here executes a process.
use crate::{Continuation, Prompt, ProviderError, StreamState};
use provenant_confinement::ConfinedPlan;
use provenant_confinement::permissions::{AccessMode, PathGrant, PathScope, ProviderProfile};
use provenant_records::ResolvedRoute;
use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// What the launcher feeds the child on stdin.
#[derive(Debug, PartialEq, Eq)]
pub enum Stdin {
    /// Write these bytes, then close.
    Bytes(Vec<u8>),
    /// Connect `/dev/null`.
    Null,
}

/// Filesystem preparation the launcher performs before anchoring and
/// compiling, all inside roots the plan or profile already names.
#[derive(Debug, PartialEq, Eq)]
pub enum SetupStep {
    /// Create this directory and any missing parents by descriptor-relative
    /// traversal from `/`, opening each component without following links,
    /// so a swapped entry fails instead of redirecting creation. Only
    /// directories created here get mode 0700; existing ones are never
    /// chmodded.
    CreateDir(PathBuf),
    /// Create a mode-0600 file with these bytes, refusing an existing entry.
    /// The launcher writes and syncs it before anchoring the profile.
    WriteFile { path: PathBuf, bytes: Vec<u8> },
    /// Like `WriteFile`, then mark the file owner-executable.
    WriteExecutable { path: PathBuf, bytes: Vec<u8> },
    /// Create `link` as a symbolic link to `target`, replacing nothing.
    Symlink { link: PathBuf, target: PathBuf },
}

/// A provider launch ready for confinement. The launcher performs `setup`,
/// anchors `required_anchors(plan, &profile, worktree)`, compiles the plan with
/// `profile`, and runs `ConfinedLaunch::command(&argv)` with exactly `env`.
pub struct LaunchSpec {
    /// `argv[0]` is the canonical executable path.
    pub argv: Vec<OsString>,
    /// The complete child environment; nothing is inherited.
    pub env: Vec<(OsString, OsString)>,
    pub stdin: Stdin,
    /// Credential bytes delivered through an inherited pipe, never the environment.
    pub secret_input: Option<provenant_os::exec::SecretInput>,
    pub cwd: PathBuf,
    pub setup: Vec<SetupStep>,
    pub profile: ProviderProfile,
    /// Decoder state bound to any continuation identity.
    pub stream: StreamState,
}

/// Frozen confinement policy and launch facts, compiled before launch by the
/// composition edge. Paths must be canonical: providers never resolve them.
pub struct PrepareInput<'a> {
    pub route: &'a ResolvedRoute,
    pub confined: &'a ConfinedPlan,
    pub prompt: &'a Prompt,
    pub continuation: Option<&'a Continuation>,
    pub grant_digest: &'a str,
    /// Canonical path of the provider CLI resolved at dispatch.
    pub executable: &'a Path,
    /// The user's home directory.
    pub home: &'a Path,
    /// The user's login name.
    pub user: &'a OsStr,
    /// Reads one host environment variable. Production passes
    /// `std::env::var_os`; tests pass a fixed lookup so the profiles do not
    /// depend on the runner.
    pub host_var: &'a dyn Fn(&str) -> Option<OsString>,
    /// Claude's login token, read by the host outside the sandbox.
    /// Selected registered identity. Absent preserves the default login source.
    pub account: Option<&'a crate::identity::RegisteredAccount>,
    pub token: Option<&'a crate::claude::OauthToken>,
    /// Working directory: the worktree for writers, a read root otherwise.
    pub cwd: &'a Path,
    /// Whether this lane received a captured Git context in its prompt.
    pub git_context: bool,
    /// Caller-supplied randomness; Claude uses it as a fresh session id.
    pub nonce: [u8; 16],
    pub provenant: crate::LaneRuntime<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    ReadOnly,
    WorktreeWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Session {
    Fresh,
    Resume,
}

/// Checks shared by every adapter, returning its access and session mode.
pub(crate) fn admit(
    adapter: &'static str,
    input: &PrepareInput<'_>,
) -> Result<(Access, Session), ProviderError> {
    if input.route.adapter != adapter {
        return Err(ProviderError::IdentityMismatch);
    }
    if let Some(account) = input.account {
        account.validate_route(input.route)?;
    } else if provenant_records::AdapterSpec::get(adapter)
        .is_some_and(|spec| spec.registered_account)
        && input.route.account_key.is_some()
    {
        return Err(ProviderError::UnknownIdentity);
    }
    if let Some(continuation) = input.continuation {
        continuation.validate(input.route, input.grant_digest)?;
    }
    for path in [input.executable, input.home, input.cwd] {
        if !path.is_absolute() {
            return Err(ProviderError::InvalidPath);
        }
    }
    let access = match input.confined.permissions().mode {
        AccessMode::ReadOnly => Access::ReadOnly,
        AccessMode::WorktreeWrite(_) => Access::WorktreeWrite,
    };
    if access == Access::ReadOnly
        && !input
            .confined
            .permissions()
            .read_roots
            .iter()
            .any(|root| input.cwd.starts_with(&root.path))
    {
        return Err(ProviderError::InvalidPath);
    }
    let session = if input.continuation.is_some() {
        Session::Resume
    } else {
        Session::Fresh
    };
    Ok((access, session))
}

/// Minimal environment: the user's identity, a fixed search path and TMPDIR
/// under the attempt's private tmp directory. Callers append provider keys.
pub(crate) fn base_env(
    input: &PrepareInput<'_>,
    scratch: &Path,
    path: &str,
) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("HOME".into(), input.home.into()),
        ("USER".into(), input.user.into()),
        ("PATH".into(), path.into()),
        ("TMPDIR".into(), scratch.into()),
        ("LANG".into(), "en_US.UTF-8".into()),
    ];
    env.extend(provenant_os::sys::git_identity(
        input.provenant.git,
        input.cwd,
    ));
    env
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod git_identity_tests {
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn reads_effective_identity_and_omits_missing_values() {
        let dir = tempfile::tempdir().unwrap();
        let git = Path::new("/usr/bin/git");
        assert!(
            std::process::Command::new(git)
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        for (key, value) in [
            ("user.name", "Lane O'Brien"),
            ("user.email", "lane@example.org"),
        ] {
            assert!(
                std::process::Command::new(git)
                    .args(["config", "--local", key, value])
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let env = provenant_os::sys::git_identity(git, dir.path());
        for (key, value) in [
            ("GIT_AUTHOR_NAME", "Lane O'Brien"),
            ("GIT_COMMITTER_NAME", "Lane O'Brien"),
            ("GIT_AUTHOR_EMAIL", "lane@example.org"),
            ("GIT_COMMITTER_EMAIL", "lane@example.org"),
        ] {
            assert!(env.iter().any(|(k, v)| k == key && v == value));
        }
        assert!(
            provenant_os::sys::git_identity(Path::new("/nonexistent/git"), dir.path()).is_empty()
        );
    }
}

/// The lane's private, writable cache root: the writer's lane cache
/// (`Library/Caches/provenant-lane-cargo/<sha256>`) or a read-only lane's
/// scratch. Package-manager caches live here, never in shared host stores.
pub(crate) fn lane_cache<'a>(input: &'a PrepareInput<'_>) -> &'a Path {
    match &input.confined.permissions().mode {
        provenant_confinement::permissions::AccessMode::ReadOnly => {
            &input.confined.permissions().scratch.path
        }
        provenant_confinement::permissions::AccessMode::WorktreeWrite(authority) => {
            &authority.cargo_cache().path
        }
    }
}

/// Rust tools for every shell-capable lane. The user's `~/.cargo` is read-only
/// (its `bin` proxies and the rustup toolchains stay executable); the host
/// later builds from its registry and Git checkouts unconfined, so no lane
/// may write them. Cargo gets a lane-private `CARGO_HOME` in the lane cache
/// and downloads crates into its own registry. A missing toolchain fails
/// clearly instead of installing. Build output stays in the lane's own cache.
/// The compiler hard-denies Cargo credentials.
pub(crate) fn rust_tools(
    input: &PrepareInput<'_>,
    access: Access,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    env: &mut Vec<(OsString, OsString)>,
    setup: &mut Vec<SetupStep>,
) {
    let cargo = input.home.join(".cargo");
    let rustup = input.home.join(".rustup");
    let cache = lane_cache(input);
    let target = cache.join("target");
    // The host prepares the writer cache itself under custody. Cargo
    // creates target/ after launch, so anchor the cache.
    tool_exec_roots.push(subtree(if access == Access::ReadOnly {
        &target
    } else {
        cache
    }));
    runtime_reads.extend([subtree(&cargo), subtree(&rustup)]);
    runtime_reads.extend(git_global_files(&input.home.join(".config")));
    tool_exec_roots.extend([
        subtree(cargo.join("bin")),
        subtree(rustup.join("toolchains")),
    ]);
    let cargo_home = cache.join("cargo-home");
    if access == Access::ReadOnly {
        // Offline builds from the already-extracted shared sources. The
        // links point at stores the lane cannot write, so it cannot alter
        // them; Cargo's lock files and downloads need writes, so it cannot
        // fetch either.
        setup.push(SetupStep::CreateDir(cargo_home.clone()));
        setup.extend(["registry", "git"].map(|name| SetupStep::Symlink {
            link: cargo_home.join(name),
            target: cargo.join(name),
        }));
    }
    env.extend([
        ("CARGO_HOME".into(), cargo_home.into_os_string()),
        ("RUSTUP_HOME".into(), rustup.into_os_string()),
        ("RUSTUP_AUTO_INSTALL".into(), "0".into()),
        ("CARGO_TARGET_DIR".into(), target.clone().into_os_string()),
        // Override build.build-dir inherited from host or ancestor Cargo config.
        (
            "CARGO_BUILD_BUILD_DIR".into(),
            target.clone().into_os_string(),
        ),
    ]);
    if access == Access::WorktreeWrite {
        // Lint-only runs set both CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR to
        // this sibling. Cargo locks the build directory, so they then do not
        // queue behind test and build runs in the lane's own target/.
        env.push((
            "PROVENANT_LANE_CARGO_LINT_TARGET".into(),
            cache.join("target-lint").into_os_string(),
        ));
    }
    if access == Access::ReadOnly {
        // The lane cannot fetch, so Cargo fails fast offline, and its cache
        // clean-up never touches the shared stores behind the links.
        env.extend([
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY".into(), "never".into()),
        ]);
    }
    system_tools(access, runtime_reads, tool_exec_roots, env);
    // Homebrew resolves bin/git into its versioned keg. Its remote helpers
    // live beside bin/, independently of whether Node enables Homebrew tools.
    // Grant only the selected Git's helper directory, and only to writers.
    if access == Access::WorktreeWrite
        && let Some(prefix) = input.provenant.git.parent().and_then(Path::parent)
    {
        let core = prefix.join("libexec/git-core");
        if core.is_dir() {
            runtime_reads.push(subtree(&core));
            tool_exec_roots.push(subtree(core));
        }
    }
    if access == Access::ReadOnly {
        setup.push(SetupStep::CreateDir(target));
    }
    // Repository scripts (a Cargo rustc wrapper, test runners) run directly.
    // Children inherit this profile, so exec stays confined; read and write
    // grants and the protected and credential denials are unchanged.
    tool_exec_roots.push(subtree(input.cwd));
}

/// Public TLS files and macOS developer tools, independent of whether Rust
/// is installed. Keep these grants shared by Codex and Claude.
#[allow(unused_variables)] // The grants are macOS-specific.
pub(crate) fn system_tools(
    access: Access,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    env: &mut Vec<(OsString, OsString)>,
) {
    // Cargo's system LibreSSL reads its public default configuration
    // (openssl.cnf) and the TLS trust roots (cert.pem, certs/) to download
    // crates. Read-only; the whole public directory holds no secrets.
    #[cfg(target_os = "macos")]
    runtime_reads.push(subtree("/private/etc/ssl"));
    // Rust's system linker also goes through xcrun. Select the real CLT
    // installation when present, rather than opening the selected Xcode app.
    #[cfg(target_os = "macos")]
    {
        let developer = Path::new("/Library/Developer/CommandLineTools");
        if developer.is_dir() {
            runtime_reads.push(subtree(developer));
            tool_exec_roots.push(subtree(developer.join("usr/bin")));
            // `/usr/bin/python3` runs the tools' python3, a symlink into this
            // framework, and Seatbelt checks the resolved path.
            let python = developer.join("Library/Frameworks/Python3.framework");
            if python.is_dir() {
                tool_exec_roots.push(subtree(python));
            }
            if access == Access::WorktreeWrite {
                tool_exec_roots.push(subtree(developer.join("usr/libexec/git-core")));
            }
            env.extend([
                ("DEVELOPER_DIR".into(), developer.into()),
                ("SDKROOT".into(), developer.join("SDKs/MacOSX.sdk").into()),
            ]);
        }
    }
}

/// Git's default XDG global ignore and attributes files, read-only. Cargo's
/// built-in Git library lists files of path dependencies outside the
/// workspace and fails hard when it cannot read them. They hold no
/// credentials. Granted only when present, as a grant needs an existing path.
pub(crate) fn git_global_files(config_home: &Path) -> Vec<PathGrant> {
    ["git/ignore", "git/attributes"]
        .map(|name| config_home.join(name))
        .into_iter()
        .filter(|path| path.is_file())
        .map(exact)
        .collect()
}

/// A Homebrew Node toolchain (node, npm, pnpm, corepack) for every
/// shell-capable lane. The host's pnpm home and store, npm cache and Corepack
/// cache are never writable: the host later runs what they hold unconfined.
/// Writers redirect each into the lane-private cache, so installs download
/// there. Read-only lanes get a scratch npm and pnpm cache, and read and exec
/// (no write) on the host's existing Corepack cache so pnpm runs.
pub(crate) fn node_tools(
    home: &Path,
    access: Access,
    cache: &Path,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    env: &mut Vec<(OsString, OsString)>,
) {
    for prefix in ["/opt/homebrew", "/usr/local"].map(Path::new) {
        if prefix.join("bin/node").exists() {
            homebrew_tools(prefix, runtime_reads, tool_exec_roots, env);
            let host_corepack = home.join(".cache/node/corepack");
            let corepack = if access == Access::ReadOnly && host_corepack.is_dir() {
                runtime_reads.push(subtree(&host_corepack));
                tool_exec_roots.push(subtree(&host_corepack));
                host_corepack
            } else {
                cache.join("corepack")
            };
            // pnpm 12 reads the pnpm_config_* names; older pnpm and npm read
            // npm_config_*. Set both so every version stays in the lane cache.
            let store = cache.join("pnpm-store");
            let pnpm_cache = cache.join("pnpm-cache");
            env.extend([
                ("npm_config_cache".into(), cache.join("npm").into()),
                ("PNPM_HOME".into(), cache.join("pnpm-home").into()),
                ("npm_config_store_dir".into(), store.clone().into()),
                ("pnpm_config_store_dir".into(), store.into()),
                ("npm_config_cache_dir".into(), pnpm_cache.clone().into()),
                ("pnpm_config_cache_dir".into(), pnpm_cache.into()),
                ("COREPACK_HOME".into(), corepack.into()),
            ]);
            // Run the installed pnpm whatever version a project pins. Switching
            // downloads `@pnpm/exe`, whose launcher stays a non-executable
            // placeholder when hard links are denied, so running it fails
            // with ENOEXEC.
            for name in ["manage_package_manager_versions", "package_manager_strict"] {
                for prefix in ["npm", "pnpm"] {
                    env.push((format!("{prefix}_config_{name}").into(), "false".into()));
                }
            }
            return;
        }
    }
}

/// The prefix is readable, including `etc/` (OpenSSL configuration and CA
/// certificates), and its programs executable. `var/` stays unreadable: it
/// holds service data such as Postgres clusters. Nothing in it is writable.
pub(crate) fn homebrew_tools(
    prefix: &Path,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    env: &mut [(OsString, OsString)],
) {
    let present = |names: &[&str]| -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| prefix.join(name))
            .filter(|directory| directory.is_dir())
            .collect()
    };
    runtime_reads.extend(
        present(&[
            "bin",
            "sbin",
            "Cellar",
            "opt",
            "lib",
            "libexec",
            "etc",
            "share",
            "include",
            "Frameworks",
        ])
        .into_iter()
        .map(subtree),
    );
    tool_exec_roots.extend(
        present(&["bin", "sbin", "Cellar", "opt", "lib", "libexec"])
            .into_iter()
            .map(subtree),
    );
    if let Some((_, path)) = env.iter_mut().find(|(key, _)| key == "PATH") {
        path.push(":");
        path.push(prefix.join("bin"));
    }
}

/// The CLI's own install directory: the parent of a `bin/` directory, or the
/// executable's directory. A `bin/` directly under a hidden home tool tree
/// (`~/.cargo/bin`, `~/.local/bin`) grants only that `bin/`, never the tree
/// (`credentials.toml`, `share/`). A shared prefix (`/opt/homebrew`, `/usr/local`,
/// `/usr`) is refused: its `var/` must never become readable.
pub(crate) fn package_root(executable: &Path) -> Result<&Path, ProviderError> {
    let package = match executable.parent() {
        Some(bin) if bin.file_name() == Some("bin".as_ref()) => {
            let tree = bin.parent();
            let hidden = tree
                .and_then(Path::file_name)
                .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."));
            if hidden { Some(bin) } else { tree }
        }
        parent => parent,
    }
    .ok_or(ProviderError::InvalidPath)?;
    let shared = [
        "/",
        "/opt",
        "/opt/homebrew",
        "/usr",
        "/usr/local",
        "/bin",
        "/usr/bin",
    ];
    if shared.iter().any(|prefix| package == Path::new(prefix)) {
        return Err(ProviderError::InvalidPath);
    }
    Ok(package)
}

pub(crate) fn exact(path: impl Into<PathBuf>) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope: PathScope::Exact,
    }
}

pub(crate) fn subtree(path: impl Into<PathBuf>) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope: PathScope::Subtree,
    }
}

/// Lowercase RFC 4122 version 4 spelling of caller randomness.
pub(crate) fn uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = provenant_core::hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
