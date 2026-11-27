#[cfg(test)]
#[allow(dead_code, unused_imports)]
mod tests {
    use super::{
        AttemptScratchMove, ConfigKnobRewrite, PortConfigSourceSpellings, PortSourceSpellings,
        ProjectGrantKeys, held_store_locks, quiesce_seconds, read_receipt, run,
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

    /// Batch 1 rewrites both base-format stores to the one live spelling and
    /// the production reader accepts it; the catalogue default is that spelling.
    #[test]
    fn batch_one_read_source_rewrites_parse_as_live() {
        use provenant_cli::run::ReadSource;

        let live = ReadSource::Live.to_string();

        // Base-format config carrying the sealed `lanes.isolation` knob.
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "[lanes]\nisolation = \"shared\"\n").unwrap();
        ConfigKnobRewrite.apply(root.path()).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        let document: toml_edit::DocumentMut = text.parse().unwrap();
        let rewritten = document["lanes"]["read_source"].as_str().unwrap();
        assert_eq!(rewritten, live.as_str());
        assert_eq!(ReadSource::parse(rewritten).unwrap(), ReadSource::Live);

        // A v3 checkpoint with a sealed `lanes.isolation`, from the shared
        // stored-checkpoint fixture.
        let mut record: Value = serde_json::from_str(include_str!(
            "../../crates/fabric-runtime/src/fixtures/graph-checkpoint.json"
        ))
        .unwrap();
        record["schema_version"] = json!(3);
        record["execution"]["lanes.isolation"] = json!("shared");
        let migrated = provenant_fabric_runtime::graph::migrate(3, 4, record).unwrap();
        let sealed = migrated["execution"]["lanes.read_source"].as_str().unwrap();
        assert_eq!(sealed, live.as_str());
        assert_eq!(ReadSource::parse(sealed).unwrap(), ReadSource::Live);

        // The knob catalogue default round-trips through the same reader.
        let catalogue: Value =
            serde_json::from_str(include_str!("../../../../config/harness-knobs.json")).unwrap();
        let default = catalogue["knobs"]["lanes.read_source"]["default"]
            .as_str()
            .unwrap();
        assert_eq!(default, live.as_str());
        assert_eq!(ReadSource::parse(default).unwrap(), ReadSource::Live);
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
        let live = provenant_cli::run::ReadSource::Live.to_string();
        assert_eq!(
            lanes.get("read_source").and_then(toml_edit::Item::as_str),
            Some(live.as_str())
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
    fn store_cutover_records_each_store_id_and_resumes_a_marker() {
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

        let applied = run(path).expect("cutover");
        assert!(applied.contains(&"batch1-graph-store-cutover".to_owned()));
        assert!(applied.contains(&"batch1-run-store-cutover".to_owned()));
        assert!(applied.contains(&"batch1-config-store-cutover".to_owned()));
        assert!(!path.join("migrating-4").exists());
        let text = std::fs::read_to_string(path.join("config.toml")).expect("read config");
        assert!(text.contains("read_source"), "{text}");
        let receipt = read_receipt(path).expect("read").expect("receipt");
        assert!(receipt.migrations.contains(&"batch1-graph-store-cutover".to_owned()));
        assert!(receipt.migrations.contains(&"batch1-run-store-cutover".to_owned()));
        assert!(receipt.migrations.contains(&"batch1-config-store-cutover".to_owned()));

        let again = run(path).expect("repeat cutover");
        assert_eq!(again, applied, "recorded steps are skipped");
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

        let error = run(path).expect_err("not quiescent");
        assert!(
            error.1.contains("UPGRADE_NOT_QUIESCENT"),
            "{error:?}"
        );
        assert_eq!(std::fs::read(&config).expect("read config"), original);
        assert!(!path.join("migrating-4").exists(), "no marker was written");
    }
}
