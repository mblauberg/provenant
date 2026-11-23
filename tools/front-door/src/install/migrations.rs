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
use provenant_records::migrate::Migration;
use provenant_records::warning::{self, MIGRATION_STEP_FAILED, Warning};
use provenant_records::{InstallReceipt, Limits};
use std::ffi::OsStr;
use std::path::Path;

/// Ordered steps for this release. Steps are deleted in the release after the
/// one that ships them.
const STEPS: &[&dyn Migration] = &[];

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
    use super::{read_receipt, run_steps};
    use provenant_records::migrate::{Migration, MigrationError, MigrationReport};
    use provenant_records::warning::{MIGRATION_STEP_FAILED, capture};
    use provenant_records::{AbsolutePath, FileDigest, InstallReceipt, SourceDigest};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
