//! Slot reset and maintenance seam; S4 supplies reset and garbage collection.

use super::{
    BuildError, InfraKind, JobOutcome, Reset,
    slots::{SlotPaths, SlotRecord, SlotState},
};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::SystemTime};

/// Pre-job action selected by the clock guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreJob {
    Proceed,
    LightReset,
    FullReset,
}

/// A backwards clock invalidates Cargo's timestamp freshness assumptions.
pub fn before_job(record: &mut SlotRecord, now_unix_ms: u64) -> PreJob {
    if record
        .last_job
        .as_ref()
        .is_some_and(|job| now_unix_ms < job.finished_unix_ms)
    {
        PreJob::FullReset
    } else {
        PreJob::Proceed
    }
}

/// Count infrastructure failures in the trailing 24-hour window, excluding cancellation.
pub fn after_job(record: &mut SlotRecord, outcome: &JobOutcome, now_unix_ms: u64) -> Reset {
    let cutoff = now_unix_ms.saturating_sub(86_400_000);
    record
        .health
        .infra_failures_unix_ms
        .retain(|time| *time >= cutoff && *time <= now_unix_ms);
    if outcome.cancelled || outcome.infra.is_none() || outcome.infra == Some(InfraKind::Clock) {
        return Reset::None;
    }
    record.health.infra_failures_unix_ms.push(now_unix_ms);
    match record.health.infra_failures_unix_ms.len() {
        1 => Reset::Light,
        2 => Reset::Full,
        _ => {
            record.health.state = SlotState::Quarantined;
            record.health.quarantine_reason = Some(format!(
                "repeated infrastructure failures: {:?}",
                outcome.infra
            ));
            Reset::Quarantine
        }
    }
}

/// Remove untracked and ignored changes from a slot checkout.
pub fn light_reset(slot: &SlotPaths) -> Result<(), BuildError> {
    for args in [
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "checkout",
            "--force",
            "--detach",
            "HEAD",
        ][..],
        &["clean", "-ffdx"][..],
    ] {
        crate::git_context::seed_git(&slot.repo, args).map_err(|error| BuildError::Infra {
            kind: InfraKind::Git,
            detail: error.to_string(),
        })?;
    }
    Ok(())
}

/// Remove fixed children of a verified slot; the slot executor recreates repo.
/// Caller must hold the slot lock for the duration.
pub fn full_reset(slot: &SlotPaths) -> Result<(), BuildError> {
    use provenant_os::fs::{Directory, FileKind, PayloadTree};
    use std::ffi::OsStr;
    let parent = slot
        .root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| BuildError::Config("invalid slot root".into()))?;
    if parent.file_name() != Some(OsStr::new("provenant-slots"))
        || slot
            .root
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|n| n.parse::<u64>().ok())
            .is_none()
        || slot.target != slot.root.join("target")
        || slot.repo != slot.root.join("repo")
    {
        return Err(BuildError::Config(
            "refusing reset outside slot cache".into(),
        ));
    }
    let cache = Directory::open_anchor(parent).map_err(|e| BuildError::Infra {
        kind: InfraKind::SlotLayout,
        detail: e.to_string(),
    })?;
    let project_name = slot
        .root
        .parent()
        .and_then(Path::file_name)
        .ok_or_else(|| BuildError::Config("invalid project slot root".into()))?;
    let slot_name = slot
        .root
        .file_name()
        .ok_or_else(|| BuildError::Config("invalid slot index".into()))?;
    let project = cache
        .open_directory(project_name)
        .map_err(|e| BuildError::Infra {
            kind: InfraKind::SlotLayout,
            detail: e.to_string(),
        })?;
    let root = project
        .open_directory(slot_name)
        .map_err(|e| BuildError::Infra {
            kind: InfraKind::SlotLayout,
            detail: e.to_string(),
        })?;
    for name in ["target", "repo"] {
        let name = OsStr::new(name);
        let metadata = match root.metadata_at(name, false) {
            Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
            Err(e) if e.is_not_found() => continue,
            Ok(_) => return Err(BuildError::Config("slot child is not a directory".into())),
            Err(e) => {
                return Err(BuildError::Infra {
                    kind: InfraKind::Disk,
                    detail: e.to_string(),
                });
            }
        };
        let opened = root.open_directory(name).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        opened.make_private().map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        let private = opened.metadata().map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        if !metadata.same_identity(&private) {
            return Err(BuildError::Config("slot child changed during reset".into()));
        }
        drop(opened);
        let tree = PayloadTree::claim(&root, name, &private).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        remove_contents(&tree, 0).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e,
        })?;
        drop(tree);
        root.rmdir(name).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
    }
    Ok(())
}

