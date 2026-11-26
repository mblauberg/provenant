//! Install-time migration of non-SQLite state only: files, ledgers, receipts
//! and metadata that no SQLite store owns. SQLite stores already migrate on
//! writable open (`provenant_sqlite::Migration` in `crates/sqlite/src/schema.rs`,
//! specified in `docs/specs/state.md`); this runner never duplicates or wraps
//! that mechanism.
//!
//! Every step not already recorded in the install receipt runs once, in
//! declaration order, after the live-attempt advisory and before any release
//! is published. Its id is recorded only after it succeeds. A failed step, or
//! one that still has warnings for entries it could not migrate, warns with the
//! state path and the install proceeds; the id stays unrecorded so the next
//! install retries the idempotent step.
use provenant_config::PortEvidenceError;
use provenant_os::fs::{Directory, FsError, PublishMode};
use provenant_records::migrate::{Migration, MigrationError, MigrationReport};
use provenant_records::warning::{self, MIGRATION_STEP_FAILED, Warning};
use provenant_records::{InstallReceipt, Limits};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Ordered steps for this release. Steps are deleted in the release after the
/// one that ships them.
const STEPS: &[&dyn Migration] = &[
    &HostProjectDirs,
    &AttemptScratchMove,
    &PortSourceSpellings,
    &PortConfigSourceSpellings,
    &RetireWarningMarkers,
    &UsageLedgerV4,
];

/// `builtin` was the old spelling of the compiled-default layer. The shared
/// vocabulary spells it `default`; rewrite it in every file-backed record and
/// bump the record's schema version so an old writer cannot be read as new.
const OLD_SOURCE: &str = "builtin";
const NEW_SOURCE: &str = "default";
const OLD_PORT_RECEIPT_VERSION: u64 = 2;
const NEW_PORT_RECEIPT_VERSION: u64 = 3;
const OLD_PORTS_VERSION: u64 = 2;
const NEW_PORTS_VERSION: u64 = 3;
/// Length of the obsolete 64-hex `project_key` directory name.
const OLD_PROJECT_KEY_LEN: usize = 64;
/// A ProjectId spelling is the first 16 bytes of that same digest, in hex.
const PROJECT_ID_HEX_LEN: usize = 32;

fn is_old_project_key(name: &str) -> bool {
    name.len() == OLD_PROJECT_KEY_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Host-project settings were once filed under `projects/<project_key>`, the
/// 64-hex SHA-256 of the canonical common Git dir. Project bindings key the same
/// directory by ProjectId: the first 16 bytes of that digest. A bound project
/// already owns the target directory (it holds its binding record), so merge
/// the old tree into it entry by entry instead of refusing to touch either.
struct HostProjectDirs;

/// A clash keeps both entries and names both paths plus the manual repair.
fn host_project_clash(source: &Path, target: &Path) -> Warning {
    Warning::new(
        MIGRATION_STEP_FAILED,
        format!(
            "host-project entry clash: {} and {} both exist; the conflicting entry was left in place",
            source.display(),
            target.display()
        ),
     provenant_records::warning::detected_source(MIGRATION_STEP_FAILED))
    .subject(source.display().to_string())
    .repair(format!(
        "compare {} with {}, copy the values you need into {}, then remove {} and rerun `provenant install`",
        source.display(),
        target.display(),
        target.display(),
        source.display()
    ))
}

/// Move one directory entry to `target`, never replacing an existing entry.
/// A check-then-`rename` sequence loses a file a concurrent writer creates in
/// the window between them; `rename_noreplace` makes the no-clobber decision
/// atomic for both files and directories. Returns `false` when the destination
/// already existed, in which case both entries are kept.
fn publish_no_clobber(source: &Path, target: &Path) -> Result<bool, MigrationError> {
    let source_parent = source
        .parent()
        .ok_or_else(|| MigrationError::new(format!("{}: no parent directory", source.display())))?;
    let target_parent = target
        .parent()
        .ok_or_else(|| MigrationError::new(format!("{}: no parent directory", target.display())))?;
    let source_name = source
        .file_name()
        .ok_or_else(|| MigrationError::new(format!("{}: no file name", source.display())))?;
    let target_name = target
        .file_name()
        .ok_or_else(|| MigrationError::new(format!("{}: no file name", target.display())))?;
    let from = Directory::open_anchor(source_parent)
        .map_err(|error| MigrationError::new(format!("{}: {error}", source_parent.display())))?;
    let to = Directory::open_anchor(target_parent)
        .map_err(|error| MigrationError::new(format!("{}: {error}", target_parent.display())))?;
    match from.rename_noreplace(source_name, &to, target_name) {
        Ok(()) => Ok(true),
        Err(FsError::Rename { source: error, .. }) if error.kind() == ErrorKind::AlreadyExists => {
            Ok(false)
        }
        Err(error) => Err(MigrationError::new(format!(
            "{} -> {}: {error}",
            source.display(),
            target.display()
        ))),
    }
}

/// Move every non-clashing entry from `from` into the existing `to`. Recurse
/// when both sides hold a directory of the same name; a true clash leaves the
/// old entry in place and warns. Returns whether nothing was left unmigrated.
fn merge_project_dir(
    from: &Path,
    to: &Path,
    report: &mut MigrationReport,
) -> Result<bool, MigrationError> {
    let mut complete = true;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        match std::fs::symlink_metadata(&target) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                // Publish atomically: a concurrent writer may create `target`
                // after the check above. A clash keeps both entries and warns.
                if publish_no_clobber(&source, &target)? {
                    report.changed.push(target.display().to_string());
                } else {
                    report.warnings.push(host_project_clash(&source, &target));
                    complete = false;
                }
            }
            Ok(_) => {
                let both_dirs = entry.file_type()?.is_dir()
                    && std::fs::symlink_metadata(&target)?.file_type().is_dir();
                if both_dirs {
                    complete &= merge_project_dir(&source, &target, report)?;
                } else {
                    report.warnings.push(host_project_clash(&source, &target));
                    complete = false;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    if complete && std::fs::read_dir(from)?.next().is_none() {
        std::fs::remove_dir(from)?;
    }
    Ok(complete)
}

impl Migration for HostProjectDirs {
    fn id(&self) -> &'static str {
        "host-project-dirs"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let projects = state_root.join("projects");
        let entries = match std::fs::read_dir(&projects) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_old_project_key(&name) {
                continue;
            }
            let project_id = &name[..PROJECT_ID_HEX_LEN];
            if provenant_core::ProjectId::parse(project_id).is_err() {
                continue;
            }
            let from = entry.path();
            let to = projects.join(project_id);
            match std::fs::symlink_metadata(&to) {
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    // A writer may create the target after the check above.
                    // Publish without replacing it; if it appeared, merge as
                    // we would for any other existing target directory.
                    if publish_no_clobber(&from, &to)? {
                        report.changed.push(to.display().to_string());
                    } else if std::fs::symlink_metadata(&to)?.file_type().is_dir() {
                        merge_project_dir(&from, &to, &mut report)?;
                    } else {
                        report.warnings.push(host_project_clash(&from, &to));
                    }
                }
                Ok(metadata) if metadata.file_type().is_dir() => {
                    // The binding record lives here; merge around it. A clash
                    // leaves the old entry behind, which keeps the step pending.
                    merge_project_dir(&from, &to, &mut report)?;
                }
                Ok(_) => report.warnings.push(host_project_clash(&from, &to)),
                Err(error) => return Err(error.into()),
            }
        }
        report.changed.sort();
        Ok(report)
    }
}

