#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use provenant_core::{PathResolver, RootInputs, Roots};
use serde::Deserialize;
use std::{io, path::Path};

struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }
}
fn roots() -> Roots {
    let mut inputs = RootInputs::new("/fixture/home".into(), "/fixture/checkout".into());
    inputs.state_dir = Some("/fixture/state".into());
    inputs.resolve(&Resolver).unwrap()
}
#[test]
fn explicit_profile_selects_the_compiled_preset() {
    let config = load(
        &roots(),
        Some(&b"schema_version = 1\nprofile = 'economy'\n"[..]),
    )
    .unwrap();
    assert!(config.profile == Profile::Economy);
}

#[test]
fn provider_state_retention_defaults_overrides_and_validation() {
    let defaults = serde_json::to_value(load::<&[u8]>(&roots(), None).unwrap()).unwrap();
    assert_eq!(defaults["retention"]["provider_state_days"], 7);
    let config = load(
        &roots(),
        Some(&b"schema_version=1\n[retention]\nprovider_state_days=14"[..]),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&config).unwrap()["retention"]["provider_state_days"],
        14
    );
    test_support::assert_leaf_origins(&config);
    for (value, rule) in [
        ("0", "provider_state_days_range"),
        ("3651", "provider_state_days_range"),
        ("-1", "type"),
        ("'seven'", "type"),
    ] {
        let text = format!("schema_version=1\n[retention]\nprovider_state_days={value}");
        let error = load(&roots(), Some(text.as_bytes())).unwrap_err();
        assert_refusal(&error, rule);
    }
}

#[test]
fn gc_settings_parse_defaults_overrides_and_refuse_invalid_values() {
    let defaults = serde_json::to_value(load::<&[u8]>(&roots(), None).unwrap()).unwrap();
    assert_eq!(defaults["gc"]["schedule"], true);
    assert_eq!(defaults["gc"]["idle_days"], 7);
    assert!(defaults["gc"]["free_floor"].is_null());
    let config = load(
        &roots(),
        Some(&b"schema_version=1\n[gc]\nschedule=false\nidle_days=14\nfree_floor='20G'"[..]),
    )
    .unwrap();
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["gc"]["schedule"], false);
    assert_eq!(value["gc"]["idle_days"], 14);
    assert_eq!(value["gc"]["free_floor"], "20G");
    test_support::assert_leaf_origins(&config);
    let configured = load(&roots(), Some(b"schema_version=1\n[gc]\nalert_below='20G'\n[gc.projects.example]\nregenerable=['target','node_modules']\nkeep=['web/target']".as_slice())).unwrap();
    assert_eq!(
        configured.gc.projects["example"].regenerable,
        ["target", "node_modules"]
    );
    assert_eq!(configured.gc.projects["example"].keep, ["web/target"]);
    test_support::assert_leaf_origins(&configured);
    for (field, rule) in [
        ("idle_days=0", "idle_days_range"),
        ("idle_days=3651", "idle_days_range"),
        ("schedule='yes'", "type"),
        ("free_floor='bad'", "invalid_cache_size"),
        ("alert_below='bad'", "invalid_cache_size"),
        (
            "projects.p.regenerable=['../target']",
            "regenerable_directory_name",
        ),
        (
            "projects.p.regenerable=['.git']",
            "regenerable_directory_name",
        ),
        ("projects.p.keep=['/absolute']", "gc_keep_relative_path"),
    ] {
        let text = format!("schema_version=1\n[gc]\n{field}");
        let error = load(&roots(), Some(text.as_bytes())).unwrap_err();
        assert_refusal(&error, rule);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestDomains {
    sample: Sample,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    limit: u64,
    enabled: bool,
}
impl DomainConfig for Sample {
    fn validate(&self) -> Vec<SectionError> {
        if self.limit == 0 || self.limit > 8 {
            vec![SectionError {
                field: "limit".into(),
                code: "outside_allowed_range",
            }]
        } else {
            vec![]
        }
    }
}
fn sample_defaults(profile: Profile) -> &'static str {
    match profile {
        Profile::Balanced => "limit = 8\nenabled = true\n",
        Profile::Economy => "limit = 4\nenabled = true\n",
        Profile::Rigorous => "limit = 8\nenabled = false\n",
    }
}
const SAMPLE: DomainSection = DomainSection::new::<Sample>("sample", sample_defaults);
const DOMAINS: DomainSections<TestDomains> = DomainSections::new(&[SAMPLE]);
#[test]
fn partial_inline_domain_override_keeps_selected_preset_defaults_and_leaf_origins() {
    let config = load_with_domains(
        &roots(),
        Some(
            &b"schema_version = 1\nprofile = 'economy'\n[domains]\nsample = { enabled = false }\n"
                [..],
        ),
        &DOMAINS,
    )
    .unwrap();
    assert_eq!(
        config.domains.sample,
        Sample {
            limit: 4,
            enabled: false
        }
    );
    assert_eq!(
        config.origins.get("domains.sample.limit"),
        Some(&Origin::Compiled {
            profile: Profile::Economy
        })
    );
    assert_eq!(
        config.origins.get("domains.sample.enabled"),
        Some(&Origin::User {
            path: provenant_records::PathWire::from_relative(
                &provenant_core::RelativePath::parse("config.toml").unwrap()
            )
            .unwrap()
        })
    );
}

