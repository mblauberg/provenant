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
use provenant_os::fs::{Directory, ForeignLock, FsError, LockMode, PublishMode};
use provenant_records::migrate::{Migration, MigrationError, MigrationReport};
use provenant_records::warning::{self, MIGRATION_STEP_FAILED, Warning};
use provenant_records::{InstallReceipt, Limits};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Pure record transform owned by one store. Store migrations run under that
/// store's lock; this trait deliberately does not open or publish files.
#[allow(dead_code)]
pub(crate) trait StoreMigration {
    fn migrate(from: u64, to: u64, record: Value) -> Result<Value, MigrationError>;
}

/// Marker spelling shared by store migrations while an interrupted cut-over
/// is resumed. The marker is written beside the owning store.
#[allow(dead_code)]
pub(crate) fn migrating_marker(to: u64) -> String {
    format!("migrating-{to}")
}

/// Default bounded wait for the store cut-over's quiesce gate. The owner can
/// override it at `domains.install.quiesce_seconds`.
const DEFAULT_QUIESCE_SECONDS: u64 = 300;
/// Version written by the config knob cut-over; the graph and run stores own
/// their own markers beside their records.
const CONFIG_KNOB_VERSION: u64 = 4;

/// Ordered steps for this release. Steps are deleted in the release after the
/// one that ships them. The store cut-over runs last: it quiesces every store
/// once, then applies the graph, run and config transforms in that order so a
/// resumed pass never sees half-migrated shapes.
const STEPS: &[&dyn Migration] = &[
    &HostProjectDirs,
    &AttemptScratchMove,
    &PortSourceSpellings,
    &PortConfigSourceSpellings,
    &RetireWarningMarkers,
    &ReadSourceLiveSpelling,
    &UsageLedgerV4,
    &ProjectGrantKeys,
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
/// The retired project key granted capabilities; the new spelling is `grants`,
/// which no longer shares a word with what an adapter can do.
const OLD_GRANTS_KEY: &str = "capabilities";
const NEW_GRANTS_KEY: &str = "grants";

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

/// Migrate the persisted read-source spelling without touching repository files.
struct ReadSourceLiveSpelling;

impl Migration for ReadSourceLiveSpelling {
    fn id(&self) -> &'static str {
        "read-source-live"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let mut paths = vec![state_root.join("config.toml")];
        if let Ok(projects) = std::fs::read_dir(state_root.join("projects")) {
            for project in projects {
                paths.push(project?.path().join("config.toml"));
            }
        }
        for path in paths.into_iter().filter(|path| path.exists()) {
            let result = (|| -> Result<bool, MigrationError> {
                let text = std::fs::read_to_string(&path)?;
                let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
                    MigrationError::new(format!("{}: invalid TOML: {error}", path.display()))
                })?;
                let Some(knobs) = doc
                    .get_mut("knobs")
                    .and_then(toml_edit::Item::as_table_like_mut)
                else {
                    return Ok(false);
                };
                let item = if knobs.contains_key("lanes.read_source") {
                    knobs.get_mut("lanes.read_source")
                } else {
                    knobs
                        .get_mut("lanes")
                        .and_then(toml_edit::Item::as_table_like_mut)
                        .and_then(|lanes| lanes.get_mut("read_source"))
                };
                let Some(item) = item else { return Ok(false) };
                if item.as_str() != Some("checkout") {
                    return Ok(false);
                }
                *item = toml_edit::value("live");
                let parent = path
                    .parent()
                    .ok_or_else(|| MigrationError::new("config has no parent"))?;
                let directory = Directory::open_anchor(parent)
                    .map_err(|error| MigrationError::new(error.to_string()))?;
                let name = path
                    .file_name()
                    .ok_or_else(|| MigrationError::new("config has no filename"))?;
                directory
                    .atomic_write(
                        name,
                        ".migration.",
                        doc.to_string().as_bytes(),
                        PublishMode::Replace,
                    )
                    .map_err(|error| MigrationError::new(error.to_string()))?;
                Ok(true)
            })();
            match result {
                Ok(true) => report.changed.push(path.display().to_string()),
                Ok(false) => {}
                Err(error) => report.warnings.push(
                    Warning::new(
                        MIGRATION_STEP_FAILED,
                        format!("read source could not be migrated: {error}"),
                        provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
                    )
                    .subject(path.display().to_string()),
                ),
            }
        }
        report.changed.sort();
        Ok(report)
    }
}

