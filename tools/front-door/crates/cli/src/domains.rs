//! The binary edge registers the closed aggregate of landed domain owners.
use provenant_config::{DomainSection, DomainSections};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Domains {
    #[serde(default)]
    pub wait: crate::wait::Settings,
    pub accounts: provenant_providers::accounts::Settings,
    pub quota: provenant_quota::Settings,
    pub routing: provenant_routing::RoutingConfig,
    pub fabric: provenant_fabric_runtime::config::FabricConfig,
    pub guard: provenant_confinement::guard::GuardConfig,
    #[serde(default)]
    pub run: provenant_confinement::run_config::RunConfig,
}
static ENTRIES: [DomainSection; 7] = [
    crate::wait::SECTION,
    provenant_providers::accounts::SECTION,
    provenant_quota::SECTION,
    provenant_routing::SECTION,
    provenant_fabric_runtime::config::SECTION,
    provenant_confinement::guard::SECTION,
    provenant_confinement::run_config::SECTION,
];
pub static SECTIONS: DomainSections<Domains> = DomainSections::new(&ENTRIES);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use provenant_core::{PathResolver, Roots};
    use serde_json::json;
    use std::path::Path;

    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<std::path::PathBuf> {
            Ok(path.into())
        }
    }
    fn roots(temp: &tempfile::TempDir) -> Roots {
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        inputs.resolve(&Resolver).unwrap()
    }
    fn edit(
        roots: &Roots,
        patch: serde_json::Value,
    ) -> Result<provenant_config::Edited<Domains>, provenant_config::ConfigError> {
        provenant_config::edit_config(
            roots,
            &SECTIONS,
            &provenant_config::EditOptions::new("test"),
            |document, current| provenant_config::apply_patch(&patch, document, current, &SECTIONS),
        )
    }

    #[test]
    fn cargo_seed_section_is_a_known_key_and_turns_the_seed_off() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        std::fs::create_dir_all(temp.path().join("state")).unwrap();
        for text in [
            "schema_version = 1\n[domains.fabric.lanes.cargo_seed]\nenabled = false\n",
            "schema_version = 1\n[gc]\nfree_floor = '1B'\n[domains.fabric.lanes.cargo_seed]\nenabled = false\n",
        ] {
            std::fs::write(temp.path().join("state/config.toml"), text).unwrap();
            let config = provenant_config::config_show(&roots, &SECTIONS).unwrap();
            assert!(config.warnings.is_empty(), "{:?}", config.warnings);
            assert!(!config.domains.fabric.lanes.cargo_seed.enabled);
        }
    }

    #[test]
    fn schema_reports_real_domain_bounds_enums_and_keyed_shapes() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let (config, revision) = provenant_config::show(&roots, &SECTIONS).unwrap();
        let schema = provenant_config::schema(&config, &SECTIONS, revision).unwrap();
        let field = |path: &str| {
            schema
                .fields
                .iter()
                .find(|field| field.path == path)
                .unwrap()
        };
        let parallel = field("domains.quota.max_parallel");
        assert_eq!(
            (parallel.kind, parallel.min, parallel.max),
            ("int", Some(1), Some(16))
        );
        let headline = field("domains.quota.display.headline");
        assert_eq!(headline.kind, "enum");
        assert_eq!(headline.values, ["auto", "5h", "weekly"]);
        let history = field("domains.quota.history_max_bytes");
        assert_eq!(
            (history.kind, history.min, history.max, &history.default),
            ("int", Some(65_536), Some(134_217_728), &json!(4_194_304))
        );
        let wait = field("domains.wait.timeout_seconds");
        assert_eq!(
            (wait.min, wait.max, &wait.default),
            (Some(1), Some(14400), &json!(300))
        );
        let max_targets = field("domains.wait.max_targets");
        assert_eq!(
            (max_targets.min, max_targets.max, &max_targets.default),
            (Some(1), Some(1024), &json!(64))
        );
        let pr_read = field("domains.wait.pr_read_timeout_seconds");
        assert_eq!(
            (pr_read.min, pr_read.max, &pr_read.default),
            (Some(1), Some(600), &json!(60))
        );
        let markers = field("domains.run.push_conflict_markers");
        assert_eq!((markers.kind, &markers.default), ("bool", &json!(true)));
        let brief_warnings = field("domains.run.brief_read_root_warnings");
        assert_eq!(
            (brief_warnings.kind, &brief_warnings.default),
            ("bool", &json!(true))
        );
        let idle = field("gc.idle_days");
        assert_eq!(
            (idle.kind, idle.min, idle.max, &idle.default),
            ("int", Some(1), Some(3650), &json!(7))
        );
        assert!(
            schema.fields.iter().any(
                |field| field.path == "domains.quota.polling_intervals.{adapter}.timer_seconds"
            )
        );
        assert!(
            schema
                .fields
                .iter()
                .any(|field| field.path == "gc.projects.{project}.foreign_worktrees")
        );
    }

    const RETIRED: &str = "\
schema_version = 1
# keep this comment
[domains.routing]
pacing_overrides = [
  # far future
  { provider = \"codex\", policy = \"pace\", expires_unix_seconds = 4102444800 },
  { provider = \"claude\", policy = \"pace\", expires_unix_seconds = 100 },
]

[[domains.routing.model_policy.rules]]
model = \"agy/gemini-3.8-flash\"
starts_unix_seconds = 1700000000
windows = [{ weekdays = [0], start_hour = 1, end_hour = 4 }]
[domains.routing.model_policy.rules.policy.lane]
read_only = true
";

    fn write_config(roots: &Roots, text: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(roots.state.as_path()).unwrap();
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn retired_keys_block_writes_until_config_migrate_rewrites_them() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let path = write_config(&roots, RETIRED);
        // The refusal names the repair.
        let error = edit(&roots, json!({"domains.quota.max_parallel": 4}))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("moved_to_schedule"), "{error}");
        assert!(error.contains("provenant config migrate"), "{error}");

        // A dry run reports and writes nothing.
        let preview = provenant_config::migrate(&roots, &SECTIONS, true).unwrap();
        assert!(preview.dry_run && preview.backup.is_none());
        assert_eq!(preview.migrations.len(), 3);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), RETIRED);

        let report = provenant_config::migrate(&roots, &SECTIONS, false).unwrap();
        let backup = report.backup.clone().unwrap();
        assert!(backup.starts_with("config.toml.bak-"), "{backup}");
        assert_eq!(
            std::fs::read_to_string(roots.state.as_path().join(&backup)).unwrap(),
            RETIRED
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# keep this comment"), "{after}");
        assert!(after.contains("# far future"), "{after}");
        assert!(after.contains("schedule"), "{after}");
        assert!(after.contains("2100-01-01T00:00:00Z"), "{after}");
        assert!(after.contains("2023-11-14T22:13:20Z"), "{after}");
        assert!(!after.contains("unix_seconds"), "{after}");
        assert!(
            !after.contains("claude"),
            "expired override is dropped: {after}"
        );
        assert!(after.contains("codex"), "{after}");
        assert!(after.contains("weekdays"), "{after}");

        // The migrated file loads cleanly and accepts writes again.
        let edited = edit(&roots, json!({"domains.quota.max_parallel": 4})).unwrap();
        assert_eq!(edited.config.domains.quota.max_parallel, 4);
        let rules = &edited.config.domains.routing.model_policy.rules;
        assert_eq!(rules[0].schedule.windows.len(), 1);
        assert_eq!(edited.config.domains.routing.pacing_overrides.len(), 1);

        // Nothing left to migrate: no backup, no change.
        let again = provenant_config::migrate(&roots, &SECTIONS, false).unwrap();
        assert!(again.migrations.is_empty() && again.backup.is_none());
    }

    #[test]
    fn migrate_refuses_a_conflicting_schedule_and_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let text = "schema_version = 1\n[domains.routing]\npacing_overrides = [{ provider = \"codex\", policy = \"pace\", expires_unix_seconds = 4102444800, schedule = { expires = \"2100-01-01T00:00:00Z\" } }]\n";
        let path = write_config(&roots, text);
        let error = provenant_config::migrate(&roots, &SECTIONS, false)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("schedule.expires"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(
            std::fs::read_dir(roots.state.as_path())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".bak-"))
        );
    }

    #[test]
    fn calibration_entry_is_created_by_one_patch() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let edited = edit(
            &roots,
            json!({
                "domains.quota.calibration.codex.five_hour": 40,
                "domains.quota.calibration.codex.expires": "2030-01-01T00:00:00Z",
            }),
        )
        .unwrap();
        assert_eq!(
            edited.config.domains.quota.calibration["codex"].five_hour,
            Some(40)
        );
        let (config, revision) = provenant_config::show(&roots, &SECTIONS).unwrap();
        let schema = provenant_config::schema(&config, &SECTIONS, revision).unwrap();
        assert!(schema.fields.iter().any(|field| {
            field.path == "domains.quota.calibration.codex.five_hour"
                && (field.kind, field.min, field.max) == ("int", Some(0), Some(100))
        }));
    }

    #[test]
    fn mixed_valid_invalid_domain_patch_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let first = edit(&roots, json!({"domains.quota.max_parallel": 4})).unwrap();
        assert_eq!(first.config.domains.quota.max_parallel, 4);
        let path = roots.state.as_path().join("config.toml");
        let before = std::fs::read_to_string(&path).unwrap();
        let entries = provenant_config::history(&roots, None, 10).unwrap().len();
        let error = edit(
            &roots,
            json!({"domains.quota.max_parallel": 8, "domains.quota.display.headline": "invalid"}),
        )
        .err()
        .unwrap();
        assert!(
            matches!(
                error,
                provenant_config::ConfigError::Domain { .. }
                    | provenant_config::ConfigError::Schema { .. }
            ),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            provenant_config::history(&roots, None, 10).unwrap().len(),
            entries
        );
        let edited = edit(&roots, json!({"domains.quota.history_max_bytes": 131_072})).unwrap();
        assert_eq!(edited.config.domains.quota.history_max_bytes, 131_072);
        let edited = edit(
            &roots,
            json!({"domains.quota.polling_intervals.claude.timer_seconds": 17}),
        )
        .unwrap();
        assert_eq!(
            edited.config.domains.quota.polling_intervals["claude"].timer_seconds,
            Some(17)
        );
    }
}