#[test]
fn each_profile_selects_the_domain_owned_asset() {
    for (profile, limit, enabled) in [
        ("balanced", 8, true),
        ("economy", 4, true),
        ("rigorous", 8, false),
    ] {
        let text = format!("schema_version=1\nprofile='{profile}'");
        let snapshot = load_with_domains(&roots(), Some(text.as_bytes()), &DOMAINS).unwrap();
        assert_eq!(snapshot.domains.sample, Sample { limit, enabled });
        test_support::assert_leaf_origins(&snapshot);
    }
    test_support::assert_leaf_origins(&load::<io::Empty>(&roots(), None).unwrap());
}

#[test]
fn closed_toml_boundary_refuses_unknowns_duplicates_bad_types_and_versions_without_echoing_input() {
    for (text, refusal) in [
        ("", "missing"),
        ("# no version", "missing"),
        ("profile='economy'", "missing"),
        ("schema_version=2", "version"),
        ("schema_version=-1", "version"),
        ("schema_version='1'", "type"),
        ("schema_version=true", "type"),
        ("schema_version=1.0", "type"),
        ("schema_version=1\nprofile='private-sentinel'", "range"),
        ("schema_version=1\nprofile=4", "type"),
        (
            "schema_version=1\nprofile='economy'\nprofile='balanced'",
            "syntax",
        ),
        ("schema_version=1\nschema_version=1", "syntax"),
        (
            "schema_version=1\ninclude='private-sentinel'",
            "unknown_key",
        ),
        ("schema_version=1\n[knobs]\nnope=1", "unknown_key"),
        ("schema_version=1\nknobs=42", "type"),
        ("schema_version=1\n[domains.unknown]", "unknown_key"),
        ("schema_version=1\n[task_classes]", "unknown_key"),
        ("schema_version=1\n[knobs]\n'effort.shift'=1", "unknown_key"),
        ("schema_version=18446744073709551616", "syntax"),
        ("schema_version=1\n[domains]\nx=inf", "unknown_key"),
        ("schema_version=1\n[domains]\nx=2026-01-01", "unknown_key"),
        ("schema_version=1\nx=['private-sentinel'", "syntax"),
        (
            "schema_version=1\n[domains.sample]\nlimit=4\n[domains.sample]\nenabled=false",
            "syntax",
        ),
    ] {
        let error = load(&roots(), Some(text.as_bytes())).unwrap_err();
        assert_refusal(&error, refusal);
        assert!(!format!("{error:?} {error}").contains("private-sentinel"));
    }
    assert!(matches!(
        load(&roots(), Some(&b"schema_version=9"[..])),
        Err(ConfigError::UnsupportedVersion { .. })
    ));
    assert!(matches!(
        load(&roots(), Some(&b""[..])),
        Err(ConfigError::MissingVersion { .. })
    ));
    assert!(matches!(
        load(&roots(), Some(&b"\xff"[..])),
        Err(ConfigError::InvalidUtf8 { .. })
    ));
}

