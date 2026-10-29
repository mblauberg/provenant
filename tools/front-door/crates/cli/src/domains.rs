//! The binary edge registers the closed aggregate of landed domain owners.
use provenant_config::{DomainSection, DomainSections};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Domains {
    pub accounts: provenant_providers::accounts::Settings,
    pub quota: provenant_quota::Settings,
    pub routing: provenant_routing::RoutingConfig,
    pub fabric: provenant_fabric_runtime::config::FabricConfig,
    pub guard: provenant_confinement::guard::GuardConfig,
    #[serde(default)]
    pub run: provenant_confinement::run_config::RunConfig,
}
static ENTRIES: [DomainSection; 6] = [
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
        let markers = field("domains.run.push_conflict_markers");
        assert_eq!((markers.kind, &markers.default), ("bool", &json!(true)));
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
