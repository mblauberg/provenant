//! Managed files use supervisor capabilities exclusively. Plans retain bounded
//! snapshots; every publication rechecks identity and bytes under owner locks.
use crate::inspection::InspectionEvidence;
use crate::release::{FILE_LIMIT, Release};
use provenant_core::{CancelToken, Clock, RelativePath, Roots, WaitBudget};
use provenant_records::{
    AbsolutePath, CanonicalV1, FileDigest, Harness, InstallReceipt, Limits, OwnedEntry, OwnedValue,
    PathWire, ResolvedHarnessTarget, VersionInfo,
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
    fn check(&self, release: &Release, cancel: &CancelToken) -> std::io::Result<VersionInfo>;
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
        let manifest_path = installed.join("release.json");
        let manifest_snapshot = snapshot(&manifest_path, fs)?;
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
        validate_release_inventory(release, &installed, fs, false)?;

        let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
        let receipt_snapshot = snapshot(&receipt_path, fs)?;
        let receipt = match &receipt_snapshot {
            None => None,
            Some(Snapshot {
                value: Value::File { bytes, mode: 0o600 },
                ..
            }) => Some(decode_receipt(bytes, targets.roots.state.as_path())?),
            Some(_) => return Err(InstallError::Conflict(receipt_path.clone())),
        };
        let selected: BTreeSet<_> = targets.harnesses.iter().map(|h| h.harness).collect();
        let mut paths: BTreeSet<_> = desired.keys().cloned().collect();
        if let Some(previous) = &receipt {
            for entry in &previous.entries {
                if entry.selector == "binary"
                    || entry
                        .harness
                        .is_some_and(|harness| selected.contains(&harness))
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
                snapshot(&path, fs)?
            };
            if completed && path.starts_with(&installed) && current.is_none() {
                return Err(InstallError::Tampered(path));
            }
            entries.insert(path, current);
        }
        entries.insert(receipt_path, receipt_snapshot);
        Ok(Self { entries, receipt })
    }
}