#[test]
fn domain_registration_checks_closed_types_and_semantics_after_applying_defaults() {
    for (text, refusal) in [
        ("schema_version=1\n[domains.sample]\nlimit=0", "domain"),
        ("schema_version=1\n[domains.sample]\nlimit=9", "domain"),
        ("schema_version=1\n[domains.sample]\nlimit=-1", "range"),
        ("schema_version=1\n[domains.sample]\nlimit=4.0", "type"),
        (
            "schema_version=1\n[domains.sample]\nlimit=9223372036854775808",
            "syntax",
        ),
        (
            "schema_version=1\n[domains.sample]\nsecret='private-sentinel'",
            "unknown_key",
        ),
        ("schema_version=1\n[domains.other]", "unknown_key"),
    ] {
        let error = load_with_domains(&roots(), Some(text.as_bytes()), &DOMAINS).unwrap_err();
        assert_refusal(&error, refusal);
        assert!(!format!("{error:?} {error}").contains("private-sentinel"));
    }
    let error = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.sample]\nlimit=0"[..]),
        &DOMAINS,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ConfigError::Domain {
            source: SectionError {
                field,
                ..
            },
            ..
        } if field == "domains.sample.limit"
    ));
    let config = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.sample]\nlimit=3"[..]),
        &DOMAINS,
    )
    .unwrap();
    assert_eq!(
        config.domains.sample,
        Sample {
            limit: 3,
            enabled: true
        }
    );
    assert!(
        serde_json::to_string(&config)
            .unwrap()
            .contains("\"limit\":3")
    );
}

#[test]
fn bounded_reader_preserves_io_failures_and_consumes_at_most_limit_plus_one() {
    struct Fails;
    impl io::Read for Fails {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }
    assert!(
        matches!(load(&roots(), Some(Fails)), Err(ConfigError::Read { source: provenant_os::fs::FsError::Io { operation: "read", source, .. }, .. }) if source.kind() == io::ErrorKind::PermissionDenied)
    );
    let mut stream = io::repeat(b' ');
    assert!(matches!(
        load(&roots(), Some(&mut stream)),
        Err(ConfigError::TooLarge {
            limit: 1_048_576,
            ..
        })
    ));
    let mut exact = String::from("schema_version=1\n#");
    exact.extend(std::iter::repeat_n(' ', 1_048_576 - exact.len()));
    assert!(load(&roots(), Some(exact.as_bytes())).is_ok());
    exact.push(' ');
    assert!(matches!(
        load(&roots(), Some(exact.as_bytes())),
        Err(ConfigError::TooLarge { .. })
    ));
}

#[test]
fn collection_bounds_refuse_deep_and_wide_domain_inputs() {
    let deep = format!(
        "schema_version=1\n[domains]\nx={}0{}",
        "[".repeat(70),
        "]".repeat(70)
    );
    assert!(matches!(
        load(&roots(), Some(deep.as_bytes())),
        Err(ConfigError::CollectionLimit { .. })
    ));
    let wide = format!("schema_version=1\n[domains]\nx=[{}0]", "0,".repeat(100_001));
    assert!(matches!(
        load(&roots(), Some(wide.as_bytes())),
        Err(ConfigError::CollectionLimit { .. })
    ));
}

struct Files {
    _temp: tempfile::TempDir,
    roots: Roots,
}
impl Files {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        // All environmental locations are supplied as values; tests never
        // discover ambient HOME, state or harness configuration.
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        std::fs::create_dir(base.join("home")).unwrap();
        std::fs::create_dir(base.join("harness")).unwrap();
        Self {
            roots: inputs.resolve(&Resolver).unwrap(),
            _temp: temp,
        }
    }
    fn config(&self) -> PathBuf {
        self.roots.state.as_path().join("config.toml")
    }
}

#[test]
fn show_and_check_are_read_only_and_only_absence_uses_defaults() {
    let fixture = Files::new();

    assert_eq!(
        config_show(&fixture.roots, &domains::NO_DOMAINS)
            .unwrap()
            .profile,
        Profile::Balanced
    );
    assert_eq!(
        inspect_config(&fixture.roots, &domains::NO_DOMAINS)
            .unwrap()
            .check
            .schema_version,
        1
    );
    assert!(!fixture.roots.state.as_path().exists());
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    assert_eq!(read(&fixture.roots).unwrap().profile, Profile::Balanced);
    assert!(!fixture.config().exists());
    let original = b"# User-owned; preserve exactly.\nschema_version=1\nprofile='rigorous'\n";
    std::fs::write(fixture.config(), original).unwrap();
    assert_eq!(
        inspect_config(&fixture.roots, &domains::NO_DOMAINS)
            .unwrap()
            .check
            .profile,
        Profile::Rigorous
    );
    assert_eq!(
        config_show(&fixture.roots, &domains::NO_DOMAINS)
            .unwrap()
            .profile,
        Profile::Rigorous
    );
    assert_eq!(std::fs::read(fixture.config()).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(fixture.roots.state.as_path())
            .unwrap()
            .count(),
        1
    );
    std::fs::write(fixture.config(), b"schema_version = 'private-sentinel'").unwrap();
    assert_eq!(
        inspect_config(&fixture.roots, &domains::NO_DOMAINS)
            .unwrap()
            .check
            .count,
        1
    );
    assert!(matches!(
        config_show(&fixture.roots, &domains::NO_DOMAINS),
        Err(ConfigError::Schema { rule: "type", .. })
    ));
}