/// Rename `domains.lanes.projects.<id>.capabilities` to `grants` in
/// `<state>/config.toml`. The document is edited structurally, so comments and
/// unrelated keys survive, and is published atomically at mode 0600 just like
/// a normal config edit. Returns whether the file changed and the projects
/// that already hold both keys; those keep both values instead of guessing a
/// winner, so a later install can retry after the owner resolves the clash.
fn rewrite_project_grants(state_root: &Path) -> Result<(bool, Vec<String>), MigrationError> {
    let path = state_root.join("config.toml");
    let directory = match Directory::open_anchor(state_root) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok((false, Vec::new())),
        Err(error) => {
            return Err(MigrationError::new(format!(
                "{}: {error}",
                state_root.display()
            )));
        }
    };
    let bytes = match directory.read_stable(OsStr::new("config.toml"), Limits::CONFIG.max_bytes) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok((false, Vec::new())),
        Err(error) => return Err(MigrationError::new(format!("{}: {error}", path.display()))),
    };
    let text = String::from_utf8(bytes).map_err(|error| {
        MigrationError::new(format!("{}: invalid UTF-8: {error}", path.display()))
    })?;
    let mut document: toml_edit::DocumentMut = text.parse().map_err(|error| {
        MigrationError::new(format!("{}: invalid TOML: {error}", path.display()))
    })?;
    let projects = document
        .get_mut("domains")
        .and_then(|item| item.get_mut("lanes"))
        .and_then(|item| item.get_mut("projects"))
        .and_then(toml_edit::Item::as_table_like_mut);
    let mut changed = false;
    let mut conflicts = Vec::new();
    if let Some(projects) = projects {
        let names: Vec<String> = projects.iter().map(|(name, _)| name.to_owned()).collect();
        for name in names {
            let Some(project) = projects
                .get_mut(&name)
                .and_then(toml_edit::Item::as_table_like_mut)
            else {
                continue;
            };
            if !project.contains_key(OLD_GRANTS_KEY) {
                continue;
            }
            if project.contains_key(NEW_GRANTS_KEY) {
                conflicts.push(name);
                continue;
            }
            if let Some(old) = project.remove(OLD_GRANTS_KEY) {
                project.insert(NEW_GRANTS_KEY, old);
                changed = true;
            }
        }
    }
    if changed {
        directory
            .atomic_write_with_permissions(
                OsStr::new("config.toml"),
                ".config-",
                document.to_string().as_bytes(),
                0o600,
                PublishMode::Replace,
            )
            .map_err(|error| MigrationError::new(format!("{}: {error}", path.display())))?
            .require_durable()
            .map_err(|error| MigrationError::new(format!("{}: {error}", path.display())))?;
    }
    Ok((changed, conflicts))
}

/// The project grant key was `capabilities`; grant resources to writer lanes
/// under `grants` instead. There is no alias: install rewrites the host config
/// once, and an unmigrated key warns as unknown rather than being read.
struct ProjectGrantKeys;

