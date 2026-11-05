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
fn census_exclude_ancestors_defaults_true_and_can_be_disabled() {
    let name = "confinement.census_exclude_ancestors";
    let defaults = load::<&[u8]>(&roots(), None).unwrap();
    assert!(defaults.census_exclude_ancestors().unwrap());
    let mut doc = toml_edit::DocumentMut::new();
    doc["schema_version"] = toml_edit::value(1);
    KnobEdit::Set {
        name,
        value: "false",
    }
    .apply(&mut doc)
    .unwrap();
    let configured = load(&roots(), Some(doc.to_string().as_bytes())).unwrap();
    assert!(!configured.census_exclude_ancestors().unwrap());
}

#[test]
fn census_ignore_knob_round_trips_and_refuses_task_overrides_or_malformed_authority() {
    let name = "confinement.census_ignore_unreadable";
    let defaults = load::<&[u8]>(&roots(), None).unwrap();
    assert!(defaults.census_ignore_unreadable().unwrap().is_empty());
    let value = r#"[{"path":"/Library/Daemon","team_id":"TEAM123"},{"path":"/usr/bin/login","team_id":"platform"}]"#;
    let mut doc = toml_edit::DocumentMut::new();
    doc["schema_version"] = toml_edit::value(1);
    KnobEdit::Set { name, value }.apply(&mut doc).unwrap();
    let config = load(&roots(), Some(doc.to_string().as_bytes())).unwrap();
    let entries = config.census_ignore_unreadable().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, Path::new("/Library/Daemon"));
    assert_eq!(entries[1].team_id, "platform");
    for value in ["[]", value] {
        assert!(
            config
                .dispatch_profile(&[format!("{name}={value}")])
                .is_err(),
            "host-only authority"
        );
    }
    for value in [
        r#"[{}]"#,
        r#"[{"path":"relative","team_id":"TEAM123"}]"#,
        r#"[{"path":"/a/../b","team_id":"TEAM123"}]"#,
        r#"[{"path":"/a","team_id":""}]"#,
        r#"[{"path":"/a","team_id":"TEAM123","extra":true}]"#,
        r#"[{"path":"/a","team_id":"A"},{"path":"/a","team_id":"B"}]"#,
    ] {
        assert!(
            KnobEdit::Set { name, value }.apply(&mut doc).is_err(),
            "{value}"
        );
    }
    for text in [
        "schema_version=1\n[knobs]\n'confinement.census_ignore_unreadable'='all'",
        "schema_version=1\n[knobs]\n'confinement.census_ignore_unreadable'=[{path='/a',team_id=''}]",
    ] {
        assert!(
            load(&roots(), Some(text.as_bytes())).is_err(),
            "strict authority must not recover"
        );
    }
    KnobEdit::Unset(name).apply(&mut doc).unwrap();
    assert!(
        load(&roots(), Some(doc.to_string().as_bytes()))
            .unwrap()
            .census_ignore_unreadable()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn explicit_profile_selects_the_compiled_preset() {
    let config = load(
        &roots(),
        Some(&b"schema_version = 1\nprofile = 'economy'\n"[..]),
    )
    .unwrap();
    assert_eq!(config.profile, Profile::Economy);
}

#[test]
fn provider_update_settings_defaults_opt_out_commands_and_validation() {
    let defaults = load::<&[u8]>(&roots(), None).unwrap();
    assert_eq!(defaults.providers["codex"], ProviderSettings::default());
    assert_eq!(defaults.providers["claude"], ProviderSettings::default());
    test_support::assert_leaf_origins(&defaults);
    let text = "schema_version=1\n[providers.codex]\nupdate=false\n[providers.claude]\nupdate={command=['claude','update']}\nupdate_interval_seconds=3600\nupdate_timeout_seconds=600";
    let config = load(&roots(), Some(text.as_bytes())).unwrap();
    assert_eq!(
        config.providers["codex"].update,
        ProviderUpdate::Enabled(false)
    );
    assert_eq!(
        config.providers["claude"].update,
        ProviderUpdate::Command {
            command: vec!["claude".into(), "update".into()]
        }
    );
    assert_eq!(config.providers["claude"].update_interval_seconds, 3600);
    assert_eq!(config.providers["claude"].update_timeout_seconds, 600);
    let capped = "schema_version=1\n[providers.codex]\nruntime_deadline_seconds=7200";
    let capped = load(&roots(), Some(capped.as_bytes())).unwrap();
    assert_eq!(
        capped.providers["codex"].runtime_deadline_seconds,
        Some(7200)
    );
    assert_eq!(config.providers["opencode"], ProviderSettings::default());
    let registered = load(&roots(), Some(b"schema_version=1\n[providers.fixture-provider]\nupdate=false\nruntime_deadline_seconds=120".as_slice())).unwrap();
    assert_eq!(
        registered.providers["fixture-provider"].update,
        ProviderUpdate::Enabled(false)
    );
    assert_eq!(
        registered.providers["fixture-provider"].runtime_deadline_seconds,
        Some(120)
    );
    test_support::assert_leaf_origins(&config);
    for (section, setting, rule) in [
        ("codex", "update=42", "type"),
        ("codex", "update='shell string'", "type"),
        ("codex", "update={command=[],typo=1}", "type"),
        ("codex", "update={command=[]}", "provider_update_command"),
        ("codex", "update={command=['']}", "provider_update_command"),
        (
            "codex",
            "update_timeout_seconds=0",
            "update_timeout_seconds_range",
        ),
        ("codex", "update_timeout_seconds=-1", "type"),
        (
            "codex",
            "update_interval_seconds=0",
            "update_interval_seconds_range",
        ),
        ("codex", "update_interval_seconds=-1", "type"),
        (
            "codex",
            "runtime_deadline_seconds=0",
            "runtime_deadline_seconds_range",
        ),
        (
            "codex",
            "runtime_deadline_seconds=14401",
            "runtime_deadline_seconds_range",
        ),
        ("codex", "runtime_deadline_seconds=-1", "type"),
        ("codex", "typo=false", "unknown_key"),
    ] {
        let text = format!("schema_version=1\n[providers.{section}]\n{setting}");
        let config = load(&roots(), Some(text.as_bytes())).unwrap();
        assert_warning(&config, rule);
        assert_eq!(config.providers["codex"], ProviderSettings::default());
    }
}

#[test]
fn retention_defaults_overrides_and_invalid_values_are_named_cases() {
    for (case, key, override_value, invalid_range_values, invalid_type) in [
        (
            "provider state days",
            "provider_state_days",
            14,
            ["0", "3651"],
            "'seven'",
        ),
        (
            "session revisions",
            "session_revisions",
            20,
            ["0", "65"],
            "'many'",
        ),
    ] {
        let defaults = load::<&[u8]>(&roots(), None).unwrap();
        assert!(defaults.retention.validate().is_ok());
        let defaults = serde_json::to_value(defaults).unwrap();
        let default = &defaults["retention"][key];
        let text = format!("schema_version=1\n[retention]\n{key}={override_value}");
        let config = load(&roots(), Some(text.as_bytes())).unwrap();
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["retention"][key], override_value, "{case} override");
        if key == "session_revisions" {
            assert_eq!(
                value["retention"]["provider_state_days"],
                defaults["retention"]["provider_state_days"],
                "{case}"
            );
        }
        test_support::assert_leaf_origins(&config);
        for (value, rule) in invalid_range_values
            .into_iter()
            .map(|value| (value, format!("{key}_range")))
            .chain([("-1", "type".to_owned()), (invalid_type, "type".to_owned())])
        {
            let text = format!("schema_version=1\n[retention]\n{key}={value}");
            let config = load(&roots(), Some(text.as_bytes())).unwrap();
            assert_warning(&config, &rule);
            assert_warning_message(&config, &format!("retention.{key}:"));
            let parsed = serde_json::to_value(config).unwrap();
            assert_eq!(&parsed["retention"][key], default, "{case}: {value}");
        }
    }
}