#[test]
fn config_filesystem_boundary_refuses_links_non_files_and_unreadable_roots() {
    let fixture = Files::new();

    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    // A final link must never read an old or foreign root, even when broken.
    std::os::unix::fs::symlink("/nonexistent/private-sentinel", fixture.config()).unwrap();
    assert!(matches!(
        read(&fixture.roots),
        Err(ConfigError::Read { .. })
    ));
    std::fs::remove_file(fixture.config()).unwrap();
    std::fs::create_dir(fixture.config()).unwrap();
    assert!(matches!(
        read(&fixture.roots),
        Err(ConfigError::Read { .. })
    ));
    std::fs::remove_dir(fixture.config()).unwrap();
    std::fs::remove_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(fixture.roots.state.as_path(), b"not a directory").unwrap();
    assert!(matches!(
        read(&fixture.roots),
        Err(ConfigError::Read { .. })
    ));
}

#[test]
fn show_serialises_origins_for_non_utf8_state_paths() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Files::new();
    let mut inputs = RootInputs::new(
        fixture.roots.state.as_path().with_file_name("home"),
        fixture.roots.state.as_path().with_file_name("checkout"),
    );
    inputs.state_dir = Some(
        fixture
            .roots
            .state
            .as_path()
            .with_file_name(std::ffi::OsString::from_vec(b"state-\xff".to_vec())),
    );
    let roots = inputs.resolve(&Resolver).unwrap();
    // APFS rejects creation of non-UTF-8 names. Inject a reader to prove the
    // native-path wire contract on both supported operating systems.
    let snapshot = load(&roots, Some(&b"schema_version=1\nprofile='economy'\n"[..])).unwrap();
    let output = serde_json::to_value(&snapshot).unwrap();
    let path_wire: provenant_records::PathWire =
        serde_json::from_value(output.pointer("/origins/profile/path").unwrap().clone()).unwrap();
    assert_eq!(
        path_wire.to_relative_path().unwrap().as_path(),
        Path::new("config.toml")
    );
}

#[test]
fn version_refusal_precedes_unknown_key_decoding() {
    assert!(matches!(
        load(
            &roots(),
            Some(&b"schema_version=2\nprivate_key='sentinel'"[..])
        ),
        Err(ConfigError::UnsupportedVersion { .. })
    ));
}

#[test]
fn inspect_collects_ordered_unknown_type_range_and_domain_failures() {
    let fixture = Files::new();
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(
        fixture.config(),
        "schema_version=1\nextra=true\n[domains.sample]\nenabled=1\nlimit=0\nother=false\n",
    )
    .unwrap();

    let check = inspect_config(&fixture.roots, &DOMAINS).unwrap().check;
    assert_eq!(check.count, 4);
    assert_eq!(
        check
            .findings
            .iter()
            .map(|f| (f.rule.as_str(), f.line))
            .collect::<Vec<_>>(),
        vec![
            ("unknown_key", 2),
            ("type", 4),
            ("domain", 5),
            ("unknown_key", 6)
        ]
    );
    std::fs::write(
        fixture.config(),
        "schema_version=1\n[domains.sample]\nlimit=-1\nenabled='private-sentinel'\n",
    )
    .unwrap();
    let check = inspect_config(&fixture.roots, &DOMAINS).unwrap().check;
    assert_eq!(
        check
            .findings
            .iter()
            .map(|f| f.rule.as_str())
            .collect::<Vec<_>>(),
        vec!["range", "type"]
    );
    assert!(
        !serde_json::to_string(&check)
            .unwrap()
            .contains("private-sentinel")
    );
    std::fs::write(fixture.config(), "schema_version = [").unwrap();
    assert!(matches!(
        inspect_config(&fixture.roots, &DOMAINS).unwrap().error,
        Some(ConfigError::Syntax { .. })
    ));
}