/// Lane scratch used to live at `attempts/<id>/tmp`, inside the attempt record.
/// Live and retained attempts from an earlier release still hold unimported Git
/// objects there, so move each one to the sibling scratch root before the new
/// readers run (#3947). Same volume: rename only, never copy-then-delete. A
/// conflict leaves both paths and warns, and the step stays pending.
struct AttemptScratchMove;

impl Migration for AttemptScratchMove {
    fn id(&self) -> &'static str {
        "attempt-scratch-move"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let attempts = state_root.join("attempts");
        let entries = match std::fs::read_dir(&attempts) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if provenant_core::AttemptId::parse(&name).is_err() {
                continue;
            }
            let source = entry.path().join("tmp");
            let metadata = match std::fs::symlink_metadata(&source) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_dir() {
                report.warnings.push(
                    Warning::new(
                        MIGRATION_STEP_FAILED,
                        format!(
                            "attempt scratch entry {} is not a directory; left in place",
                            source.display()
                        ),
                     provenant_records::warning::detected_source(MIGRATION_STEP_FAILED))
                    .subject(source.display().to_string())
                    .repair("inspect the entry, move any objects you need, then rerun `provenant install`"),
                );
                continue;
            }
            let target_parent = state_root.join(provenant_run::ATTEMPT_SCRATCH).join(&name);
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(&target_parent)?;
            let target = target_parent.join("tmp");
            if publish_no_clobber(&source, &target)? {
                report.changed.push(target.display().to_string());
            } else {
                report.warnings.push(
                    Warning::new(
                        MIGRATION_STEP_FAILED,
                        format!(
                            "attempt scratch clash: {} and {} both exist; both were left in place",
                            source.display(),
                            target.display()
                        ),
                     provenant_records::warning::detected_source(MIGRATION_STEP_FAILED))
                    .subject(source.display().to_string())
                    .repair(format!(
                        "compare {} with {}, keep the objects you need, remove the stale path, then rerun `provenant install`",
                        source.display(),
                        target.display()
                    )),
                );
            }
        }
        report.changed.sort();
        Ok(report)
    }
}