impl Migration for ProjectGrantKeys {
    fn id(&self) -> &'static str {
        "project-grant-keys"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let path = state_root.join("config.toml");
        match rewrite_project_grants(state_root) {
            Ok((changed, conflicts)) => {
                if changed {
                    report.changed.push(path.display().to_string());
                }
                // A clash keeps both keys: neither value was guessed, and the
                // warning leaves the step pending for the next install.
                for name in conflicts {
                    report.warnings.push(
                        Warning::new(
                            MIGRATION_STEP_FAILED,
                            format!(
                                "{} holds both capabilities and grants for project {name}; neither value was overwritten",
                                path.display()
                            ),
                            provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
                        )
                        .subject(path.display().to_string())
                        .repair(format!(
                            "keep the value to use under domains.lanes.projects.{name}.grants, remove its capabilities key, then rerun `provenant install`; the step is idempotent"
                        )),
                    );
                }
            }
            Err(error) => report.warnings.push(
                Warning::new(
                    MIGRATION_STEP_FAILED,
                    format!("project grant keys could not be migrated: {error}"),
                    provenant_records::warning::detected_source(MIGRATION_STEP_FAILED),
                )
                .subject(path.display().to_string())
                .repair(format!(
                    "repair {}, then rerun `provenant install`; the step is idempotent",
                    path.display()
                )),
            ),
        }
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

/// The bounded wait comes from `domains.install.quiesce_seconds`; an absent or
/// malformed value keeps the 300 second default.
fn quiesce_seconds(state_root: &Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(state_root.join("config.toml")) else {
        return DEFAULT_QUIESCE_SECONDS;
    };
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return DEFAULT_QUIESCE_SECONDS;
    };
    document
        .get("domains")
        .and_then(|item| item.get("install"))
        .and_then(|item| item.get("quiesce_seconds"))
        .and_then(toml_edit::Item::as_integer)
        .filter(|seconds| *seconds >= 0)
        .map(|seconds| seconds as u64)
        .unwrap_or(DEFAULT_QUIESCE_SECONDS)
}

/// Every store lock a cut-over must see drain. `try_flock_foreign` observes an
/// existing lock through a read-only descriptor: it never creates a lock file
/// for an idle store, so a quiescence check changes nothing. The bounded loop
/// in [`quiesce`] does the waiting.
fn held_store_locks(state_root: &Path) -> Vec<String> {
    let mut held = Vec::new();
    let graphs = state_root.join("fabric").join("graphs");
    if let Ok(projects) = std::fs::read_dir(&graphs) {
        for project in projects.flatten() {
            if !project.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(directory) = Directory::open_anchor(project.path()) else {
                continue;
            };
            let Ok(entries) = directory.entries() else {
                continue;
            };
            for name in entries.flatten() {
                let Some(text) = name.to_str() else { continue };
                // Probe the lock file itself: a pending graph holds its lock
                // before any checkpoint exists, so deriving names from
                // `graph-*.json` would miss it.
                if !text.ends_with(".lock") {
                    continue;
                }
                if matches!(
                    directory.try_flock_foreign(OsStr::new(text), LockMode::Exclusive),
                    Ok(ForeignLock::Held)
                ) {
                    held.push(format!("{} {text}", project.file_name().to_string_lossy()));
                }
            }
        }
    }
    let queue_lock = crate::fabric::queue_registry::registry_lock_path(state_root);
    if let (Some(parent), Some(name)) = (queue_lock.parent(), queue_lock.file_name())
        && let Ok(directory) = Directory::open_anchor(parent)
    {
        if matches!(
            directory.try_flock_foreign(name, LockMode::Exclusive),
            Ok(ForeignLock::Held)
        ) {
            held.push("queue registry.lock".to_owned());
        }
    }
    if let Ok(attempts) = std::fs::read_dir(state_root.join("attempts")) {
        for attempt in attempts.flatten() {
            if !attempt.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(directory) = Directory::open_anchor(attempt.path()) else {
                continue;
            };
            for name in ["writer.lock", "custody.lock", "settlement.lock"] {
                if matches!(
                    directory.try_flock_foreign(OsStr::new(name), LockMode::Exclusive),
                    Ok(ForeignLock::Held)
                ) {
                    held.push(format!("{} {name}", attempt.file_name().to_string_lossy()));
                }
            }
        }
    }
    held.sort();
    held.dedup();
    held
}