fn remove_contents(tree: &provenant_os::fs::PayloadTree, depth: usize) -> Result<(), String> {
    use provenant_os::fs::FileKind;
    if depth >= 128 {
        return Err("slot tree nesting too deep".into());
    }
    tree.make_writable().map_err(|e| e.to_string())?;
    for entry in tree.entries().map_err(|e| e.to_string())? {
        let name = entry.map_err(|e| e.to_string())?;
        let meta = tree.metadata_at(&name).map_err(|e| e.to_string())?;
        if meta.kind == FileKind::Directory {
            let child = tree.child(&name).map_err(|e| e.to_string())?;
            remove_contents(&child, depth + 1)?;
            drop(child);
            tree.rmdir(&name).map_err(|e| e.to_string())?;
        } else {
            tree.unlink(&name).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Result of slot and store garbage collection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    pub removed_slots: u64,
    pub removed_bytes: u64,
    pub warnings: Vec<String>,
}

/// Report that collection is unavailable until store and slot custody integrate.
pub fn gc(_home: &Path, _state: &Path, _limit: u64, _now: SystemTime) -> GcReport {
    GcReport {
        warnings: vec!["build GC is unavailable: slot and store custody are not integrated".into()],
        ..GcReport::default()
    }
}

/// One observed slot row for the status command.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotStatus {
    pub project_key: String,
    pub index: u64,
    pub allocated_bytes: u64,
    pub health: String,
    pub last_job: Option<super::slots::SlotJobMark>,
}

/// Read slot records and allocated sizes without following symlinks.
pub fn status(home: &Path, _state: &Path) -> Vec<SlotStatus> {
    let root = super::paths::slot_cache_root(home);
    let mut rows = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return rows;
    };
    for project in projects.flatten() {
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let project_key = project.file_name().to_string_lossy().into_owned();
        let Ok(slots) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in slots.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(index) = entry.file_name().to_string_lossy().parse::<u64>() else {
                continue;
            };
            let record: Option<SlotRecord> = std::fs::read(entry.path().join("slot.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            rows.push(SlotStatus {
                project_key: project_key.clone(),
                index,
                allocated_bytes: allocated(&entry.path()),
                health: record
                    .as_ref()
                    .map_or("unknown", |r| match r.health.state {
                        SlotState::Ok => "ok",
                        SlotState::Quarantined => "quarantined",
                    })
                    .into(),
                last_job: record.and_then(|r| r.last_job),
            });
        }
    }
    rows.sort_by(|a, b| (&a.project_key, a.index).cmp(&(&b.project_key, b.index)));
    rows
}

/// Allocated bytes occupied by the build stores.
pub fn store_bytes(state: &Path) -> u64 {
    allocated(&super::paths::store_root(state))
}

fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let mut total = meta.blocks().saturating_mul(512);
    if meta.file_type().is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.flatten() {
            total = total.saturating_add(allocated(&entry.path()));
        }
    }
    total
}

/// Result of a maintenance/prewarm request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainOutcome {
    pub ran: bool,
    pub warning: Option<String>,
}

/// Report unavailable until non-blocking slot maintenance is integrated.
pub fn maintain(_state: &Path, _home: &Path, _project: &Path, _now: SystemTime) -> MaintainOutcome {
    MaintainOutcome {
        ran: false,
        warning: Some("build maintenance is unavailable: slot execution is not integrated".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_ladder_window_and_cancellation() {
        let mut record = SlotRecord::default();
        let mut outcome = super::super::report::test_row();
        outcome.infra = Some(InfraKind::Git);
        assert_eq!(after_job(&mut record, &outcome, 1), Reset::Light);
        outcome.cancelled = true;
        assert_eq!(after_job(&mut record, &outcome, 2), Reset::None);
        outcome.cancelled = false;
        assert_eq!(after_job(&mut record, &outcome, 3), Reset::Full);
        assert_eq!(after_job(&mut record, &outcome, 4), Reset::Quarantine);
        assert_eq!(record.health.state, SlotState::Quarantined);
        let mut record = SlotRecord::default();
        assert_eq!(after_job(&mut record, &outcome, 1), Reset::Light);
        assert_eq!(after_job(&mut record, &outcome, 86_400_002), Reset::Light);
    }
    #[test]
    fn clock_rollback_requires_full_reset_without_ladder() {
        let mut record = SlotRecord {
            last_job: Some(super::super::slots::SlotJobMark {
                snapshot: "a".into(),
                base: None,
                owner: "lane".into(),
                finished_unix_ms: 100,
            }),
            ..SlotRecord::default()
        };
        assert_eq!(before_job(&mut record, 99), PreJob::FullReset);
        assert_eq!(before_job(&mut record, 100), PreJob::Proceed);
        let mut outcome = super::super::report::test_row();
        outcome.infra = Some(InfraKind::Clock);
        assert_eq!(after_job(&mut record, &outcome, 99), Reset::None);
        assert!(record.health.infra_failures_unix_ms.is_empty());
    }
}