#[test]
fn exact_source_locations_survive_default_merging_and_inline_tables() {
    for (text, key, line, column) in [
        (
            "schema_version=1\n[domains.sample]\n  surprise=5",
            "domains.sample.surprise",
            3,
            3,
        ),
        (
            "schema_version=1\n[domains]\nsample = { enabled = 9 }",
            "domains.sample.enabled",
            3,
            22,
        ),
    ] {
        let error = load_with_domains(&roots(), Some(text.as_bytes()), &DOMAINS).unwrap_err();
        match error {
            ConfigError::Schema { location, .. } => {
                assert!(matches!(location.origin, Origin::User { .. }));
                assert_eq!(
                    (location.key_path.as_str(), location.line, location.column),
                    (key, line, column)
                );
            }
            other => panic!("wrong refusal: {other:?}"),
        }
    }
}

#[test]
fn compiled_default_failures_are_never_attributed_to_the_user_file() {
    fn bad_defaults(_: Profile) -> &'static str {
        "limit=0\nenabled=true\n"
    }
    const BAD: DomainSections<TestDomains> =
        DomainSections::new(&[DomainSection::new::<Sample>("sample", bad_defaults)]);
    let error = load_with_domains(&roots(), Some(&b"schema_version=1"[..]), &BAD).unwrap_err();
    assert!(matches!(
        error,
        ConfigError::Domain {
            location: Location {
                origin: Origin::Compiled { .. },
                line: 1,
                column: 7,
                ..
            },
            ..
        }
    ));
    fn wrong_defaults(_: Profile) -> &'static str {
        "limit='private-sentinel'\nenabled=true\n"
    }
    const WRONG: DomainSections<TestDomains> =
        DomainSections::new(&[DomainSection::new::<Sample>("sample", wrong_defaults)]);
    let error = load_with_domains(&roots(), Some(&b"schema_version=1"[..]), &WRONG).unwrap_err();
    assert!(matches!(
        error,
        ConfigError::Schema {
            location: Location {
                origin: Origin::Compiled { .. },
                ..
            },
            ..
        }
    ));
}

#[test]
fn show_uses_registered_domains_and_redacts_secret_leaves() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Secrets {
        token: Redacted<String>,
        labels: Vec<String>,
    }
    impl DomainConfig for Secrets {}
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SecretDomains {
        secret: Secrets,
    }
    fn defaults(_: Profile) -> &'static str {
        "token='compiled-private'\nlabels=['one', 'two']"
    }
    const SECRETS: DomainSections<SecretDomains> =
        DomainSections::new(&[DomainSection::new::<Secrets>("secret", defaults)]);
    let fixture = Files::new();
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(
        fixture.config(),
        "schema_version=1\n[domains.secret]\ntoken='user-private'\nlabels=['three']",
    )
    .unwrap();
    let snapshot = config_show(&fixture.roots, &SECRETS).unwrap();
    assert_eq!(snapshot.domains.secret.token.expose(), "user-private");
    let rendered = serde_json::to_string(&snapshot).unwrap();
    assert!(!rendered.contains("user-private"));
    assert!(!rendered.contains("compiled-private"));
    assert!(rendered.contains("[redacted]"));
    assert!(!format!("{snapshot:?}").contains("user-private"));
    test_support::assert_leaf_origins(&snapshot);
    let sample = config_show(&fixture.roots, &SECRETS).unwrap();
    test_support::assert_leaf_origins(&sample);
}

fn assert_refusal(error: &ConfigError, expected: &str) {
    let code = match error {
        ConfigError::MissingVersion { .. } => "missing",
        ConfigError::UnsupportedVersion { .. } => "version",
        ConfigError::Syntax { .. } => "syntax",
        ConfigError::Schema { rule, .. } => rule,
        ConfigError::Domain { .. } => "domain",
        other => panic!("unexpected refusal: {other:?}"),
    };
    assert_eq!(code, expected, "wrong refusal: {error:?}");
}

