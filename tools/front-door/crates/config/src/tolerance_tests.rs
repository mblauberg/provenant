#![allow(clippy::unwrap_used)]
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    weight: u64,
    enabled: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    count: u64,
    rows: Vec<Row>,
    entries: BTreeMap<String, Row>,
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        if self.count == 0 {
            errors.push(SectionError {
                field: "count".into(),
                code: "nonzero",
            });
        }
        for (index, row) in self.rows.iter().enumerate() {
            if row.weight == 0 {
                errors.push(SectionError {
                    field: format!("rows.{index}.weight"),
                    code: "nonzero",
                });
            }
            if !row.enabled {
                errors.push(SectionError {
                    field: format!("rows.{index}.enabled"),
                    code: "disabled",
                });
            }
        }
        errors
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Domains {
    sample: Settings,
}
fn defaults(_: Profile) -> &'static str {
    "count=3\nrows=[]\n[entries]"
}
const SECTIONS: DomainSections<Domains> =
    DomainSections::new(&[DomainSection::new::<Settings>("sample", defaults)]);

fn roots(path: &std::path::Path) -> provenant_core::Roots {
    struct Resolver;
    impl provenant_core::PathResolver for Resolver {
        fn resolve_existing_ancestor(
            &self,
            path: &std::path::Path,
        ) -> std::io::Result<std::path::PathBuf> {
            Ok(path.to_owned())
        }
    }
    let mut input = provenant_core::RootInputs::new("/fixture/home".into(), "/fixture/repo".into());
    input.state_dir = Some(path.to_owned());
    input.resolve(&Resolver).unwrap()
}

#[test]
fn warnings_recover_rows_and_preserve_effective_origins() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let text = "schema_version=1\nextra=true\n[domains.sample]\ncount=0\nrows=[{weight=4,enabled=true},{weight=0,enabled=false},{weight=7,enabled=true},{weight='bad',enabled=true}]\n[domains.sample.entries.good]\nweight=5\nenabled=true\n[domains.sample.entries.bad]\nweight='bad'\nenabled=true\n";
    std::fs::write(dir.path().join("config.toml"), text).unwrap();
    let config = read_with_domains(&roots, &SECTIONS).unwrap();
    assert_eq!(config.domains.sample.count, 3);
    assert_eq!(
        config
            .domains
            .sample
            .rows
            .iter()
            .map(|row| row.weight)
            .collect::<Vec<_>>(),
        [4, 7]
    );
    assert_eq!(config.domains.sample.entries.len(), 1);
    assert_eq!(config.domains.sample.entries["good"].weight, 5);
    assert_eq!(config.warnings.len(), 6);
    assert!(matches!(
        config.origins["domains.sample.count"],
        Origin::Compiled { .. }
    ));
    assert!(matches!(
        config.origins["domains.sample.rows.1.weight"],
        Origin::User { .. }
    ));
    assert!(!config.origins.contains_key("domains.sample.rows.2.weight"));
    assert_eq!(
        inspect_config(&roots, &SECTIONS).unwrap().check.findings,
        config.warnings
    );
    crate::test_support::assert_leaf_origins(&config);
}

#[test]
fn core_sections_keep_valid_siblings_and_handle_quoted_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let text = "schema_version=1\nproviders={codex={update=false}}\n[gc]\n'future.option'=true\nschedule=false\nidle_days=0\n[retention.projects.bad]\ncheckpoint_soft_bytes=0\n[retention.projects.good]\ncheckpoint_soft_bytes=4096\n";
    let config = load(&roots, Some(text.as_bytes())).unwrap();
    assert!(!config.gc.schedule);
    assert_eq!(config.gc.idle_days, 7);
    assert_eq!(
        config.providers["codex"].update,
        ProviderUpdate::Enabled(false)
    );
    assert_eq!(
        config.retention.projects["good"].checkpoint_soft_bytes,
        4096
    );
    assert_eq!(config.warnings.len(), 3);
    assert!(
        config
            .warnings
            .iter()
            .any(|warning| warning.message.starts_with("gc.future.option:"))
    );
    crate::test_support::assert_leaf_origins(&config);
}