/// Rewrite one JSON file in place, returning whether its bytes changed.
fn rewrite_json(
    path: &Path,
    mut edit: impl FnMut(&mut Value) -> bool,
) -> Result<bool, MigrationError> {
    let bytes = std::fs::read(path)?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        MigrationError::new(format!("{}: invalid JSON: {error}", path.display()))
    })?;
    if !edit(&mut value) {
        return Ok(false);
    }
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| MigrationError::new(format!("{}: encode: {error}", path.display())))?;
    let directory = path
        .parent()
        .ok_or_else(|| MigrationError::new(format!("{}: no parent directory", path.display())))?;
    let directory = Directory::open_anchor(directory)
        .map_err(|error| MigrationError::new(format!("{}: {error}", directory.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| MigrationError::new(format!("{}: no file name", path.display())))?;
    directory
        .atomic_write(name, ".migration.", &encoded, PublishMode::Replace)
        .map_err(|error| MigrationError::new(format!("{}: {error}", path.display())))?;
    Ok(true)
}

/// Rewrite one file, reporting a failure against its own path and carrying on.
/// A warning means the step did not finish, so its id stays off the receipt and
/// the next install retries it.
fn rewrite_json_or_warn(
    path: &Path,
    what: &str,
    report: &mut MigrationReport,
    edit: impl FnMut(&mut Value) -> bool,
) {
    match rewrite_json(path, edit) {
        Ok(true) => report.changed.push(path.display().to_string()),
        Ok(false) => {}
        Err(error) => report.warnings.push(
            Warning::new(
                MIGRATION_STEP_FAILED,
                format!("{what} could not be migrated: {error}"),
                provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
            )
            .subject(path.display().to_string())
            .repair(format!(
                "repair {}, then rerun `provenant install`; the step is idempotent",
                path.display()
            )),
        ),
    }
}

/// If `value` carries the old `builtin` source spelling, set the new spelling
/// and move `schema_version` from `old_version` to `new_version`.
fn rename_source(value: &mut Value, old_version: u64, new_version: u64) -> bool {
    let mut changed = false;
    if value.get("source").and_then(Value::as_str) == Some(OLD_SOURCE) {
        value["source"] = json!(NEW_SOURCE);
        changed = true;
    }
    if value.get("schema_version").and_then(Value::as_u64) == Some(old_version) {
        value["schema_version"] = json!(new_version);
        changed = true;
    }
    changed
}

/// Rewrite stored port-command receipts: `port-receipts/<owner>/port-command-*.json`.
struct PortSourceSpellings;

impl Migration for PortSourceSpellings {
    fn id(&self) -> &'static str {
        "port-source-spellings"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let root = state_root.join("port-receipts");
        let owners = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(error.into()),
        };
        for owner in owners {
            let owner = owner?;
            if !owner.file_type()?.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(owner.path())? {
                let path = entry?.path();
                let is_receipt = path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| {
                        name.starts_with("port-command-") && name.ends_with(".json")
                    });
                if !is_receipt {
                    continue;
                }
                // A live publisher holds this lock while writing a receipt.
                // Take it for one file only: holding it for the whole pass
                // makes every concurrent record_port_command get Busy and
                // drop its receipt. A busy lock skips just this file; the step
                // stays pending on its receipt and a later install retries it.
                let lock = match provenant_config::try_lock_port_command_receipts(state_root) {
                    Ok(lock) => lock,
                    Err(PortEvidenceError::Busy) => {
                        report.warnings.push(
                            Warning::new(
                                MIGRATION_STEP_FAILED,
                                format!("port receipt {} is busy; its source spellings were not migrated", path.display()),
                             provenant_records::warning::detected_source(MIGRATION_STEP_FAILED))
                            .subject(path.display().to_string())
                            .repair("stop the publisher, then rerun `provenant install`; the step is idempotent"),
                        );
                        continue;
                    }
                    Err(error) => {
                        return Err(MigrationError::new(format!("port receipts lock: {error}")));
                    }
                };
                rewrite_json_or_warn(&path, "port-command receipt", &mut report, |value| {
                    rename_source(value, OLD_PORT_RECEIPT_VERSION, NEW_PORT_RECEIPT_VERSION)
                });
                drop(lock);
            }
        }
        report.changed.sort();
        Ok(report)
    }
}

/// Rewrite the host and host-project port tables: `ports.json` and
/// `projects/<key>/ports.json`. Each declaration carries a `source`.
struct PortConfigSourceSpellings;

impl Migration for PortConfigSourceSpellings {
    fn id(&self) -> &'static str {
        "port-config-source-spellings"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let mut tables: Vec<PathBuf> = vec![state_root.join("ports.json")];
        if let Ok(projects) = std::fs::read_dir(state_root.join("projects")) {
            for project in projects {
                let path = project?.path().join("ports.json");
                if path.exists() {
                    tables.push(path);
                }
            }
        }
        for path in tables {
            if !path.exists() {
                continue;
            }
            rewrite_json_or_warn(&path, "port table", &mut report, |value| {
                let mut changed = rename_source(value, OLD_PORTS_VERSION, NEW_PORTS_VERSION);
                if let Some(declarations) = value.as_object_mut() {
                    for (name, declaration) in declarations {
                        if name == "schema_version" {
                            continue;
                        }
                        if declaration.is_object()
                            && rename_source(declaration, OLD_PORTS_VERSION, NEW_PORTS_VERSION)
                        {
                            changed = true;
                        }
                    }
                }
                // `leaf_sources` values are Layer spellings, not `source`
                // fields, so the shared rename above cannot see them.
                if let Some(leaves) = value
                    .pointer_mut("/build/leaf_sources")
                    .and_then(Value::as_object_mut)
                {
                    for source in leaves.values_mut() {
                        if source.as_str() == Some(OLD_SOURCE) {
                            *source = json!(NEW_SOURCE);
                            changed = true;
                        }
                    }
                }
                changed
            });
        }
        report.changed.sort();
        Ok(report)
    }
}

