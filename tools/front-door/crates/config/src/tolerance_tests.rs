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
                detail: None,
                field: "count".into(),
                code: "nonzero",
            });
        }
        for (index, row) in self.rows.iter().enumerate() {
            if row.weight == 0 {
                errors.push(SectionError {
                    detail: None,
                    field: format!("rows.{index}.weight"),
                    code: "nonzero",
                });
            }
            if !row.enabled {
                errors.push(SectionError {
                    detail: None,
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
    // A malformed container of a boundary section refuses too; it recovers
    // only when no section under it controls a boundary.
    let text = b"schema_version=1\ndomains=false\n";
    assert!(load_with_domains(&roots, Some(&text[..]), &STRICT).is_err());
    let config = load_with_domains(&roots, Some(&text[..]), &SECTIONS).unwrap();
    assert_eq!(config.warnings.len(), 1);
    let config = load_with_domains(
        &roots,
        Some(&b"schema_version=1\n[domains.sample]\nfuture=true"[..]),
        &STRICT,
    )
    .unwrap();
    assert_eq!(config.warnings.len(), 1);
}

#[test]
fn malformed_ancestor_refuses_standalone_and_aggregate_boundary_reads() {
    const STRICT: DomainSections<Domains> =
        DomainSections::new(&[
            DomainSection::new::<Settings>("sample", defaults).with_boundary_checks()
        ]);
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    std::fs::write(
        dir.path().join("config.toml"),
        "schema_version=1\ndomains=false\n",
    )
    .unwrap();
    #[derive(Debug, Default, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Defaulted {
        count: u64,
    }
    let error = read_domain::<Defaulted>(dir.path(), "sample").unwrap_err();
    assert!(error.to_string().contains("domains"), "{error}");
    let text = b"schema_version=1\ndomains=false\n";
    assert!(load_with_domains(&roots, Some(&text[..]), &STRICT).is_err());
    // A non-boundary section still recovers, as the aggregate does.
    assert!(
        crate::loader::read_settings::<Defaulted>(dir.path(), "domains.sample", |_| Ok(())).is_ok()
    );
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
fn malformed_authority_knobs_fall_back_to_their_restrictive_value() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    // `lanes.publish` defaults on and hands an open-network writer the gh
    // login, so a malformed entry must take the restrictive value (off), not
    // the default and not a permissive reading of the entry.
    for (value, name, rule, restrictive) in [
        (
            "'lanes.publish'='false'",
            "lanes.publish",
            "type",
            serde_json::json!(false),
        ),
        (
            "'lanes.publish'=0",
            "lanes.publish",
            "type",
            serde_json::json!(false),
        ),
        (
            "'fabric.accept_handoff'='true'",
            "fabric.accept_handoff",
            "type",
            serde_json::json!(false),
        ),
        (
            "'chair.auto_switch'=1",
            "chair.auto_switch",
            "type",
            serde_json::json!(false),
        ),
        (
            "'chair.switch_accounts'='pro'",
            "chair.switch_accounts",
            "type",
            serde_json::json!([]),
        ),
        (
            "'lanes.max_depth'=0",
            "lanes.max_depth",
            "outside_allowed_range",
            serde_json::json!(1),
        ),
    ] {
        let text = format!("schema_version=1\n[knobs]\n{value}\n");
        let config = load(&roots, Some(text.as_bytes())).unwrap();
        assert_eq!(config.knobs[name].value, restrictive, "{value}");
        assert_eq!(config.warnings.len(), 1, "{value}");
        assert_eq!(config.warnings[0].rule, rule, "{value}");
        assert_eq!(config.warnings[0].line, 3, "{value}");
        std::fs::write(dir.path().join("config.toml"), &text).unwrap();
        let inspection = inspect_config(&roots, &crate::domains::NO_DOMAINS).unwrap();
        assert!(inspection.error.is_none());
        assert_eq!(inspection.check.count, 1);
    }
    // A malformed knobs container holds authority knobs, so every one of them
    // takes its restrictive value while the container is ignored.
    for text in [
        "schema_version=1\nknobs=false\n",
        "schema_version=1\nknobs='strict'\n",
    ] {
        let config = load(&roots, Some(text.as_bytes())).unwrap();
        assert_eq!(config.warnings.len(), 1, "{text}");
        assert_eq!(config.warnings[0].rule, "type", "{text}");
        assert_eq!(config.knobs["lanes.publish"].value, false, "{text}");
        assert_eq!(config.knobs["lanes.max_depth"].value, 1, "{text}");
        assert_eq!(
            config.knobs["lanes.max_concurrency"].source,
            KnobSource::Default
        );
    }
    let config = load(
        &roots,
        Some(&b"schema_version=1\n[knobs]\n'lanes.publish'=true\n"[..]),
    )
    .unwrap();
    assert_eq!(config.knobs["lanes.publish"].value, true);
    assert!(config.warnings.is_empty());
    // A non-authority knob recovers to its default with one warning.
    let config = load(
        &roots,
        Some(&b"schema_version=1\n[knobs]\n'lanes.silence_seconds'='soon'\n"[..]),
    )
    .unwrap();
    assert_eq!(config.knobs["lanes.silence_seconds"].value, 1800);
    assert_eq!(
        config.knobs["lanes.silence_seconds"].source,
        KnobSource::Default
    );
    assert_eq!(config.warnings.len(), 1);
    assert_eq!(config.warnings[0].rule, "type");
}

#[test]
fn a_misspelt_knob_warns_once_with_the_nearest_name_and_keeps_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let text = "schema_version = 1\n\n[knobs]\n\"lanes.max_concurency\" = 4\n";
    let config = load(&roots, Some(text.as_bytes())).unwrap();
    assert_eq!(config.warnings.len(), 1);
    assert_eq!(config.warnings[0].rule, "unknown_key");
    assert_eq!(config.warnings[0].line, 4);
    assert_eq!(
        config.knobs["lanes.max_concurrency"].source,
        KnobSource::Default
    );
    assert_eq!(
        crate::knobs::nearest_knobs("lanes.max_concurency"),
        ["lanes.max_concurrency"]
    );
    // The location prints as plain text, never as a Debug rendering.
    let error = load(&roots, Some(&b"schema_version=1\n[knobs]\nx=[\n"[..])).unwrap_err();
    let ConfigError::Syntax { location } = &error else {
        panic!("expected a syntax error, got {error:?}");
    };
    let shown = location.to_string();
    assert!(
        !shown.contains("PathWire") && !shown.contains("RelativePath"),
        "{shown}"
    );
    // `config show` reads the same file without failing.
    std::fs::write(dir.path().join("config.toml"), text).unwrap();
    assert_eq!(
        config_show(&roots, &crate::domains::NO_DOMAINS)
            .unwrap()
            .warnings
            .len(),
        1
    );
}