#[test]
fn array_members_are_checked_independently_at_their_exact_paths() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        first: u64,
        second: u64,
        enabled: bool,
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Rows {
        rows: Vec<Row>,
        numbers: Vec<u64>,
    }
    impl DomainConfig for Rows {}
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        array: Rows,
    }
    fn defaults(_: Profile) -> &'static str {
        "rows=[{first=1, second=2, enabled=true}]\nnumbers=[]"
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<Rows>("array", defaults)]);
    let fixture = Files::new();
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(fixture.config(), "schema_version=1\n[domains.array]\nrows=[{first=-1, second=-2, enabled=1, unknown=true}]\nnumbers=[-1,-2,1.0]\n").unwrap();
    let check = inspect_config(&fixture.roots, &ENTRIES).unwrap().check;
    assert_eq!(check.count, 7, "{check:?}");
    for key in [
        "rows.0.first",
        "rows.0.second",
        "rows.0.enabled",
        "rows.0.unknown",
        "numbers.0",
        "numbers.1",
        "numbers.2",
    ] {
        assert!(
            check.findings.iter().any(|finding| finding
                .message
                .starts_with(&format!("domains.array.{key}:"))),
            "missing {key}"
        );
    }
    std::fs::write(fixture.config(), "schema_version=1\n[domains.array]\nnumbers=[4,5]\n[[domains.array.rows]]\nfirst=3\nsecond=4\nenabled=false").unwrap();
    let snapshot = config_show(&fixture.roots, &ENTRIES).unwrap();
    assert_eq!(snapshot.domains.array.rows[0].first, 3);
    test_support::assert_leaf_origins(&snapshot);
}

#[test]
fn replacement_array_tables_cannot_hide_missing_fields_behind_defaults() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct List {
        values: Vec<Sample>,
    }
    impl DomainConfig for List {}
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        list: List,
    }
    fn defaults(_: Profile) -> &'static str {
        "values=[{limit=1,enabled=true}]"
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<List>("list", defaults)]);
    let error = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.list]\nvalues=[{limit=2}, {enabled=false}]"[..]),
        &ENTRIES,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ConfigError::Schema {
            location: Location {
                origin: Origin::User { .. },
                ..
            },
            rule: "type"
        }
    ));
}

#[test]
fn every_registered_section_reports_all_its_semantic_failures() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        first: Sample,
        second: Sample,
    }
    const ENTRIES: DomainSections<Aggregate> = DomainSections::new(&[
        DomainSection::new::<Sample>("first", sample_defaults),
        DomainSection::new::<Sample>("second", sample_defaults),
    ]);
    let fixture = Files::new();
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(
        fixture.config(),
        "schema_version=1\n[domains.first]\nlimit=0\n[domains.second]\nlimit=9",
    )
    .unwrap();
    let check = inspect_config(&fixture.roots, &ENTRIES).unwrap().check;
    assert_eq!(check.count, 2);
    assert!(
        check
            .findings
            .iter()
            .all(|finding| finding.rule == "domain")
    );
    assert_eq!(
        check
            .findings
            .iter()
            .map(|finding| finding.line)
            .collect::<Vec<_>>(),
        [3, 5]
    );
}

#[test]
fn an_invalid_array_member_does_not_hide_other_members_semantic_failures() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct List {
        values: Vec<Sample>,
    }
    impl DomainConfig for List {
        fn validate(&self) -> Vec<SectionError> {
            self.values
                .iter()
                .enumerate()
                .flat_map(|(index, sample)| {
                    sample.validate().into_iter().map(move |mut error| {
                        error.field = format!("values.{index}.{}", error.field);
                        error
                    })
                })
                .collect()
        }
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        list: List,
    }
    fn defaults(_: Profile) -> &'static str {
        "[[values]]\nlimit=1\nenabled=true"
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<List>("list", defaults)]);
    let fixture = Files::new();
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(
        fixture.config(),
        "schema_version=1\n[domains.list]\nvalues=[{limit=1,enabled=4}, {limit=0,enabled=true}]",
    )
    .unwrap();
    let check = inspect_config(&fixture.roots, &ENTRIES).unwrap().check;
    assert_eq!(
        check
            .findings
            .iter()
            .map(|finding| finding.rule.as_str())
            .collect::<Vec<_>>(),
        ["type", "domain"]
    );
}

#[test]
fn semantic_locations_keep_original_indices_after_invalid_empty_default_array_members() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct List {
        values: Vec<u64>,
    }
    impl DomainConfig for List {
        fn validate(&self) -> Vec<SectionError> {
            self.values
                .iter()
                .enumerate()
                .filter(|(_, value)| **value == 0)
                .map(|(index, _)| SectionError {
                    field: format!("values.{index}"),
                    code: "nonzero",
                })
                .collect()
        }
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        list: List,
    }
    fn defaults(_: Profile) -> &'static str {
        "values=[]"
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<List>("list", defaults)]);
    let fixture = Files::new();
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(
        fixture.config(),
        "schema_version=1\n[domains.list]\nvalues=[\n'private-sentinel',\n0\n]",
    )
    .unwrap();
    let check = inspect_config(&fixture.roots, &ENTRIES).unwrap().check;
    assert_eq!(check.count, 2);
    assert_eq!(
        (check.findings[1].rule.as_str(), check.findings[1].line),
        ("domain", 5)
    );
    assert!(
        check.findings[1]
            .message
            .starts_with("domains.list.values.1:")
    );
}