/// Warning dedup moved from per-finding marker files to `warnings.sqlite3`.
/// Remove only the two retired marker prefixes; leave every other state file.
struct RetireWarningMarkers;

impl Migration for RetireWarningMarkers {
    fn id(&self) -> &'static str {
        "retire-warning-markers"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let Ok(directory) = Directory::open_anchor(state_root) else {
            return Ok(report);
        };
        let Ok(entries) = directory.entries() else {
            return Ok(report);
        };
        for entry in entries {
            let Ok(name) = entry else { continue };
            if !name.to_str().is_some_and(|text| {
                text.starts_with("config-warning-") || text.starts_with("root-warning-")
            }) {
                continue;
            }
            if directory.unlink(&name).is_ok() {
                report
                    .changed
                    .push(state_root.join(&name).display().to_string());
            }
        }
        Ok(report)
    }
}

/// Usage ledger rows move from v3 to v4. Buckets carry over; the source
/// spelling maps through one exhaustive table. A line that cannot be mapped is
/// reported and copied unchanged to a `.v3-unmapped` sidecar, never guessed.
struct UsageLedgerV4;

impl Migration for UsageLedgerV4 {
    fn id(&self) -> &'static str {
        "usage-ledger-v4"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let result = provenant_telemetry::ledger::migrate_to_v4(state_root)
            .map_err(|error| MigrationError::new(error.to_string()))?;
        let mut report = MigrationReport::new(self.id());
        report.changed = result.changed;
        for unmapped in result.unmapped {
            warning::warn(
                Warning::new(
                    MIGRATION_STEP_FAILED,
                    format!("usage ledger line could not be mapped to v4: {unmapped}"),
                    provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
                )
                .subject(unmapped.segment.clone())
                .repair("inspect the .v3-unmapped sidecar; no value was guessed"),
            );
        }
        Ok(report)
    }
}

/// Run every pending step against the state root and return every recorded id.
pub(super) fn run(state_root: &Path) -> Vec<String> {
    run_steps(state_root, STEPS)
}

/// Record ids on the receipt install just published, for a step that ran
/// before any receipt existed.
pub(super) fn record(state_root: &Path, applied: &[String]) {
    if let Err(error) = persist(state_root, applied) {
        warning::warn(
            Warning::new(
                MIGRATION_STEP_FAILED,
                format!("migration ids could not be recorded in the install receipt: {error}"),
                provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
            )
            .subject(state_root.display().to_string())
            .repair("rerun `provenant install`; the steps are idempotent"),
        );
    }
}

fn run_steps(state_root: &Path, steps: &[&dyn Migration]) -> Vec<String> {
    let mut applied = match read_receipt(state_root) {
        Ok(Some(receipt)) => receipt.migrations,
        Ok(None) => Vec::new(),
        Err(error) => {
            warning::warn(
                Warning::new(
                    MIGRATION_STEP_FAILED,
                    format!(
                        "the install receipt under {} could not be read: {error}",
                        state_root.display()
                    ),
                    provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
                )
                .subject(state_root.display().to_string())
                .repair("repair or remove the install receipt, then rerun `provenant install`"),
            );
            Vec::new()
        }
    };
    let recorded = applied.clone();
    for step in steps {
        if recorded.iter().any(|id| id.as_str() == step.id()) {
            continue;
        }
        match step.apply(state_root) {
            Ok(report) => {
                // A step that still has warnings left something unmigrated:
                // keep its id off the receipt so the next install retries it.
                let pending = !report.warnings.is_empty();
                for message in report.warnings {
                    warning::warn(message);
                }
                if pending {
                    continue;
                }
                applied.push(step.id().to_owned());
                if let Err(error) = persist(state_root, &applied) {
                    warning::warn(
                        Warning::new(
                            MIGRATION_STEP_FAILED,
                            format!(
                                "migration step {} applied but its receipt entry could not be written: {error}",
                                step.id()
                            ),
                         provenant_records::warning::detected_source(MIGRATION_STEP_FAILED))
                        .subject(state_root.display().to_string())
                        .repair("rerun `provenant install`; the step is idempotent"),
                    );
                }
            }
            Err(error) => {
                warning::warn(
                    Warning::new(
                        MIGRATION_STEP_FAILED,
                        format!(
                            "migration step {} failed under {}: {error}",
                            step.id(),
                            state_root.display()
                        ),
                        provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
                    )
                    .subject(state_root.display().to_string())
                    .repair("rerun `provenant install`; the step is idempotent"),
                );
            }
        }
    }
    applied
}