/// A cut-over migrates nothing until the graph, queue and lease locks drain.
/// A holder that outlives the window is named in `UPGRADE_NOT_QUIESCENT`, and
/// every store step returns before touching a record.
fn quiesce(state_root: &Path, seconds: u64) -> Result<(), MigrationError> {
    // An explicit real-time boundary: use the OS monotonic coordinate and its
    // bounded pause rather than sampling std time or sleeping directly.
    let deadline = provenant_os::sys::monotonic_now()
        .as_micros()
        .saturating_add(seconds.saturating_mul(1_000_000));
    loop {
        let holders = held_store_locks(state_root);
        if holders.is_empty() {
            return Ok(());
        }
        if provenant_os::sys::monotonic_now().as_micros() >= deadline {
            return Err(MigrationError::new(format!(
                "UPGRADE_NOT_QUIESCENT: store locks are still held: {}",
                holders.join(", ")
            )));
        }
        provenant_os::sys::wait_pause(std::time::Duration::from_millis(50));
    }
}

fn old_source_replacement(text: &str) -> Option<&'static str> {
    match text {
        "shipped_default" | "bundled" | "builtin" => Some("default"),
        "dispatch" => Some("launch"),
        _ => None,
    }
}

/// Rewrite persisted source tags in one TOML table and every nested table.
/// Only keys that name a source are touched, so an unrelated value that happens
/// to spell `builtin` is left alone.
fn rewrite_toml_sources(table: &mut dyn toml_edit::TableLike) -> bool {
    let mut changed = false;
    let names: Vec<String> = table.iter().map(|(name, _)| name.to_owned()).collect();
    for name in names {
        let Some(item) = table.get_mut(&name) else {
            continue;
        };
        let is_source = name == "source" || name.ends_with("_source");
        if let Some(value) = item.as_value_mut() {
            if is_source && value.as_str().and_then(old_source_replacement).is_some() {
                let replacement = value
                    .as_str()
                    .and_then(old_source_replacement)
                    .expect("checked above");
                *value = toml_edit::Value::from(replacement);
                changed = true;
            }
        } else if let Some(child) = item.as_table_like_mut() {
            changed |= rewrite_toml_sources(child);
        }
    }
    changed
}

/// The batch 1 config transform. `lanes.isolation` becomes `lanes.read_source`
/// (`shared` pins a checkout, `private` is dropped because destination rules
/// replace it); `lanes.pr = never` set by the owner is deliberately kept.
fn rewrite_config_knobs(document: &mut toml_edit::DocumentMut) -> bool {
    let mut changed = false;
    if let Some(lanes) = document
        .get_mut("lanes")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        let isolation = lanes
            .get("isolation")
            .and_then(toml_edit::Item::as_str)
            .map(str::to_owned);
        match isolation.as_deref() {
            Some("shared") => {
                lanes.remove("isolation");
                lanes.insert("read_source", toml_edit::value("checkout"));
                changed = true;
            }
            Some("private") => {
                lanes.remove("isolation");
                changed = true;
            }
            _ => {}
        }
    }
    changed |= rewrite_toml_sources(document.as_table_mut());
    changed
}

