//! Managed files use OS capabilities exclusively. Plans retain bounded
//! snapshots; every publication rechecks identity and bytes under owner locks.
use crate::inspection::InspectionEvidence;
use crate::release::{FILE_LIMIT, Release};
use provenant_core::{CancelToken, Clock, RelativePath, Roots, WaitBudget};
use provenant_os::fs::{
    Directory, FileKind, FsError, LockMode, Metadata, Publication, PublishMode,
};
use provenant_records::{
    AbsolutePath, CanonicalV1, FileDigest, Harness, InstallReceipt, Limits, OwnedEntry, OwnedValue,
    PathWire, ResolvedHarnessTarget, VersionInfo,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

const CLAUDE_CONFIG_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error(transparent)]
    Decode(#[from] provenant_records::DecodeError),
    #[error(transparent)]
    Encode(#[from] provenant_records::EncodeError),
    #[error(transparent)]
    PathWire(#[from] provenant_records::PathWireError),
    #[error("invalid absolute installation path: {0}")]
    Path(PathBuf),
    #[error("invalid release manifest: {0}")]
    Manifest(&'static str),
    #[error("release platform does not match selected target")]
    Platform,
    #[error(
        "release conflict: existing manifest {existing_manifest_digest}, requested manifest {requested_manifest_digest}; {recovery}"
    )]
    ReleaseConflict {
        existing_manifest_digest: FileDigest,
        requested_manifest_digest: FileDigest,
        recovery: &'static str,
    },
    #[error("release file bytes or mode differ: {0}")]
    Tampered(PathBuf),
    #[error("unmanaged destination conflicts: {0}")]
    Conflict(PathBuf),
    #[error(
        "peer-agent plist conflicts: {0}; move it aside, run launchctl bootout gui/$UID/dev.provenant.peer, then rerun install"
    )]
    PeerAgentConflict(PathBuf),
    #[error("destination changed since preview: {0}")]
    Drift(PathBuf),
    #[error("select at least one harness explicitly, without duplicates")]
    HarnessSelection,
    #[error("hook registration refused: no fixture-backed HookSpec is available")]
    HooksUnavailable,
    #[error("invalid installation receipt: {0}; reinstall Provenant")]
    ReceiptDecode(#[source] provenant_records::DecodeError),
    #[error(transparent)]
    Config(#[from] crate::ConfigError),
    #[error("invalid harness configuration: {0}")]
    InvalidConfig(&'static str),
    #[error(
        "symlinked or non-regular harness config refused: {0}; replace the link with a regular copy, then rerun install"
    )]
    ConfigFile(PathBuf),
    #[error("preview output failed")]
    Preview(#[source] std::io::Error),
    #[error("release smoke check failed")]
    Smoke(#[source] std::io::Error),
    #[error("installation partially applied; resolve the failure and re-apply install")]
    Partial {
        completed: Vec<Action>,
        remaining: Vec<Action>,
        #[source]
        source: Box<InstallError>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    File { bytes: Vec<u8>, mode: u64 },
    Link(PathBuf),
    Other,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub(crate) value: Value,
    pub(crate) metadata: Metadata,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Create,
    Replace,
    Remove,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    pub path: AbsolutePath,
    pub disposition: Disposition,
    pub selector: String,
    pub before: Option<OwnedValue>,
    pub after: Option<OwnedValue>,
    pub identity: Option<EntryIdentity>,
    pub edits: Vec<SelectorChange>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectorChange {
    pub selector: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryIdentity {
    pub device: u64,
    pub inode: u64,
}
fn identity(snapshot: &Option<Snapshot>) -> Option<EntryIdentity> {
    snapshot.as_ref().map(|s| EntryIdentity {
        device: s.metadata.device,
        inode: s.metadata.inode,
    })
}
#[derive(Debug, Clone)]
pub struct HarnessTarget {
    pub harness: Harness,
    pub skills_dir: PathBuf,
    pub mcp_file: PathBuf,
    pub hook_file: Option<PathBuf>,
    pub instruction: Option<PathBuf>,
}
/// Fixture-backed registration supplied by the CLI composition root.
/// `None` harness denotes Git, outside the managed harness configuration.
#[derive(Debug, Clone, Copy)]
pub struct HookRegistration {
    pub harness: Option<Harness>,
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub timeout: Option<u64>,
    pub command: &'static [&'static str],
}
#[derive(Debug, Clone)]
pub struct Targets {
    pub roots: Roots,
    pub home: PathBuf,
    pub bin_dir: PathBuf,
    pub harnesses: Vec<HarnessTarget>,
    pub adopt_existing: bool,
    pub hooks: bool,
    pub remove_hooks: bool,
    /// Rendered per-user LaunchAgent, supplied only by the process edge.
    pub peer_agent: Option<Vec<u8>>,
    /// Rendered scheduled cache collector, supplied only by the process edge.
    pub gc_agent: Option<Vec<u8>>,
    /// Exact recognised historical rendering, for migration from pre-receipt agents.
    pub peer_agent_previous: Option<Vec<u8>>,
    pub hook_specs: Vec<HookRegistration>,
    pub protected_roots: Vec<PathBuf>,
    pub(crate) configured_homes: BTreeSet<Harness>,
    pub(crate) configured_configs: BTreeSet<Harness>,
    /// Harnesses whose home came from `CLAUDE_CONFIG_DIR`/`CODEX_HOME`.
    /// Uninstall limits these to the matching recorded target.
    pub(crate) environment_homes: BTreeSet<Harness>,
}
/// One receipt may record several targets of a harness, such as one per
/// `CLAUDE_CONFIG_DIR`. A target is its harness plus skills directory; every
/// harness entry belongs to the recorded target whose paths permit it.
pub(crate) type TargetId = (Harness, PathBuf);
impl HarnessTarget {
    pub(crate) fn id(&self) -> TargetId {
        (self.harness, self.skills_dir.clone())
    }
}
fn recorded_id(target: &ResolvedHarnessTarget) -> Result<TargetId, InstallError> {
    Ok((target.harness, target.skills_dir.to_path_buf()?))
}
impl Targets {
    /// Defaults from captured HOME. Composition applies explicit config/flag
    /// paths once before snapshotting; no environment is read in this crate.
    pub fn new(roots: Roots, home: &Path, harnesses: &[Harness]) -> Self {
        let targets = harnesses
            .iter()
            .map(|h| {
                let (base, instruction) = match h {
                    Harness::Claude => (".claude", Some("CLAUDE.md")),
                    Harness::Codex => (".codex", Some("AGENTS.md")),
                    Harness::Cursor => (".cursor", None),
                    Harness::Agy => (".gemini", Some("GEMINI.md")),
                    Harness::Opencode => (".config/opencode", Some("AGENTS.md")),
                };
                let base = home.join(base);
                HarnessTarget {
                    harness: *h,
                    mcp_file: crate::harness::default_mcp(*h, home, &base),
                    hook_file: crate::harness::default_hooks(*h, &base),
                    skills_dir: base.join("skills"),
                    instruction: instruction.map(|n| base.join(n)),
                }
            })
            .collect();
        Self {
            roots,
            home: home.to_path_buf(),
            bin_dir: home.join(".local/bin"),
            harnesses: targets,
            adopt_existing: false,
            hooks: false,
            remove_hooks: false,
            peer_agent: None,
            gc_agent: None,
            peer_agent_previous: None,
            hook_specs: Vec::new(),
            protected_roots: Vec::new(),
            configured_homes: BTreeSet::new(),
            configured_configs: BTreeSet::new(),
            environment_homes: BTreeSet::new(),
        }
    }
    /// Composition calls this after config, environment and flags. Resolve
    /// parents only: final config symlinks and whole skills links stay visible.
    pub fn resolve_destinations(
        &mut self,
        resolver: &impl provenant_core::PathResolver,
    ) -> Result<(), InstallError> {
        let resolve = |path: &Path| -> Result<PathBuf, InstallError> {
            resolver.resolve_existing_ancestor(path).map_err(|source| {
                InstallError::Fs(FsError::Io {
                    operation: "resolve install destination",
                    path: path.into(),
                    source,
                })
            })
        };
        let destination = |path: &Path| -> Result<PathBuf, InstallError> {
            let parent = path
                .parent()
                .ok_or_else(|| InstallError::Conflict(path.into()))?;
            let name = path
                .file_name()
                .ok_or_else(|| InstallError::Conflict(path.into()))?;
            Ok(resolve(parent)?.join(name))
        };
        self.bin_dir = resolve(&self.bin_dir)?;
        for target in &mut self.harnesses {
            target.skills_dir = destination(&target.skills_dir)?;
            target.mcp_file = destination(&target.mcp_file)?;
            target.hook_file = target.hook_file.as_deref().map(destination).transpose()?;
            target.instruction = target.instruction.as_deref().map(destination).transpose()?;
        }
        let global = destination(&self.home.join(".agents/AGENTS.md"))?;
        if self
            .protected_roots
            .iter()
            .any(|root| global.starts_with(root))
        {
            return Err(InstallError::Conflict(global));
        }
        self.validate()
    }

    /// Validate all destinations before any source or harness file is read.
    pub fn validate(&self) -> Result<(), InstallError> {
        if self.harnesses.is_empty()
            || self
                .harnesses
                .iter()
                .map(|h| h.harness)
                .collect::<BTreeSet<_>>()
                .len()
                != self.harnesses.len()
        {
            return Err(InstallError::HarnessSelection);
        }
        // Hooks apply to the selected harnesses that have specs; refuse only
        // when none does, so one install can cover every client.
        if self.hooks
            && !self.harnesses.iter().any(|target| {
                self.hook_specs
                    .iter()
                    .any(|spec| spec.harness == Some(target.harness))
            })
        {
            return Err(InstallError::HooksUnavailable);
        }
        normal_absolute(&self.home)?;
        for root in &self.protected_roots {
            normal_absolute(root)?;
            if self.roots.state.as_path().starts_with(root) {
                return Err(InstallError::Conflict(self.roots.state.as_path().into()));
            }
        }
        normal_absolute(&self.bin_dir)?;
        let mut file_paths = BTreeSet::new();
        let mut skills_paths = BTreeSet::new();
        for h in &self.harnesses {
            if !skills_paths.insert(h.skills_dir.clone()) {
                return Err(InstallError::Conflict(h.skills_dir.clone()));
            }
            for path in std::iter::once(&h.mcp_file)
                .chain(h.hook_file.iter())
                .chain(h.instruction.iter())
                .chain(
                    h.instruction
                        .iter()
                        .map(|p| p.with_file_name("HARNESS.md"))
                        .collect::<Vec<_>>()
                        .iter(),
                )
            {
                if !file_paths.insert(path.clone()) {
                    return Err(InstallError::Conflict(path.clone()));
                }
            }
            normal_absolute(&h.skills_dir)?;
            normal_absolute(&h.mcp_file)?;
            if let Some(path) = &h.hook_file {
                normal_absolute(path)?;
            }
            if let Some(p) = &h.instruction {
                normal_absolute(p)?;
            }
        }
        for path in file_paths.iter().chain(skills_paths.iter()).chain([
            &self.bin_dir.join("provenant"),
            &self.home.join(".agents/AGENTS.md"),
        ]) {
            if path.starts_with(self.roots.state.as_path())
                || self
                    .protected_roots
                    .iter()
                    .any(|root| path.starts_with(root))
            {
                return Err(InstallError::Conflict(path.clone()));
            }
        }
        for path in &file_paths {
            if skills_paths
                .iter()
                .any(|directory| path.starts_with(directory))
                || file_paths
                    .iter()
                    .any(|other| path != other && path.starts_with(other))
            {
                return Err(InstallError::Conflict(path.clone()));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub(crate) struct Desired {
    pub(crate) value: Value,
    harness: Option<Harness>,
    selector: String,
}
#[derive(Debug)]
pub struct Snapshots {
    entries: BTreeMap<PathBuf, Option<Snapshot>>,
    receipt: Option<InstallReceipt>,
    harness: crate::harness::HarnessPlan,
    stale_targets: BTreeSet<TargetId>,
}
#[derive(Debug)]
pub struct InstallPlan {
    targets: Targets,
    release: Release,
    before: Snapshots,
    desired: BTreeMap<PathBuf, Desired>,
    receipt: InstallReceipt,
    actions: Vec<Action>,
    manual_includes: Vec<ManualInclude>,
}
/// Re-previews allowed after a live harness rewrites its own JSON file.
const JSON_DRIFT_RETRIES: usize = 2;
impl InstallPlan {
    /// Whether `path` is a harness-owned JSON file that install merges keys
    /// into, such as `.claude.json`, which a running harness rewrites freely.
    fn merges_json_into(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "json")
            && self.before.harness.owned.iter().any(|entry| {
                matches!(entry.expected, OwnedValue::Selector { .. })
                    && entry.path.to_path_buf().is_ok_and(|owned| owned == path)
            })
    }
    pub fn findings(&self) -> &[InstallFinding] {
        &self.before.harness.findings
    }
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }
}
#[derive(Debug, Serialize)]
pub struct InstallOutcome {
    pub findings: Vec<InstallFinding>,
    pub actions: Vec<Action>,
    pub manual_includes: Vec<ManualInclude>,
    pub dry_run: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManualInclude {
    pub path: AbsolutePath,
    pub include: AbsolutePath,
}

/// Preview must succeed before any root, lock, staging file or receipt is made.
pub trait InstallReporter {
    /// Called under the install lock after rechecking an exact match or absence,
    /// before unlinking or forgetting an already missing managed agent.
    fn before_remove(&mut self, _action: &Action) -> std::io::Result<()> {
        Ok(())
    }
    fn findings(&mut self, _findings: &[InstallFinding]) -> std::io::Result<()> {
        Ok(())
    }
    fn preview(&mut self, actions: &[Action]) -> std::io::Result<()>;
    fn manual_includes(&mut self, _includes: &[ManualInclude]) -> std::io::Result<()> {
        Ok(())
    }
}
/// Real process access belongs to binary composition/supervisor. Implementations
/// must run version/help with a 30-second budget and capped output, no probes.
pub trait ReleaseSmoke {
    fn check(&self, release: &Release, cancel: &CancelToken) -> std::io::Result<VersionInfo>;
}

impl Snapshots {
    pub fn capture(release: &Release, targets: &Targets) -> Result<Self, InstallError> {
        targets.validate()?;
        let desired = desired(release, targets)?;
        let snapshot = |path: &Path| snapshot_scoped(path, targets.roots.state.as_path());
        let installed = release_root(
            targets.roots.state.as_path(),
            release.manifest.source_digest,
        );
        let manifest_path = installed.join("release.json");
        let manifest_snapshot = snapshot(&manifest_path)?;
        let completed = match &manifest_snapshot {
            None => false,
            Some(Snapshot {
                value: Value::File { bytes, mode: 0o600 },
                ..
            }) if bytes == &release.manifest_bytes => true,
            Some(Snapshot {
                value: Value::File { bytes, mode: 0o600 },
                ..
            }) => {
                return Err(InstallError::ReleaseConflict {
                    existing_manifest_digest: CanonicalV1::file_digest(bytes),
                    requested_manifest_digest: CanonicalV1::file_digest(&release.manifest_bytes),
                    recovery: "select a new source digest or manually remove the immutable release after checking its links",
                });
            }
            Some(_) => return Err(InstallError::Tampered(manifest_path)),
        };
        validate_release_inventory(release, &installed, targets.roots.state.as_path(), false)?;

        let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
        let receipt_snapshot = snapshot(&receipt_path)?;
        let receipt = match &receipt_snapshot {
            None => None,
            Some(Snapshot {
                value: Value::File { bytes, mode: 0o600 },
                ..
            }) => Some(decode_receipt(bytes, targets)?),
            Some(_) => return Err(InstallError::Conflict(receipt_path.clone())),
        };
        let findings = receipt
            .as_ref()
            .map(Decoded::findings)
            .transpose()?
            .unwrap_or_default();
        let stale_targets = receipt
            .as_ref()
            .map(|decoded| decoded.stale_targets.clone())
            .unwrap_or_default();
        let receipt = receipt.map(|decoded| decoded.receipt);
        let selected: BTreeSet<_> = targets.harnesses.iter().map(HarnessTarget::id).collect();
        let mut paths: BTreeSet<_> = desired.keys().cloned().collect();
        if let Some(previous) = &receipt {
            for entry in &previous.entries {
                if entry.selector == "binary"
                    || entry.selector == "gc-agent"
                    || entry_owner(entry, previous, targets.roots.state.as_path())?
                        .is_some_and(|owner| selected.contains(&owner))
                {
                    paths.insert(entry.path.to_path_buf()?);
                }
            }
        }

        let mut entries = BTreeMap::new();
        for path in paths {
            let current = if path == manifest_path {
                manifest_snapshot.clone()
            } else {
                snapshot_scoped(&path, targets.roots.state.as_path())?
            };
            if completed && path.starts_with(&installed) && current.is_none() {
                return Err(InstallError::Tampered(path));
            }
            entries.insert(path, current);
        }
        entries.insert(receipt_path, receipt_snapshot);
        let mut harness =
            crate::harness::capture(release, targets, &mut entries, receipt.as_ref())?;
        harness.findings.extend(findings);
        Ok(Self {
            entries,
            receipt,
            harness,
            stale_targets,
        })
    }
}

fn validate_release_inventory(
    release: &Release,
    installed: &Path,
    state_root: &Path,
    remove_staging: bool,
) -> Result<(), InstallError> {
    let inventory = directory_scoped(installed, state_root, false)
        .and_then(|dir| crate::release::inventory_held(installed, &dir, remove_staging));
    match inventory {
        Ok(paths) => {
            for path in paths {
                if !release.files.contains_key(&path) {
                    return Err(InstallError::Tampered(installed.join(path)));
                }
            }
            Ok(())
        }
        Err(InstallError::Fs(error)) if error.is_not_found() => Ok(()),
        Err(error) => Err(error),
    }
}

fn desired(
    release: &Release,
    targets: &Targets,
) -> Result<BTreeMap<PathBuf, Desired>, InstallError> {
    targets.validate()?;
    let root = release_root(
        targets.roots.state.as_path(),
        release.manifest.source_digest,
    );
    let mut result = BTreeMap::new();
    let mut insert =
        |p: PathBuf, value: Value, harness, selector: String| -> Result<(), InstallError> {
            normal_absolute(&p)?;
            if targets
                .protected_roots
                .iter()
                .any(|root| p.starts_with(root))
            {
                return Err(InstallError::Conflict(p));
            }
            if selector != "release"
                && (p.starts_with(targets.roots.state.as_path())
                    || p.starts_with(&release.source)
                    || release.source.starts_with(&p))
            {
                return Err(InstallError::Conflict(p));
            }
            if result
                .insert(
                    p.clone(),
                    Desired {
                        value,
                        harness,
                        selector,
                    },
                )
                .is_some()
            {
                return Err(InstallError::Conflict(p));
            }
            Ok(())
        };
    for (path, (bytes, mode)) in &release.files {
        insert(
            root.join(path),
            Value::File {
                bytes: bytes.clone(),
                mode: *mode,
            },
            None,
            "release".into(),
        )?;
    }
    insert(
        root.join("release.json"),
        Value::File {
            bytes: release.manifest_bytes.clone(),
            mode: 0o600,
        },
        None,
        "release".into(),
    )?;
    if !release.files.contains_key(Path::new("bin/provenant")) {
        return Err(InstallError::Manifest("missing executable"));
    }
    insert(
        targets.bin_dir.join("provenant"),
        Value::Link(root.join("bin/provenant")),
        None,
        "binary".into(),
    )?;
    if let Some(bytes) = &targets.peer_agent {
        let path = peer_agent_plist(targets.roots.state.as_path()).ok_or(
            InstallError::InvalidConfig("peer agent requires the default state root"),
        )?;
        let home =
            provenant_os::fs::canonicalize(&targets.home).unwrap_or_else(|_| targets.home.clone());
        if path != home.join("Library/LaunchAgents/dev.provenant.peer.plist") {
            return Err(InstallError::Conflict(path));
        }
        insert(
            path,
            Value::File {
                bytes: bytes.clone(),
                mode: 0o600,
            },
            None,
            "peer-agent".into(),
        )?;
    }
    if let Some(bytes) = &targets.gc_agent {
        let path = gc_agent_plist(targets.roots.state.as_path()).ok_or(
            InstallError::InvalidConfig("gc agent requires the default state root"),
        )?;
        let home =
            provenant_os::fs::canonicalize(&targets.home).unwrap_or_else(|_| targets.home.clone());
        if path != home.join("Library/LaunchAgents/dev.provenant.gc.plist") {
            return Err(InstallError::Conflict(path));
        }
        insert(
            path,
            Value::File {
                bytes: bytes.clone(),
                mode: 0o600,
            },
            None,
            "gc-agent".into(),
        )?;
    }
    for target in &targets.harnesses {
        let mut skills = BTreeMap::new();
        for path in release.files.keys() {
            let mut components = path.components();
            if let (
                Some(Component::Normal(area)),
                Some(Component::Normal(name)),
                Some(Component::Normal(file)),
                None,
            ) = (
                components.next(),
                components.next(),
                components.next(),
                components.next(),
            ) && (area == "skills" || area == "library")
                && file == "SKILL.md"
                && skills
                    .insert(name.to_os_string(), Path::new(area).join(name))
                    .is_some()
            {
                return Err(InstallError::Manifest("duplicate retained skill name"));
            }
        }
        for (name, relative) in skills {
            insert(
                target.skills_dir.join(&name),
                Value::Link(root.join(relative)),
                Some(target.harness),
                "skill".into(),
            )?;
        }
        if let Some(path) = &target.instruction {
            for (destination, asset) in [
                (path.clone(), "instructions/AGENTS.md"),
                (path.with_file_name("HARNESS.md"), "instructions/HARNESS.md"),
            ] {
                if release.files.contains_key(Path::new(asset)) {
                    insert(
                        destination,
                        Value::Link(root.join(asset)),
                        Some(target.harness),
                        "instruction".into(),
                    )?;
                }
            }
        }
    }
    let paths: Vec<_> = result.keys().collect();
    for pair in paths.windows(2) {
        if let [a, b] = pair
            && b.starts_with(a)
        {
            return Err(InstallError::Conflict((*b).clone()));
        }
    }
    Ok(result)
}
pub(crate) fn release_root(state: &Path, digest: provenant_records::SourceDigest) -> PathBuf {
    state.join("releases").join(digest.to_string())
}

/// The single per-user agent belongs only to an account's default state root.
pub fn peer_agent_plist(state: &Path) -> Option<PathBuf> {
    let home = state.ancestors().nth(4)?;
    (state == home.join(".local/share/provenant/v1"))
        .then(|| home.join("Library/LaunchAgents/dev.provenant.peer.plist"))
}

/// The scheduled collector shares the per-user default-state ownership scope.
pub fn gc_agent_plist(state: &Path) -> Option<PathBuf> {
    peer_agent_plist(state).map(|path| path.with_file_name("dev.provenant.gc.plist"))
}

fn validate_removal(
    path: &Path,
    targets: &Targets,
    source: Option<&Path>,
) -> Result<(), InstallError> {
    normal_absolute(path)?;
    if path.starts_with(targets.roots.state.as_path())
        || source.is_some_and(|root| path.starts_with(root) || root.starts_with(path))
        || targets
            .protected_roots
            .iter()
            .any(|root| path.starts_with(root))
    {
        return Err(InstallError::Conflict(path.into()));
    }
    Ok(())
}

pub(crate) fn owned(value: &Value) -> Result<OwnedValue, InstallError> {
    match value {
        Value::File { bytes, mode } => Ok(OwnedValue::File {
            digest: CanonicalV1::file_digest(bytes),
            mode: *mode,
        }),
        Value::Link(target) => Ok(OwnedValue::Link {
            target: AbsolutePath::new(target)?,
        }),
        Value::Other => Err(InstallError::Manifest("unsupported ownership value")),
    }
}
pub(crate) fn matches_owned(value: &Value, expected: &OwnedValue) -> bool {
    owned(value).is_ok_and(|v| &v == expected)
}

pub fn plan_install(
    release: Release,
    targets: Targets,
    mut snapshots: Snapshots,
) -> Result<InstallPlan, InstallError> {
    let mut desired = desired(&release, &targets)?;
    for (path, value) in &snapshots.harness.files {
        normal_absolute(path)?;
        if desired.contains_key(path)
            || path.starts_with(targets.roots.state.as_path())
            || path.starts_with(&release.source)
            || release.source.starts_with(path)
            || targets
                .protected_roots
                .iter()
                .any(|root| path.starts_with(root))
        {
            return Err(InstallError::Conflict(path.clone()));
        }
        desired.insert(
            path.clone(),
            Desired {
                value: value.clone(),
                harness: None,
                selector: "config".into(),
            },
        );
    }
    let state_root = targets.roots.state.as_path();
    let selected: BTreeSet<_> = targets.harnesses.iter().map(HarnessTarget::id).collect();
    let mut actions = Vec::new();
    let mut entries = snapshots.harness.owned.clone();
    let mut manual_includes = Vec::new();
    for (path, want) in &desired {
        let previous = snapshots
            .entries
            .get(path)
            .ok_or_else(|| InstallError::Drift(path.clone()))?;
        if previous
            .as_ref()
            .is_none_or(|snapshot| snapshot.value != want.value)
        {
            let immutable = want.selector == "release";
            if let Some(current) = previous {
                let was_owned = snapshots.receipt.as_ref().is_some_and(|receipt| {
                    receipt.entries.iter().any(|entry| {
                        entry
                            .path
                            .to_path_buf()
                            .is_ok_and(|owned_path| &owned_path == path)
                            && matches_owned(&current.value, &entry.expected)
                    })
                });
                let adopt_binary = targets.adopt_existing
                    && want.selector == "binary"
                    && matches!(current.value, Value::File { .. });
                if want.selector == "instruction"
                    && !was_owned
                    && matches!(current.value, Value::File { .. })
                {
                    // A file that already names the shared instructions includes them itself.
                    let shared = targets.home.join(".agents/AGENTS.md");
                    let shared = shared.to_string_lossy();
                    let already_includes_shared = matches!(
                        &current.value,
                        Value::File { bytes, .. }
                            if [b"~/.agents/AGENTS.md".as_slice(), shared.as_bytes()]
                                .iter()
                                .any(|name| bytes.windows(name.len()).any(|part| part == *name))
                    );
                    if !already_includes_shared && let Value::Link(include) = &want.value {
                        manual_includes.push(ManualInclude {
                            path: AbsolutePath::new(path)?,
                            include: AbsolutePath::new(include)?,
                        });
                    }
                    continue;
                }
                let release_root = release_root(
                    targets.roots.state.as_path(),
                    release.manifest.source_digest,
                );
                let complete = snapshots
                    .entries
                    .get(&release_root.join("release.json"))
                    .is_some_and(Option::is_some);
                if immutable && complete {
                    return Err(InstallError::Tampered(path.clone()));
                }
                let legacy_peer = want.selector == "peer-agent"
                    && !snapshots.receipt.as_ref().is_some_and(|receipt| {
                        receipt
                            .entries
                            .iter()
                            .any(|entry| entry.selector == "peer-agent")
                    })
                    && matches!(&current.value, Value::File { bytes, mode: 0o600 | 0o644 }
                        if targets.peer_agent_previous.as_ref() == Some(bytes));
                if !immutable
                    && !was_owned
                    && !adopt_binary
                    && !legacy_peer
                    && want.selector != "config"
                {
                    if want.selector == "peer-agent" {
                        return Err(InstallError::PeerAgentConflict(path.clone()));
                    }
                    return Err(InstallError::Conflict(path.clone()));
                }
            }
            actions.push(Action {
                path: AbsolutePath::new(path)?,
                disposition: if previous.is_some() {
                    Disposition::Replace
                } else {
                    Disposition::Create
                },
                selector: want.selector.clone(),
                before: previous
                    .as_ref()
                    .and_then(|snapshot| owned(&snapshot.value).ok()),
                after: Some(owned(&want.value)?),
                identity: identity(previous),
                edits: Vec::new(),
            });
        }
        if want.selector != "release" && want.selector != "config" {
            entries.push(OwnedEntry {
                path: AbsolutePath::new(path)?,
                harness: want.harness,
                selector: want.selector.clone(),
                expected: owned(&want.value)?,
            });
        }
    }

    // Selected targets and the stable binary drop old owned paths that are
    // no longer desired. Exact old links become Remove actions; edited links
    // are relinquished as unmanaged state. Missing links are already settled.
    // Entries of unselected targets, including other targets of a selected
    // harness, carry over unchanged.
    if let Some(previous) = &snapshots.receipt {
        for entry in &previous.entries {
            let owner = entry_owner(entry, previous, state_root)?;
            if owner
                .as_ref()
                .is_some_and(|id| snapshots.stale_targets.contains(id))
            {
                continue;
            }
            if matches!(entry.expected, OwnedValue::Selector { .. }) {
                if !owner.is_some_and(|id| selected.contains(&id)) {
                    entries.push(entry.clone());
                }
                continue;
            }
            if entries.iter().any(|current| current.path == entry.path)
                || manual_includes
                    .iter()
                    .any(|include| include.path == entry.path)
            {
                continue;
            }
            if entry.selector == "peer-agent" {
                entries.push(entry.clone());
                continue;
            }
            if !matches!(entry.selector.as_str(), "binary" | "gc-agent") {
                let Some(owner) = owner else {
                    continue;
                };
                if !selected.contains(&owner) {
                    entries.push(entry.clone());
                    continue;
                }
            }
            let path = entry.path.to_path_buf()?;
            let current = snapshots
                .entries
                .get(&path)
                .ok_or_else(|| InstallError::Drift(path.clone()))?;
            if current
                .as_ref()
                .is_some_and(|before| matches_owned(&before.value, &entry.expected))
                || (current.is_none() && entry.selector == "gc-agent")
            {
                actions.push(Action {
                    path: entry.path.clone(),
                    disposition: Disposition::Remove,
                    selector: entry.selector.clone(),
                    before: current.as_ref().map(|_| entry.expected.clone()),
                    after: None,
                    identity: identity(current),
                    edits: Vec::new(),
                });
            } else if entry.selector == "gc-agent" {
                snapshots.harness.findings.push(InstallFinding {
                    rule: "relinquished_modified_entry".into(),
                    path: entry.path.clone(),
                });
            }
        }
    }

    for action in &actions {
        if action.disposition == Disposition::Remove {
            let path = action.path.to_path_buf()?;
            validate_removal(&path, &targets, Some(&release.source))?;
        }
    }
    for action in &mut actions {
        if let Ok(path) = action.path.to_path_buf() {
            action.edits = snapshots
                .harness
                .changes
                .get(&path)
                .cloned()
                .unwrap_or_default();
        }
    }
    let mut resolved_targets = BTreeMap::new();
    if let Some(previous) = &snapshots.receipt {
        for target in &previous.harness_targets {
            let id = recorded_id(target)?;
            if !selected.contains(&id) && !snapshots.stale_targets.contains(&id) {
                resolved_targets.insert(id, target.clone());
            }
        }
    }
    for target in &targets.harnesses {
        resolved_targets.insert(
            target.id(),
            ResolvedHarnessTarget {
                harness: target.harness,
                skills_dir: AbsolutePath::new(&target.skills_dir)?,
                mcp_file: AbsolutePath::new(&target.mcp_file)?,
                hook_file: target
                    .hook_file
                    .as_deref()
                    .map(AbsolutePath::new)
                    .transpose()?,
                instruction: target
                    .instruction
                    .as_deref()
                    .map(AbsolutePath::new)
                    .transpose()?,
            },
        );
    }
    distinct_target_paths(resolved_targets.values())?;
    let harnesses: Vec<_> = resolved_targets
        .keys()
        .map(|(harness, _)| *harness)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let receipt = InstallReceipt {
        schema_version: 1,
        source_digest: release.manifest.source_digest,
        manifest_digest: CanonicalV1::file_digest(&release.manifest_bytes),
        asset_digest: release.manifest.asset_digest,
        target: release.manifest.target.clone(),
        toolchain: release.manifest.toolchain.clone(),
        bin_dir: AbsolutePath::new(&targets.bin_dir)?,
        harnesses,
        harness_targets: resolved_targets.into_values().collect(),
        entries,
        legacy: None,
    };
    let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
    let bytes = provenant_records::encode(&receipt)?;
    if bytes.len() as u64 > Limits::CONFIG.max_bytes {
        return Err(InstallError::Manifest("receipt byte bound"));
    }
    let current = snapshots
        .entries
        .get(&receipt_path)
        .ok_or_else(|| InstallError::Drift(receipt_path.clone()))?;
    if current.as_ref().is_none_or(|snapshot| {
        snapshot.value
            != (Value::File {
                bytes: bytes.clone(),
                mode: 0o600,
            })
    }) {
        actions.push(Action {
            path: AbsolutePath::new(&receipt_path)?,
            disposition: if current.is_some() {
                Disposition::Replace
            } else {
                Disposition::Create
            },
            selector: "receipt".into(),
            before: current
                .as_ref()
                .and_then(|snapshot| owned(&snapshot.value).ok()),
            after: Some(OwnedValue::File {
                digest: CanonicalV1::file_digest(&bytes),
                mode: 0o600,
            }),
            identity: identity(current),
            edits: Vec::new(),
        });
    }

    // Publish release contents first, then release.json as its completion
    // marker, then the stable command, selected harness links, and receipt.
    actions.sort_by_key(|action| {
        let path = action.path.to_path_buf().ok();
        match action.selector.as_str() {
            "release"
                if path
                    .as_deref()
                    .is_some_and(|p| p.file_name() == Some(OsStr::new("release.json"))) =>
            {
                1
            }
            "release" => 0,
            "binary" => 2,
            "receipt" => 5,
            _ => 4,
        }
    });
    Ok(InstallPlan {
        targets,
        release,
        before: snapshots,
        desired,
        receipt,
        actions,
        manual_includes,
    })
}

pub struct InstallContext<'a> {
    pub clock: &'a dyn Clock,
    pub budget: WaitBudget,
    pub cancel: &'a CancelToken,
}
pub fn install(
    plan: &InstallPlan,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
    smoke: &dyn ReleaseSmoke,
    dry_run: bool,
) -> Result<InstallOutcome, InstallError> {
    reporter
        .preview(&plan.actions)
        .map_err(InstallError::Preview)?;
    reporter
        .manual_includes(&plan.manual_includes)
        .map_err(InstallError::Preview)?;
    reporter
        .findings(plan.findings())
        .map_err(InstallError::Preview)?;
    if dry_run {
        return Ok(InstallOutcome {
            findings: plan.findings().to_vec(),
            actions: plan.actions.clone(),
            manual_includes: plan.manual_includes.clone(),
            dry_run: true,
        });
    }
    crate::release::verify_binary_identity(&plan.release, smoke, context.cancel)?;

    let state_root = plan.targets.roots.state.as_path();
    let install_root = plan.targets.roots.state.as_path().join("install");
    let dir = directory_scoped(&install_root, state_root, true)?;
    let _lock = dir.lock(
        OsStr::new("install.lock"),
        LockMode::Exclusive,
        context.budget,
        context.cancel,
        context.clock,
    )?;
    // Validate the complete preview before the first authoritative file write.
    for (path, before) in &plan.before.entries {
        if &snapshot_scoped(path, plan.targets.roots.state.as_path())? != before {
            return Err(InstallError::Drift(path.clone()));
        }
    }
    let release_root = release_root(
        plan.targets.roots.state.as_path(),
        plan.release.manifest.source_digest,
    );
    let unfinished = plan
        .before
        .entries
        .get(&release_root.join("release.json"))
        .is_some_and(Option::is_none);
    // Only the lock holder may collect abandoned staging files. A concurrent
    // preview ignores those names so it cannot interrupt a live publication.
    validate_release_inventory(&plan.release, &release_root, state_root, unfinished)?;
    let mut completed = Vec::new();
    for (index, action) in plan.actions.iter().enumerate() {
        let result = (|| {
            let path = action.path.to_path_buf()?;
            if context.cancel.is_cancelled() {
                return Err(InstallError::Fs(FsError::Cancelled { path }));
            }
            let before = plan
                .before
                .entries
                .get(&path)
                .ok_or_else(|| InstallError::Drift(path.clone()))?;
            recheck(&path, before, state_root)?;
            if action.disposition == Disposition::Remove {
                reporter
                    .before_remove(action)
                    .map_err(InstallError::Preview)?;
                recheck(&path, before, state_root)?;
                if before.is_some() {
                    remove_entry(&path, state_root)
                } else {
                    Ok(())
                }
            } else {
                let value = if action.selector == "receipt" {
                    Value::File {
                        bytes: provenant_records::encode(&plan.receipt)?,
                        mode: 0o600,
                    }
                } else {
                    plan.desired
                        .get(&path)
                        .ok_or_else(|| InstallError::Drift(path.clone()))?
                        .value
                        .clone()
                };
                publish(&path, &value, before.is_some(), state_root)
            }
        })();
        if let Err(source) = result {
            return Err(InstallError::Partial {
                completed,
                remaining: plan.actions.iter().skip(index).cloned().collect(),
                source: Box::new(source),
            });
        }
        completed.push(action.clone());
    }
    Ok(InstallOutcome {
        actions: completed,
        manual_includes: plan.manual_includes.clone(),
        findings: plan.findings().to_vec(),
        dry_run: false,
    })
}
/// Preview and apply, repeating both up to twice when the only change since
/// preview is a harness-owned JSON file install merges keys into. A running
/// harness rewrites such files often; any other change still refuses.
pub fn plan_and_install(
    release: &Release,
    targets: &Targets,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
    smoke: &dyn ReleaseSmoke,
    dry_run: bool,
) -> Result<InstallOutcome, InstallError> {
    let mut retries = 0;
    loop {
        let snapshots = Snapshots::capture(release, targets)?;
        let plan = plan_install(release.clone(), targets.clone(), snapshots)?;
        match install(&plan, context, reporter, smoke, dry_run) {
            Err(InstallError::Drift(path))
                if retries < JSON_DRIFT_RETRIES && plan.merges_json_into(&path) =>
            {
                retries += 1;
            }
            result => return result,
        }
    }
}
pub(crate) fn recheck(
    path: &Path,
    before: &Option<Snapshot>,
    root: &Path,
) -> Result<(), InstallError> {
    if &snapshot_scoped(path, root)? != before {
        return Err(InstallError::Drift(path.into()));
    }
    Ok(())
}
pub(crate) fn publish(
    path: &Path,
    value: &Value,
    replace: bool,
    root: &Path,
) -> Result<(), InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let dir = directory_scoped(parent, root, true)?;
    let mode = if replace {
        PublishMode::Replace
    } else {
        PublishMode::NoReplace
    };
    match value {
        Value::File {
            bytes,
            mode: permissions,
        } => {
            durable(filesystem_operation(
                &dir,
                name,
                Some(bytes),
                FilesystemOperation::Publish,
                || {
                    dir.atomic_write_with_permissions(
                        name,
                        ".provenant-staging.",
                        bytes,
                        *permissions as u32,
                        mode,
                    )
                },
            )?)?;
            Ok(())
        }
        Value::Link(target) => {
            let temporary = provenant_os::fs::temporary_name()?;
            dir.symlink(target, &temporary)?;
            let result = if replace {
                dir.replace(&temporary, &dir, name)
            } else {
                dir.rename_noreplace(&temporary, &dir, name)
            };
            if let Err(error) = result {
                let _ = dir.unlink(&temporary);
                return Err(error.into());
            }
            filesystem_operation(&dir, name, None, FilesystemOperation::SyncLink, || {
                dir.sync()
            })?;
            Ok(())
        }
        Value::Other => Err(InstallError::Conflict(path.into())),
    }
}
pub(crate) fn remove_entry(path: &Path, root: &Path) -> Result<(), InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let dir = directory_scoped(parent, root, false)?;
    dir.unlink(name)?;
    dir.sync()?;
    Ok(())
}

pub(crate) fn durable(publication: Publication) -> Result<(), InstallError> {
    Ok(publication.require_durable()?)
}
pub(crate) fn normal_absolute(path: &Path) -> Result<(), InstallError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if !path.is_absolute()
        || path.parent().is_none()
        || bytes.contains(&0)
        || bytes.split(|b| *b == b'/').any(|s| s == b"." || s == b"..")
    {
        return Err(InstallError::Path(path.into()));
    }
    Ok(())
}
pub(crate) fn path_wire(path: &Path) -> Result<PathWire, InstallError> {
    Ok(PathWire::from_relative(
        &RelativePath::parse(path).map_err(provenant_records::PathWireError::from)?,
    )?)
}
#[derive(Clone, Copy)]
enum FilesystemOperation {
    Open,
    Publish,
    SyncLink,
}

// The callback surrounds one OS operation; callers retain the same traversal
// and publication path when tests inject a failure.
fn filesystem_operation<T>(
    directory: &Directory,
    name: &OsStr,
    bytes: Option<&[u8]>,
    kind: FilesystemOperation,
    operation: impl FnOnce() -> Result<T, FsError>,
) -> Result<T, FsError> {
    #[cfg(test)]
    return tests::filesystem_operation(directory, name, bytes, kind, operation);
    #[cfg(not(test))]
    {
        let _ = (directory, name, bytes, kind);
        operation()
    }
}

pub(crate) fn open_verified_directory(
    directory: &Directory,
    name: &OsStr,
) -> Result<Directory, FsError> {
    filesystem_operation(directory, name, None, FilesystemOperation::Open, || {
        directory.open_verified_directory(name)
    })
}

pub(crate) fn directory(path: &Path, create: bool) -> Result<Directory, InstallError> {
    normal_absolute(path)?;
    // Bootstrap absolute destinations without following any path component.
    // A selected path may have mount ancestors (e.g. a separately mounted
    // HOME or temporary release root). Re-anchor only a held, identity-checked
    // no-follow mount handle; never reopen an absolute path through aliases.
    let mut dir = Directory::root(true)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        dir = match open_verified_directory(&dir, name) {
            Ok(next) => next,
            Err(error) if error.is_cross_device() => {
                let before = dir.metadata_at(name, false)?;
                let child = dir.open_directory(name)?;
                let opened = child.metadata()?;
                let after = dir.metadata_at(name, false)?;
                if before.kind != FileKind::Directory
                    || opened.kind != FileKind::Directory
                    || after.kind != FileKind::Directory
                    || !before.same_identity(&opened)
                    || !opened.same_identity(&after)
                {
                    return Err(FsError::RaceRefused { name: name.into() }.into());
                }
                child.into_anchor()?
            }
            Err(error) if create && error.is_not_found() => {
                match dir.mkdir(name, 0o700) {
                    Ok(()) => dir.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
                dir.open_verified_directory(name)?
            }
            Err(error) => return Err(error.into()),
        };
    }
    Ok(dir)
}

/// Open or create an absolute private directory tree without following path
/// components. Mount boundaries are re-anchored only after identity checks.
pub fn private_directory(path: &Path, create: bool) -> Result<Directory, InstallError> {
    directory(path, create)
}

pub(crate) fn directory_scoped(
    path: &Path,
    root: &Path,
    create: bool,
) -> Result<Directory, InstallError> {
    let Ok(tail) = path.strip_prefix(root) else {
        return directory(path, create);
    };
    let mut dir = directory(root, create)?;
    for part in tail.components() {
        let Component::Normal(name) = part else {
            return Err(InstallError::Path(path.into()));
        };
        dir = match open_verified_directory(&dir, name) {
            Ok(next) => next,
            Err(error) if create && error.is_not_found() => {
                match dir.mkdir(name, 0o700) {
                    Ok(()) => dir.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
                dir.open_verified_directory(name)?
            }
            Err(error) => return Err(error.into()),
        };
    }
    Ok(dir)
}

pub(crate) fn snapshot_scoped(path: &Path, root: &Path) -> Result<Option<Snapshot>, InstallError> {
    snapshot_scoped_inspected(path, root, None)
}
pub(crate) fn snapshot_scoped_inspected(
    path: &Path,
    root: &Path,
    evidence: Option<&mut InspectionEvidence>,
) -> Result<Option<Snapshot>, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let dir = match directory_scoped(parent, root, false) {
        Ok(dir) => dir,
        Err(InstallError::Fs(error)) if error.is_not_found() => {
            if let Some(evidence) = evidence {
                evidence.observe(b"snapshot", path, None);
            }
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    snapshot_at_inspected(path, &dir, evidence)
}

pub(crate) fn snapshot_below_inspected(
    path: &Path,
    root: &Path,
    anchor: &Directory,
    evidence: Option<&mut InspectionEvidence>,
) -> Result<Option<Snapshot>, InstallError> {
    let tail = path
        .strip_prefix(root)
        .map_err(|_| InstallError::Path(path.into()))?;
    let parent = tail
        .parent()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let mut held: Option<Directory> = None;
    for part in parent.components() {
        let Component::Normal(name) = part else {
            return Err(InstallError::Path(path.into()));
        };
        let next = held
            .as_ref()
            .unwrap_or(anchor)
            .open_verified_directory(name)?;
        held = Some(next);
    }
    snapshot_at_inspected(path, held.as_ref().unwrap_or(anchor), evidence)
}

pub(crate) fn snapshot(path: &Path) -> Result<Option<Snapshot>, InstallError> {
    snapshot_inspected(path, None)
}
pub(crate) fn snapshot_inspected(
    path: &Path,
    evidence: Option<&mut InspectionEvidence>,
) -> Result<Option<Snapshot>, InstallError> {
    let Some(parent) = path.parent() else {
        return Err(InstallError::Path(path.into()));
    };
    let dir = match directory(parent, false) {
        Ok(dir) => dir,
        Err(InstallError::Fs(error)) if error.is_not_found() => {
            if let Some(evidence) = evidence {
                evidence.observe(b"snapshot", path, None);
            }
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    snapshot_at_inspected(path, &dir, evidence)
}
fn snapshot_at_inspected(
    path: &Path,
    dir: &Directory,
    mut evidence: Option<&mut InspectionEvidence>,
) -> Result<Option<Snapshot>, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let metadata = match dir.metadata_at(name, false) {
        Ok(m) => m,
        Err(error) if error.is_not_found() => {
            if let Some(evidence) = evidence.as_deref_mut() {
                evidence.observe(b"snapshot", path, None);
            }
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(evidence) = evidence.as_deref_mut() {
        evidence.metadata(path, metadata);
    }
    let limit = if name == OsStr::new(".claude.json") {
        CLAUDE_CONFIG_LIMIT
    } else if Path::new(name)
        .extension()
        .is_some_and(|ext| matches!(ext.to_str(), Some("json" | "jsonc" | "toml" | "md")))
    {
        Limits::CONFIG.max_bytes
    } else {
        FILE_LIMIT
    };
    if metadata.kind == FileKind::Regular && metadata.size > limit {
        return Err(FsError::TooLarge { limit }.into());
    }
    let value = match metadata.kind {
        FileKind::Regular => {
            let bytes = dir.read_stable(name, limit).map_err(|error| match error {
                FsError::RaceRefused { .. } => InstallError::Drift(path.into()),
                FsError::ReadIo { source, .. } => InstallError::Fs(FsError::Io {
                    operation: "read snapshot",
                    path: path.into(),
                    source,
                }),
                other => InstallError::Fs(other),
            })?;
            if dir.metadata_at(name, false)? != metadata {
                return Err(InstallError::Drift(path.into()));
            }
            if let Some(evidence) = evidence.as_deref_mut() {
                evidence.observe(b"file", path, Some(&bytes));
            }
            Value::File {
                bytes,
                mode: u64::from(metadata.mode & 0o7777),
            }
        }
        FileKind::Symlink => {
            let target = dir.read_link(name)?;
            if let Some(evidence) = evidence {
                evidence.observe(b"link", path, Some(target.as_os_str().as_encoded_bytes()));
            }
            let absolute = if target.is_absolute() {
                target
            } else {
                parent.join(target)
            };
            let mut normal = PathBuf::new();
            for part in absolute.components() {
                match part {
                    Component::ParentDir => {
                        normal.pop();
                    }
                    Component::CurDir => {}
                    other => normal.push(other.as_os_str()),
                }
            }
            Value::Link(normal)
        }
        _ => Value::Other,
    };
    if dir.metadata_at(name, false)? != metadata {
        return Err(InstallError::Drift(path.into()));
    }
    Ok(Some(Snapshot { value, metadata }))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallFinding {
    pub rule: String,
    pub path: AbsolutePath,
}
#[cfg(test)]
pub(crate) fn read_receipt(targets: &Targets) -> Result<Option<InstallReceipt>, InstallError> {
    Ok(read_receipt_inspected(targets, None)?.map(|decoded| decoded.receipt))
}
/// Validated stable binary path for recognising a historical peer plist.
pub fn installed_binary(targets: &Targets) -> Result<Option<PathBuf>, InstallError> {
    read_receipt_inspected(targets, None)?
        .filter(|decoded| !decoded.stale.iter().any(|entry| entry.selector == "binary"))
        .map(|decoded| Ok(decoded.receipt.bin_dir.to_path_buf()?.join("provenant")))
        .transpose()
}
/// The recorded targets uninstall of `targets` selects, and every recorded
/// target. Shared entries go only when the two match.
fn uninstall_selection(
    receipt: &InstallReceipt,
    targets: &Targets,
) -> Result<(BTreeSet<TargetId>, BTreeSet<TargetId>), InstallError> {
    let mut registered = BTreeSet::new();
    let mut selected = BTreeSet::new();
    for recorded in &receipt.harness_targets {
        let id = recorded_id(recorded)?;
        if targets.harnesses.iter().any(|target| {
            target.harness == recorded.harness
                && (!targets.environment_homes.contains(&target.harness) || target.id() == id)
        }) {
            selected.insert(id.clone());
        }
        registered.insert(id);
    }
    Ok((selected, registered))
}
fn read_receipt_inspected(
    targets: &Targets,
    evidence: Option<&mut InspectionEvidence>,
) -> Result<Option<Decoded>, InstallError> {
    let path = targets.roots.state.as_path().join("install/receipt.json");
    match snapshot_scoped_inspected(&path, targets.roots.state.as_path(), evidence)? {
        None => Ok(None),
        Some(Snapshot {
            value: Value::File { bytes, mode: 0o600 },
            ..
        }) => Ok(Some(decode_receipt(&bytes, targets)?)),
        Some(_) => Err(InstallError::Conflict(path)),
    }
}
#[derive(Debug)]
struct Decoded {
    receipt: InstallReceipt,
    moved: Option<PathBuf>,
    stale: Vec<OwnedEntry>,
    stale_targets: BTreeSet<TargetId>,
}
impl Decoded {
    fn findings(&self) -> Result<Vec<InstallFinding>, InstallError> {
        let mut findings = Vec::new();
        if let Some(old_state) = &self.moved {
            findings.push(InstallFinding {
                rule: "state_root_moved".into(),
                path: AbsolutePath::new(old_state)?,
            });
        }
        findings.extend(self.stale.iter().map(|entry| InstallFinding {
            rule: "relocated_stale_entry".into(),
            path: entry.path.clone(),
        }));
        Ok(findings)
    }
}
fn decode_receipt(bytes: &[u8], targets: &Targets) -> Result<Decoded, InstallError> {
    let mut receipt: InstallReceipt =
        provenant_records::decode_v1(bytes, Limits::CONFIG).map_err(InstallError::ReceiptDecode)?;
    let state_root = targets.roots.state.as_path();
    normal_absolute(&receipt.bin_dir.to_path_buf()?)?;
    let mut harnesses = BTreeSet::new();
    for harness in &receipt.harnesses {
        if !harnesses.insert(*harness) {
            return Err(InstallError::Manifest("duplicate receipt harness"));
        }
    }
    let mut target_harnesses = BTreeSet::new();
    let mut target_ids = BTreeSet::new();
    for target in &receipt.harness_targets {
        target_harnesses.insert(target.harness);
        if !target_ids.insert(recorded_id(target)?) {
            return Err(InstallError::Manifest("duplicate receipt target"));
        }
        normal_absolute(&target.skills_dir.to_path_buf()?)?;
        normal_absolute(&target.mcp_file.to_path_buf()?)?;
        if let Some(path) = &target.hook_file {
            normal_absolute(&path.to_path_buf()?)?;
        }
        if let Some(path) = &target.instruction {
            normal_absolute(&path.to_path_buf()?)?;
        }
    }
    if harnesses != target_harnesses {
        return Err(InstallError::Manifest(
            "receipt harness targets do not match harnesses",
        ));
    }
    distinct_target_paths(receipt.harness_targets.iter())
        .map_err(|_| InstallError::Manifest("receipt targets share a path"))?;
    let binaries: Vec<_> = receipt
        .entries
        .iter()
        .filter(|entry| entry.selector == "binary" && entry.harness.is_none())
        .collect();
    let [binary] = binaries.as_slice() else {
        return Err(InstallError::Manifest("receipt requires one binary entry"));
    };
    let old_state = match &binary.expected {
        OwnedValue::Link { target } => {
            let target = target.to_path_buf()?;
            let root = target
                .ancestors()
                .nth(4)
                .filter(|root| {
                    target == release_root(root, receipt.source_digest).join("bin/provenant")
                })
                .ok_or(InstallError::Manifest(
                    "receipt entry outside resolved targets",
                ))?;
            normal_absolute(root)?;
            root.to_path_buf()
        }
        _ => {
            return Err(InstallError::Manifest(
                "receipt entry outside resolved targets",
            ));
        }
    };
    let moved = (old_state != state_root).then_some(old_state);
    let home =
        provenant_os::fs::canonicalize(&targets.home).unwrap_or_else(|_| targets.home.clone());
    let bin_dir = receipt.bin_dir.to_path_buf()?;
    let before_tail = |path: &Path, tail: &Path| {
        path.ancestors()
            .nth(tail.components().count())
            .filter(|old| old.join(tail) == path)
            .map(Path::to_path_buf)
    };
    // Current config may still name the old home, and uninstall cannot repeat
    // an install's --bin-dir flag. Try recorded defaults independently too.
    let mut old_homes = vec![
        targets
            .bin_dir
            .strip_prefix(&home)
            .ok()
            .filter(|tail| !tail.as_os_str().is_empty())
            .and_then(|tail| before_tail(&bin_dir, tail)),
        before_tail(&bin_dir, Path::new(".local/bin")),
        moved
            .as_deref()
            .and_then(|root| before_tail(root, Path::new(".local/share/provenant/v1"))),
    ];
    let defaults = Targets::new(targets.roots.clone(), &home, &receipt.harnesses);
    for recorded in &receipt.harness_targets {
        let Some(default) = defaults
            .harnesses
            .iter()
            .find(|default| default.harness == recorded.harness)
        else {
            continue;
        };
        if let Ok(tail) = default.skills_dir.strip_prefix(&home) {
            old_homes.push(before_tail(&recorded.skills_dir.to_path_buf()?, tail));
        }
    }
    // A coincidental suffix match within HOME (or above it) is not a rename.
    let separate_home = |old: &Path| {
        !old.starts_with(&home)
            && !home.starts_with(old)
            && !old.starts_with(&targets.home)
            && !targets.home.starts_with(old)
    };
    let old_home = old_homes.iter().flatten().find(|old| separate_home(old));
    let mut stale_targets = BTreeSet::new();
    for target in &receipt.harness_targets {
        let target_paths = [
            Some(&target.skills_dir),
            Some(&target.mcp_file),
            target.hook_file.as_ref(),
            target.instruction.as_ref(),
        ];
        for path in target_paths.into_iter().flatten() {
            let path = path.to_path_buf()?;
            if old_home.is_some_and(|old| path.starts_with(old) && !path.starts_with(&home)) {
                stale_targets.insert(recorded_id(target)?);
                break;
            }
        }
    }
    let mut paths = BTreeSet::new();
    let mut entries = Vec::new();
    let mut stale = Vec::new();
    for entry in &receipt.entries {
        let path = entry.path.to_path_buf()?;
        if !paths.insert((path.clone(), entry.selector.clone())) {
            return Err(InstallError::Manifest("duplicate receipt path"));
        }
        let permitted = permitted_entry(entry, &receipt, state_root)?;
        if !permitted
            && !moved
                .as_deref()
                .map(|root| permitted_entry(entry, &receipt, root))
                .transpose()?
                .unwrap_or(false)
        {
            return Err(InstallError::Manifest(
                "receipt entry outside resolved targets",
            ));
        }
        if !permitted
            || old_home.is_some_and(|old| path.starts_with(old) && !path.starts_with(&home))
        {
            stale.push(entry.clone());
        } else {
            entries.push(entry.clone());
        }
    }
    receipt.entries = entries;
    Ok(Decoded {
        receipt,
        moved,
        stale,
        stale_targets,
    })
}
/// Recorded targets never share a destination, so each entry has one owner.
fn distinct_target_paths<'a>(
    targets: impl Iterator<Item = &'a ResolvedHarnessTarget>,
) -> Result<(), InstallError> {
    let mut seen = BTreeSet::new();
    for target in targets {
        let instruction = target
            .instruction
            .as_ref()
            .map(AbsolutePath::to_path_buf)
            .transpose()?;
        let paths = [
            Some(target.skills_dir.to_path_buf()?),
            Some(target.mcp_file.to_path_buf()?),
            target
                .hook_file
                .as_ref()
                .map(AbsolutePath::to_path_buf)
                .transpose()?,
            instruction.as_ref().map(|p| p.with_file_name("HARNESS.md")),
            instruction,
        ];
        for path in paths.into_iter().flatten() {
            if !seen.insert(path.clone()) {
                return Err(InstallError::Conflict(path));
            }
        }
    }
    Ok(())
}
fn asset_target(target: &Path, state_root: &Path, expected_tail: &Path) -> bool {
    let releases = state_root.join("releases");
    let Ok(relative) = target.strip_prefix(&releases) else {
        return false;
    };
    let mut components = relative.components();
    let Some(Component::Normal(digest)) = components.next() else {
        return false;
    };
    if digest
        .to_str()
        .is_none_or(|text| provenant_records::SourceDigest::parse(text).is_err())
    {
        return false;
    }
    let mut tail = PathBuf::new();
    for part in components {
        tail.push(part.as_os_str());
    }
    tail == expected_tail
}
fn permitted_entry(
    entry: &OwnedEntry,
    receipt: &InstallReceipt,
    state_root: &Path,
) -> Result<bool, InstallError> {
    let path = entry.path.to_path_buf()?;
    normal_absolute(&path)?;
    if entry.harness.is_none() {
        if matches!(entry.selector.as_str(), "peer-agent" | "gc-agent") {
            let expected_path = if entry.selector == "peer-agent" {
                peer_agent_plist(state_root)
            } else {
                gc_agent_plist(state_root)
            };
            return Ok(expected_path.as_ref() == Some(&path)
                && matches!(
                    entry.expected,
                    OwnedValue::File {
                        mode: 0o600 | 0o644,
                        ..
                    }
                ));
        }
        let binary = receipt.bin_dir.to_path_buf()?.join("provenant");
        let expected_target = release_root(state_root, receipt.source_digest).join("bin/provenant");
        return Ok(entry.selector == "binary"
            && path == binary
            && matches!(
                &entry.expected,
                OwnedValue::Link { target }
                    if target.to_path_buf()? == expected_target
            ));
    }
    Ok(entry_owner(entry, receipt, state_root)?.is_some())
}
/// The recorded target that owns a harness entry; `None` for shared entries.
fn entry_owner(
    entry: &OwnedEntry,
    receipt: &InstallReceipt,
    state_root: &Path,
) -> Result<Option<TargetId>, InstallError> {
    for target in &receipt.harness_targets {
        if Some(target.harness) == entry.harness && permitted_for(entry, target, state_root)? {
            return Ok(Some(recorded_id(target)?));
        }
    }
    Ok(None)
}
fn permitted_for(
    entry: &OwnedEntry,
    target: &ResolvedHarnessTarget,
    state_root: &Path,
) -> Result<bool, InstallError> {
    let path = entry.path.to_path_buf()?;
    if matches!(entry.expected, OwnedValue::Selector { .. }) {
        return crate::harness::permitted_selector(entry, target);
    }
    let skills_dir = target.skills_dir.to_path_buf()?;
    if entry.selector == "skill"
        && path.parent() == Some(skills_dir.as_path())
        && path.file_name().is_some_and(|name| {
            matches!(
                &entry.expected,
                OwnedValue::Link { target }
                    if target.to_path_buf().is_ok_and(|link| {
                        asset_target(
                            &link,
                            state_root,
                            &Path::new("skills").join(name),
                        ) || asset_target(&link, state_root, &Path::new("library").join(name))
                    })
            )
        })
    {
        return Ok(true);
    }
    if entry.selector == "instruction"
        && let Some(instruction) = &target.instruction
    {
        let instruction = instruction.to_path_buf()?;
        let agents = path == instruction;
        let harness = path == instruction.with_file_name("HARNESS.md");
        if (agents || harness)
            && matches!(
                &entry.expected,
                OwnedValue::Link { target }
                    if target.to_path_buf().is_ok_and(|link| {
                        asset_target(
                            &link,
                            state_root,
                            Path::new(if agents {
                                "instructions/AGENTS.md"
                            } else {
                                "instructions/HARNESS.md"
                            }),
                        )
                    })
            )
        {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Complete findings plus the bytes, metadata, links, absence and inventory
/// actually inspected. No locks, repair, extra filesystem reads or state writes.
#[derive(Debug)]
pub struct InstallInspection {
    pub error: Option<InstallError>,
    pub findings: Vec<InstallFinding>,
    pub input_digest: FileDigest,
}

pub fn inspect_install(
    targets: &Targets,
    source: Option<&Release>,
    target: &str,
    smoke: &dyn ReleaseSmoke,
    cancel: &CancelToken,
) -> Result<InstallInspection, InstallError> {
    let mut evidence = InspectionEvidence::default();
    if let Some(source) = source {
        evidence.observe(
            b"source-manifest",
            &source.source,
            Some(&source.manifest_bytes),
        );
        for (path, (bytes, mode)) in &source.files {
            let path = source.source.join(path);
            evidence.observe(b"source-file", &path, Some(bytes));
            evidence.observe(b"source-mode", &path, Some(&mode.to_be_bytes()));
        }
    }
    let (findings, error) =
        match check_install_inspected(targets, source, target, smoke, cancel, &mut evidence) {
            Ok(findings) => (findings, None),
            Err(
                error @ (InstallError::Decode(_)
                | InstallError::Manifest(_)
                | InstallError::Tampered(_)
                | InstallError::Conflict(_)
                | InstallError::Platform
                | InstallError::Path(_)
                | InstallError::InvalidConfig(_)
                | InstallError::ConfigFile(_)
                | InstallError::ReceiptDecode(_)
                | InstallError::Config(_)),
            ) => (Vec::new(), Some(error)),
            Err(error) => return Err(error),
        };
    Ok(InstallInspection {
        error,
        findings,
        input_digest: evidence.finish(),
    })
}

fn check_install_inspected(
    targets: &Targets,
    source: Option<&Release>,
    target: &str,
    smoke: &dyn ReleaseSmoke,
    cancel: &CancelToken,
    evidence: &mut InspectionEvidence,
) -> Result<Vec<InstallFinding>, InstallError> {
    let state_root = targets.roots.state.as_path();
    let receipt_path = state_root.join("install/receipt.json");
    let Some(decoded) = read_receipt_inspected(targets, Some(evidence))? else {
        return Ok(vec![InstallFinding {
            rule: "missing_receipt".into(),
            path: AbsolutePath::new(&receipt_path)?,
        }]);
    };
    let mut findings = decoded.findings()?;
    let stale_binary = decoded.stale.iter().any(|entry| entry.selector == "binary");
    let receipt = decoded.receipt;
    let release_path = release_root(state_root, receipt.source_digest);
    match Release::read_scoped_inspected(&release_path, target, state_root, Some(evidence)) {
        Ok(release) => {
            crate::release::verify_binary_identity(&release, smoke, cancel)?;
            if release.manifest.source_digest == receipt.source_digest
                && release.manifest.target == receipt.target
                && release.manifest.toolchain == receipt.toolchain
                && release.manifest.asset_digest == receipt.asset_digest
                && CanonicalV1::file_digest(&release.manifest_bytes) == receipt.manifest_digest
            {
                let expected = OwnedValue::Link {
                    target: AbsolutePath::new(&release_path.join("bin/provenant"))?,
                };
                if !stale_binary
                    && !receipt.entries.iter().any(|entry| {
                        entry.selector == "binary"
                            && entry.harness.is_none()
                            && entry.expected == expected
                    })
                {
                    return Err(InstallError::Manifest(
                        "receipt binary differs from release",
                    ));
                }
            } else {
                findings.push(InstallFinding {
                    rule: "release_drift".into(),
                    path: AbsolutePath::new(&release_path)?,
                });
            }
        }
        Err(InstallError::Tampered(_)) | Err(InstallError::Manifest(_)) => {
            findings.push(InstallFinding {
                rule: "release_drift".into(),
                path: AbsolutePath::new(&release_path)?,
            });
        }
        Err(InstallError::Fs(error)) if error.is_not_found() => {
            findings.push(InstallFinding {
                rule: "missing_release".into(),
                path: AbsolutePath::new(&release_path)?,
            });
        }
        Err(error) => return Err(error),
    }
    if let Some(source) = source {
        crate::release::verify_binary_identity(source, smoke, cancel)?;
        if source.manifest.source_digest != receipt.source_digest
            || source.manifest.asset_digest != receipt.asset_digest
            || source.manifest.target != receipt.target
            || source.manifest.toolchain != receipt.toolchain
            || CanonicalV1::file_digest(&source.manifest_bytes) != receipt.manifest_digest
        {
            findings.push(InstallFinding {
                rule: "source_digest".into(),
                path: AbsolutePath::new(&receipt_path)?,
            });
        }
    }
    for entry in &receipt.entries {
        if !permitted_entry(entry, &receipt, state_root)? {
            return Err(InstallError::Conflict(entry.path.to_path_buf()?));
        }
        let path = entry.path.to_path_buf()?;
        let current = snapshot_scoped_inspected(&path, state_root, Some(evidence))?;
        let matches = match &current {
            Some(current) => crate::harness::entry_matches(&current.value, entry)?,
            None => false,
        };
        if !matches {
            findings.push(InstallFinding {
                rule: "owned_entry_drift".into(),
                path: entry.path.clone(),
            });
        }
    }
    Ok(findings)
}

/// Removes unchanged entries of the selected recorded targets, preserves user
/// edits, and forgets those targets in the receipt. A harness whose home came
/// from the environment selects only its matching recorded target; otherwise
/// every recorded target of the harness is selected. State and the stable
/// command remain installed.
fn not_installed(
    receipt_path: &Path,
    mut findings: Vec<InstallFinding>,
    reporter: &mut dyn InstallReporter,
) -> Result<InstallOutcome, InstallError> {
    reporter.preview(&[]).map_err(InstallError::Preview)?;
    findings.insert(
        0,
        InstallFinding {
            rule: "not_installed".into(),
            path: AbsolutePath::new(receipt_path)?,
        },
    );
    reporter
        .findings(&findings)
        .map_err(InstallError::Preview)?;
    Ok(InstallOutcome {
        findings,
        actions: Vec::new(),
        manual_includes: Vec::new(),
        dry_run: false,
    })
}

pub fn uninstall(
    targets: &Targets,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
) -> Result<InstallOutcome, InstallError> {
    targets.validate()?;

    let state_root = targets.roots.state.as_path();
    let snapshot = |path: &Path| snapshot_scoped(path, targets.roots.state.as_path());
    let receipt_path = state_root.join("install/receipt.json");
    let before_receipt = snapshot(&receipt_path)?;
    let Some(decoded) = read_receipt_inspected(targets, None)? else {
        return not_installed(&receipt_path, Vec::new(), reporter);
    };
    let mut findings = decoded.findings()?;
    let Decoded {
        mut receipt,
        moved,
        stale,
        stale_targets: _,
    } = decoded;
    let (selected, registered) = uninstall_selection(&receipt, targets)?;
    if selected.is_empty() {
        return not_installed(&receipt_path, findings, reporter);
    }
    // Older installers wrote this fixed plist without recording it. The
    // process edge recognises its exact historical rendering, never its label
    // alone. Unknown bytes still become a preserved-and-reported finding.
    let recorded_peer_agent = receipt
        .entries
        .iter()
        .any(|entry| entry.selector == "peer-agent");
    if selected == registered
        && !recorded_peer_agent
        && let Some(desired) = &targets.peer_agent
        && let Some(path) = peer_agent_plist(state_root)
    {
        let current = snapshot(&path)?;
        let mode = match current.as_ref().map(|snapshot| &snapshot.value) {
            Some(Value::File { mode: 0o644, .. }) => 0o644,
            _ => 0o600,
        };
        receipt.entries.push(OwnedEntry {
            path: AbsolutePath::new(&path)?,
            harness: None,
            selector: "peer-agent".into(),
            expected: owned(&Value::File {
                bytes: targets
                    .peer_agent_previous
                    .clone()
                    .unwrap_or_else(|| desired.clone()),
                mode,
            })?,
        });
    }
    let mut owners = Vec::with_capacity(receipt.entries.len());
    for entry in &receipt.entries {
        owners.push(entry_owner(entry, &receipt, state_root)?);
    }
    let removed = |entry: &OwnedEntry, owner: &Option<TargetId>| {
        owner.as_ref().is_some_and(|id| selected.contains(id))
            || (matches!(entry.selector.as_str(), "peer-agent" | "gc-agent")
                && selected == registered)
    };

    let mut removals = Vec::new();
    let mut actions = Vec::new();
    let mut selector_files = BTreeMap::new();
    for (entry, owner) in receipt.entries.iter().zip(&owners) {
        if !removed(entry, owner) {
            continue;
        }
        if !permitted_entry(entry, &receipt, state_root)? {
            return Err(InstallError::Conflict(entry.path.to_path_buf()?));
        }
        let path = entry.path.to_path_buf()?;
        if matches!(entry.expected, OwnedValue::Selector { .. }) {
            let before = snapshot(&path)?;
            if let Some(before) = before {
                let current = selector_files.get(&path).unwrap_or(&before.value);
                if let Some((value, edits)) = crate::harness::remove_selector(current, entry)? {
                    if let Some(action) = actions
                        .iter_mut()
                        .find(|a: &&mut Action| a.path == entry.path)
                    {
                        action.after = Some(owned(&value)?);
                        action.edits.extend(edits);
                    } else {
                        actions.push(Action {
                            path: entry.path.clone(),
                            disposition: Disposition::Replace,
                            selector: "config".into(),
                            before: Some(owned(&before.value)?),
                            after: Some(owned(&value)?),
                            identity: identity(&Some(before.clone())),
                            edits,
                        });
                        removals.push((path.clone(), Some(before)));
                    }
                    selector_files.insert(path, value);
                } else {
                    findings.push(InstallFinding {
                        rule: "relinquished_modified_entry".into(),
                        path: entry.path.clone(),
                    });
                }
            }
            continue;
        }
        let before = snapshot(&path)?;
        // A deleted plist can still have a loaded service. Keep its stop in the
        // same locked, retryable removal path as an unchanged recorded plist.
        if before.is_none()
            && entry.selector != "gc-agent"
            && (entry.selector != "peer-agent" || !recorded_peer_agent)
        {
            continue;
        }
        if before
            .as_ref()
            .is_none_or(|current| matches_owned(&current.value, &entry.expected))
        {
            actions.push(Action {
                path: entry.path.clone(),
                disposition: Disposition::Remove,
                selector: entry.selector.clone(),
                before: before.as_ref().map(|_| entry.expected.clone()),
                after: None,
                identity: identity(&before),
                edits: Vec::new(),
            });
            removals.push((path, before));
        } else {
            findings.push(InstallFinding {
                rule: "relinquished_modified_entry".into(),
                path: entry.path.clone(),
            });
        }
    }

    let mut owners = owners.iter();
    receipt
        .entries
        .retain(|entry| owners.next().is_none_or(|owner| !removed(entry, owner)));
    for entry in stale {
        let owner = entry_owner(&entry, &receipt, moved.as_deref().unwrap_or(state_root))?;
        if !owner.as_ref().is_some_and(|id| selected.contains(id))
            && !(entry.selector == "peer-agent" && selected == registered)
        {
            receipt.entries.push(entry);
        }
    }
    receipt
        .harness_targets
        .retain(|target| recorded_id(target).is_ok_and(|id| !selected.contains(&id)));
    let remaining: BTreeSet<_> = receipt
        .harness_targets
        .iter()
        .map(|target| target.harness)
        .collect();
    receipt
        .harnesses
        .retain(|harness| remaining.contains(harness));
    let receipt_bytes = provenant_records::encode(&receipt)?;
    actions.push(Action {
        path: AbsolutePath::new(&receipt_path)?,
        disposition: Disposition::Replace,
        selector: "receipt".into(),
        before: before_receipt
            .as_ref()
            .and_then(|snapshot| owned(&snapshot.value).ok()),
        after: Some(OwnedValue::File {
            digest: CanonicalV1::file_digest(&receipt_bytes),
            mode: 0o600,
        }),
        identity: identity(&before_receipt),
        edits: Vec::new(),
    });

    for (path, _) in &removals {
        validate_removal(path, targets, None)?;
    }
    reporter.preview(&actions).map_err(InstallError::Preview)?;
    reporter
        .findings(&findings)
        .map_err(InstallError::Preview)?;
    let install_dir = directory_scoped(&state_root.join("install"), state_root, false)?;
    let _lock = install_dir.lock(
        OsStr::new("install.lock"),
        LockMode::Exclusive,
        context.budget,
        context.cancel,
        context.clock,
    )?;
    recheck(&receipt_path, &before_receipt, state_root)?;
    for (path, before) in &removals {
        recheck(path, before, state_root)?;
    }

    let mut completed = Vec::new();
    for (index, (path, before)) in removals.iter().enumerate() {
        let result = (|| {
            if context.cancel.is_cancelled() {
                return Err(InstallError::Fs(FsError::Cancelled { path: path.clone() }));
            }
            recheck(path, before, state_root)?;
            if let Some(value) = selector_files.get(path) {
                publish(path, value, true, state_root)
            } else {
                reporter
                    .before_remove(&actions[index])
                    .map_err(InstallError::Preview)?;
                recheck(path, before, state_root)?;
                if before.is_some() {
                    remove_entry(path, state_root)
                } else {
                    Ok(())
                }
            }
        })();
        if let Err(source) = result {
            return Err(InstallError::Partial {
                completed,
                remaining: actions.iter().skip(index).cloned().collect(),
                source: Box::new(source),
            });
        }
        completed.push(actions[index].clone());
    }
    let result = recheck(&receipt_path, &before_receipt, state_root).and_then(|()| {
        publish(
            &receipt_path,
            &Value::File {
                bytes: receipt_bytes,
                mode: 0o600,
            },
            true,
            state_root,
        )
    });
    if let Err(source) = result {
        return Err(InstallError::Partial {
            completed,
            remaining: actions.last().cloned().into_iter().collect(),
            source: Box::new(source),
        });
    }
    if let Some(receipt_action) = actions.last() {
        completed.push(receipt_action.clone());
    }
    Ok(InstallOutcome {
        actions: completed,
        findings,
        manual_includes: Vec::new(),
        dry_run: false,
    })
}

#[cfg(test)]
#[path = "managed_tests.rs"]
pub(crate) mod tests;
