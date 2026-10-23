//! Managed files use supervisor capabilities exclusively. Plans retain bounded
//! snapshots; every publication rechecks identity and bytes under owner locks.
use crate::release::{FILE_LIMIT, Release};
use provenant_core::{CancelToken, Clock, RelativePath, Roots, WaitBudget};
use provenant_records::{
    AbsolutePath, CanonicalV1, Harness, InstallReceipt, Limits, OwnedEntry, OwnedValue, PathWire,
};
use provenant_supervisor::fs::{
    FileKind, FsError, LockMode, Metadata, Publication, PublishMode, SafeDir, SafeFs,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io::Read,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

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
    #[error("release file bytes or mode differ: {0}")]
    Tampered(PathBuf),
    #[error("unmanaged destination conflicts: {0}")]
    Conflict(PathBuf),
    #[error("destination changed since preview: {0}")]
    Drift(PathBuf),
    #[error("select at least one harness explicitly, without duplicates")]
    HarnessSelection,
    #[error("hook registration belongs to harness cut-over")]
    HooksUnavailable,
    #[error("legacy matching belongs to harness cut-over")]
    LegacyUnavailable,
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
    pub instruction: Option<PathBuf>,
}
#[derive(Debug, Clone)]
pub struct Targets {
    pub roots: Roots,
    pub bin_dir: PathBuf,
    pub harnesses: Vec<HarnessTarget>,
    pub adopt_existing: bool,
    pub hooks: bool,
    pub legacy_checkout: Option<PathBuf>,
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
                    skills_dir: base.join("skills"),
                    instruction: instruction.map(|n| base.join(n)),
                }
            })
            .collect();
        Self {
            roots,
            bin_dir: home.join(".local/bin"),
            harnesses: targets,
            adopt_existing: false,
            hooks: false,
            legacy_checkout: None,
        }
    }
    fn validate(&self) -> Result<(), InstallError> {
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
        if self.hooks {
            return Err(InstallError::HooksUnavailable);
        }
        if self.legacy_checkout.is_some() {
            return Err(InstallError::LegacyUnavailable);
        }
        normal_absolute(&self.bin_dir)?;
        for h in &self.harnesses {
            normal_absolute(&h.skills_dir)?;
            if let Some(p) = &h.instruction {
                normal_absolute(p)?;
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
struct Desired {
    value: Value,
    harness: Option<Harness>,
    selector: String,
}
#[derive(Debug)]
pub struct Snapshots {
    entries: BTreeMap<PathBuf, Option<Snapshot>>,
    receipt: Option<InstallReceipt>,
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
impl InstallPlan {
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }
}
#[derive(Debug, Serialize)]
pub struct InstallOutcome {
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
    fn preview(&mut self, actions: &[Action]) -> std::io::Result<()>;
    fn manual_includes(&mut self, _includes: &[ManualInclude]) -> std::io::Result<()> {
        Ok(())
    }
}
/// Real process access belongs to binary composition/supervisor. Implementations
/// must run version/help with a 30-second budget and capped output, no probes.
pub trait ReleaseSmoke {
    fn check(&self, release: &Release, cancel: &CancelToken) -> std::io::Result<()>;
}

impl Snapshots {
    pub fn capture(
        release: &Release,
        targets: &Targets,
        fs: &dyn SafeFs,
    ) -> Result<Self, InstallError> {
        targets.validate()?;
        let desired = desired(release, targets)?;
        let installed = targets
            .roots
            .state
            .as_path()
            .join("releases")
            .join(release.manifest.source_digest.to_string());
        match crate::release::inventory(&installed, fs) {
            Ok(paths) => {
                for p in paths {
                    if !release.files.contains_key(&p) {
                        return Err(InstallError::Tampered(installed.join(p)));
                    }
                }
            }
            Err(InstallError::Fs(e)) if e.is_not_found() => {}
            Err(e) => return Err(e),
        }

        let completed = snapshot(&installed.join("release.json"), fs)?.is_some();
        let mut entries = BTreeMap::new();
        for path in desired.keys() {
            let current = snapshot(path, fs)?;
            if completed && path.starts_with(&installed) && current.is_none() {
                return Err(InstallError::Tampered(path.clone()));
            }
            entries.insert(path.clone(), current);
        }
        let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
        let receipt_snapshot = snapshot(&receipt_path, fs)?;
        let receipt = match &receipt_snapshot {
            None => None,
            Some(Snapshot {
                value: Value::File { bytes, mode: 0o600 },
                ..
            }) => Some(decode_receipt(bytes)?),
            Some(_) => return Err(InstallError::Conflict(receipt_path)),
        };
        entries.insert(receipt_path, receipt_snapshot);
        Ok(Self { entries, receipt })
    }
}
fn desired(
    release: &Release,
    targets: &Targets,
) -> Result<BTreeMap<PathBuf, Desired>, InstallError> {
    targets.validate()?;
    let root = targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(release.manifest.source_digest.to_string());
    let mut result = BTreeMap::new();
    let mut insert =
        |p: PathBuf, value: Value, harness, selector: String| -> Result<(), InstallError> {
            normal_absolute(&p)?;
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
    let (binary, mode) = release
        .files
        .get(Path::new("provenant"))
        .ok_or(InstallError::Manifest("missing executable"))?;
    insert(
        targets.bin_dir.join("provenant"),
        Value::File {
            bytes: binary.clone(),
            mode: *mode,
        },
        None,
        "binary".into(),
    )?;
    for target in &targets.harnesses {
        let mut skills = BTreeSet::new();
        for path in release.files.keys() {
            let Ok(tail) = path.strip_prefix("assets/skills") else {
                continue;
            };
            let mut components = tail.components();
            if let (Some(Component::Normal(name)), Some(Component::Normal(file)), None) =
                (components.next(), components.next(), components.next())
                && file == "SKILL.md"
            {
                skills.insert(name.to_os_string());
            }
        }
        for name in skills {
            insert(
                target.skills_dir.join(&name),
                Value::Link(root.join("assets/skills").join(&name)),
                Some(target.harness),
                "skill".into(),
            )?;
        }
        if let Some(path) = &target.instruction {
            for (destination, asset) in [
                (path.clone(), "assets/instructions/AGENTS.md"),
                (
                    path.with_file_name("HARNESS.md"),
                    "assets/instructions/HARNESS.md",
                ),
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
fn owned(value: &Value) -> Result<OwnedValue, InstallError> {
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
fn matches_owned(value: &Value, expected: &OwnedValue) -> bool {
    owned(value).is_ok_and(|v| &v == expected)
}

pub fn plan_install(
    release: Release,
    targets: Targets,
    snapshots: Snapshots,
) -> Result<InstallPlan, InstallError> {
    let desired = desired(&release, &targets)?;
    let mut actions = Vec::new();
    let mut entries = Vec::new();
    let mut manual_includes = Vec::new();
    for (path, want) in &desired {
        let previous = snapshots
            .entries
            .get(path)
            .ok_or_else(|| InstallError::Drift(path.clone()))?;
        if previous.as_ref().is_none_or(|s| s.value != want.value) {
            let immutable = want.selector == "release";
            if let Some(current) = previous {
                let was_owned = snapshots.receipt.as_ref().is_some_and(|r| {
                    r.entries.iter().any(|e| {
                        e.path.to_path_buf().is_ok_and(|p| &p == path)
                            && matches_owned(&current.value, &e.expected)
                    })
                });
                let adopt_binary = targets.adopt_existing
                    && want.selector == "binary"
                    && matches!(current.value, Value::File { .. });
                if want.selector == "instruction"
                    && !was_owned
                    && matches!(current.value, Value::File { .. })
                {
                    if let Value::Link(include) = &want.value {
                        manual_includes.push(ManualInclude {
                            path: AbsolutePath::new(path)?,
                            include: AbsolutePath::new(include)?,
                        });
                    }
                    continue;
                }
                if immutable {
                    return Err(InstallError::Tampered(path.clone()));
                }
                if !was_owned && !adopt_binary {
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
                before: previous.as_ref().and_then(|s| owned(&s.value).ok()),
                after: Some(owned(&want.value)?),
                identity: identity(previous),
            });
        }
        if want.selector != "release" {
            entries.push(OwnedEntry {
                path: AbsolutePath::new(path)?,
                harness: want.harness,
                selector: want.selector.clone(),
                expected: owned(&want.value)?,
            });
        }
    }
    // Keep ownership for unselected clients and removed/edited skills so a
    // later explicit uninstall can make an unchanged-only decision.
    if let Some(previous) = &snapshots.receipt {
        for entry in &previous.entries {
            if !entries.iter().any(|e| e.path == entry.path)
                && !manual_includes.iter().any(|i| i.path == entry.path)
            {
                entries.push(entry.clone());
            }
        }
    }
    let mut harnesses: BTreeSet<_> = targets.harnesses.iter().map(|h| h.harness).collect();
    if let Some(previous) = &snapshots.receipt {
        harnesses.extend(previous.harnesses.iter().copied());
    }
    let receipt = InstallReceipt {
        schema_version: 1,
        source_digest: release.manifest.source_digest,
        manifest_digest: CanonicalV1::file_digest(&release.manifest_bytes),
        harnesses: harnesses.into_iter().collect(),
        entries,
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
    if current.as_ref().is_none_or(|s| {
        s.value
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
            before: current.as_ref().and_then(|s| owned(&s.value).ok()),
            after: Some(OwnedValue::File {
                digest: CanonicalV1::file_digest(&bytes),
                mode: 0o600,
            }),
            identity: identity(current),
        });
    }
    // Files are published before the binary, then links, then receipt, regardless
    // of user-selected destination path ordering.
    actions.sort_by_key(|a| match a.selector.as_str() {
        "release" => 0,
        "binary" => 1,
        "receipt" => 3,
        _ => 2,
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
    pub fs: &'a dyn SafeFs,
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
    if dry_run {
        return Ok(InstallOutcome {
            actions: plan.actions.clone(),
            manual_includes: plan.manual_includes.clone(),
            dry_run: true,
        });
    }
    smoke
        .check(&plan.release, context.cancel)
        .map_err(InstallError::Smoke)?;
    let fs = context.fs;
    let install_root = plan.targets.roots.state.as_path().join("install");
    let dir = directory(&install_root, true, fs)?;
    let _lock = dir.lock(
        OsStr::new("lock"),
        LockMode::Exclusive,
        context.budget,
        context.cancel,
        context.clock,
    )?;
    let parents: BTreeSet<_> = plan
        .desired
        .iter()
        .filter(|(_, d)| d.selector != "release")
        .filter_map(|(p, _)| p.parent().map(Path::to_path_buf))
        .collect();
    let mut locks = Vec::new();
    for parent in parents {
        let dir = directory(&parent, true, fs)?;
        locks.push(dir.lock(
            OsStr::new(".provenant-install.lock"),
            LockMode::Exclusive,
            context.budget,
            context.cancel,
            context.clock,
        )?);
    }
    // Validate the complete preview before the first authoritative file write.
    for (path, before) in &plan.before.entries {
        recheck(path, before, fs)?;
    }
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
            recheck(&path, before, fs)?;
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
            publish(&path, &value, before.is_some(), fs)
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
        dry_run: false,
    })
}
fn recheck(path: &Path, before: &Option<Snapshot>, fs: &dyn SafeFs) -> Result<(), InstallError> {
    if &snapshot(path, fs)? != before {
        return Err(InstallError::Drift(path.into()));
    }
    Ok(())
}
fn publish(path: &Path, value: &Value, replace: bool, fs: &dyn SafeFs) -> Result<(), InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let dir = directory(parent, true, fs)?;
    let mode = if replace {
        PublishMode::Replace
    } else {
        PublishMode::NoReplace
    };
    match value {
        Value::File {
            bytes,
            mode: permissions,
        } => durable(dir.publish(name, bytes, *permissions as u32, mode)?),
        Value::Link(target) => {
            let temporary = fs.temporary_name()?;
            dir.symlink(target, &temporary)?;
            let result = if replace {
                dir.replace(&temporary, name)
            } else {
                dir.rename_noreplace(&temporary, name)
            };
            if let Err(error) = result {
                let _ = dir.unlink(&temporary);
                return Err(error.into());
            }
            dir.sync()?;
            Ok(())
        }
        Value::Other => Err(InstallError::Conflict(path.into())),
    }
}
pub(crate) fn durable(publication: Publication) -> Result<(), InstallError> {
    match publication {
        Publication::Durable => Ok(()),
        Publication::VisibleDurabilityUnconfirmed { sync_error } => Err(sync_error.into()),
    }
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
pub(crate) fn directory(
    path: &Path,
    create: bool,
    fs: &dyn SafeFs,
) -> Result<Box<dyn SafeDir>, InstallError> {
    let mut dir = fs.root(path.is_absolute())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                dir = match dir.open_verified_directory(name) {
                    Ok(next) => next,
                    Err(error) if create && error.is_not_found() => {
                        match dir.mkdir(name, 0o700) {
                            Ok(()) => dir.sync()?,
                            Err(e) if e.is_exists() => {}
                            Err(e) => return Err(e.into()),
                        }
                        dir.open_verified_directory(name)?
                    }
                    Err(error) => return Err(error.into()),
                };
            }
            _ => return Err(InstallError::Path(path.into())),
        }
    }
    Ok(dir)
}
pub(crate) fn snapshot(path: &Path, fs: &dyn SafeFs) -> Result<Option<Snapshot>, InstallError> {
    let Some(parent) = path.parent() else {
        return Err(InstallError::Path(path.into()));
    };
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let dir = match directory(parent, false, fs) {
        Ok(dir) => dir,
        Err(InstallError::Fs(e)) if e.is_not_found() => return Ok(None),
        Err(e) => return Err(e),
    };
    let metadata = match dir.metadata_at(name, false) {
        Ok(m) => m,
        Err(e) if e.is_not_found() => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let limit = if name == "release.json" || name == "receipt.json" {
        Limits::CONFIG.max_bytes
    } else {
        FILE_LIMIT
    };
    if metadata.kind == FileKind::Regular && metadata.size > limit {
        return Err(FsError::TooLarge { limit }.into());
    }
    let value = match metadata.kind {
        FileKind::Regular => {
            let mut file = dir.open_read(name)?;
            if file.metadata()? != metadata {
                return Err(InstallError::Drift(path.into()));
            }
            let mut bytes = Vec::new();
            file.by_ref()
                .take(limit + 1)
                .read_to_end(&mut bytes)
                .map_err(|source| FsError::Io {
                    operation: "read snapshot",
                    path: path.into(),
                    source,
                })?;
            if bytes.len() as u64 > limit {
                return Err(FsError::TooLarge { limit }.into());
            }
            if file.metadata()? != metadata {
                return Err(InstallError::Drift(path.into()));
            }
            Value::File {
                bytes,
                mode: u64::from(metadata.mode & 0o7777),
            }
        }
        FileKind::Symlink => {
            let target = dir.read_link(name)?;
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
fn read_receipt(
    targets: &Targets,
    fs: &dyn SafeFs,
) -> Result<Option<InstallReceipt>, InstallError> {
    let path = targets.roots.state.as_path().join("install/receipt.json");
    match snapshot(&path, fs)? {
        None => Ok(None),
        Some(Snapshot {
            value: Value::File { bytes, mode: 0o600 },
            ..
        }) => Ok(Some(decode_receipt(&bytes)?)),
        Some(_) => Err(InstallError::Conflict(path)),
    }
}
fn decode_receipt(bytes: &[u8]) -> Result<InstallReceipt, InstallError> {
    let receipt: InstallReceipt = provenant_records::decode_v1(bytes, Limits::CONFIG)?;
    let mut paths = BTreeSet::new();
    for entry in &receipt.entries {
        if !paths.insert(entry.path.to_path_buf()?) {
            return Err(InstallError::Manifest("duplicate receipt path"));
        }
    }
    if receipt
        .entries
        .iter()
        .filter(|e| e.selector == "binary" && e.harness.is_none())
        .count()
        != 1
    {
        return Err(InstallError::Manifest("receipt requires one binary entry"));
    }
    Ok(receipt)
}
fn permitted_entry(entry: &OwnedEntry, targets: &Targets) -> Result<bool, InstallError> {
    let path = entry.path.to_path_buf()?;
    normal_absolute(&path)?;
    if entry.harness.is_none() {
        return Ok(entry.selector == "binary"
            && path == targets.bin_dir.join("provenant")
            && matches!(entry.expected, OwnedValue::File { mode: 0o755, .. }));
    }
    for h in &targets.harnesses {
        if Some(h.harness) != entry.harness {
            continue;
        }
        if entry.selector == "skill"
            && path.parent() == Some(h.skills_dir.as_path())
            && matches!(entry.expected, OwnedValue::Link { .. })
        {
            return Ok(true);
        }
        if entry.selector == "instruction"
            && h.instruction
                .as_ref()
                .is_some_and(|i| &path == i || path == i.with_file_name("HARNESS.md"))
            && matches!(entry.expected, OwnedValue::Link { .. })
        {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Read-only digest comparison, without locks, repair or initialisation.
pub fn check_install(
    targets: &Targets,
    source: Option<&Release>,
    target: &str,
    fs: &dyn SafeFs,
) -> Result<Vec<InstallFinding>, InstallError> {
    targets.validate()?;
    let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
    let Some(receipt) = read_receipt(targets, fs)? else {
        return Ok(vec![InstallFinding {
            rule: "missing_receipt".into(),
            path: AbsolutePath::new(&receipt_path)?,
        }]);
    };
    let mut findings = Vec::new();
    let release_path = targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(receipt.source_digest.to_string());
    match Release::read(&release_path, target, fs) {
        Ok(release)
            if release.manifest.source_digest == receipt.source_digest
                && CanonicalV1::file_digest(&release.manifest_bytes) == receipt.manifest_digest =>
        {
            let (bytes, mode) = release
                .files
                .get(Path::new("provenant"))
                .ok_or(InstallError::Manifest("missing executable"))?;
            let expected = OwnedValue::File {
                digest: CanonicalV1::file_digest(bytes),
                mode: *mode,
            };
            if !receipt.entries.iter().any(|entry| {
                entry.selector == "binary" && entry.harness.is_none() && entry.expected == expected
            }) {
                return Err(InstallError::Manifest(
                    "receipt binary differs from release",
                ));
            }
        }
        Ok(_) | Err(InstallError::Tampered(_)) | Err(InstallError::Manifest(_)) => {
            findings.push(InstallFinding {
                rule: "release_drift".into(),
                path: AbsolutePath::new(&release_path)?,
            })
        }
        Err(InstallError::Fs(e)) if e.is_not_found() => findings.push(InstallFinding {
            rule: "missing_release".into(),
            path: AbsolutePath::new(&release_path)?,
        }),
        Err(e) => return Err(e),
    }
    if source.is_some_and(|s| {
        s.manifest.source_digest != receipt.source_digest
            || CanonicalV1::file_digest(&s.manifest_bytes) != receipt.manifest_digest
    }) {
        findings.push(InstallFinding {
            rule: "source_digest".into(),
            path: AbsolutePath::new(&receipt_path)?,
        });
    }
    for entry in &receipt.entries {
        if !permitted_entry(entry, targets)? {
            return Err(InstallError::Conflict(entry.path.to_path_buf()?));
        }
        let path = entry.path.to_path_buf()?;
        if snapshot(&path, fs)?.is_none_or(|s| !matches_owned(&s.value, &entry.expected)) {
            findings.push(InstallFinding {
                rule: "owned_entry_drift".into(),
                path: entry.path.clone(),
            });
        }
    }
    Ok(findings)
}
/// Removes unchanged links for selected clients; keeps state, releases, edited
/// entries and the running executable. Receipt paths cannot widen authority.
pub fn uninstall(
    targets: &Targets,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
) -> Result<InstallOutcome, InstallError> {
    targets.validate()?;
    let fs = context.fs;
    let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
    let before_receipt = snapshot(&receipt_path, fs)?;
    let Some(mut receipt) = read_receipt(targets, fs)? else {
        reporter.preview(&[]).map_err(InstallError::Preview)?;
        return Ok(InstallOutcome {
            actions: Vec::new(),
            manual_includes: Vec::new(),
            dry_run: false,
        });
    };
    let mut settled = Vec::new();
    let mut removals = Vec::new();
    let mut actions = Vec::new();
    for entry in &receipt.entries {
        if entry.harness.is_none() || !permitted_entry(entry, targets)? {
            continue;
        }
        let path = entry.path.to_path_buf()?;
        let current = snapshot(&path, fs)?;
        if current.is_none() {
            settled.push(path.clone());
        }
        if let Some(before) = current
            && matches_owned(&before.value, &entry.expected)
        {
            settled.push(path.clone());
            actions.push(Action {
                path: entry.path.clone(),
                disposition: Disposition::Remove,
                selector: entry.selector.clone(),
                before: Some(entry.expected.clone()),
                after: None,
                identity: Some(EntryIdentity {
                    device: before.metadata.device,
                    inode: before.metadata.inode,
                }),
            });
            removals.push((path, Some(before)));
        }
    }
    if !settled.is_empty() {
        let mut next = receipt.clone();
        next.entries.retain(|e| {
            !settled
                .iter()
                .any(|p| e.path.to_path_buf().is_ok_and(|path| &path == p))
        });
        let bytes = provenant_records::encode(&next)?;
        actions.push(Action {
            path: AbsolutePath::new(&receipt_path)?,
            disposition: Disposition::Replace,
            selector: "receipt".into(),
            before: before_receipt.as_ref().and_then(|s| owned(&s.value).ok()),
            after: Some(OwnedValue::File {
                digest: CanonicalV1::file_digest(&bytes),
                mode: 0o600,
            }),
            identity: identity(&before_receipt),
        });
    }
    reporter.preview(&actions).map_err(InstallError::Preview)?;
    if settled.is_empty() {
        return Ok(InstallOutcome {
            actions,
            manual_includes: Vec::new(),
            dry_run: false,
        });
    }
    let dir = directory(&targets.roots.state.as_path().join("install"), false, fs)?;
    let _lock = dir.lock(
        OsStr::new("lock"),
        LockMode::Exclusive,
        context.budget,
        context.cancel,
        context.clock,
    )?;
    let parents: BTreeSet<_> = removals
        .iter()
        .filter_map(|(p, _)| p.parent().map(Path::to_path_buf))
        .collect();
    let mut locks = Vec::new();
    for p in parents {
        locks.push(directory(&p, false, fs)?.lock(
            OsStr::new(".provenant-install.lock"),
            LockMode::Exclusive,
            context.budget,
            context.cancel,
            context.clock,
        )?);
    }
    recheck(&receipt_path, &before_receipt, fs)?;
    for (path, before) in &removals {
        recheck(path, before, fs)?;
    }
    let mut completed = Vec::new();
    for (index, (path, before)) in removals.iter().enumerate() {
        let result = (|| {
            if context.cancel.is_cancelled() {
                return Err(InstallError::Fs(FsError::Cancelled { path: path.clone() }));
            }
            recheck(path, before, fs)?;
            let parent = path
                .parent()
                .ok_or_else(|| InstallError::Path(path.clone()))?;
            let name = path
                .file_name()
                .ok_or_else(|| InstallError::Path(path.clone()))?;
            let dir = directory(parent, false, fs)?;
            dir.unlink(name)?;
            dir.sync()?;
            Ok(())
        })();
        if let Err(source) = result {
            return Err(InstallError::Partial {
                completed,
                remaining: actions.iter().skip(index).cloned().collect(),
                source: Box::new(source),
            });
        }
        if let Some(action) = actions.get(index) {
            completed.push(action.clone());
        }
    }
    receipt.entries.retain(|e| {
        !settled
            .iter()
            .any(|p| e.path.to_path_buf().is_ok_and(|path| &path == p))
    });
    let result = recheck(&receipt_path, &before_receipt, fs).and_then(|()| {
        publish(
            &receipt_path,
            &Value::File {
                bytes: provenant_records::encode(&receipt)?,
                mode: 0o600,
            },
            true,
            fs,
        )
    });
    if let Err(source) = result {
        return Err(InstallError::Partial {
            completed,
            remaining: actions.last().cloned().into_iter().collect(),
            source: Box::new(source),
        });
    }
    if let Some(action) = actions.last() {
        completed.push(action.clone());
    }
    Ok(InstallOutcome {
        actions: completed,
        manual_includes: Vec::new(),
        dry_run: false,
    })
}

#[cfg(test)]
#[path = "managed_tests.rs"]
mod tests;