#[test]
fn structured_arrays_without_a_complete_default_key_set_report_findings() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct List {
        values: Vec<Sample>,
    }
    impl DomainConfig for List {}
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        list: List,
    }
    fn defaults(_: Profile) -> &'static str {
        "values=[]"
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<List>("list", defaults)]);
    let fixture = Files::new();
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(
        fixture.config(),
        "schema_version=1\n[domains.list]\nvalues=[{limit=0,enabled=1}, {limit=0,enabled=true}]",
    )
    .unwrap();
    let check = inspect_config(&fixture.roots, &ENTRIES).unwrap().check;
    assert_eq!(check.count, 2);
    assert!(check.findings.iter().all(|finding| finding.rule == "type"));
}

#[test]
fn malformed_value_reports_its_recoverable_dotted_key_path() {
    let error = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.sample]\nenabled=["[..]),
        &DOMAINS,
    )
    .unwrap_err();
    assert!(
        matches!(error, ConfigError::Syntax { location: Location { key_path, line: 3, column, .. } } if key_path == "domains.sample.enabled" && column > 0)
    );
}

#[test]
fn inspect_config_returns_all_findings_and_binds_bytes_and_presence() {
    let files = Files::new();

    let absent = inspect_config(&files.roots, &domains::NO_DOMAINS).unwrap();
    std::fs::create_dir_all(files.roots.state.as_path()).unwrap();
    std::fs::write(files.config(), "schema_version=1\n").unwrap();
    let present = inspect_config(&files.roots, &domains::NO_DOMAINS).unwrap();
    assert_eq!(absent.check.findings, present.check.findings);
    assert_ne!(absent.input_digest, present.input_digest);
    let mut text = String::from("schema_version=1\n");
    for index in 0_u64..150 {
        text.push_str(&format!("unknown_{index}=true\n"));
    }
    std::fs::write(files.config(), &text).unwrap();
    let first = inspect_config(&files.roots, &domains::NO_DOMAINS).unwrap();
    assert_eq!(first.check.findings.len(), 150);
    assert_eq!(first.check.count, 150);
    text.push_str("# changed bytes, same findings\n");
    std::fs::write(files.config(), text).unwrap();
    let second = inspect_config(&files.roots, &domains::NO_DOMAINS).unwrap();
    assert_eq!(first.check, second.check);
    assert_ne!(first.input_digest, second.input_digest);
}

#[test]
fn inspect_config_preserves_malformed_input_digest_without_rereading() {
    let files = Files::new();

    std::fs::create_dir_all(files.roots.state.as_path()).unwrap();
    std::fs::write(files.config(), "schema_version=1\nbroken=[").unwrap();
    let first = inspect_config(&files.roots, &domains::NO_DOMAINS).unwrap();
    assert!(matches!(first.error, Some(ConfigError::Syntax { .. })));
    std::fs::write(files.config(), "schema_version=1\nbroken={").unwrap();
    let second = inspect_config(&files.roots, &domains::NO_DOMAINS).unwrap();
    assert!(matches!(second.error, Some(ConfigError::Syntax { .. })));
    assert_ne!(first.input_digest, second.input_digest);
}

#[test]
fn empty_compiled_typed_map_accepts_new_keys() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Maps {
        values: std::collections::BTreeMap<String, u64>,
    }
    impl DomainConfig for Maps {}
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        maps: Maps,
    }
    fn defaults(_: Profile) -> &'static str {
        "[values]"
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<Maps>("maps", defaults)]);
    let loaded = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.maps.values]\nnew_key=7"[..]),
        &ENTRIES,
    )
    .unwrap();
    assert_eq!(loaded.domains.maps.values["new_key"], 7);
}