#[test]
fn chair_log_retention_and_gc_settings_are_named_cases() {
    for (case, check) in [
        (
            "chair log retention",
            assert_chair_log_retention_defaults_overrides_and_validation as fn(),
        ),
        (
            "garbage collection settings",
            assert_gc_settings_parse_defaults_and_warnings,
        ),
    ] {
        if let Err(panic) = std::panic::catch_unwind(check) {
            if let Some(message) = panic.downcast_ref::<String>() {
                panic!("{case}: {message}");
            }
            if let Some(message) = panic.downcast_ref::<&str>() {
                panic!("{case}: {message}");
            }
            panic!("{case} failed");
        }
    }
}

fn assert_chair_log_retention_defaults_overrides_and_validation() {
    let defaults = load::<&[u8]>(&roots(), None).unwrap();
    assert!(defaults.retention.validate().is_ok());
    let short = load(
        &roots(),
        Some(&b"schema_version=1\n[retention]\nchair_log_days=1"[..]),
    )
    .unwrap();
    assert_eq!(short.retention.chair_log_replay_seconds(), 7 * 86_400);
    let config = load(&roots(), Some(&b"schema_version=1\n[retention]\nchair_log_days=12\nchair_log_replay_days=8\nchair_log_entries=20\nchair_log_bytes=65536"[..])).unwrap();
    test_support::assert_leaf_origins(&config);
    let value = serde_json::to_value(config).unwrap();
    assert_eq!(value["retention"]["chair_log_days"], 12);
    assert_eq!(value["retention"]["chair_log_replay_days"], 8);
    assert_eq!(value["retention"]["chair_log_entries"], 20);
    assert_eq!(value["retention"]["chair_log_bytes"], 65_536);
    for (key, value) in [
        ("chair_log_days", "0"),
        ("chair_log_days", "3651"),
        ("chair_log_replay_days", "6"),
        ("chair_log_replay_days", "3651"),
        ("chair_log_entries", "0"),
        ("chair_log_entries", "100001"),
        ("chair_log_bytes", "65535"),
        ("chair_log_bytes", "1073741825"),
    ] {
        let text = format!("schema_version=1\n[retention]\n{key}={value}");
        let config = load(&roots(), Some(text.as_bytes())).unwrap();
        assert_warning(&config, &format!("{key}_range"));
        assert_warning_message(&config, &format!("retention.{key}:"));
        assert_eq!(config.retention, RetentionSettings::default());
    }
}