/// Edit one config file structurally, preserving comments and unrelated keys,
/// and publish it atomically at mode 0600 like a normal config write.
fn rewrite_config_file(path: &Path) -> Result<bool, MigrationError> {
    let parent = path
        .parent()
        .ok_or_else(|| MigrationError::new(format!("{}: no parent", path.display())))?;
    let directory = Directory::open_anchor(parent)
        .map_err(|error| MigrationError::new(format!("{}: {error}", parent.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| MigrationError::new(format!("{}: no file name", path.display())))?;
    let bytes = directory
        .read_stable(name, Limits::CONFIG.max_bytes)
        .map_err(|error| MigrationError::new(format!("{}: {error}", path.display())))?;
    let text = String::from_utf8(bytes).map_err(|error| {
        MigrationError::new(format!("{}: invalid UTF-8: {error}", path.display()))
    })?;
    let mut document: toml_edit::DocumentMut = text.parse().map_err(|error| {
        MigrationError::new(format!("{}: invalid TOML: {error}", path.display()))
    })?;
    if !rewrite_config_knobs(&mut document) {
        return Ok(false);
    }
    directory
        .atomic_write_with_permissions(
            name,
            ".config-migrate.",
            document.to_string().as_bytes(),
            0o600,
            PublishMode::Replace,
        )
        .map_err(|error| MigrationError::new(format!("{}: {error}", path.display())))?
        .require_durable()
        .map_err(|error| MigrationError::new(format!("{}: {error}", path.display())))?;
    Ok(true)
}

/// Config-store transform: host and project config files, behind a marker so
/// an interrupted pass resumes. Each file reads as a no-op on a second run.
struct ConfigKnobRewrite;

impl Migration for ConfigKnobRewrite {
    fn id(&self) -> &'static str {
        "config-knob-rewrite"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        let mut paths = vec![state_root.join("config.toml")];
        if let Ok(projects) = std::fs::read_dir(state_root.join("projects")) {
            for project in projects {
                let path = project?.path().join("config.toml");
                if path.exists() {
                    paths.push(path);
                }
            }
        }
        let marker = state_root.join(migrating_marker(CONFIG_KNOB_VERSION));
        std::fs::write(&marker, b"")?;
        for path in paths {
            if !path.exists() {
                continue;
            }
            match rewrite_config_file(&path) {
                Ok(true) => report.changed.push(path.display().to_string()),
                Ok(false) => {}
                Err(error) => report.warnings.push(
                    Warning::new(
                        MIGRATION_STEP_FAILED,
                        format!(
                            "config knobs could not be migrated in {}: {error}",
                            path.display()
                        ),
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
        if report.warnings.is_empty() {
            std::fs::remove_file(&marker)?;
        }
        report.changed.sort();
        Ok(report)
    }
}

/// The one batch 1 cut-over. Quiesce once, then migrate graph, run store and
/// config in that order. A failure in any store leaves its marker and stops
/// the sequence; a later install resumes from the first unchanged record.
struct StoreCutover;

impl Migration for StoreCutover {
    fn id(&self) -> &'static str {
        "batch1-store-cutover"
    }

    fn apply(&self, state_root: &Path) -> Result<MigrationReport, MigrationError> {
        let mut report = MigrationReport::new(self.id());
        quiesce(state_root, quiesce_seconds(state_root))?;
        let graph = provenant_fabric_runtime::graph::migrate_store(state_root)
            .map_err(|error| MigrationError::new(format!("graph store cut-over: {error}")))?;
        report.changed.extend(graph);
        provenant_run::lease_migrate::migrate_database(state_root)
            .map_err(|error| MigrationError::new(format!("run database cut-over: {error}")))?;
        let run = provenant_run::lease_migrate::migrate_store(state_root)
            .map_err(|error| MigrationError::new(format!("run store cut-over: {error}")))?;
        report.changed.extend(run);
        // The queue registry's batch 2 transform slots in here, before config;
        // this batch owns only the three stores it can migrate to final shapes.
        let config = ConfigKnobRewrite.apply(state_root)?;
        report.changed.extend(config.changed);
        report.warnings.extend(config.warnings);
        if !report.warnings.is_empty() {
            return Err(MigrationError::new(
                "config cut-over left records unmigrated",
            ));
        }
        report.changed.extend(
            crate::pr::landing::receipt::migrate_pending(state_root).map_err(|error| {
                MigrationError::new(format!("pending landing cut-over: {error}"))
            })?,
        );
        report.changed.sort();
        Ok(report)
    }
}

/// Run every pending step against the state root and return every recorded id.
pub(super) fn cutover_pending(state_root: &Path) -> Result<bool, String> {
    Ok(!read_receipt(state_root)?
        .is_some_and(|receipt| receipt.migrations.iter().any(|id| id == StoreCutover.id())))
}

pub(super) fn run(state_root: &Path) -> Result<Vec<String>, (&'static str, String)> {
    if cutover_pending(state_root).map_err(|error| ("UPGRADE_MIGRATION_FAILED", error))? {
        StoreCutover.apply(state_root).map_err(|error| {
            let message = error.to_string();
            let code = if message.contains("UPGRADE_NOT_QUIESCENT") {
                "UPGRADE_NOT_QUIESCENT"
            } else {
                "UPGRADE_MIGRATION_FAILED"
            };
            (code, message)
        })?;
        let mut applied = read_receipt(state_root)
            .map_err(|error| ("UPGRADE_MIGRATION_FAILED", error))?
            .map(|receipt| receipt.migrations)
            .unwrap_or_default();
        applied.push(StoreCutover.id().to_owned());
        persist(state_root, &applied).map_err(|error| ("UPGRADE_MIGRATION_FAILED", error))?;
        let mut remaining = run_steps(state_root, STEPS);
        if !remaining.iter().any(|id| id == StoreCutover.id()) {
            remaining.push(StoreCutover.id().to_owned());
        }
        Ok(remaining)
    } else {
        Ok(run_steps(state_root, STEPS))
    }
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
        AttemptScratchMove, ConfigKnobRewrite, PortConfigSourceSpellings, PortSourceSpellings,
        ProjectGrantKeys, StoreCutover, held_store_locks, quiesce_seconds, read_receipt, run_steps,
    };
    use provenant_os::fs::{Directory, LockMode};
    use provenant_records::migrate::{Migration, MigrationError, MigrationReport};
    use provenant_records::warning::{MIGRATION_STEP_FAILED, Warning, capture};
    use provenant_records::{AbsolutePath, FileDigest, InstallReceipt, SourceDigest};
    use serde_json::{Value, json};
    use std::ffi::OsStr;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Host and host-project configurations migrate once; unrelated settings survive.
    #[test]
    fn read_source_live_migrates_host_and_projects_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("projects/one");
        std::fs::create_dir_all(&project).unwrap();
        let host = root.path().join("config.toml");
        let scoped = project.join("config.toml");
        std::fs::write(&host, "schema_version=1\n[knobs]\nlanes.read_source='checkout'\nlanes.read_source_fetch_first=true\n").unwrap();
        std::fs::write(
            &scoped,
            "schema_version=1\n[knobs]\n'lanes.read_source'='checkout'\nother='kept'\n",
        )
        .unwrap();
        let step = super::ReadSourceLiveSpelling;
        let report = step.apply(root.path()).unwrap();
        assert_eq!(report.changed.len(), 2);
        for path in [&host, &scoped] {
            let text = std::fs::read_to_string(path).unwrap();
            assert_eq!(
                provenant_config::read_source_value(&text).as_deref(),
                Some("live")
            );
            assert!(text.contains(if path == &host {
                "read_source_fetch_first=true"
            } else {
                "other='kept'"
            }));
        }
        assert!(step.apply(root.path()).unwrap().changed.is_empty());
    }

    /// A merge that keeps only one side's migration silently drops the other's
    /// state repair. Both colliding steps must stay registered exactly once.
    #[test]
    fn read_source_and_project_grant_steps_are_registered_once() {
        let ids: Vec<&str> = super::STEPS.iter().map(|step| step.id()).collect();
        for id in ["read-source-live", "project-grant-keys"] {
            assert_eq!(
                ids.iter().filter(|registered| **registered == id).count(),
                1,
                "{id} must be registered exactly once"
            );
        }
    }

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
    fn project_grant_keys_are_renamed_once_without_overwriting_a_grant() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().expect("tempdir");
        let config = root.path().join("config.toml");
        std::fs::write(
            &config,
            r#"schema_version = 1
[domains.lanes.projects.writer]
capabilities = ["postgres"] # keep this comment
shared_paths = [".shared"]

[domains.lanes.projects.both]
capabilities = ["postgres"]
grants = ["other"]

[domains.lanes.projects.ready]
grants = ["postgres"]
"#,
        )
        .expect("write config");

        let report = ProjectGrantKeys.apply(root.path()).expect("apply once");
        assert_eq!(report.id, "project-grant-keys");
        assert_eq!(report.changed, [config.display().to_string()]);
        assert_eq!(report.warnings.len(), 1, "{report:?}");
        assert!(
            report.warnings[0].message.contains("project both"),
            "{report:?}"
        );
        let text = std::fs::read_to_string(&config).expect("read config");
        assert!(text.contains("# keep this comment"), "{text}");
        let document: toml_edit::DocumentMut = text.parse().expect("parse config");
        let projects = document["domains"]["lanes"]["projects"]
            .as_table_like()
            .expect("projects table");
        let writer = projects
            .get("writer")
            .and_then(toml_edit::Item::as_table_like)
            .expect("writer project");
        assert!(!writer.contains_key("capabilities"), "{text}");
        assert_eq!(
            writer
                .get("grants")
                .and_then(toml_edit::Item::as_array)
                .and_then(|array| array.iter().next())
                .and_then(toml_edit::Value::as_str),
            Some("postgres")
        );
        assert_eq!(
            writer
                .get("shared_paths")
                .and_then(toml_edit::Item::as_array)
                .and_then(|array| array.iter().next())
                .and_then(toml_edit::Value::as_str),
            Some(".shared")
        );
        let both = projects
            .get("both")
            .and_then(toml_edit::Item::as_table_like)
            .expect("both project");
        assert!(both.contains_key("capabilities"), "clash was kept: {text}");
        assert_eq!(
            both.get("grants")
                .and_then(toml_edit::Item::as_array)
                .and_then(|array| array.iter().next())
                .and_then(toml_edit::Value::as_str),
            Some("other")
        );
        assert_eq!(
            std::fs::metadata(&config).expect("metadata").mode() & 0o777,
            0o600
        );

        let again = ProjectGrantKeys.apply(root.path()).expect("apply twice");
        assert!(again.changed.is_empty(), "second run must change nothing");
        assert_eq!(again.warnings.len(), 1, "clash stays pending: {again:?}");
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
        let next = warning.next.first().expect("next").display_text();
        assert!(next.contains(&old.display().to_string()), "{next}");
        assert!(next.contains(&target.display().to_string()), "{next}");
        assert!(next.contains("remove"), "{next}");
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

    /// Spec: docs/specs/agent-native.md#3.10-store-migrations
    #[test]
    fn config_knob_rewrite_maps_isolation_and_source_tags_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let config = root.path().join("config.toml");
        std::fs::write(
            &config,
            r#"[lanes]
isolation = "shared"
pr = "never"

[domains.install]
quiesce_seconds = 0

[domains.lanes.projects.writer]
source = "builtin"
leaf_source = "dispatch"
"#,
        )
        .expect("write config");

        let report = ConfigKnobRewrite.apply(root.path()).expect("apply once");
        assert_eq!(report.id, "config-knob-rewrite");
        assert_eq!(report.changed, [config.display().to_string()]);
        assert!(report.warnings.is_empty(), "{report:?}");
        let text = std::fs::read_to_string(&config).expect("read config");
        let document: toml_edit::DocumentMut = text.parse().expect("parse config");
        let lanes = document["lanes"].as_table_like().expect("lanes table");
        assert!(!lanes.contains_key("isolation"), "{text}");
        assert_eq!(
            lanes.get("read_source").and_then(toml_edit::Item::as_str),
            Some("checkout")
        );
        assert_eq!(
            lanes.get("pr").and_then(toml_edit::Item::as_str),
            Some("never"),
            "an owner-set lanes.pr is kept"
        );
        let writer = document["domains"]["lanes"]["projects"]["writer"]
            .as_table_like()
            .expect("writer project");
        assert_eq!(
            writer.get("source").and_then(toml_edit::Item::as_str),
            Some("default")
        );
        assert_eq!(
            writer.get("leaf_source").and_then(toml_edit::Item::as_str),
            Some("launch")
        );
        assert_eq!(quiesce_seconds(root.path()), 0);
        assert!(
            !root.path().join("migrating-4").exists(),
            "marker removed after a complete pass"
        );

        let again = ConfigKnobRewrite.apply(root.path()).expect("apply twice");
        assert!(again.changed.is_empty(), "second pass changes nothing");
        assert!(again.warnings.is_empty(), "{again:?}");
    }

    /// Spec: docs/specs/agent-native.md#3.10-store-migrations
    #[test]
    fn config_knob_rewrite_drops_private_isolation() {
        let root = tempfile::tempdir().expect("tempdir");
        let config = root.path().join("config.toml");
        std::fs::write(&config, "[lanes]\nisolation = \"private\"\n").expect("write config");
        let report = ConfigKnobRewrite.apply(root.path()).expect("apply");
        assert_eq!(report.changed, [config.display().to_string()]);
        let text = std::fs::read_to_string(&config).expect("read config");
        assert!(!text.contains("isolation"), "{text}");
        assert!(!text.contains("read_source"), "{text}");
    }

    /// Spec: docs/specs/agent-native.md#3.10-store-migrations
    #[test]
    fn store_cutover_records_its_id_and_resumes_a_marker() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path();
        write_receipt(path, Vec::new());
        std::fs::write(
            path.join("config.toml"),
            "[lanes]\nisolation = \"shared\"\n[domains.install]\nquiesce_seconds = 0\n",
        )
        .expect("write config");
        // An interrupted pass left the config marker behind. The cut-over
        // resumes instead of refusing to start.
        std::fs::write(path.join("migrating-4"), b"").expect("marker");

        let (applied, warnings) = capture(|| run_steps(path, &[&StoreCutover]));
        assert_eq!(applied, ["batch1-store-cutover"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!path.join("migrating-4").exists());
        let text = std::fs::read_to_string(path.join("config.toml")).expect("read config");
        assert!(text.contains("read_source"), "{text}");
        let receipt = read_receipt(path).expect("read").expect("receipt");
        assert_eq!(receipt.migrations, ["batch1-store-cutover"]);

        let (again, warnings) = capture(|| run_steps(path, &[&StoreCutover]));
        assert_eq!(again, ["batch1-store-cutover"], "recorded step is skipped");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// Spec: docs/specs/agent-native.md#3.10-store-migrations
    #[test]
    fn not_quiescent_migrates_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path();
        let config = path.join("config.toml");
        std::fs::write(
            &config,
            "[lanes]\nisolation = \"shared\"\n[domains.install]\nquiesce_seconds = 0\n",
        )
        .expect("write config");
        let original = std::fs::read(&config).expect("read original config");
        let queue_lock = crate::fabric::queue_registry::registry_lock_path(path);
        std::fs::create_dir_all(queue_lock.parent().unwrap()).expect("queue dir");
        let directory = Directory::open_anchor(queue_lock.parent().unwrap()).expect("queue anchor");
        let _holder = directory
            .try_lock(queue_lock.file_name().unwrap(), LockMode::Exclusive)
            .expect("hold queue lock");
        assert!(
            held_store_locks(path)
                .iter()
                .any(|name| name.contains("registry.lock")),
            "the held lock is visible to the gate"
        );

        let error = StoreCutover.apply(path).expect_err("not quiescent");
        assert!(
            error.to_string().contains("UPGRADE_NOT_QUIESCENT"),
            "{error}"
        );
        assert_eq!(std::fs::read(&config).expect("read config"), original);
        assert!(!path.join("migrating-4").exists(), "no marker was written");
    }
}
