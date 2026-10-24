//! Prepared launches: exact argv, environment allow-list, stdin, cwd and the
//! fixture-backed confinement profile. Nothing here executes a process.
use crate::{Continuation, Prompt, ProviderError, StreamState};
use provenant_confinement::ConfinedPlan;
use provenant_confinement::permissions::{
    AccessMode, FixtureOperation, FixtureOutcome, PathGrant, PathScope, ProviderProfile,
};
use provenant_records::ResolvedRoute;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
    /// Canonical path of the provider CLI recorded by preflight.
    pub executable: &'a Path,
    /// The user's home directory.
    pub home: &'a Path,
    /// The user's login name.
    pub user: &'a OsStr,
    /// Claude's login token, read by the host outside the sandbox.
    /// Selected registered identity. Absent preserves the default login source.
    pub account: Option<&'a crate::identity::RegisteredAccount>,
    pub token: Option<&'a crate::claude::OauthToken>,
    /// Working directory: the worktree for writers, a read root otherwise.
    pub cwd: &'a Path,
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

#[derive(Deserialize)]
struct Matrix {
    execution_modes: Vec<Mode>,
}
#[derive(Deserialize)]
struct Mode {
    adapter: String,
    access_mode: Access,
    session_mode: Session,
    enabled: bool,
    #[serde(default)]
    shell_enabled: bool,
}

const ACTIVATION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/providers/activation.json"
));

fn modes() -> &'static [Mode] {
    static MODES: OnceLock<Vec<Mode>> = OnceLock::new();
    // A malformed matrix enables nothing; the registry test pins its shape.
    MODES.get_or_init(|| {
        serde_json::from_str::<Matrix>(ACTIVATION)
            .map(|matrix| matrix.execution_modes)
            .unwrap_or_default()
    })
}

/// Activation is the recorded matrix, not configuration or cache state.
pub fn mode_enabled(adapter: &str, access: Access, session: Session) -> bool {
    modes().iter().any(|mode| {
        mode.adapter == adapter
            && mode.access_mode == access
            && mode.session_mode == session
            && mode.enabled
    })
}

/// Shell activation needs its own writer proof. The recorder alone can
/// prepare the candidate before that evidence has been accepted.
pub(crate) fn shell_enabled(adapter: &str, access: Access, session: Session) -> bool {
    access == Access::WorktreeWrite
        && (cfg!(feature = "recording")
            || modes().iter().any(|mode| {
                mode.adapter == adapter
                    && mode.access_mode == access
                    && mode.session_mode == session
                    && mode.enabled
                    && mode.shell_enabled
            }))
}

pub(crate) fn any_mode_enabled(adapter: &str) -> bool {
    modes()
        .iter()
        .any(|mode| mode.adapter == adapter && mode.enabled)
}

#[derive(Deserialize)]
struct Observations {
    read_only: Option<BTreeMap<FixtureOperation, FixtureOutcome>>,
    worktree_write: Option<BTreeMap<FixtureOperation, FixtureOutcome>>,
}

/// Recorded confinement observations for one access mode. The recorder
/// (feature `recording`) supplies its own claims and verifies them by probe.
pub(crate) fn observations(
    text: &str,
    access: Access,
) -> Result<BTreeMap<FixtureOperation, FixtureOutcome>, ProviderError> {
    let parsed =
        serde_json::from_str::<Observations>(text)
            .ok()
            .and_then(|recorded| match access {
                Access::ReadOnly => recorded.read_only,
                Access::WorktreeWrite => recorded.worktree_write,
            });
    match parsed {
        Some(map) => Ok(map),
        None if cfg!(feature = "recording") => Ok(BTreeMap::new()),
        None => Err(ProviderError::Malformed),
    }
}

/// Checks shared by every adapter, returning the gated mode.
pub(crate) fn admit(
    adapter: &'static str,
    input: &PrepareInput<'_>,
) -> Result<(Access, Session), ProviderError> {
    if input.route.adapter != adapter {
        return Err(ProviderError::IdentityMismatch);
    }
    if let Some(account) = input.account {
        account.validate_route(input.route)?;
    } else if matches!(adapter, "claude" | "codex") && input.route.account_key.is_some() {
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
    if !cfg!(feature = "recording") && !mode_enabled(adapter, access, session) {
        return Err(ProviderError::Disabled { adapter });
    }
    Ok((access, session))
}

/// Minimal environment: the user's identity, a fixed search path and the
/// attempt's scratch as temporary directory. Callers append provider keys.
pub(crate) fn base_env(
    input: &PrepareInput<'_>,
    scratch: &Path,
    path: &str,
) -> Vec<(OsString, OsString)> {
    vec![
        ("HOME".into(), input.home.into()),
        ("USER".into(), input.user.into()),
        ("PATH".into(), path.into()),
        ("TMPDIR".into(), scratch.into()),
        ("LANG".into(), "en_US.UTF-8".into()),
    ]
}

/// The CLI's own install directory: the parent of a `bin/` directory, or the
/// executable's directory. A shared prefix (`/opt/homebrew`, `/usr/local`,
/// `/usr`) is refused: its `etc/` and `var/` must never become readable.
pub(crate) fn package_root(executable: &Path) -> Result<&Path, ProviderError> {
    let package = match executable.parent() {
        Some(bin) if bin.file_name() == Some("bin".as_ref()) => bin.parent(),
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
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Adapters without a recorded launch form refuse even if the matrix drifts.
pub(crate) fn unrecorded(
    _: &PrepareInput<'_>,
    _: Access,
    _: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    Err(ProviderError::Disabled {
        adapter: stream.adapter(),
    })
}