fn assert_gc_settings_parse_defaults_and_warnings() {
    let default_config = load::<&[u8]>(&roots(), None).unwrap();
    assert!(matches!(
        default_config.origins["gc.graph_checkpoint_days"],
        Origin::Compiled { .. }
    ));
    let defaults = serde_json::to_value(default_config).unwrap();
    assert_eq!(defaults["gc"]["schedule"], true);
    assert_eq!(defaults["gc"]["idle_days"], 7);
    assert_eq!(defaults["gc"]["graph_checkpoint_days"], 90);
    assert!(defaults["gc"]["free_floor"].is_null());
    let config = load(
        &roots(),
        Some(&b"schema_version=1\n[gc]\nschedule=false\nidle_days=14\ngraph_checkpoint_days=30\nfree_floor='20G'"[..]),
    )
    .unwrap();
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["gc"]["schedule"], false);
    assert_eq!(value["gc"]["idle_days"], 14);
    assert_eq!(value["gc"]["graph_checkpoint_days"], 30);
    assert!(matches!(
        config.origins["gc.graph_checkpoint_days"],
        Origin::User { .. }
    ));
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
        ("graph_checkpoint_days=0", "graph_checkpoint_days_range"),
        ("graph_checkpoint_days=3651", "graph_checkpoint_days_range"),
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
        let config = load(&roots(), Some(text.as_bytes())).unwrap();
        assert_warning(&config, rule);
        assert_warning_message(
            &config,
            &format!("gc.{}:", field.split('=').next().unwrap()),
        );
        assert_eq!(config.gc.idle_days, 7);
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
                detail: None,
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
    let mut samples = Vec::new();
    for profile in ["balanced", "economy"] {
        let text = format!("schema_version=1\nprofile='{profile}'");
        let snapshot = load_with_domains(&roots(), Some(text.as_bytes()), &DOMAINS).unwrap();
        samples.push(snapshot.domains.sample.clone());
        test_support::assert_leaf_origins(&snapshot);
    }
    assert_ne!(samples[0], samples[1]);
    test_support::assert_leaf_origins(&load::<io::Empty>(&roots(), None).unwrap());
}