#[test]
fn empty_compiled_closed_section_rejects_unknown_keys() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Closed {}
    impl DomainConfig for Closed {}
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        closed: Closed,
    }
    fn defaults(_: Profile) -> &'static str {
        ""
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<Closed>("closed", defaults)]);
    assert!(load_with_domains(&roots(), None::<&[u8]>, &ENTRIES).is_ok());
    assert!(
        load_with_domains(
            &roots(),
            Some(&b"schema_version=1\n[domains.closed]\nunknown=7"[..]),
            &ENTRIES
        )
        .is_err()
    );
}

fn state_roots(dir: &Path) -> Roots {
    let mut inputs = RootInputs::new("/fixture/home".into(), "/fixture/checkout".into());
    inputs.state_dir = Some(dir.into());
    inputs.resolve(&Resolver).unwrap()
}
fn knob_edit(roots: &Roots, edit: KnobEdit) -> Result<ResolvedConfig, ConfigError> {
    edit_config(roots, &crate::domains::NO_DOMAINS, |document| {
        edit.apply(document)
    })
}
#[test]
fn set_knob_is_written_atomically_and_shown_with_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());

    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "schema_version = 1 # keep\nknobs = { \"review.extra_reviewers\" = 1 } # inline\n",
    )
    .unwrap();
    let shown = knob_edit(
        &roots,
        KnobEdit::Set {
            name: "review.extra_reviewers",
            value: "2",
        },
    )
    .unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("# inline"));
    assert_eq!(shown.knobs["review.extra_reviewers"].value, 2);
    assert_eq!(read(&roots).unwrap(), shown);
    knob_edit(&roots, KnobEdit::Unset("review.extra_reviewers")).unwrap();
    assert_eq!(
        read(&roots).unwrap().knobs["review.extra_reviewers"].source,
        KnobSource::Default
    );
    knob_edit(&roots, KnobEdit::Profile("rigorous")).unwrap();
    let rigorous = read(&roots).unwrap();
    let knob = &rigorous.knobs["review.extra_reviewers"];
    assert_eq!(
        (knob.value.as_i64(), knob.source),
        (Some(1), KnobSource::Profile)
    );
    knob_edit(&roots, KnobEdit::Profile("economy")).unwrap();
    knob_edit(
        &roots,
        KnobEdit::Set {
            name: "lanes.max_concurrency",
            value: "2",
        },
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# keep"));
    let shown = read(&roots).unwrap();
    assert_eq!(shown.profile, Profile::Economy);
    let knob = &shown.knobs["lanes.max_concurrency"];
    assert_eq!(
        (knob.value.as_i64(), knob.source),
        (Some(2), KnobSource::Override)
    );
    assert_eq!(shown.knobs["review.polish"].source, KnobSource::Profile);
    knob_edit(&roots, KnobEdit::Unset("lanes.max_concurrency")).unwrap();
    let shown = read(&roots).unwrap();
    assert_eq!(
        shown.knobs["lanes.max_concurrency"].source,
        KnobSource::Profile
    );
}
#[test]
fn set_rejects_out_of_range_values_and_unknown_knobs() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());
    let set = |name, value| knob_edit(&roots, KnobEdit::Set { name, value });
    assert!(matches!(
        set("lanes.max_concurrency", "99"),
        Err(ConfigError::KnobValue {
            rule: "outside_allowed_range",
            ..
        })
    ));
    match set("lanes.max_concurrenc", "2") {
        Err(ConfigError::UnknownKnob { close, .. }) => {
            assert_eq!(close, ["lanes.max_concurrency"]);
        }
        other => panic!("expected unknown knob, got {other:?}"),
    }
    // The advisory knobs moved into the orchestrate skill's prose.
    assert!(matches!(
        set("effort.shift", "1"),
        Err(ConfigError::UnknownKnob { .. })
    ));
    assert!(!dir.path().join("config.toml").exists());
}
#[test]
fn knob_edit_refuses_to_rewrite_an_invalid_config() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());
    let path = dir.path().join("config.toml");
    let invalid = "schema_version = 1\nunknown = 1\n";
    std::fs::write(&path, invalid).unwrap();
    assert!(matches!(
        knob_edit(&roots, KnobEdit::Profile("rigorous")),
        Err(ConfigError::Schema {
            rule: "unknown_key",
            ..
        })
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), invalid);
}
#[test]
fn set_creates_a_missing_private_state_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let roots = state_roots(&state);
    knob_edit(&roots, KnobEdit::Profile("rigorous")).unwrap();
    assert_eq!(
        std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(read(&roots).unwrap().profile, Profile::Rigorous);
}

