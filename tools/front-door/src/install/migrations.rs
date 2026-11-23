//! Install-time migration of non-SQLite state only: files, ledgers, receipts
//! and metadata that no SQLite store owns. SQLite stores already migrate on
//! writable open (`provenant_sqlite::Migration` in `crates/sqlite/src/schema.rs`,
//! specified in `docs/specs/state.md`); this runner never duplicates or wraps
//! that mechanism.
//!
//! Every step not already recorded in the install receipt runs once, in
//! declaration order, after the live-attempt advisory and before any release
//! is published. Its id is recorded only after it succeeds. A failed step
//! warns with the state path and the install proceeds; the id stays unrecorded
//! so the next install retries the idempotent step.
use provenant_os::fs::{Directory, PublishMode};
use provenant_records::migrate::{Migration, MigrationError, MigrationReport};
use provenant_records::warning::{self, MIGRATION_STEP_FAILED, Warning};
use provenant_records::{InstallReceipt, Limits};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Ordered steps for this release. Steps are deleted in the release after the
/// one that ships them.
const STEPS: &[&dyn Migration] = &[
    &PortSourceSpellings,
    &PortConfigSourceSpellings,
    &RetireWarningMarkers,
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
                let changed = rewrite_json(&path, |value| {
                    rename_source(value, OLD_PORT_RECEIPT_VERSION, NEW_PORT_RECEIPT_VERSION)
                })?;
                if changed {
                    report.changed.push(path.display().to_string());
                }
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
            let changed = rewrite_json(&path, |value| {
                let mut changed = rename_source(value, OLD_PORTS_VERSION, NEW_PORTS_VERSION);
                if let Some(declarations) = value.as_object_mut() {
                    for (name, declaration) in declarations {
                        if name == "schema_version" {
                            continue;
                        }
                        if let Some(object) = declaration.as_object_mut()
                            && rename_source(object, OLD_PORTS_VERSION, NEW_PORTS_VERSION)
                        {
                            changed = true;
                        }
                    }
                }
                changed
            })?;
            if changed {
                report.changed.push(path.display().to_string());
            }
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
                for message in report.warnings {
                    warning::warn(message);
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
                        )
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
mod tests {
    use super::{PortConfigSourceSpellings, PortSourceSpellings, read_receipt, run_steps};
    use provenant_records::migrate::{Migration, MigrationError, MigrationReport};
    use provenant_records::warning::{MIGRATION_STEP_FAILED, capture};
    use provenant_records::{AbsolutePath, FileDigest, InstallReceipt, SourceDigest};
    use serde_json::{Value, json};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
}