#[test]
fn config_warns_on_optional_input_but_refuses_syntax_and_versions_without_echoing_input() {
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
        ("schema_version=1\nprofile='rigorous'", "range"),
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
        ("schema_version=1\n[domains.unknown]", "unknown_key"),
        ("schema_version=1\n[task_classes]", "unknown_key"),
        ("schema_version=1\n[knobs]\n'effort.shift'=1", "unknown_key"),
        (
            "schema_version=1\n[knobs]\n'review.polish'='always'",
            "unknown_key",
        ),
        ("schema_version=18446744073709551616", "syntax"),
        ("schema_version=1\n[domains]\nx=inf", "unknown_key"),
        ("schema_version=1\n[domains]\nx=2026-01-01", "unknown_key"),
        ("schema_version=1\nx=['private-sentinel'", "syntax"),
        (
            "schema_version=1\n[domains.sample]\nlimit=4\n[domains.sample]\nenabled=false",
            "syntax",
        ),
    ] {
        let loaded = load(&roots(), Some(text.as_bytes()));
        if (text.contains("profile='") && refusal == "range")
            || (text.contains("[knobs]") && refusal == "unknown_key")
            || matches!(refusal, "missing" | "version" | "syntax")
            || (text.starts_with("schema_version=")
                && !text.starts_with("schema_version=1\n")
                && refusal == "type")
        {
            let error = loaded.unwrap_err();
            assert_refusal(&error, refusal);
            assert!(!format!("{error:?} {error}").contains("private-sentinel"));
        } else {
            let config = loaded.unwrap();
            assert_warning(&config, refusal);
            assert!(!format!("{:?}", config.warnings).contains("private-sentinel"));
            assert_eq!(config.profile, Profile::Balanced);
        }
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
fn domain_registration_warns_and_uses_defaults_for_invalid_user_values() {
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
        let loaded = load_with_domains(&roots(), Some(text.as_bytes()), &DOMAINS);
        if refusal == "syntax" {
            assert_refusal(&loaded.unwrap_err(), refusal);
        } else {
            let config = loaded.unwrap();
            assert_warning(&config, refusal);
            assert!(!format!("{:?}", config.warnings).contains("private-sentinel"));
            assert_eq!(
                config.domains.sample,
                Sample {
                    limit: 8,
                    enabled: true
                }
            );
        }
    }
    let config = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.sample]\nlimit=0"[..]),
        &DOMAINS,
    )
    .unwrap();
    assert!(
        config
            .warnings
            .iter()
            .any(|warning| warning.message.starts_with("domains.sample.limit:"))
    );
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
fn github_policy_is_scoped_to_project_and_defaults_only_when_absent() {
    let fixture = Files::new();
    let state = fixture.roots.state.as_path();
    std::fs::create_dir(state).unwrap();
    let project = provenant_core::ProjectId::from_bytes([1; 16]);
    let other = provenant_core::ProjectId::from_bytes([2; 16]);
    assert_eq!(
        GithubPolicy::read(&fixture.roots.state, project).unwrap(),
        GithubPolicy::default()
    );
    let dir = state.join("projects").join(project.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("github.json"),
        r#"{"deny_verbs":["release create"],"deny_labels":[" ci ","", " \t ","needs review"]}"#,
    )
    .unwrap();
    let policy = GithubPolicy::read(&fixture.roots.state, project).unwrap();
    assert_eq!(policy.deny_verbs, ["release create"]);
    assert_eq!(policy.deny_labels, ["ci", "needs review"]);
    assert_eq!(
        GithubPolicy::read(&fixture.roots.state, other).unwrap(),
        GithubPolicy::default()
    );
    std::fs::write(dir.join("github.json"), b"{broken").unwrap();
    assert!(matches!(
        GithubPolicy::read(&fixture.roots.state, project),
        Err(ConfigError::InvalidGithubPolicy)
    ));
}