fn validate_release_inventory(
    release: &Release,
    installed: &Path,
    fs: &dyn SafeFs,
    remove_staging: bool,
) -> Result<(), InstallError> {
    match crate::release::inventory_with_cleanup(installed, fs, remove_staging) {
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
    if !release.files.contains_key(Path::new("bin/provenant")) {
        return Err(InstallError::Manifest("missing executable"));
    }
    insert(
        targets.bin_dir.join("provenant"),
        Value::Link(root.join("bin/provenant")),
        None,
        "binary".into(),
    )?;
    for target in &targets.harnesses {
        let mut skills = BTreeSet::new();
        for path in release.files.keys() {
            let Ok(tail) = path.strip_prefix("skills") else {
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
                Value::Link(root.join("skills").join(&name)),
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
    let selected: BTreeSet<_> = targets.harnesses.iter().map(|h| h.harness).collect();
    let mut actions = Vec::new();
    let mut entries = Vec::new();
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
                    if let Value::Link(include) = &want.value {
                        manual_includes.push(ManualInclude {
                            path: AbsolutePath::new(path)?,
                            include: AbsolutePath::new(include)?,
                        });
                    }
                    continue;
                }
                let release_root = targets
                    .roots
                    .state
                    .as_path()
                    .join("releases")
                    .join(release.manifest.source_digest.to_string());
                let complete = snapshots
                    .entries
                    .get(&release_root.join("release.json"))
                    .is_some_and(Option::is_some);
                if immutable && complete {
                    return Err(InstallError::Tampered(path.clone()));
                }
                if !immutable && !was_owned && !adopt_binary {
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

    // Selected harnesses and the stable binary drop old owned paths that are
    // no longer desired. Exact old links become Remove actions; edited links
    // are relinquished as unmanaged state. Missing links are already settled.
    if let Some(previous) = &snapshots.receipt {
        for entry in &previous.entries {
            if entries.iter().any(|current| current.path == entry.path)
                || manual_includes
                    .iter()
                    .any(|include| include.path == entry.path)
            {
                continue;
            }
            if entry.selector != "binary" {
                let Some(harness) = entry.harness else {
                    continue;
                };
                if !selected.contains(&harness) {
                    entries.push(entry.clone());
                    continue;
                }
            }
            let path = entry.path.to_path_buf()?;
            let current = snapshots
                .entries
                .get(&path)
                .ok_or_else(|| InstallError::Drift(path.clone()))?;
            if let Some(before) = current
                && matches_owned(&before.value, &entry.expected)
            {
                actions.push(Action {
                    path: entry.path.clone(),
                    disposition: Disposition::Remove,
                    selector: entry.selector.clone(),
                    before: Some(entry.expected.clone()),
                    after: None,
                    identity: identity(current),
                });
            }
        }
    }

    let mut resolved_targets = BTreeMap::new();
    if let Some(previous) = &snapshots.receipt {
        for target in &previous.harness_targets {
            if !selected.contains(&target.harness) {
                resolved_targets.insert(target.harness, target.clone());
            }
        }
    }
    for target in &targets.harnesses {
        resolved_targets.insert(
            target.harness,
            ResolvedHarnessTarget {
                harness: target.harness,
                skills_dir: AbsolutePath::new(&target.skills_dir)?,
                instruction: target
                    .instruction
                    .as_deref()
                    .map(AbsolutePath::new)
                    .transpose()?,
            },
        );
    }
    let harnesses: Vec<_> = resolved_targets.keys().copied().collect();
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
            "receipt" => 4,
            _ => 3,
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
    crate::release::verify_binary_identity(&plan.release, smoke, context.cancel)?;
    let fs = context.fs;
    let install_root = plan.targets.roots.state.as_path().join("install");
    let dir = directory(&install_root, true, fs)?;
    let _lock = dir.lock(
        OsStr::new("install.lock"),
        LockMode::Exclusive,
        context.budget,
        context.cancel,
        context.clock,
    )?;
    // Validate the complete preview before the first authoritative file write.
    for (path, before) in &plan.before.entries {
        recheck(path, before, fs)?;
    }
    let release_root = plan
        .targets
        .roots
        .state
        .as_path()
        .join("releases")
        .join(plan.release.manifest.source_digest.to_string());
    let unfinished = plan
        .before
        .entries
        .get(&release_root.join("release.json"))
        .is_some_and(Option::is_none);
    // Only the lock holder may collect abandoned staging files. A concurrent
    // preview ignores those names so it cannot interrupt a live publication.
    validate_release_inventory(&plan.release, &release_root, fs, unfinished)?;
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
            if action.disposition == Disposition::Remove {
                remove_entry(&path, fs)
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
                publish(&path, &value, before.is_some(), fs)
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
        } => durable(dir.atomic_write(
            name,
            ".provenant-staging.",
            bytes,
            *permissions as u32,
            mode,
        )?),
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
fn remove_entry(path: &Path, fs: &dyn SafeFs) -> Result<(), InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let dir = directory(parent, false, fs)?;
    dir.unlink(name)?;
    dir.sync()?;
    Ok(())
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
    snapshot_inspected(path, fs, None)
}
pub(crate) fn snapshot_inspected(
    path: &Path,
    fs: &dyn SafeFs,
    mut evidence: Option<&mut InspectionEvidence>,
) -> Result<Option<Snapshot>, InstallError> {
    let Some(parent) = path.parent() else {
        return Err(InstallError::Path(path.into()));
    };
    let name = path
        .file_name()
        .ok_or_else(|| InstallError::Path(path.into()))?;
    let dir = match directory(parent, false, fs) {
        Ok(dir) => dir,
        Err(InstallError::Fs(e)) if e.is_not_found() => {
            if let Some(evidence) = evidence.as_deref_mut() {
                evidence.observe(b"snapshot", path, None);
            }
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let metadata = match dir.metadata_at(name, false) {
        Ok(m) => m,
        Err(e) if e.is_not_found() => {
            if let Some(evidence) = evidence.as_deref_mut() {
                evidence.observe(b"snapshot", path, None);
            }
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    if let Some(evidence) = evidence.as_deref_mut() {
        evidence.metadata(path, metadata);
    }
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
fn read_receipt(
    targets: &Targets,
    fs: &dyn SafeFs,
) -> Result<Option<InstallReceipt>, InstallError> {
    read_receipt_inspected(targets, fs, None)
}
fn read_receipt_inspected(
    targets: &Targets,
    fs: &dyn SafeFs,
    evidence: Option<&mut InspectionEvidence>,
) -> Result<Option<InstallReceipt>, InstallError> {
    let path = targets.roots.state.as_path().join("install/receipt.json");
    match snapshot_inspected(&path, fs, evidence)? {
        None => Ok(None),
        Some(Snapshot {
            value: Value::File { bytes, mode: 0o600 },
            ..
        }) => Ok(Some(decode_receipt(&bytes, targets.roots.state.as_path())?)),
        Some(_) => Err(InstallError::Conflict(path)),
    }
}
fn decode_receipt(bytes: &[u8], state_root: &Path) -> Result<InstallReceipt, InstallError> {
    let receipt: InstallReceipt = provenant_records::decode_v1(bytes, Limits::CONFIG)?;
    normal_absolute(&receipt.bin_dir.to_path_buf()?)?;
    let mut harnesses = BTreeSet::new();
    for harness in &receipt.harnesses {
        if !harnesses.insert(*harness) {
            return Err(InstallError::Manifest("duplicate receipt harness"));
        }
    }
    let mut target_harnesses = BTreeSet::new();
    for target in &receipt.harness_targets {
        if !target_harnesses.insert(target.harness) {
            return Err(InstallError::Manifest("duplicate receipt target"));
        }
        normal_absolute(&target.skills_dir.to_path_buf()?)?;
        if let Some(path) = &target.instruction {
            normal_absolute(&path.to_path_buf()?)?;
        }
    }
    if harnesses != target_harnesses {
        return Err(InstallError::Manifest(
            "receipt harness targets do not match harnesses",
        ));
    }
    let mut paths = BTreeSet::new();
    for entry in &receipt.entries {
        if !paths.insert(entry.path.to_path_buf()?) {
            return Err(InstallError::Manifest("duplicate receipt path"));
        }
        if !permitted_entry(entry, &receipt, state_root)? {
            return Err(InstallError::Manifest(
                "receipt entry outside resolved targets",
            ));
        }
    }
    if receipt
        .entries
        .iter()
        .filter(|entry| entry.selector == "binary" && entry.harness.is_none())
        .count()
        != 1
    {
        return Err(InstallError::Manifest("receipt requires one binary entry"));
    }
    Ok(receipt)
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
        let binary = receipt.bin_dir.to_path_buf()?.join("provenant");
        let expected_target = state_root
            .join("releases")
            .join(receipt.source_digest.to_string())
            .join("bin/provenant");
        return Ok(entry.selector == "binary"
            && path == binary
            && matches!(
                &entry.expected,
                OwnedValue::Link { target }
                    if target.to_path_buf()? == expected_target
            ));
    }
    let Some(target) = receipt
        .harness_targets
        .iter()
        .find(|target| Some(target.harness) == entry.harness)
    else {
        return Ok(false);
    };
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
                        )
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
    fs: &dyn SafeFs,
    smoke: &dyn ReleaseSmoke,
    cancel: &CancelToken,
) -> Result<InstallInspection, InstallError> {
    let mut evidence = InspectionEvidence::default();
    // Requested releases are immutable verified snapshots, supplied by caller.
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
        match check_install_inspected(targets, source, target, fs, smoke, cancel, &mut evidence) {
            Ok(findings) => (findings, None),
            Err(
                error @ (InstallError::Decode(_)
                | InstallError::Manifest(_)
                | InstallError::Tampered(_)
                | InstallError::Conflict(_)
                | InstallError::Platform
                | InstallError::Path(_)),
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
    fs: &dyn SafeFs,
    smoke: &dyn ReleaseSmoke,
    cancel: &CancelToken,
    evidence: &mut InspectionEvidence,
) -> Result<Vec<InstallFinding>, InstallError> {
    let receipt_path = targets.roots.state.as_path().join("install/receipt.json");
    let Some(receipt) = read_receipt_inspected(targets, fs, Some(evidence))? else {
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
    match Release::read_inspected(&release_path, target, fs, Some(evidence)) {
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
                if !receipt.entries.iter().any(|entry| {
                    entry.selector == "binary"
                        && entry.harness.is_none()
                        && entry.expected == expected
                }) {
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
            })
        }
        Err(InstallError::Fs(error)) if error.is_not_found() => findings.push(InstallFinding {
            rule: "missing_release".into(),
            path: AbsolutePath::new(&release_path)?,
        }),
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
        if !permitted_entry(entry, &receipt, targets.roots.state.as_path())? {
            return Err(InstallError::Conflict(entry.path.to_path_buf()?));
        }
        let path = entry.path.to_path_buf()?;
        if snapshot_inspected(&path, fs, Some(evidence))?
            .is_none_or(|current| !matches_owned(&current.value, &entry.expected))
        {
            findings.push(InstallFinding {
                rule: "owned_entry_drift".into(),
                path: entry.path.clone(),
            });
        }
    }
    Ok(findings)
}

/// Removes unchanged selected harness entries, preserves user edits, and
/// forgets the selected harnesses in the receipt. State and the stable command
/// remain installed.
pub fn uninstall(
    targets: &Targets,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
) -> Result<InstallOutcome, InstallError> {
    targets.validate()?;
    let fs = context.fs;
    let state_root = targets.roots.state.as_path();
    let receipt_path = state_root.join("install/receipt.json");
    let before_receipt = snapshot(&receipt_path, fs)?;
    let Some(mut receipt) = read_receipt(targets, fs)? else {
        reporter.preview(&[]).map_err(InstallError::Preview)?;
        return Ok(InstallOutcome {
            actions: Vec::new(),
            manual_includes: Vec::new(),
            dry_run: false,
        });
    };
    let requested: BTreeSet<_> = targets
        .harnesses
        .iter()
        .map(|target| target.harness)
        .collect();
    let registered: BTreeSet<_> = receipt.harnesses.iter().copied().collect();
    let selected: BTreeSet<_> = requested.intersection(&registered).copied().collect();
    if selected.is_empty() {
        reporter.preview(&[]).map_err(InstallError::Preview)?;
        return Ok(InstallOutcome {
            actions: Vec::new(),
            manual_includes: Vec::new(),
            dry_run: false,
        });
    }

    let mut removals = Vec::new();
    let mut actions = Vec::new();
    for entry in &receipt.entries {
        if !entry
            .harness
            .is_some_and(|harness| selected.contains(&harness))
        {
            continue;
        }
        if !permitted_entry(entry, &receipt, state_root)? {
            return Err(InstallError::Conflict(entry.path.to_path_buf()?));
        }
        let path = entry.path.to_path_buf()?;
        if let Some(before) = snapshot(&path, fs)?
            && matches_owned(&before.value, &entry.expected)
        {
            actions.push(Action {
                path: entry.path.clone(),
                disposition: Disposition::Remove,
                selector: entry.selector.clone(),
                before: Some(entry.expected.clone()),
                after: None,
                identity: identity(&Some(before.clone())),
            });
            removals.push((path, Some(before)));
        }
    }

    receipt.entries.retain(|entry| {
        !entry
            .harness
            .is_some_and(|harness| selected.contains(&harness))
    });
    receipt
        .harnesses
        .retain(|harness| !selected.contains(harness));
    receipt
        .harness_targets
        .retain(|target| !selected.contains(&target.harness));
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
    });

    reporter.preview(&actions).map_err(InstallError::Preview)?;
    let install_dir = directory(&state_root.join("install"), false, fs)?;
    let _lock = install_dir.lock(
        OsStr::new("install.lock"),
        LockMode::Exclusive,
        context.budget,
        context.cancel,
        context.clock,
    )?;
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
            remove_entry(path, fs)
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
    let result = recheck(&receipt_path, &before_receipt, fs).and_then(|()| {
        publish(
            &receipt_path,
            &Value::File {
                bytes: receipt_bytes,
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
    if let Some(receipt_action) = actions.last() {
        completed.push(receipt_action.clone());
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
