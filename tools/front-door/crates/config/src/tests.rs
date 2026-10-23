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
    assert_eq!(config.profile, Profile::Economy);
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
const DOMAINS: DomainSections<TestDomains> = DomainSections {
    balanced: "[sample]\nlimit = 8\nenabled = true\n",
    economy: "[sample]\nlimit = 4\nenabled = true\n",
    rigorous: "[sample]\nlimit = 8\nenabled = false\n",
    validate: |data| {
        if data.sample.limit == 0 || data.sample.limit > 8 {
            Err(SectionError {
                field: "sample.limit",
                code: "outside_allowed_range",
            })
        } else {
            Ok(())
        }
    },
};
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
fn absence_and_explicit_presets_have_typed_intent_without_domain_policy_copies() {
    let default = load::<io::Empty>(&roots(), None).unwrap();
    assert_eq!(default.profile, Profile::Balanced);
    assert_eq!(default.preset.concurrency_cap, None);
    assert_eq!(default.preset.effort, EffortPolicy::Default);
    assert_eq!(
        default.origins.get("profile"),
        Some(&Origin::Compiled {
            profile: Profile::Balanced
        })
    );
    let economy = load(&roots(), Some(&b"schema_version=1\nprofile='economy'"[..])).unwrap();
    assert_eq!(economy.preset.concurrency_cap, Some(4));
    assert_eq!(economy.preset.context_cap_tokens, Some(200_000));
    assert_eq!(
        economy.preset.pool_selection,
        PoolSelection::WeightedRotation
    );
    let rigorous = load(&roots(), Some(&b"schema_version=1\nprofile='rigorous'"[..])).unwrap();
    assert_eq!(
        rigorous.preset.effort,
        EffortPolicy::RaiseExceptMechanicalAndBulk
    );
    assert_eq!(rigorous.preset.additional_reviews, 1);
    assert_eq!(rigorous.preset.cross_family, CrossFamily::Require);
    assert_eq!(rigorous.preset.rereview_scope, RereviewScope::Full);
}

#[test]
fn closed_toml_boundary_refuses_unknowns_duplicates_bad_types_and_versions_without_echoing_input() {
    for text in [
        "",
        "# no version",
        "profile='economy'",
        "schema_version=2",
        "schema_version=-1",
        "schema_version='1'",
        "schema_version=true",
        "schema_version=1.0",
        "schema_version=1\nprofile='private-sentinel'",
        "schema_version=1\nprofile=4",
        "schema_version=1\nprofile='economy'\nprofile='balanced'",
        "schema_version=1\nschema_version=1",
        "schema_version=1\ninclude='private-sentinel'",
        "schema_version=1\n[knobs]",
        "schema_version=1\n[domains.unknown]",
        "schema_version=1\n[install]",
        "schema_version=1\n[task_classes]",
        "schema_version=18446744073709551616",
        "schema_version=1\n[domains]\nx=inf",
        "schema_version=1\n[domains]\nx=2026-01-01",
        "schema_version=1\nx=['private-sentinel'",
        "schema_version=1\n[domains.sample]\nlimit=4\n[domains.sample]\nenabled=false",
    ] {
        let error = load(&roots(), Some(text.as_bytes())).unwrap_err();
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
    for text in [
        "schema_version=1\n[domains.sample]\nlimit=0",
        "schema_version=1\n[domains.sample]\nlimit=9",
        "schema_version=1\n[domains.sample]\nlimit=-1",
        "schema_version=1\n[domains.sample]\nlimit=4.0",
        "schema_version=1\n[domains.sample]\nlimit=9223372036854775808",
        "schema_version=1\n[domains.sample]\nsecret='private-sentinel'",
        "schema_version=1\n[domains.other]",
    ] {
        assert!(load_with_domains(&roots(), Some(text.as_bytes()), &DOMAINS).is_err());
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
                field: "sample.limit",
                ..
            },
            ..
        }
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
        matches!(load(&roots(), Some(Fails)), Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied)
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
    assert!(load(&roots(), Some(deep.as_bytes())).is_err());
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
        let temp = tempfile::tempdir_in("/tmp").unwrap();
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
    let fs = provenant_supervisor::fs::LocalFs;
    assert_eq!(
        config_show(&fixture.roots, &fs).unwrap().profile,
        Profile::Balanced
    );
    assert_eq!(check_config(&fixture.roots, &fs).unwrap().schema_version, 1);
    assert!(!fixture.roots.state.as_path().exists());
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    assert_eq!(
        read(&fixture.roots, &fs).unwrap().profile,
        Profile::Balanced
    );
    assert!(!fixture.config().exists());
    let original = b"# User-owned; preserve exactly.\nschema_version=1\nprofile='rigorous'\n";
    std::fs::write(fixture.config(), original).unwrap();
    assert_eq!(
        check_config(&fixture.roots, &fs).unwrap().profile,
        Profile::Rigorous
    );
    assert_eq!(
        config_show(&fixture.roots, &fs).unwrap().profile,
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
    assert!(check_config(&fixture.roots, &fs).is_err());
    assert!(config_show(&fixture.roots, &fs).is_err());
}

#[test]
fn config_filesystem_boundary_refuses_links_non_files_and_unreadable_roots() {
    let fixture = Files::new();
    let fs = provenant_supervisor::fs::LocalFs;
    std::fs::create_dir(fixture.roots.state.as_path()).unwrap();
    // A final link must never read an old or foreign root, even when broken.
    std::os::unix::fs::symlink("/nonexistent/private-sentinel", fixture.config()).unwrap();
    assert!(matches!(
        read(&fixture.roots, &fs),
        Err(ConfigError::Read { .. })
    ));
    std::fs::remove_file(fixture.config()).unwrap();
    std::fs::create_dir(fixture.config()).unwrap();
    assert!(matches!(
        read(&fixture.roots, &fs),
        Err(ConfigError::Read { .. })
    ));
    std::fs::remove_dir(fixture.config()).unwrap();
    std::fs::remove_dir(fixture.roots.state.as_path()).unwrap();
    std::fs::write(fixture.roots.state.as_path(), b"not a directory").unwrap();
    assert!(matches!(
        read(&fixture.roots, &fs),
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