#[test]
fn show_and_check_are_read_only_and_preserve_hard_schema_errors() {
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
    let original = b"# User-owned; preserve exactly.\nschema_version=1\nprofile='economy'\n";
    std::fs::write(fixture.config(), original).unwrap();
    assert_eq!(
        inspect_config(&fixture.roots, &domains::NO_DOMAINS)
            .unwrap()
            .check
            .profile,
        Profile::Economy
    );
    assert_eq!(
        config_show(&fixture.roots, &domains::NO_DOMAINS)
            .unwrap()
            .profile,
        Profile::Economy
    );
    assert_eq!(std::fs::read(fixture.config()).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(fixture.roots.state.as_path())
            .unwrap()
            .count(),
        1
    );
    std::fs::write(fixture.config(), b"schema_version = 'private-sentinel'").unwrap();
    let inspection = inspect_config(&fixture.roots, &domains::NO_DOMAINS).unwrap();
    assert!(inspection.check.findings.is_empty());
    assert!(matches!(
        inspection.error,
        Some(ConfigError::Schema { rule: "type", .. })
    ));
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
fn warning_source_locations_survive_default_merging_and_inline_tables() {
    for (text, key, line) in [
        (
            "schema_version=1\n[domains.sample]\n  surprise=5",
            "domains.sample.surprise",
            3,
        ),
        (
            "schema_version=1\n[domains]\nsample = { enabled = 9 }",
            "domains.sample.enabled",
            3,
        ),
    ] {
        let config = load_with_domains(&roots(), Some(text.as_bytes()), &DOMAINS).unwrap();
        assert_eq!(config.warnings.len(), 1);
        let warning = &config.warnings[0];
        assert_eq!(
            warning.path.to_relative_path().unwrap().as_path(),
            Path::new("config.toml")
        );
        assert_eq!(warning.line, line);
        assert!(warning.message.starts_with(&format!("{key}:")));
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

fn assert_warning<T>(config: &ResolvedConfig<T>, rule: &str) {
    assert!(
        config.warnings.iter().any(|warning| warning.rule == rule),
        "missing {rule} warning: {:?}",
        config.warnings
    );
    for warning in &config.warnings {
        assert_eq!(
            warning.path.to_relative_path().unwrap().as_path(),
            Path::new("config.toml")
        );
        assert!(warning.line > 0);
    }
}

fn assert_warning_message<T>(config: &ResolvedConfig<T>, expected: &str) {
    assert!(
        config
            .warnings
            .iter()
            .any(|warning| warning.message.starts_with(expected)),
        "missing message {expected:?}: {:?}",
        config.warnings
    );
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
fn replacement_array_tables_warn_and_omit_rows_with_missing_fields() {
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
    let config = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.list]\nvalues=[{limit=2}, {enabled=false}]"[..]),
        &ENTRIES,
    )
    .unwrap();
    assert_warning(&config, "type");
    assert!(config.domains.list.values.is_empty());
}

#[test]
fn empty_default_array_accepts_valid_typed_table_rows() {
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct List {
        rows: Vec<Sample>,
    }
    impl DomainConfig for List {}
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Aggregate {
        list: List,
    }
    fn defaults(_: Profile) -> &'static str {
        "rows=[]"
    }
    const ENTRIES: DomainSections<Aggregate> =
        DomainSections::new(&[DomainSection::new::<List>("list", defaults)]);
    let input = b"schema_version=1\n[domains.list]\nrows=[{limit=2,enabled=false}]";
    let config = load_with_domains(&roots(), Some(&input[..]), &ENTRIES).unwrap();
    assert_eq!(config.domains.list.rows.len(), 1);
    let invalid = b"schema_version=1\n[domains.list]\nrows=[{limit=2}]";
    let invalid = load_with_domains(&roots(), Some(&invalid[..]), &ENTRIES).unwrap();
    assert_warning(&invalid, "type");
    assert!(invalid.domains.list.rows.is_empty());
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
                    detail: None,
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
fn structured_arrays_with_empty_defaults_isolate_invalid_rows() {
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
    assert_eq!(check.count, 1);
    assert!(check.findings.iter().all(|finding| finding.rule == "type"));
    let config = read_with_domains(&fixture.roots, &ENTRIES).unwrap();
    assert_eq!(config.domains.list.values.len(), 1);
    assert_eq!(
        config.domains.list.values[0],
        Sample {
            limit: 0,
            enabled: true
        }
    );
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
fn empty_compiled_closed_section_warns_on_unknown_keys() {
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
    let config = load_with_domains(
        &roots(),
        Some(&b"schema_version=1\n[domains.closed]\nunknown=7"[..]),
        &ENTRIES,
    )
    .unwrap();
    assert_warning(&config, "unknown_key");
}

pub(crate) fn state_roots(dir: &Path) -> Roots {
    let mut inputs = RootInputs::new("/fixture/home".into(), "/fixture/checkout".into());
    inputs.state_dir = Some(dir.into());
    inputs.resolve(&Resolver).unwrap()
}
fn knob_edit(roots: &Roots, edit: KnobEdit) -> Result<ResolvedConfig, ConfigError> {
    edit_config(
        roots,
        &crate::domains::NO_DOMAINS,
        &crate::EditOptions::new("test"),
        |document, _| edit.apply(document),
    )
    .map(|edited| edited.config)
}

#[test]
fn config_edit_bumps_change_generation() {
    let temp = tempfile::tempdir().unwrap();
    let roots = state_roots(temp.path());
    knob_edit(
        &roots,
        KnobEdit::Set {
            name: "lanes.max_concurrency",
            value: "2",
        },
    )
    .unwrap();
    assert_eq!(
        provenant_os::changes::read(temp.path())
            .unwrap()
            .configuration,
        Some(1)
    );
    knob_edit(&roots, KnobEdit::Unset("lanes.max_concurrency")).unwrap();
    assert_eq!(
        provenant_os::changes::read(temp.path())
            .unwrap()
            .configuration,
        Some(2)
    );
}

#[test]
fn config_edit_commits_when_the_change_signal_cannot_publish() {
    let temp = tempfile::tempdir().unwrap();
    let roots = state_roots(temp.path());
    // A directory at the signal name cannot be published. The write still succeeds.
    std::fs::create_dir(temp.path().join(provenant_core::changes::FILE_NAME)).unwrap();
    knob_edit(
        &roots,
        KnobEdit::Set {
            name: "lanes.max_concurrency",
            value: "2",
        },
    )
    .unwrap();
    assert!(temp.path().join("config.toml").exists());
}

#[test]
fn host_lane_build_settings_read_and_validate() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\nmax_concurrent_builds=2\ncargo_build_jobs=5\n",
    )
    .unwrap();
    let settings = crate::LaneBuildSettings::read(temp.path()).unwrap();
    assert_eq!(settings.max_concurrent_builds, Some(2));
    assert_eq!(settings.cargo_build_jobs, Some(5));
    std::fs::write(
        temp.path().join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\nmax_concurrent_builds=0\n",
    )
    .unwrap();
    // Invalid optional settings warn and use the same compiled defaults as startup.
    let settings = crate::LaneBuildSettings::read(temp.path()).unwrap();
    assert!(settings.invalid_keys().is_empty());
    assert_eq!(settings.max_concurrent_builds, None);
    std::fs::write(
        temp.path().join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\ncargo_build_jobs=-2\n",
    )
    .unwrap();
    assert_eq!(
        crate::LaneBuildSettings::read(temp.path())
            .unwrap()
            .cargo_build_jobs,
        None
    );
}
#[test]
fn set_knob_is_written_atomically_and_shown_with_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());

    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "schema_version = 1 # keep\nknobs = { \"lanes.max_concurrency\" = 1 } # inline\n",
    )
    .unwrap();
    let shown = knob_edit(
        &roots,
        KnobEdit::Set {
            name: "lanes.max_concurrency",
            value: "2",
        },
    )
    .unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("# inline"));
    assert_eq!(shown.knobs["lanes.max_concurrency"].value, 2);
    assert_eq!(read(&roots).unwrap(), shown);
    knob_edit(&roots, KnobEdit::Unset("lanes.max_concurrency")).unwrap();
    assert_eq!(
        read(&roots).unwrap().knobs["lanes.max_concurrency"].source,
        KnobSource::Default
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
    knob_edit(&roots, KnobEdit::Unset("lanes.max_concurrency")).unwrap();
    let shown = read(&roots).unwrap();
    assert_eq!(
        shown.knobs["lanes.max_concurrency"].source,
        KnobSource::Profile
    );
}
#[test]
fn writer_publishing_knob_defaults_off_and_can_be_enabled_per_dispatch() {
    let config = load::<&[u8]>(&roots(), None).unwrap();
    let profile = config.dispatch_profile(&[]).unwrap();
    assert_eq!(
        profile.knobs.get("lanes.publish"),
        Some(&serde_json::json!(false))
    );
    let enabled = config
        .dispatch_profile(&["lanes.publish=true".into()])
        .unwrap();
    assert_eq!(enabled.knobs["lanes.publish"], true);
    let config = load(
        &roots(),
        Some(b"schema_version=1\n[knobs]\n'lanes.publish'=true".as_slice()),
    )
    .unwrap();
    assert_eq!(
        config.dispatch_profile(&[]).unwrap().knobs["lanes.publish"],
        true
    );
}