fn persist(state_root: &Path, applied: &[String]) -> Result<(), String> {
    let Some(mut receipt) = read_receipt(state_root)? else {
        // A first install publishes its receipt after this point; install calls
        // `record` again once the receipt exists.
        return Ok(());
    };
    if receipt.migrations.as_slice() == applied {
        return Ok(());
    }
    receipt.migrations = applied.to_vec();
    let bytes = provenant_records::encode(&receipt).map_err(|error| error.to_string())?;
    let directory =
        Directory::open_anchor(&state_root.join("install")).map_err(|error| error.to_string())?;
    directory
        .atomic_write(
            OsStr::new("receipt.json"),
            ".receipt.",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn read_receipt(state_root: &Path) -> Result<Option<InstallReceipt>, String> {
    let directory = match Directory::open_anchor(&state_root.join("install")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    match directory.read_stable(OsStr::new("receipt.json"), Limits::CONFIG.max_bytes) {
        Ok(bytes) => {
            let receipt: InstallReceipt =
                provenant_records::decode(bytes.as_slice(), Limits::CONFIG)
                    .map_err(|error| error.to_string())?;
            if receipt.schema_version != provenant_records::INSTALL_RECEIPT_SCHEMA_VERSION {
                return Err("unsupported install receipt schema version".into());
            }
            Ok(Some(receipt))
        }
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
#[allow(dead_code, unused_imports)]
mod tests {
    use super::{
        AttemptScratchMove, PortConfigSourceSpellings, PortSourceSpellings, read_receipt, run_steps,
    };
    use provenant_records::migrate::{Migration, MigrationError, MigrationReport};
    use provenant_records::warning::{MIGRATION_STEP_FAILED, Warning, capture};
    use provenant_records::{AbsolutePath, FileDigest, InstallReceipt, SourceDigest};
    use serde_json::{Value, json};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Spec: docs/specs/run-launch.md#lane-scratch-stays-outside-the-attempt-record
    #[test]
    fn attempt_scratch_move_renames_live_tmp_and_is_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let id = provenant_core::AttemptId::from_bytes([7; 16]);
        let old = root
            .path()
            .join("attempts")
            .join(id.to_string())
            .join("tmp");
        std::fs::create_dir_all(&old).expect("old tmp");
        std::fs::write(old.join("git-objects"), "private objects").expect("write objects");
        let new = root
            .path()
            .join(provenant_run::ATTEMPT_SCRATCH)
            .join(id.to_string())
            .join("tmp");

        let report = AttemptScratchMove.apply(root.path()).expect("apply once");
        assert_eq!(report.id, "attempt-scratch-move");
        assert_eq!(report.changed, [new.display().to_string()]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(!old.exists(), "old tmp was renamed, not copied");
        assert_eq!(
            std::fs::read_to_string(new.join("git-objects")).expect("read objects"),
            "private objects"
        );

        let again = AttemptScratchMove.apply(root.path()).expect("apply twice");
        assert!(again.changed.is_empty(), "second run must change nothing");
        assert!(again.warnings.is_empty(), "{:?}", again.warnings);
    }

    /// Spec: docs/specs/run-launch.md#lane-scratch-stays-outside-the-attempt-record
    #[test]
    fn attempt_scratch_move_conflict_warns_and_leaves_both() {
        let root = tempfile::tempdir().expect("tempdir");
        let id = provenant_core::AttemptId::from_bytes([8; 16]);
        let old = root
            .path()
            .join("attempts")
            .join(id.to_string())
            .join("tmp");
        let new = root
            .path()
            .join(provenant_run::ATTEMPT_SCRATCH)
            .join(id.to_string())
            .join("tmp");
        std::fs::create_dir_all(&old).expect("old tmp");
        std::fs::create_dir_all(&new).expect("new tmp");
        std::fs::write(old.join("old"), "old").expect("write old");
        std::fs::write(new.join("new"), "new").expect("write new");

        let report = AttemptScratchMove
            .apply(root.path())
            .expect("apply conflict");
        assert!(report.changed.is_empty(), "{:?}", report.changed);
        assert!(
            report.warnings.iter().any(|warning| {
                warning.code.as_ref() == MIGRATION_STEP_FAILED.0
                    && warning.message.contains(&old.display().to_string())
            }),
            "{:?}",
            report.warnings
        );
        assert_eq!(
            std::fs::read_to_string(old.join("old")).expect("old kept"),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(new.join("new")).expect("new kept"),
            "new"
        );
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn port_source_spellings_round_trip_and_are_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let owner = root.path().join("port-receipts/admission");
        std::fs::create_dir_all(&owner).expect("owner dir");
        let receipt = owner.join("port-command-release-abc.json");
        std::fs::write(
            &receipt,
            r#"{"schema_version":2,"source":"builtin","owner":"admission"}"#,
        )
        .expect("write receipt");
        let report = PortSourceSpellings.apply(root.path()).expect("apply once");
        assert_eq!(report.id, "port-source-spellings");
        assert_eq!(report.changed, [receipt.display().to_string()]);
        let bytes = std::fs::read(&receipt).expect("read receipt");
        let value: Value = serde_json::from_slice(&bytes).expect("parse receipt");
        assert_eq!(value["source"], json!("default"));
        assert_eq!(value["schema_version"], json!(3));
        let again = PortSourceSpellings.apply(root.path()).expect("apply twice");
        assert!(again.changed.is_empty(), "second run must change nothing");
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn port_source_spellings_takes_the_lock_per_file() {
        use std::cell::Cell;
        use std::rc::Rc;

        let root = tempfile::tempdir().expect("tempdir");
        let owner = root.path().join("port-receipts/admission");
        std::fs::create_dir_all(&owner).expect("owner dir");
        for name in ["port-command-a.json", "port-command-b.json"] {
            std::fs::write(
                owner.join(name),
                r#"{"schema_version":2,"source":"builtin","owner":"admission"}"#,
            )
            .expect("write receipt");
        }

        let acquisitions = Rc::new(Cell::new(0usize));
        let counter = Rc::clone(&acquisitions);
        provenant_os::fixture::on_owner_lock_acquire(move |name| {
            if name.to_str() == Some(".port-receipts.lock") {
                counter.set(counter.get() + 1);
            }
        });

        let report = PortSourceSpellings.apply(root.path()).expect("apply");
        assert_eq!(report.changed.len(), 2, "{report:?}");
        assert!(report.warnings.is_empty(), "{report:?}");
        // One acquisition per receipt, not one for the whole pass: a live
        // publisher then waits at most one file rewrite for the lock.
        assert_eq!(acquisitions.get(), 2);
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn port_source_spellings_busy_lock_skips_only_that_receipt() {
        let root = tempfile::tempdir().expect("tempdir");
        let owner = root.path().join("port-receipts/admission");
        std::fs::create_dir_all(&owner).expect("owner dir");
        let receipt = owner.join("port-command-a.json");
        std::fs::write(
            &receipt,
            r#"{"schema_version":2,"source":"builtin","owner":"admission"}"#,
        )
        .expect("write receipt");
        // A live publisher holds the receipt lock through this whole pass.
        let _publisher =
            provenant_config::try_lock_port_command_receipts(root.path()).expect("publisher lock");

        let report = PortSourceSpellings.apply(root.path()).expect("apply");
        assert!(report.changed.is_empty(), "{report:?}");
        assert_eq!(report.warnings.len(), 1, "{report:?}");
        let warning = &report.warnings[0];
        assert_eq!(warning.code.as_ref(), MIGRATION_STEP_FAILED.0);
        assert!(
            warning.message.contains(&receipt.display().to_string()),
            "{:?}",
            warning.message
        );
        let value: Value =
            serde_json::from_slice(&std::fs::read(&receipt).expect("read")).expect("parse");
        assert_eq!(value["source"], json!("builtin"), "busy receipt untouched");
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn port_config_source_spellings_rewrite_host_and_project_tables() {
        let root = tempfile::tempdir().expect("tempdir");
        let host = root.path().join("ports.json");
        std::fs::write(
            &host,
            r#"{"schema_version":2,"verify":{"source":"builtin"},"build":{"source":"host"}}"#,
        )
        .expect("write host table");
        let project_dir = root.path().join("projects/key");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        let project = project_dir.join("ports.json");
        std::fs::write(
            &project,
            r#"{"schema_version":2,"land":{"source":"builtin"}}"#,
        )
        .expect("write project table");
        let report = PortConfigSourceSpellings
            .apply(root.path())
            .expect("apply once");
        assert_eq!(report.changed.len(), 2, "{report:?}");
        for path in [&host, &project] {
            let value: Value =
                serde_json::from_slice(&std::fs::read(path).expect("read")).expect("parse");
            assert_eq!(value["schema_version"], json!(3));
            assert!(
                serde_json::to_string(&value)
                    .expect("serialise")
                    .contains("\"default\""),
                "{path:?}: {value}"
            );
        }
        let again = PortConfigSourceSpellings
            .apply(root.path())
            .expect("apply twice");
        assert!(again.changed.is_empty(), "second run must change nothing");
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn host_project_dirs_moves_the_old_key_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let old_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let old = root.path().join("projects").join(old_key);
        std::fs::create_dir_all(&old).expect("old project dir");
        std::fs::write(old.join("config.toml"), "[admission.test]\nslots = 3\n")
            .expect("write config");
        let target = root.path().join("projects").join(&old_key[..32]);

        let report = super::HostProjectDirs
            .apply(root.path())
            .expect("apply once");
        assert_eq!(report.id, "host-project-dirs");
        assert_eq!(report.changed, [target.display().to_string()]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(!old.exists(), "old key directory was moved");
        assert_eq!(
            std::fs::read_to_string(target.join("config.toml")).expect("read moved config"),
            "[admission.test]\nslots = 3\n"
        );
        let again = super::HostProjectDirs
            .apply(root.path())
            .expect("apply twice");
        assert!(again.changed.is_empty(), "second run must change nothing");
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn host_project_dirs_conflict_warns_and_leaves_both() {
        let root = tempfile::tempdir().expect("tempdir");
        let old_key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let old = root.path().join("projects").join(old_key);
        let target = root.path().join("projects").join(&old_key[..32]);
        std::fs::create_dir_all(&old).expect("old project dir");
        std::fs::create_dir_all(&target).expect("binding project dir");
        std::fs::write(old.join("config.toml"), "slots = 3\n").expect("write old config");
        std::fs::write(target.join("config.toml"), "slots = 7\n").expect("write new config");

        let report = super::HostProjectDirs.apply(root.path()).expect("apply");
        assert!(report.changed.is_empty(), "{report:?}");
        assert_eq!(report.warnings.len(), 1, "{report:?}");
        let warning = &report.warnings[0];
        assert!(
            warning.message.contains(&old.display().to_string())
                && warning.message.contains(&target.display().to_string()),
            "{:?}",
            warning.message
        );
        let repair = warning.repair.as_deref().expect("repair");
        assert!(repair.contains(&old.display().to_string()), "{repair}");
        assert!(repair.contains(&target.display().to_string()), "{repair}");
        assert!(repair.contains("remove"), "{repair}");
        assert_eq!(
            std::fs::read_to_string(old.join("config.toml")).expect("old kept"),
            "slots = 3\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("config.toml")).expect("target kept"),
            "slots = 7\n"
        );
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn host_project_dirs_merges_around_the_binding_record() {
        let root = tempfile::tempdir().expect("tempdir");
        let old_key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let old = root.path().join("projects").join(old_key);
        let target = root.path().join("projects").join(&old_key[..32]);
        std::fs::create_dir_all(&old).expect("old project dir");
        std::fs::create_dir_all(&target).expect("binding project dir");
        std::fs::write(old.join("config.toml"), "[admission.build]\nslots = 3\n")
            .expect("write old config");
        std::fs::write(target.join("ports.json"), r#"{"schema_version":3}"#)
            .expect("write binding record");

        let report = super::HostProjectDirs.apply(root.path()).expect("apply");
        assert_eq!(
            report.changed,
            [target.join("config.toml").display().to_string()]
        );
        assert!(report.warnings.is_empty(), "{report:?}");
        assert!(!old.exists(), "the merged old directory was removed");
        assert_eq!(
            std::fs::read_to_string(target.join("config.toml")).expect("moved config"),
            "[admission.build]\nslots = 3\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("ports.json")).expect("binding record kept"),
            r#"{"schema_version":3}"#
        );

        let again = super::HostProjectDirs
            .apply(root.path())
            .expect("apply twice");
        assert!(again.changed.is_empty(), "second run must change nothing");
        assert!(again.warnings.is_empty(), "{again:?}");
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn publish_no_clobber_moves_when_the_target_is_absent() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.json");
        let target = root.path().join("target.json");
        std::fs::write(&source, "payload").expect("write source");
        assert!(super::publish_no_clobber(&source, &target).expect("publish"));
        assert!(!source.exists(), "source was moved");
        assert_eq!(std::fs::read_to_string(&target).expect("moved"), "payload");
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn publish_no_clobber_never_replaces_an_existing_entry() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source.json");
        let target = root.path().join("target.json");
        std::fs::write(&source, "from old key").expect("write source");
        std::fs::write(&target, "from concurrent writer").expect("write target");

        // A check-then-rename would silently replace `target` with `source`.
        assert!(!super::publish_no_clobber(&source, &target).expect("publish"));
        assert_eq!(
            std::fs::read_to_string(&source).expect("source kept"),
            "from old key"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("target kept"),
            "from concurrent writer"
        );
    }

    struct Succeeds(&'static str);

    impl Migration for Succeeds {
        fn id(&self) -> &'static str {
            self.0
        }

        fn apply(&self, _state_root: &Path) -> Result<MigrationReport, MigrationError> {
            Ok(MigrationReport::new(self.0))
        }
    }

    struct FailsThenSucceeds {
        id: &'static str,
        failures: AtomicUsize,
    }

    impl Migration for FailsThenSucceeds {
        fn id(&self) -> &'static str {
            self.id
        }

        fn apply(&self, _state_root: &Path) -> Result<MigrationReport, MigrationError> {
            if self.failures.load(Ordering::SeqCst) > 0 {
                self.failures.fetch_sub(1, Ordering::SeqCst);
                return Err(MigrationError::new("fixture failure"));
            }
            Ok(MigrationReport::new(self.id))
        }
    }

    struct WarnsThenSucceeds {
        id: &'static str,
        warnings: AtomicUsize,
    }

    impl Migration for WarnsThenSucceeds {
        fn id(&self) -> &'static str {
            self.id
        }

        fn apply(&self, _state_root: &Path) -> Result<MigrationReport, MigrationError> {
            let mut report = MigrationReport::new(self.id);
            if self.warnings.load(Ordering::SeqCst) > 0 {
                self.warnings.fetch_sub(1, Ordering::SeqCst);
                report.warnings.push(
                    Warning::new(
                        MIGRATION_STEP_FAILED,
                        "fixture left something behind",
                        provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
                    )
                    .subject("fixture")
                    .repair("retry"),
                );
            }
            Ok(report)
        }
    }

    fn write_receipt(root: &Path, migrations: Vec<String>) -> InstallReceipt {
        let install = root.join("install");
        std::fs::create_dir_all(&install).expect("install directory");
        let receipt = InstallReceipt {
            extra: Default::default(),
            schema_version: provenant_records::INSTALL_RECEIPT_SCHEMA_VERSION,
            state_format_version: provenant_records::STATE_FORMAT_VERSION,
            live_record_format: Some(provenant_records::LIVE_RECORD_FORMAT),
            source_digest: SourceDigest::from_bytes([1; 32]),
            manifest_digest: FileDigest::from_bytes([2; 32]),
            asset_digest: SourceDigest::from_bytes([3; 32]),
            target: "fixture".into(),
            toolchain: "fixture".into(),
            bin_dir: AbsolutePath::new(&root.join("bin")).expect("bin dir"),
            harnesses: Vec::new(),
            harness_targets: Vec::new(),
            entries: Vec::new(),
            migrations,
        };
        std::fs::write(
            install.join("receipt.json"),
            provenant_records::encode(&receipt).expect("encode receipt"),
        )
        .expect("write receipt");
        receipt
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn only_successful_ids_are_recorded_and_a_failed_step_reruns() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path();
        write_receipt(path, Vec::new());
        let success = Succeeds("first");
        let flaky = FailsThenSucceeds {
            id: "second",
            failures: AtomicUsize::new(1),
        };
        let steps: [&dyn Migration; 2] = [&success, &flaky];

        let (applied, warnings) = capture(|| run_steps(path, &steps));
        assert_eq!(applied, ["first"]);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code.as_ref() == MIGRATION_STEP_FAILED.0),
            "{warnings:?}"
        );
        let receipt = read_receipt(path).expect("read").expect("receipt");
        assert_eq!(receipt.migrations, ["first"]);

        let (applied, warnings) = capture(|| run_steps(path, &steps));
        assert_eq!(applied, ["first", "second"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let receipt = read_receipt(path).expect("read").expect("receipt");
        assert_eq!(receipt.migrations, ["first", "second"]);
    }

    #[test]
    // Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    fn retiring_warning_markers_deletes_only_marker_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path();
        for name in [
            "config-warning-abc",
            "root-warning-def",
            "config.toml",
            "warnings.sqlite3",
        ] {
            std::fs::write(path.join(name), b"fixture\n").expect("write state file");
        }
        let step = super::RetireWarningMarkers;
        let report = step.apply(path).expect("apply migration");
        assert_eq!(report.changed.len(), 2);
        assert!(!path.join("config-warning-abc").exists());
        assert!(!path.join("root-warning-def").exists());
        assert!(path.join("config.toml").exists());
        assert!(path.join("warnings.sqlite3").exists());
    }

    #[test]
    fn a_recorded_step_does_not_apply_again() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path();
        write_receipt(path, vec!["done".to_owned()]);
        let step = Succeeds("done");
        let steps: [&dyn Migration; 1] = [&step];
        let (applied, warnings) = capture(|| run_steps(path, &steps));
        assert_eq!(applied, ["done"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        // A first install has no receipt to record into; the returned ids are
        // what `record` writes once install publishes the receipt.
        let absent = tempfile::tempdir().expect("tempdir");
        let (applied, warnings) = capture(|| run_steps(absent.path(), &steps));
        assert_eq!(applied, ["done"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn a_step_with_unmigrated_entries_is_retried() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path();
        write_receipt(path, Vec::new());
        let step = WarnsThenSucceeds {
            id: "pending",
            warnings: AtomicUsize::new(1),
        };
        let steps: [&dyn Migration; 1] = [&step];

        let (applied, warnings) = capture(|| run_steps(path, &steps));
        assert!(applied.is_empty(), "{applied:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let receipt = read_receipt(path).expect("read").expect("receipt");
        assert!(receipt.migrations.is_empty(), "{receipt:?}");

        let (applied, warnings) = capture(|| run_steps(path, &steps));
        assert_eq!(applied, ["pending"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let receipt = read_receipt(path).expect("read").expect("receipt");
        assert_eq!(receipt.migrations, ["pending"]);
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn one_unparseable_receipt_warns_without_aborting_the_other_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let owner = root.path().join("port-receipts/admission");
        std::fs::create_dir_all(&owner).expect("owner dir");
        let broken = owner.join("port-command-broken.json");
        let good = owner.join("port-command-good.json");
        std::fs::write(&broken, "{not json").expect("write broken receipt");
        std::fs::write(
            &good,
            r#"{"schema_version":2,"source":"builtin","owner":"admission"}"#,
        )
        .expect("write good receipt");

        let report = PortSourceSpellings.apply(root.path()).expect("apply");
        assert_eq!(report.changed, [good.display().to_string()]);
        assert_eq!(report.warnings.len(), 1, "{report:?}");
        let warning = &report.warnings[0];
        assert!(
            warning.message.contains(&broken.display().to_string()),
            "{:?}",
            warning.message
        );
        let value: Value =
            serde_json::from_slice(&std::fs::read(&good).expect("read")).expect("parse");
        assert_eq!(value["source"], json!("default"));
        assert_eq!(value["schema_version"], json!(3));
    }

    /// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
    #[test]
    fn port_table_leaf_sources_migrate_the_retired_spelling() {
        let root = tempfile::tempdir().expect("tempdir");
        let host = root.path().join("ports.json");
        std::fs::write(
            &host,
            r#"{"schema_version":2,"build":{"source":"builtin","leaf_sources":{"build.strategy":"builtin","build.offload":"host"}}}"#,
        )
        .expect("write host table");

        let report = PortConfigSourceSpellings.apply(root.path()).expect("apply");
        assert_eq!(report.changed, [host.display().to_string()]);
        assert!(report.warnings.is_empty(), "{report:?}");
        let value: Value =
            serde_json::from_slice(&std::fs::read(&host).expect("read")).expect("parse");
        assert_eq!(value["schema_version"], json!(3));
        assert_eq!(value["build"]["source"], json!("default"));
        assert_eq!(
            value["build"]["leaf_sources"]["build.strategy"],
            json!("default")
        );
        assert_eq!(
            value["build"]["leaf_sources"]["build.offload"],
            json!("host")
        );
    }
}