#[test]
fn boundary_sections_keep_invalid_known_input_as_hard_errors() {
    const STRICT: DomainSections<Domains> =
        DomainSections::new(&[
            DomainSection::new::<Settings>("sample", defaults).with_boundary_checks()
        ]);
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    for value in [
        "count=0",
        "count='bad'",
        "entries.bad={weight='bad',enabled=true}",
    ] {
        let text = format!("schema_version=1\n[domains.sample]\n{value}");
        assert!(load_with_domains(&roots, Some(text.as_bytes()), &STRICT).is_err());
    }
    let config = load_with_domains(
        &roots,
        Some(&b"schema_version=1\n[domains.sample]\nfuture=true"[..]),
        &STRICT,
    )
    .unwrap();
    assert_eq!(config.warnings.len(), 1);
}

#[test]
fn unknown_row_fields_warn_without_discarding_valid_policy() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let text = "schema_version=1\n[domains.sample]\nrows=[{weight=4,enabled=true,future=true}]\n[domains.sample.entries.good]\nweight=5\nenabled=true\nfuture=true\n";
    let config = load_with_domains(&roots, Some(text.as_bytes()), &SECTIONS).unwrap();
    assert_eq!(config.domains.sample.rows[0].weight, 4);
    assert_eq!(config.domains.sample.entries["good"].weight, 5);
    assert_eq!(config.warnings.len(), 2);
    assert!(
        config
            .warnings
            .iter()
            .all(|warning| warning.rule == "unknown_key")
    );
    crate::test_support::assert_leaf_origins(&config);
}

#[test]
fn inspection_keeps_warnings_alongside_a_boundary_error() {
    const STRICT: DomainSections<Domains> =
        DomainSections::new(&[
            DomainSection::new::<Settings>("sample", defaults).with_boundary_checks()
        ]);
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    std::fs::write(
        dir.path().join("config.toml"),
        "schema_version=1\nextra=true\n[domains.sample]\ncount=0",
    )
    .unwrap();
    let inspection = inspect_config(&roots, &STRICT).unwrap();
    assert!(inspection.error.is_some());
    assert!(
        inspection
            .check
            .findings
            .iter()
            .any(|finding| finding.message.starts_with("extra:"))
    );
}

#[test]
fn malformed_authority_knobs_refuse_instead_of_granting_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    // `lanes.publish` defaults to true: recovering a malformed `false` would
    // hand an open-network writer the gh login the user withheld.
    for value in [
        "'lanes.publish'='false'",
        "'lanes.publish'=0",
        "'fabric.accept_handoff'='true'",
        "'chair.auto_switch'=1",
        "'chair.switch_accounts'='pro'",
        "'lanes.max_depth'=0",
    ] {
        let text = format!("schema_version=1\n[knobs]\n{value}\n");
        let error = load(&roots, Some(text.as_bytes())).unwrap_err();
        let name = value.split('\'').nth(1).unwrap();
        assert!(error.to_string().contains(name), "{value}: {error}");
        std::fs::write(dir.path().join("config.toml"), &text).unwrap();
        assert!(
            inspect_config(&roots, &crate::domains::NO_DOMAINS)
                .unwrap()
                .error
                .is_some()
        );
    }
    let config = load(
        &roots,
        Some(&b"schema_version=1\n[knobs]\n'lanes.publish'=false\n"[..]),
    )
    .unwrap();
    assert_eq!(config.knobs["lanes.publish"].value, false);
    // A non-authority knob still recovers with a warning naming it.
    let config = load(
        &roots,
        Some(&b"schema_version=1\n[knobs]\n'lanes.silence_seconds'='soon'\n"[..]),
    )
    .unwrap();
    assert_eq!(config.knobs["lanes.silence_seconds"].value, 300);
    assert!(
        config.warnings[0]
            .message
            .starts_with("knobs.lanes.silence_seconds:")
    );
}