#[test]
fn read_only_batch_concurrency_has_its_own_knob() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());
    let shown = read(&roots).unwrap();
    let writers = &shown.knobs["lanes.max_concurrency"];
    let readers = &shown.knobs["lanes.max_concurrency_read_only"];
    assert_eq!(
        (writers.value.as_i64(), readers.value.as_i64()),
        (Some(8), Some(16))
    );
    assert_eq!(readers.source, KnobSource::Default);
    let set = |name, value| knob_edit(&roots, KnobEdit::Set { name, value });
    set("lanes.max_concurrency_read_only", "16").unwrap();
    assert!(matches!(
        set("lanes.max_concurrency_read_only", "17"),
        Err(ConfigError::KnobValue {
            rule: "outside_allowed_range",
            ..
        })
    ));
    knob_edit(&roots, KnobEdit::Profile("economy")).unwrap();
    knob_edit(&roots, KnobEdit::Unset("lanes.max_concurrency_read_only")).unwrap();
    let shown = read(&roots).unwrap();
    assert_eq!(
        shown.knobs["lanes.max_concurrency_read_only"]
            .value
            .as_i64(),
        Some(8)
    );
}
#[test]
fn knob_is_set_tells_a_user_choice_from_a_default_or_preset() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());
    let name = "lanes.max_concurrency";
    assert!(!read(&roots).unwrap().knob_is_set(name, &[]));
    knob_edit(&roots, KnobEdit::Profile("economy")).unwrap();
    assert!(
        !read(&roots).unwrap().knob_is_set(name, &[]),
        "a preset is not a choice"
    );
    let shown = read(&roots).unwrap();
    assert!(shown.knob_is_set(name, &["lanes.max_concurrency=2".into()]));
    assert!(!shown.knob_is_set(name, &["lanes.max_concurrency_read_only=2".into()]));
    knob_edit(&roots, KnobEdit::Set { name, value: "3" }).unwrap();
    assert!(read(&roots).unwrap().knob_is_set(name, &[]));
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
            assert_eq!(
                close,
                ["lanes.max_concurrency", "lanes.max_concurrency_read_only"]
            );
        }
        other => panic!("expected unknown knob, got {other:?}"),
    }
    // The advisory knobs moved into the orchestrate skill's prose.
    assert!(matches!(
        set("review.polish", "always"),
        Err(ConfigError::UnknownKnob { .. })
    ));
    assert!(!dir.path().join("config.toml").exists());
}
#[test]
fn knob_edit_preserves_unknown_keys_while_applying_the_edit() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());
    let path = dir.path().join("config.toml");
    let invalid = "schema_version = 1\nunknown = 1\n";
    std::fs::write(&path, invalid).unwrap();
    let config = knob_edit(&roots, KnobEdit::Profile("economy")).unwrap();
    assert_eq!(config.profile, Profile::Economy);
    assert_warning(&config, "unknown_key");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("unknown = 1"));
    assert_eq!(read(&roots).unwrap().profile, Profile::Economy);
}
#[test]
fn migrate_drops_rigorous_profile_and_refused_knobs() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());
    let path = dir.path().join("config.toml");
    let content = "schema_version = 1\nprofile = 'rigorous'\n[knobs]\n'review.polish' = 'always'\n'domains.routing.capacity_cooldown_seconds' = 30\n'confinement.census_host_list_complete' = true\n'lanes.max_concurrency' = 4\n";
    std::fs::write(&path, content).unwrap();
    assert!(read(&roots).is_err());

    let report = crate::migrate(&roots, &crate::domains::NO_DOMAINS, false).unwrap();
    assert_eq!(report.migrations.len(), 2);
    let profile_m = report
        .migrations
        .iter()
        .find(|m| m.location == "profile")
        .unwrap();
    assert_eq!(profile_m.action, "dropped");
    assert_eq!(profile_m.keys, vec!["rigorous"]);
    let knobs_m = report
        .migrations
        .iter()
        .find(|m| m.location == "knobs")
        .unwrap();
    assert_eq!(knobs_m.action, "dropped");
    assert!(knobs_m.keys.contains(&"review.polish".to_owned()));
    assert!(
        knobs_m
            .keys
            .contains(&"domains.routing.capacity_cooldown_seconds".to_owned())
    );
    assert!(
        knobs_m
            .keys
            .contains(&"confinement.census_host_list_complete".to_owned())
    );

    let loaded = read(&roots).unwrap();
    assert_eq!(loaded.profile, Profile::Balanced);
    assert_eq!(loaded.knobs["lanes.max_concurrency"].value, 4);
    let after_text = std::fs::read_to_string(&path).unwrap();
    assert!(!after_text.contains("rigorous"));
    assert!(!after_text.contains("review.polish"));
    assert!(after_text.contains("lanes.max_concurrency"));
}
#[test]
fn migrate_preserves_unrecognised_knobs() {
    let dir = tempfile::tempdir().unwrap();
    let roots = state_roots(dir.path());
    let path = dir.path().join("config.toml");
    let original =
        "schema_version = 1\n[knobs]\n'review.polish' = 'always'\n'domains.routing.future' = 1\n";
    std::fs::write(&path, original).unwrap();
    assert!(crate::migrate(&roots, &crate::domains::NO_DOMAINS, false).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}
#[test]
fn set_creates_a_missing_private_state_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let roots = state_roots(&state);
    knob_edit(&roots, KnobEdit::Profile("economy")).unwrap();
    assert_eq!(
        std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(read(&roots).unwrap().profile, Profile::Economy);
}

#[test]
fn unknown_key_in_a_serde_decoded_section_reports_unknown_key_at_the_key() {
    for (text, key_path, line) in [
        (
            "schema_version=1\n[install]\nunknown=true",
            "install.unknown",
            3,
        ),
        ("schema_version=1\n[gc]\nunknown=true", "gc.unknown", 3),
        (
            "schema_version=1\n[gc]\ncensus_excused_executables=['/usr/libexec/daemon']",
            "gc.census_excused_executables",
            3,
        ),
        (
            "schema_version=1\n[retention]\nunknown=true",
            "retention.unknown",
            3,
        ),
        // The same name at two depths: the one serde rejected, not the first.
        (
            "schema_version=1\n[gc]\nidle_days=7\n[gc.projects.p]\nidle_days=7",
            "gc.projects.p.idle_days",
            5,
        ),
        (
            "schema_version=1\n[gc]\nunknown=true\n[gc.projects.p]\nunknown=true",
            "gc.unknown",
            3,
        ),
        (
            "schema_version=1\n[gc.projects.p]\nunknown=true",
            "gc.projects.p.unknown",
            3,
        ),
        (
            "schema_version=1\n[install.targets.claude]\nunknown=true",
            "install.targets.claude.unknown",
            3,
        ),
    ] {
        let config = load(&roots(), Some(text.as_bytes())).unwrap();
        assert_warning(&config, "unknown_key");
        assert!(
            config.warnings.iter().any(|warning| warning.line == line
                && warning.message.starts_with(&format!("{key_path}:"))),
            "{text}: {:?}",
            config.warnings
        );
    }
}

#[test]
fn wrong_value_type_in_a_serde_decoded_section_still_reports_type() {
    for text in [
        "schema_version=1\n[install]\nbin_dir=4",
        "schema_version=1\n[gc]\nidle_days='7'",
        "schema_version=1\n[retention]\nprovider_state_days='7'",
    ] {
        let config = load(&roots(), Some(text.as_bytes())).unwrap();
        assert_warning(&config, "type");
    }
}

#[test]
fn read_knobs_ignores_domain_sections_it_does_not_know() {
    let fixture = Files::new();
    let state = fixture.roots.state.as_path();
    std::fs::create_dir(state).unwrap();
    std::fs::write(
        fixture.config(),
        "schema_version = 1\n[domains.fabric.lanes.cargo_seed]\nenabled = false\n",
    )
    .unwrap();
    // Without the domain sections every configured domain is an unknown key.
    // `read_knobs` is `config_show`: it never prints that advisory.
    let loud = config_show(&fixture.roots, &DomainSections::<NoDomains>::new(&[])).unwrap();
    assert!(
        loud.warnings.iter().any(|w| w.rule == "unknown_key"),
        "{:?}",
        loud.warnings
    );
    let quiet = read_knobs(&fixture.roots).unwrap();
    assert!(quiet.census_ignore_unreadable().unwrap().is_empty());
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReplaceTableDomains {
    routing: ReplaceTableConfig,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReplaceTableConfig {
    rules: std::collections::BTreeMap<String, bool>,
}
impl DomainConfig for ReplaceTableConfig {
    fn validate(&self) -> Vec<SectionError> {
        Vec::new()
    }
}
fn replace_table_defaults(_: Profile) -> &'static str {
    "[rules]\nbundled = true\n"
}
const REPLACE_TABLE_SECTION: DomainSection =
    DomainSection::new::<ReplaceTableConfig>("routing", replace_table_defaults)
        .replacing_tables(&["rules"]);
const REPLACE_TABLE_DOMAINS: DomainSections<ReplaceTableDomains> =
    DomainSections::new(&[REPLACE_TABLE_SECTION]);

#[test]
fn owner_marked_tables_replace_defaults_and_allow_empty_overrides() {
    for (value, expected) in [
        (
            "[domains.routing.rules]\ncustom = false\n",
            std::collections::BTreeMap::from([("custom".into(), false)]),
        ),
        (
            "[domains.routing.rules]\n",
            std::collections::BTreeMap::new(),
        ),
    ] {
        let text = format!("schema_version = 1\n{value}");
        let config =
            load_with_domains(&roots(), Some(text.as_bytes()), &REPLACE_TABLE_DOMAINS).unwrap();
        assert_eq!(config.domains.routing.rules, expected);
    }
}

#[test]
fn registered_knobs_have_production_readers() {
    fn production_only(text: &str) -> String {
        let mut result = String::new();
        let mut skip = false;
        let mut depth = 0_i32;
        let mut opened = false;
        for line in text.lines() {
            if line.trim() == "#[cfg(test)]" {
                skip = true;
                depth = 0;
                opened = false;
                continue;
            }
            if skip {
                depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
                opened |= line.contains('{');
                if (opened && depth == 0) || (!opened && line.trim_end().ends_with(';')) {
                    skip = false;
                }
            } else if !line.trim_start().starts_with("//") {
                result.push_str(line);
                result.push('\n');
            }
        }
        result
    }
    assert!(
        !production_only("#[cfg(test)]\nmod tests {\n get(\"unread.knob\");\n}\n")
            .contains("unread.knob")
    );
    fn sources(path: &std::path::Path, output: &mut String) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, output);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().unwrap() != "knobs.rs"
                && !path.file_name().unwrap().to_string_lossy().contains("test")
            {
                let text = std::fs::read_to_string(path).unwrap();
                output.push_str(&production_only(&text));
                output.push('\n');
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut source = String::new();
    sources(&root.join("src"), &mut source);
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            sources(&src, &mut source);
        }
    }
    let source: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    fn has_reader(source: &str, name: &str) -> bool {
        // Confinement knobs are read through typed accessors on the config.
        if let Some(accessor) = name.strip_prefix("confinement.") {
            return source.contains(&format!(".{accessor}()"));
        }
        [".get(", "get(", "knob(", ".knob_u64(", ".knob_is_set("]
            .iter()
            .any(|call| source.contains(&format!("{call}\"{name}\"")))
            || (name.starts_with("routing.") && source.contains(&format!("(\"{name}\",")))
    }
    assert!(!has_reader("log(\"unread.knob\", value)", "unread.knob"));
    assert!(has_reader(
        "config.knobs.get(\"unread.knob\")",
        "unread.knob"
    ));
    for name in crate::knobs::registry().unwrap().knobs.keys() {
        assert!(
            has_reader(&source, name),
            "registered knob {name} has no reader"
        );
    }
}

#[test]
fn misplaced_domain_knob_points_to_its_domain_key() {
    let error = load(
        &roots(),
        Some(&b"schema_version=1\n[knobs]\n'domains.routing.capacity_cooldown_seconds'=30"[..]),
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("domains.routing.capacity_cooldown_seconds"));
    assert!(message.contains("outside [knobs]"));
}

#[test]
fn unknown_domain_knob_is_an_unknown_key_not_misplaced() {
    let error = load(
        &roots(),
        Some(&b"schema_version=1\n[knobs]\n'domains.routing.future'=1"[..]),
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        ConfigError::Schema {
            rule: "unknown_key",
            ..
        }
    ));
    assert!(!error.to_string().contains("outside [knobs]"));
}
