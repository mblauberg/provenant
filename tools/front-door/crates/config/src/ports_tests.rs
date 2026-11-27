use super::*;
use provenant_records::warning::capture;

#[test]
fn ci_observation_declares_review_gate_contexts() {
    let mut table = PortTable::builtin();
    assert!(
        table.ci_observation.review_gate_contexts.is_empty(),
        "a port that declares no gates names none"
    );
    let declared: CiObservationPort = serde_json::from_value(serde_json::json!({
        "source": "project",
        "backend": {"kind": "builtin"},
        "review_gate_contexts": ["verdict"]
    }))
    .unwrap();
    assert_eq!(declared.review_gate_contexts, ["verdict"]);
    let as_port: Port = (&declared).into();
    assert_eq!(as_port.source, declared.source);
    table.ci_observation = declared;
    let value = serde_json::to_value(&table).unwrap();
    assert_eq!(
        value["ci_observation"]["review_gate_contexts"][0],
        "verdict"
    );
}

#[test]
fn declared_port_rows_and_backend_shapes_round_trip() {
    let table = PortTable::builtin();
    let value = serde_json::to_value(&table).unwrap();
    let entries: Vec<_> = table.entries().collect();
    let declared: std::collections::BTreeSet<_> = PortTable::PORT_NAMES.iter().copied().collect();
    let observed: std::collections::BTreeSet<_> = entries.iter().map(|entry| entry.0).collect();
    assert_eq!(observed, declared);
    for (name, source, backend, _, _) in entries {
        let entry = value
            .get(name)
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(
            entry.get("source"),
            Some(&serde_json::to_value(source).unwrap()),
            "{name} source"
        );
        if name != "build" {
            // build declares no backend; it is builtin by construction.
            let expected_backend = match backend {
                PortBackendKind::Builtin => "builtin",
                PortBackendKind::Command => "command",
                PortBackendKind::None => "none",
            };
            assert_eq!(
                entry.get("backend").and_then(|value| value.get("kind")),
                Some(&serde_json::json!(expected_backend)),
                "{name} backend"
            );
        }
    }
    assert_eq!(serde_json::from_value::<PortTable>(value).unwrap(), table);

    let cases = [
        PortBackend::Builtin,
        PortBackend::Command {
            command: PortCommand {
                argv: vec!["verify".into()],
                exit_codes: CommandExitCodes {
                    success: vec![0],
                    unknown: vec![75],
                    other: ExitDisposition::Refused,
                },
            },
        },
        PortBackend::None,
    ];
    for backend in cases {
        let mut layout = table.worktree_layout.clone();
        layout.backend = backend.clone();
        assert_eq!(
            layout.unavailable_reason().is_none(),
            layout.path_for_branch("feature/topic").is_ok()
        );
        let value = serde_json::to_value(&backend).unwrap();
        assert_eq!(
            serde_json::from_value::<PortBackend>(value).unwrap(),
            backend
        );
    }

    let land_cases = [
        LandBackend::Builtin,
        LandBackend::Command {
            command: provenant_records::landing::RepositoryCommand {
                argv: vec!["provenant-land".into()],
                unknown_exit_code: 75,
                timeout_seconds: 30,
            },
        },
        LandBackend::None,
    ];
    for backend in land_cases {
        let value = serde_json::to_value(&backend).unwrap();
        assert_eq!(
            serde_json::from_value::<LandBackend>(value).unwrap(),
            backend
        );
    }

    let selection_cases = [
        VerifySelection::Packages,
        VerifySelection::Integrations,
        VerifySelection::All,
    ];
    for selection in selection_cases {
        let value = serde_json::to_value(selection).unwrap();
        assert_eq!(
            serde_json::from_value::<VerifySelection>(value).unwrap(),
            selection
        );
    }

    let command = provenant_records::landing::RepositoryCommand {
        argv: vec!["land".into()],
        unknown_exit_code: 75,
        timeout_seconds: 30,
    };
    assert_eq!(
        command.outcome(0),
        provenant_records::landing::RepositoryCommandOutcome::Landed,
    );
    assert_eq!(
        command.outcome(1),
        provenant_records::landing::RepositoryCommandOutcome::Refused,
    );
    assert_eq!(
        command.outcome(75),
        provenant_records::landing::RepositoryCommandOutcome::Unknown,
    );

    let source_cases = [
        Layer::Default,
        Layer::Detected,
        Layer::Host,
        Layer::HostProject,
        Layer::Project,
    ];
    for source in source_cases {
        let value = serde_json::to_value(source).unwrap();
        assert_eq!(serde_json::from_value::<Layer>(value).unwrap(), source);
    }
}

#[test]
fn worktree_layout_resolves_default_and_custom_roots_from_one_declaration() {
    let mut table = PortTable::builtin();
    assert!(table.validate().is_ok());
    {
        let custom = &mut table.worktree_layout;
        custom.roots = vec![
            PathBuf::from(".worktrees"),
            PathBuf::from(".claude/worktrees"),
        ];
        custom.default = PathBuf::from(".claude/worktrees/<branch>");
        assert_eq!(
            custom.path_for_branch("feature/topic").unwrap(),
            PathBuf::from(".claude/worktrees/feature-topic"),
        );
        assert_eq!(
            custom.branch_component(Path::new(".claude/worktrees/feature-topic")),
            Some("feature-topic"),
        );
        assert!(custom.contains_worktree(Path::new(".worktrees/other")));
        assert!(
            custom
                .branch_component(Path::new(".claude/worktrees/deep/name"))
                .is_none()
        );
        custom.roots = vec![PathBuf::from("trees"), PathBuf::from("legacy")];
        custom.default = PathBuf::from("trees/topic-<branch>/checkout");
        let path = custom.path_for_branch("feature/topic").unwrap();
        assert_eq!(path, PathBuf::from("trees/topic-feature-topic/checkout"));
        assert_eq!(custom.branch_component(&path), Some("feature-topic"));
        assert_eq!(
            custom.branch_component(Path::new("legacy/topic-feature-topic/checkout")),
            Some("feature-topic"),
        );
    }
    assert!(table.validate().is_ok());
    table.worktree_layout.roots = vec![PathBuf::from("trees/<branch>")];
    table.worktree_layout.default = PathBuf::from("trees/<branch>/checkout");
    assert!(table.validate().is_err());

    // Spec: docs/specs/state.md#one-sourced-port-table
    // A parent-relative root places worktrees beside the checkout; an
    // absolute root places them anywhere. Both are declared roots and are
    // treated exactly like the default one.
    table = PortTable::builtin();
    table.worktree_layout.roots = vec![PathBuf::from("../outside")];
    table.worktree_layout.default = PathBuf::from("../outside/<branch>");
    assert!(table.validate().is_ok());
    assert_eq!(
        table
            .worktree_layout
            .path_for_branch("feature/topic")
            .unwrap(),
        PathBuf::from("../outside/feature-topic"),
    );
    let (anchor, tail) = table
        .worktree_layout
        .allocation_for(Path::new("/repo"), "feature/topic")
        .unwrap();
    assert_eq!(anchor, PathBuf::from("/repo/../outside"));
    assert_eq!(tail, PathBuf::from("feature-topic"));
    assert_eq!(
        table
            .worktree_layout
            .branch_for_path(Path::new("/repo"), Path::new("/outside/feature-topic")),
        Some("feature-topic"),
    );
    table.worktree_layout.roots = vec![PathBuf::from("/srv/worktrees")];
    table.worktree_layout.default = PathBuf::from("/srv/worktrees/<branch>");
    assert!(table.validate().is_ok());
    assert_eq!(
        table
            .worktree_layout
            .path_for_branch("feature/topic")
            .unwrap(),
        PathBuf::from("/srv/worktrees/feature-topic"),
    );
    assert_eq!(
        table
            .worktree_layout
            .branch_for_path(Path::new("/repo"), Path::new("/outside/feature-topic")),
        None,
    );
}

#[test]
fn command_environment_inherits_execution_context_only() {
    let ambient = COMMAND_CONTEXT_ENV
        .iter()
        .map(|name| (OsString::from(*name), OsString::from("context")))
        .chain([
            ("GH_TOKEN".into(), "credential".into()),
            ("SSH_AUTH_SOCK".into(), "agent".into()),
        ]);
    let result: BTreeMap<_, _> =
        port_command_environment(ambient, &[("PATH".into(), "explicit".into())])
            .into_iter()
            .collect();
    for name in COMMAND_CONTEXT_ENV {
        assert!(result.contains_key(&OsString::from(*name)));
    }
    assert_eq!(
        result.get(&OsString::from("PATH")),
        Some(&OsString::from("explicit"))
    );
    assert!(!result.contains_key(&OsString::from("GH_TOKEN")));
    assert!(!result.contains_key(&OsString::from("SSH_AUTH_SOCK")));
}

#[test]
fn inspection_reports_invalid_ports_without_executing_them() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join(".provenant")).unwrap();
    std::fs::write(directory.path().join(PORTS_CONFIG), r#"{"schema_version":3,"naming":{"backend":{"kind":"command","command":{"argv":[],"exit_codes":{"success":[0],"unknown":[],"other":"refused"}}}}}"#).unwrap();
    let table = PortTable::read_for_inspection(Some(directory.path()), directory.path()).unwrap();
    let naming = table.entries().find(|entry| entry.0 == "naming").unwrap();
    assert_eq!(naming.1, Layer::Project);
    assert!(!naming.3);
    assert!(naming.4.is_some());
    assert!(PortTable::read_for(Some(directory.path()), directory.path()).is_err());
    let mut invalid = PortTable::builtin();
    // The refused shape is the wrong branch token, not the root location.
    invalid.worktree_layout.default = "outside/{branch}".into();
    assert!(
        invalid
            .worktree_layout
            .path_for_branch("feature/topic")
            .is_err()
    );
    for (_, _, _, valid, reason) in invalid
        .entries()
        .filter(|entry| entry.0 == "worktree_layout")
    {
        assert!(!valid);
        assert!(reason.is_some());
    }
}

#[test]
fn port_config_unknown_root_and_nested_keys_warn_with_their_layer_source() {
    let directory = tempfile::tempdir().unwrap();
    let host = directory.path().join("host");
    let project = directory.path().join("project");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(project.join(".provenant")).unwrap();
    let builtin = serde_json::to_value(PortTable::builtin()).unwrap();

    let mut host_overlay = serde_json::json!({
        "schema_version": PORTS_SCHEMA_VERSION,
        "verify": builtin["verify"].clone(),
        "future_port": {"enabled": true},
        "build_admission": {"backend": {"kind": "builtin"}},
    });
    host_overlay["verify"]["backend"] = serde_json::json!({"kind": "none"});
    host_overlay["verify"]["test_threads"] = serde_json::Value::Null;
    host_overlay["verify"]["future_nested"] = serde_json::json!({"enabled": true});
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&host_overlay).unwrap(),
    )
    .unwrap();

    let mut project_overlay = serde_json::json!({
        "schema_version": PORTS_SCHEMA_VERSION,
        "naming": builtin["naming"].clone(),
    });
    project_overlay["naming"]["future_nested"] = serde_json::json!("ignored");
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&project_overlay).unwrap(),
    )
    .unwrap();

    let mut warnings = Vec::new();
    let table = PortTable::read_for_inspection_with(Some(&project), &host, |_, warning| {
        warnings.push((warning.source.clone(), warning.key.clone()))
    })
    .unwrap();
    assert_eq!(table.verify.backend, PortBackend::None);
    assert_eq!(table.verify.source, Layer::Host);
    assert_eq!(table.naming.source, Layer::Project);
    let encoded = serde_json::to_value(&table).unwrap();
    assert!(encoded.get("future_port").is_none());
    assert!(encoded["verify"].get("future_nested").is_none());
    assert!(encoded["naming"].get("future_nested").is_none());

    let host_source = host.join("ports.json").display().to_string();
    let project_source = project.join(PORTS_CONFIG).display().to_string();
    assert_eq!(
        warnings
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([
            (host_source.clone(), "future_port".to_owned()),
            (host_source.clone(), "build_admission".to_owned()),
            (host_source.clone(), "verify.future_nested".to_owned()),
            (project_source, "naming.future_nested".to_owned()),
        ])
    );

    let malformed = serde_json::json!({
        "schema_version": PORTS_SCHEMA_VERSION,
        "verify": false,
    });
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&malformed).unwrap(),
    )
    .unwrap();
    // A malformed project overlay is optional: it is skipped with a
    // warning that names the file and the offending key.
    let mut malformed_warnings = Vec::new();
    let table = PortTable::read_layers(
        Some(&project),
        Some(&host),
        |_, _| {},
        |path, error| malformed_warnings.push((path.to_path_buf(), error.to_string())),
    )
    .unwrap();
    assert_eq!(table.verify.backend, PortBackend::None);
    assert_eq!(malformed_warnings.len(), 1);
    assert!(malformed_warnings[0].0.ends_with(PORTS_CONFIG));
    assert!(
        malformed_warnings[0].1.contains("verify"),
        "{malformed_warnings:?}"
    );

    // A malformed host layer is a real boundary: it still refuses, but
    // names its file and key instead of masquerading as a Git error.
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&malformed).unwrap(),
    )
    .unwrap();
    let message = PortTable::read_for_inspection(Some(&project), &host)
        .unwrap_err()
        .to_string();
    assert!(message.contains("ports.json"), "{message}");
    assert!(message.contains("verify"), "{message}");
    assert!(!message.contains("Git configuration"), "{message}");
}

#[test]
fn partial_overlay_deep_merges_fields_and_keeps_lower_sources() {
    let temp = tempfile::tempdir().unwrap();
    let host = temp.path().join("host");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(project.join(".provenant")).unwrap();

    // schema_version is optional in a layer; the host sets a non-default
    // field and a tier override, the project sets only verify.base.
    let host_overlay = serde_json::json!({
        "verify": {
            "test_threads": 4,
            "tiers": {"2": {"warn_for_host_load": false}},
        },
    });
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&host_overlay).unwrap(),
    )
    .unwrap();
    let project_overlay = serde_json::json!({"verify": {"base": "upstream/master"}});
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&project_overlay).unwrap(),
    )
    .unwrap();

    let table = PortTable::read_for(Some(&project), &host).unwrap();
    assert_eq!(table.verify.base, "upstream/master");
    assert_eq!(table.verify.source, Layer::Project);
    assert_eq!(table.verify.test_threads, Some(4));
    assert!(!table.verify.tiers["2"].warn_for_host_load);
    assert_eq!(table.verify.remote, "origin");
    assert_eq!(table.verify.default_tier, "0");
    assert_eq!(table.land.source, Layer::Default);

    // The same field-level merge applies to a host overlay over builtins.
    std::fs::remove_file(project.join(PORTS_CONFIG)).unwrap();
    let table = PortTable::read_for(None, &host).unwrap();
    assert_eq!(table.verify.source, Layer::Host);
    assert_eq!(table.verify.test_threads, Some(4));
    assert_eq!(table.verify.base, "");
}

#[test]
fn invalid_tier_command_warns_and_falls_back() {
    for (cwd, argv, reason) in [
        (".", vec![], "empty argv"),
        (".", vec![""], "empty program"),
        ("/absolute", vec!["true"], "cwd"),
        ("a/../b", vec!["true"], "cwd"),
    ] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        let path = root.path().join(PORTS_CONFIG);
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"verify":{"tiers":{
                "0":{"commands":[{"cwd":cwd,"argv":argv}],"selection":"all"},
                "1":{"commands":[{"argv":["valid"]}]}
            }}}))
            .unwrap(),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let table = PortTable::read_layers(
            Some(root.path()),
            None,
            |_, _| {},
            |file, error| {
                warnings.push((file.to_owned(), error.to_string()));
            },
        )
        .unwrap();
        assert!(table.validate().is_ok());
        assert_eq!(
            table.verify.tiers["0"],
            PortTable::builtin().verify.tiers["0"]
        );
        assert_eq!(
            table.provenance()["verify.tiers.0.commands"],
            Layer::Default
        );
        assert_eq!(table.verify.tiers["1"].commands[0].argv, ["valid"]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, path);
        assert!(warnings[0].1.contains("verify.tiers.0") && warnings[0].1.contains(reason));
    }
}

/// A static check is usable only with a command, a relative cwd and a
/// positive time cap. These are the invariants the writer-preamble mapping
/// uses to drop advice that could not be run.
#[test]
fn verify_static_check_validity_table() {
    let valid = VerifyStaticCheck {
        argv: vec!["scripts/check-extra-fields".into()],
        cwd: PathBuf::from("."),
        timeout_seconds: 30,
    };
    assert!(valid.is_valid(), "{valid:?}");
    for (label, check) in [
        (
            "empty argv",
            VerifyStaticCheck {
                argv: vec![],
                ..valid.clone()
            },
        ),
        (
            "empty program",
            VerifyStaticCheck {
                argv: vec!["".into()],
                ..valid.clone()
            },
        ),
        (
            "absolute cwd",
            VerifyStaticCheck {
                cwd: PathBuf::from("/absolute"),
                ..valid.clone()
            },
        ),
        (
            "parent-relative cwd",
            VerifyStaticCheck {
                cwd: PathBuf::from("a/../b"),
                ..valid.clone()
            },
        ),
        (
            "zero timeout",
            VerifyStaticCheck {
                timeout_seconds: 0,
                ..valid.clone()
            },
        ),
    ] {
        assert!(!check.is_valid(), "{label}: {check:?}");
    }
}

// Spec: docs/specs/state.md#one-sourced-port-table
#[test]
fn tier_command_string_and_table_forms_decode_equal() {
    let text = "cargo fmt {packages} -- --check";
    let argv = ["cargo", "fmt", "{packages}", "--", "--check"];
    let from_string: TierCommand = serde_json::from_value(serde_json::json!(text)).unwrap();
    let from_table: TierCommand =
        serde_json::from_value(serde_json::json!({"argv": argv})).unwrap();
    assert_eq!(from_string, from_table);
    assert_eq!(
        from_string,
        serde_json::from_value::<TierCommand>(serde_json::json!({
            "stack": "",
            "cwd": ".",
            "argv": argv,
        }))
        .unwrap()
    );
    assert_eq!(from_string.argv, argv);
    assert!(from_string.stack.is_empty());
    assert_eq!(from_string.cwd, PathBuf::from("."));

    // The pre-decode filter accepts the shorthand, so it survives the
    // layer walk and serialises back in the table shape.
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".provenant")).unwrap();
    let path = root.path().join(PORTS_CONFIG);
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "verify": {"tiers": {
                "0": {"commands": [text], "selection": "packages"},
            }},
        }))
        .unwrap(),
    )
    .unwrap();
    let mut warnings = Vec::new();
    let table = PortTable::read_layers(
        Some(root.path()),
        None,
        |_, _| {},
        |file, error| warnings.push((file.to_owned(), error.to_string())),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(table.verify.tiers["0"].commands, [from_string]);
    assert_eq!(
        table.provenance()["verify.tiers.0.commands"],
        Layer::Project
    );
    let value = serde_json::to_value(&table).unwrap();
    assert!(value["verify"]["tiers"]["0"]["commands"][0].is_object());
    // Provenance is derived from the layer walk and never serialised, so
    // the decoded table carries the same ports but not the labels' history.
    let decoded = serde_json::from_value::<PortTable>(value.clone()).unwrap();
    assert_eq!(decoded.verify, table.verify);
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
}

// Spec: docs/specs/state.md#one-sourced-port-table
#[test]
fn tier_command_string_honours_quotes_and_escapes() {
    let words = |text: &str| -> Vec<String> {
        serde_json::from_value::<TierCommand>(serde_json::json!(text))
            .unwrap()
            .argv
    };
    // Double quotes honour a backslash before a quote or another backslash;
    // single quotes keep a backslash literally.
    assert_eq!(
        words(r#"tool "say \"hi\"" "back\\slash" 'a\b' 'single "quote"'"#),
        [
            "tool",
            "say \"hi\"",
            "back\\slash",
            "a\\b",
            "single \"quote\""
        ]
    );
    // A doubled backslash immediately before the closing quote is a literal
    // backslash, not an escaped delimiter.
    assert_eq!(words(r#"tool "ends\\""#), ["tool", "ends\\"]);
    assert_eq!(words(r"tool back\\slash"), ["tool", "back\\slash"]);
}

// Spec: docs/specs/state.md#one-sourced-port-table
#[test]
fn unterminated_tier_command_string_warns_and_falls_back() {
    for text in [
        "cargo fmt 'unbalanced",
        "cargo fmt \"unbalanced",
        "cargo fmt trailing\\",
        "cargo fmt \"trailing\\",
    ] {
        assert!(
            serde_json::from_value::<TierCommand>(serde_json::json!(text)).is_err(),
            "{text}"
        );
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        let path = root.path().join(PORTS_CONFIG);
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "verify": {"tiers": {
                    "0": {"commands": [text, "cargo check"], "selection": "all"},
                    "1": {"commands": ["cargo check", {"argv": ["true"]}]},
                }},
            }))
            .unwrap(),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let table = PortTable::read_layers(
            Some(root.path()),
            None,
            |_, _| {},
            |file, error| warnings.push((file.to_owned(), error.to_string())),
        )
        .unwrap();
        assert!(table.validate().is_ok());
        assert_eq!(
            table.verify.tiers["0"],
            PortTable::builtin().verify.tiers["0"]
        );
        assert_eq!(table.verify.tiers["1"].commands[0].argv, ["cargo", "check"]);
        assert_eq!(table.verify.tiers["1"].commands[1].argv, ["true"]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].0, path);
        assert!(
            warnings[0].1.contains("verify.tiers.0.commands[0]"),
            "{warnings:?}"
        );
    }
}

#[test]
fn malformed_command_drops_only_its_tier_from_each_layer() {
    for command in [serde_json::json!({}), serde_json::json!({"argv": 42})] {
        for host in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let path = if host {
                state.path().join("ports.json")
            } else {
                std::fs::create_dir(root.path().join(".provenant")).unwrap();
                root.path().join(PORTS_CONFIG)
            };
            std::fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({
                    "verify": {"base":"upstream/main", "tiers":{"1":{"commands":[command]}}}
                }))
                .unwrap(),
            )
            .unwrap();
            let mut warnings = Vec::new();
            let table = PortTable::read_layers(
                Some(root.path()),
                Some(state.path()),
                |_, _| {},
                |file, error| warnings.push((file.to_owned(), error.to_string())),
            )
            .unwrap();
            assert_eq!(table.verify.base, "upstream/main");
            assert_eq!(
                table.verify.tiers["1"],
                PortTable::builtin().verify.tiers["1"]
            );
            assert_eq!(
                table.provenance()["verify.tiers.1.commands"],
                Layer::Default
            );
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].0, path);
            assert!(warnings[0].1.contains("verify.tiers.1.commands[0]"));
        }
    }
}

#[test]
fn detected_selection_respects_stack_mix() {
    for (rust, node, expected0, expected1) in [
        (
            true,
            false,
            VerifySelection::Packages,
            VerifySelection::Packages,
        ),
        (false, true, VerifySelection::All, VerifySelection::All),
        (true, true, VerifySelection::All, VerifySelection::All),
    ] {
        let root = tempfile::tempdir().unwrap();
        if rust {
            std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        }
        if node {
            std::fs::write(
                root.path().join("package.json"),
                r#"{"scripts":{"lint":"ok","test":"ok"}}"#,
            )
            .unwrap();
        }
        let table = PortTable::read_layers(
            Some(root.path()),
            None,
            |_, _| {},
            |_, error| panic!("{error}"),
        )
        .unwrap();
        assert_eq!(table.verify.tiers["0"].selection, expected0);
        assert_eq!(table.verify.tiers["1"].selection, expected1);
        if rust {
            assert_eq!(table.verify.tiers["2"].selection, VerifySelection::All);
        } else {
            assert!(!table.verify.tiers.contains_key("2"));
        }
        assert!(!table.verify.tiers.contains_key("3"));
    }
}

#[test]
fn verify_format_is_detected_then_configured_then_off() {
    // Spec: docs/specs/state.md#every-setting-reports-its-layer
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    let table = PortTable::read_for(Some(root.path()), state.path()).unwrap();
    let detected = table.verify_format();
    assert_eq!(detected.source.layer, Layer::Detected);
    assert_eq!(
        detected.source.origin.as_deref(),
        Some("detected#verify.format")
    );
    let VerifyFormat::Command(command) = detected.value else {
        panic!("stack detection must supply a write-mode formatter");
    };
    assert_eq!(command.argv, ["cargo", "fmt"]);
    assert_eq!(command.cwd.as_path(), std::path::Path::new("."));

    // A declared command keeps the configured layer that supplied it.
    std::fs::create_dir(root.path().join(".provenant")).unwrap();
    std::fs::write(
        root.path().join(PORTS_CONFIG),
        r#"{"verify":{"format":"myfmt --write"}}"#,
    )
    .unwrap();
    let table = PortTable::read_for(Some(root.path()), state.path()).unwrap();
    let configured = table.verify_format();
    assert_eq!(configured.source.layer, Layer::Project);
    assert_eq!(
        configured.source.origin.as_deref(),
        Some(".provenant/ports.json#verify.format")
    );
    let VerifyFormat::Command(command) = configured.value else {
        panic!("a declared command must be kept verbatim");
    };
    assert_eq!(command.argv, ["myfmt", "--write"]);

    // The literal "none" turns the carry off, whatever detection found.
    std::fs::write(
        root.path().join(PORTS_CONFIG),
        r#"{"verify":{"format":"none"}}"#,
    )
    .unwrap();
    let table = PortTable::read_for(Some(root.path()), state.path()).unwrap();
    let off = table.verify_format();
    assert_eq!(off.source.layer, Layer::Project);
    assert_eq!(off.value, VerifyFormat::None);
}

#[test]
fn detection_without_commands_leaves_no_builtin_tiers() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("package.json"), "{}").unwrap();
    let table = PortTable::read_for(Some(root.path()), state.path()).unwrap();
    assert!(table.verify.tiers.is_empty());
    assert!(table.verify.default_tier.is_empty());
    assert_eq!(table.provenance()["verify.default_tier"], Layer::Detected);
}

#[test]
fn detected_tiers_replace_builtin_and_configured_wins() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("package.json"),
        r#"{"scripts":{"lint":"ok","test":"ok"}}"#,
    )
    .unwrap();
    std::fs::create_dir(root.path().join(".provenant")).unwrap();
    std::fs::write(
        root.path().join(PORTS_CONFIG),
        r#"{"verify":{"tiers":{"1":{"commands":[{"argv":["custom"]}]}}}}"#,
    )
    .unwrap();
    let table = PortTable::read_for(Some(root.path()), state.path()).unwrap();
    assert_eq!(table.verify.tiers["0"].commands[0].stack, "node");
    assert_eq!(table.verify.tiers["1"].commands[0].argv, ["custom"]);
    assert!(!table.verify.tiers.contains_key("2"));
    assert!(!table.verify.tiers.contains_key("3"));
    assert_eq!(
        table.provenance()["verify.tiers.0.commands"],
        Layer::Detected
    );
    assert_eq!(
        table.provenance()["verify.tiers.1.commands"],
        Layer::Project
    );
}

#[test]
fn builtin_verify_tiers_are_neutral_and_base_is_inferred() {
    let builtin = PortTable::builtin();
    assert!(builtin.verify.base.is_empty());
    for tier in builtin.verify.tiers.values() {
        for command in &tier.commands {
            assert!(
                command.argv.iter().all(|arg| !arg.contains("scripts/")),
                "{command:?}"
            );
            assert_eq!(command.argv.first().map(String::as_str), Some("cargo"));
        }
    }
}

#[test]
fn verify_base_inference_uses_the_remote_head_symref_target() {
    let inferred = infer_verify_base("origin", "refs/remotes/origin/master").unwrap();
    assert_eq!(inferred.0, "origin/master");
    assert_eq!(inferred.1, "refs/remotes/origin/HEAD");
    let inferred = infer_verify_base("upstream", "refs/remotes/upstream/main").unwrap();
    assert_eq!(inferred.0, "upstream/main");
    assert_eq!(inferred.1, "refs/remotes/upstream/HEAD");
    assert!(infer_verify_base("origin", "").is_none());
    assert!(infer_verify_base("origin", "refs/heads/main").is_none());
    assert!(infer_verify_base("", "refs/remotes/origin/main").is_none());
    assert!(
        verify_base_unset_error()
            .to_string()
            .contains(".provenant/ports.json")
    );
}

#[test]
fn command_digest_binds_argv_and_each_exit_disposition() {
    let base = PortCommand {
        argv: vec!["admit".into(), "--phase".into()],
        exit_codes: CommandExitCodes {
            success: vec![0],
            unknown: vec![75],
            other: ExitDisposition::Refused,
        },
    };
    assert_eq!(base.digest(), base.digest());
    let mut variants = Vec::new();
    let mut changed = base.clone();
    changed.argv[0] = "another-admit".into();
    variants.push(changed);
    let mut changed = base.clone();
    changed.exit_codes.success = vec![0, 2];
    variants.push(changed);
    let mut changed = base.clone();
    changed.exit_codes.unknown = vec![75, 76];
    variants.push(changed);
    let mut changed = base.clone();
    changed.exit_codes.other = ExitDisposition::Unknown;
    variants.push(changed);
    assert!(
        variants
            .iter()
            .all(|command| command.digest() != base.digest())
    );
}

#[test]
fn every_declared_port_accepts_layered_overrides_without_relabelling_omissions() {
    let temp = tempfile::tempdir().unwrap();
    let host = temp.path().join("host");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(project.join(".provenant")).unwrap();
    let builtin = serde_json::to_value(PortTable::builtin()).unwrap();

    for &name in PortTable::PORT_NAMES {
        for serialised_source in [None, Some(Layer::Default)] {
            let mut entry = builtin[name].clone();
            if name != "build" {
                // build has no backend field; its layer override is prepare.
                entry["backend"] = serde_json::json!({"kind": "none"});
            }
            let object = entry.as_object_mut().unwrap();
            object.remove("source");
            if let Some(source) = serialised_source {
                object.insert("source".into(), serde_json::to_value(source).unwrap());
            }
            let mut overlay = serde_json::json!({"schema_version": PORTS_SCHEMA_VERSION});
            overlay[name] = entry;
            let bytes = serde_json::to_vec(&overlay).unwrap();
            std::fs::write(host.join("ports.json"), &bytes).unwrap();

            for (project_overlay, expected_source) in [
                (None, Layer::Host),
                (
                    Some(serde_json::json!({"schema_version": PORTS_SCHEMA_VERSION})),
                    Layer::Host,
                ),
                (Some(overlay.clone()), Layer::Project),
            ] {
                if let Some(document) = &project_overlay {
                    std::fs::write(
                        project.join(PORTS_CONFIG),
                        serde_json::to_vec(document).unwrap(),
                    )
                    .unwrap();
                }
                let project_path = project_overlay.as_ref().map(|_| project.as_path());
                let table = PortTable::read_for(project_path, &host).unwrap();
                for (port, source, _, _, _) in table.entries() {
                    assert_eq!(
                        source,
                        if port == name {
                            expected_source
                        } else {
                            Layer::Default
                        },
                        "{port}: overlay for {name} must not relabel omitted ports"
                    );
                }
                let mut expected = builtin.clone();
                expected[name] = overlay[name].clone();
                expected[name]["source"] = serde_json::to_value(expected_source).unwrap();
                if name == "build" {
                    // This overlay restates every build leaf, so each leaf
                    // legitimately reports the layer that declared it.
                    if let Some(sources) = expected["build"]["leaf_sources"].as_object_mut() {
                        for source in sources.values_mut() {
                            *source = serde_json::to_value(expected_source).unwrap();
                        }
                    }
                }
                assert_eq!(serde_json::to_value(&table).unwrap(), expected);
            }
        }
    }
}

#[test]
fn build_defaults_are_slots_and_generic() {
    let table = PortTable::builtin();
    let build = &table.build;
    assert_eq!(build.source, Layer::Default);
    assert!(build.prepare.is_empty());
    assert_eq!(build.strategy, "slots");
    assert_eq!(build.offload, "auto");
    assert_eq!(build.overflow, "auto");
    assert_eq!(build.clean, "untracked");
    assert_eq!(build.target, "toolchain,worktree");
    assert!(build.stacks.is_empty());
    assert_eq!(build.snapshot_retention, "7d");
    assert_eq!(build.priority, default_build_priority());
    assert_eq!(build.queue_aging, "120s");
    assert_eq!(build.prewarm_budget, 1);
    assert_eq!(build.canary_every, "24h");
    assert_eq!(build.slot_idle, "7d");
    assert_eq!(build.soak, BuildSoakPort::default());
    assert_eq!(build.strategy_name().value, BuildStrategyName::Slots);
    assert_eq!(build.overflow_policy().value, BuildOverflow::Auto);
    assert_eq!(build.clean_policy().value, BuildClean::Untracked);
    assert_eq!(
        build.target_layout().value,
        DEFAULT_BUILD_TARGET_KEYS.to_vec()
    );
    assert_eq!(build.offload_policy().value, BuildOffload::Auto);
    assert_eq!(build.priority_for("test").value, BuildPriority::Test);
    assert_eq!(build.queue_aging_duration().value, Duration::from_secs(120));
    let (_, source, backend, valid, reason) =
        table.entries().find(|entry| entry.0 == "build").unwrap();
    assert_eq!(source, Layer::Default);
    assert_eq!(backend, PortBackendKind::Builtin);
    assert!(valid);
    assert!(reason.is_none());
}

#[test]
fn build_target_layout_derives_per_toolchain_and_per_worktree_keys() {
    let table = PortTable::builtin();
    let build = &table.build;
    assert_eq!(
        build.target_layout().value,
        vec![BuildTargetKey::Toolchain, BuildTargetKey::Worktree]
    );
    assert_eq!(build.source_for_leaf("target"), Layer::Default);

    let mut reordered = build.clone();
    reordered.target = "worktree, toolchain, worktree".into();
    let resolved = reordered.target_layout();
    // The order is the user's, duplicates are dropped, and the source follows
    // the leaf layer so `config show`/`check config` can name it.
    assert_eq!(
        resolved.value,
        vec![BuildTargetKey::Worktree, BuildTargetKey::Toolchain]
    );
    assert!(resolved.warnings.is_empty());

    let mut shared = build.clone();
    shared.target = "shared".into();
    assert!(shared.target_layout().value.is_empty());

    let mut unknown = build.clone();
    unknown.target = "toolchain,channel".into();
    let fallback = unknown.target_layout();
    assert_eq!(fallback.value, DEFAULT_BUILD_TARGET_KEYS.to_vec());
    assert!(fallback.warnings[0].contains("build.target"));
    assert!(fallback.warnings[0].contains("channel"));
}

#[test]
fn build_prepare_layers_project_over_host() {
    let temp = tempfile::tempdir().unwrap();
    let host = temp.path().join("host");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(project.join(".provenant")).unwrap();
    let host_prepare = vec![vec!["node".to_owned(), "host-prepare.mjs".to_owned()]];
    let project_prepare = vec![vec!["node".to_owned(), "project-prepare.mjs".to_owned()]];

    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": PORTS_SCHEMA_VERSION,
            "build": {"prepare": host_prepare},
        }))
        .unwrap(),
    )
    .unwrap();

    // No project layer: the host value and its source survive.
    let table = PortTable::read_for(None, &host).unwrap();
    assert_eq!(table.build.prepare, host_prepare);
    assert_eq!(table.build.source, Layer::Host);

    // A project layer that sets build.prepare wins and reports its source.
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": PORTS_SCHEMA_VERSION,
            "build": {"prepare": project_prepare},
        }))
        .unwrap(),
    )
    .unwrap();
    let table = PortTable::read_for(Some(project.as_path()), &host).unwrap();
    assert_eq!(table.build.prepare, project_prepare);
    assert_eq!(table.build.source, Layer::Project);

    assert_eq!(table.build.source_for_leaf("strategy"), Layer::Default);
    assert_eq!(table.build.source_for_leaf("offload"), Layer::Default);
    // Arrays replace rather than concatenate across layers.
    assert_eq!(table.build.prepare.len(), 1);

    // A project layer that omits build keeps the host declaration.
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": PORTS_SCHEMA_VERSION,
        }))
        .unwrap(),
    )
    .unwrap();
    let table = PortTable::read_for(Some(project.as_path()), &host).unwrap();
    assert_eq!(table.build.prepare, host_prepare);
    assert_eq!(table.build.source, Layer::Host);

    // With no layer setting build at all, it stays builtin and empty.
    let empty_host = temp.path().join("empty-host");
    std::fs::create_dir_all(&empty_host).unwrap();
    let table = PortTable::read_for(None, &empty_host).unwrap();
    assert_eq!(table.build.source, Layer::Default);
    assert!(table.build.prepare.is_empty());
}

#[test]
fn invalid_prepare_argv_is_invalid_port() {
    let mut table = PortTable::builtin();
    table.build.prepare.push(Vec::new());
    assert!(table.validate().is_err());
    let (_, _, _, valid, reason) = table.entries().find(|entry| entry.0 == "build").unwrap();
    assert!(!valid);
    assert!(reason.is_some());

    let mut table = PortTable::builtin();
    table.build.prepare.push(vec![String::new()]);
    assert!(table.validate().is_err());

    // A declared program is accepted; arguments after it are free-form.
    let mut table = PortTable::builtin();
    table.build.prepare.push(vec![
        "node".to_owned(),
        "tools/ensure-generated.mjs".to_owned(),
    ]);
    assert!(table.validate().is_ok());
}

fn checkout_with_key(temp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let host = temp.join("host");
    let project = temp.join("project");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::create_dir_all(project.join(".provenant")).unwrap();
    let dir = crate::host_project_dir(&host, &project)
        .expect("a checkout resolves its host project directory");
    (host, project, dir)
}

#[test]
fn host_project_layer_order_for_ports() {
    let temp = tempfile::tempdir().unwrap();
    let (host, project, keyed) = checkout_with_key(temp.path());
    std::fs::create_dir_all(&keyed).unwrap();

    // builtin < host < host-project < project: each leaf overrides the
    // layer below it, and the leaf reports the layer that set it.
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "verify": {"base": "host/main", "remote": "host-remote"},
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        keyed.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "verify": {"base": "host-project/main"},
            "build": {"strategy": "slots"},
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&serde_json::json!({
            "verify": {"base": "project/main"},
        }))
        .unwrap(),
    )
    .unwrap();

    let table = PortTable::read_for(Some(&project), &host).unwrap();
    assert_eq!(table.verify.base, "project/main");
    assert_eq!(table.verify.remote, "host-remote");
    assert_eq!(table.build.strategy, "slots");

    let provenance = table.provenance();
    assert_eq!(provenance.get("verify.base"), Some(&Layer::Project));
    assert_eq!(provenance.get("verify.remote"), Some(&Layer::Host));
    assert_eq!(provenance.get("build.strategy"), Some(&Layer::HostProject));
    assert_eq!(provenance.get("build.prepare"), Some(&Layer::Default));
    assert_eq!(
        provenance.get("verify.tiers.0.commands"),
        Some(&Layer::Default)
    );

    // Port labels agree with the leaves they own.
    assert_eq!(table.verify.source, Layer::Project);
    assert_eq!(table.build.source, Layer::HostProject);
    assert_eq!(table.land.source, Layer::Default);
}

#[test]
fn provenance_reports_highest_layer_per_leaf() {
    let temp = tempfile::tempdir().unwrap();
    let (host, project, keyed) = checkout_with_key(temp.path());
    std::fs::create_dir_all(&keyed).unwrap();
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "build": {"prepare": [["host-prepare"]], "offload": "remote"},
            "verify": {"base": "host/main"},
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        keyed.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "build": {"prepare": [["host-project-prepare"]]},
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&serde_json::json!({
            "build": {"clean": "none"},
        }))
        .unwrap(),
    )
    .unwrap();

    let table = PortTable::read_for(Some(&project), &host).unwrap();
    let provenance = table.provenance();
    assert_eq!(provenance.get("build.prepare"), Some(&Layer::HostProject));
    assert_eq!(provenance.get("build.offload"), Some(&Layer::Host));
    assert_eq!(provenance.get("build.clean"), Some(&Layer::Project));
    assert_eq!(provenance.get("build.strategy"), Some(&Layer::Default));
    assert_eq!(provenance.get("verify.base"), Some(&Layer::Host));
    assert_eq!(table.build.source, Layer::Project);

    // The table's full provenance remains derived state; only the frozen
    // build context carries the per-build-leaf map needed by lane jobs.
    let encoded = serde_json::to_value(&table).unwrap();
    assert!(encoded.get("provenance").is_none());
    assert!(encoded["build"]["leaf_sources"].is_object());
    assert_eq!(
        serde_json::from_value::<PortTable>(encoded)
            .unwrap()
            .provenance()
            .len(),
        0
    );
}

// Spec: docs/specs/state.md#every-setting-reports-its-layer
#[test]
fn sourced_leaves_report_the_winner_origin_and_shadowed_layers() {
    let temp = tempfile::tempdir().unwrap();
    let (host, project, keyed) = checkout_with_key(temp.path());
    std::fs::create_dir_all(&keyed).unwrap();
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "verify": {"remote": "host-remote"},
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&serde_json::json!({
            "verify": {"remote": "project-remote"},
        }))
        .unwrap(),
    )
    .unwrap();

    let table = PortTable::read_for(Some(&project), &host).unwrap();
    let candidates = table.sourced_leaf("verify.remote");
    // The compiled default, the host declaration and the project override.
    assert_eq!(candidates.len(), 3);
    let winner = candidates.last().unwrap();
    assert_eq!(winner.value, serde_json::json!("project-remote"));
    assert_eq!(winner.source.layer, Layer::Project);
    assert_eq!(
        winner.source.origin.as_deref(),
        Some(".provenant/ports.json#verify.remote")
    );
    let shadowed: Vec<_> = candidates[..candidates.len() - 1]
        .iter()
        .map(|candidate| (&candidate.value, candidate.source.layer))
        .collect();
    assert_eq!(shadowed[1].0, &serde_json::json!("host-remote"));
    assert_eq!(shadowed[1].1, Layer::Host);
    assert_eq!(shadowed[0].1, Layer::Default);
    assert!(table.sourced_leaf("absent.leaf").is_empty());
}

// Spec: docs/specs/state.md#every-setting-reports-its-layer
#[test]
fn outside_worktree_roots_report_their_layer_and_origin() {
    let temp = tempfile::tempdir().unwrap();
    let (host, project, keyed) = checkout_with_key(temp.path());
    std::fs::create_dir_all(&keyed).unwrap();
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "worktree_layout": {
                "roots": ["../host-writers"],
                "default": "../host-writers/<branch>",
            },
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        project.join(PORTS_CONFIG),
        serde_json::to_vec(&serde_json::json!({
            "worktree_layout": {
                "roots": ["/srv/project-writers"],
                "default": "/srv/project-writers/<branch>",
            },
        }))
        .unwrap(),
    )
    .unwrap();

    let table = PortTable::read_for(Some(&project), &host).unwrap();
    assert_eq!(
        table.worktree_layout.roots,
        vec![PathBuf::from("/srv/project-writers")]
    );
    let candidates = table.sourced_leaf("worktree_layout.roots");
    let winner = candidates.last().unwrap();
    assert_eq!(winner.source.layer, Layer::Project);
    assert_eq!(
        winner.source.origin.as_deref(),
        Some(".provenant/ports.json#worktree_layout.roots")
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.source.layer == Layer::Host)
    );
}

#[test]
fn typed_build_values_warn_with_leaf_source_and_fall_back_to_builtins() {
    let mut build = PortTable::builtin().build;
    build.strategy = "future_strategy".into();
    build.overflow = "future_overflow".into();
    build.clean = "future_clean".into();
    build.queue_aging = "tomorrow".into();
    build
        .leaf_sources
        .insert("build.strategy".into(), Layer::Project);
    build
        .leaf_sources
        .insert("build.overflow".into(), Layer::Host);
    build
        .leaf_sources
        .insert("build.clean".into(), Layer::Project);
    build
        .leaf_sources
        .insert("build.queue_aging".into(), Layer::Host);

    let strategy = build.strategy_name();
    assert_eq!(strategy.value, BuildStrategyName::Slots);
    assert_eq!(strategy.source, Layer::Default);
    assert!(strategy.warnings[0].contains("build.strategy"));
    assert!(strategy.warnings[0].contains("project"));

    let overflow = build.overflow_policy();
    assert_eq!(overflow.value, BuildOverflow::Auto);
    assert!(overflow.warnings[0].contains("host"));

    let clean = build.clean_policy();
    assert_eq!(clean.value, BuildClean::Untracked);
    assert!(clean.warnings[0].contains("project"));

    let aging = build.queue_aging_duration();
    assert_eq!(aging.value, Duration::from_secs(120));
    assert!(aging.warnings[0].contains("build.queue_aging"));
    assert!(aging.warnings[0].contains("host"));
}

#[test]
fn host_project_absent_is_silent() {
    let temp = tempfile::tempdir().unwrap();
    let host = temp.path().join("host");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(project.join(".provenant")).unwrap();
    std::fs::write(
        host.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({"build": {"strategy": "slots"}})).unwrap(),
    )
    .unwrap();

    // No key (a directory with no checkout marker) skips the layer.
    let table = PortTable::read_for(Some(&project), &host).unwrap();
    assert_eq!(table.build.strategy, "slots");
    assert_eq!(table.build.source, Layer::Host);
    assert!(
        table
            .provenance()
            .values()
            .all(|source| *source != Layer::HostProject)
    );

    // A key with no layer directory also skips it silently.
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let table = PortTable::read_for(Some(&project), &host).unwrap();
    assert_eq!(table.build.strategy, "slots");
    assert_eq!(table.build.source, Layer::Host);
    assert!(
        table
            .provenance()
            .values()
            .all(|source| *source != Layer::HostProject)
    );
    std::fs::write(host.join("projects"), "not a directory").unwrap();
    let table = PortTable::read_for(Some(&project), &host).unwrap();
    assert_eq!(table.build.strategy, "slots");
    assert_eq!(table.build.source, Layer::Host);
}

#[test]
fn project_key_moved_and_reexported_is_stable() {
    let fixed = Path::new("/repo/project/.git");
    assert_eq!(
        project_key(fixed),
        "e76e734a733dbce36a72e56fc8d5cff62d989351bb7e0aa357eb627686706354"
    );
    assert_eq!(project_key(fixed), project_key(fixed));
}

#[test]
fn port_declaration_warnings_rule_over_every_port_leaf() {
    let temp = tempfile::tempdir().unwrap();
    let (host, project, binding) = checkout_with_key(temp.path());
    let config = project.join(".provenant/config.toml");
    let config_text = config.display().to_string();

    // A clean config.toml declares no port leaves, so nothing is warned.
    std::fs::write(&config, "# no port declarations here\n").unwrap();
    assert!(
        port_declaration_warnings(Some(&project), &host).is_empty(),
        "a config with no port declarations must not warn"
    );

    // Rule: every compiled PortTable leaf, placed in config.toml, warns
    // naming its own dotted key and no other. The leaves come from the
    // declaration, so a new port key is covered without editing this test.
    let table = serde_json::to_value(PortTable::builtin()).unwrap_or_default();
    let mut leaves = Vec::new();
    port_leaf_paths(&table, "", &mut leaves);
    assert!(!leaves.is_empty(), "the compiled port table has leaves");
    for leaf in &leaves {
        std::fs::write(&config, format!("{leaf} = true\n")).unwrap();
        let warnings = port_declaration_warnings(Some(&project), &host);
        let config_warnings: Vec<_> = warnings
            .iter()
            .filter(|warning| warning.contains(&config_text))
            .collect();
        assert!(
            config_warnings
                .iter()
                .any(|warning| warning.contains(leaf.as_str())),
            "leaf {leaf} did not warn: {warnings:?}"
        );
        assert_eq!(
            config_warnings.len(),
            1,
            "leaf {leaf} should warn exactly once: {config_warnings:?}"
        );
    }

    // An inline table is a declaration too, and must warn on its leaf.
    std::fs::write(&config, "build = { prepare = [[\"node\", \"gen.mjs\"]] }\n").unwrap();
    let warnings = port_declaration_warnings(Some(&project), &host);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains(&config_text) && warning.contains("build.prepare")),
        "{warnings:?}"
    );
    assert_eq!(
        PortTable::read_for(Some(&project), &host)
            .unwrap()
            .build
            .prepare,
        Vec::<Vec<String>>::new(),
        "a misplaced declaration is advisory and leaves the table untouched"
    );

    // A host-project layer under the obsolete 64-digit target-seed key
    // names the file and the keys it actually holds, not a hard-coded leaf,
    // and points at the binding directory as the correct path.
    std::fs::write(&config, "# no port declarations here\n").unwrap();
    let key = project_key_for(&project).unwrap();
    let obsolete = host.join("projects").join(&key);
    std::fs::create_dir_all(&obsolete).unwrap();
    std::fs::write(
        obsolete.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "build": {"prepare": [["node", "tools/ensure-generated.mjs"]]},
            "verify": {"base": "origin/main"},
        }))
        .unwrap(),
    )
    .unwrap();
    let warnings = port_declaration_warnings(Some(&project), &host);
    assert!(
        warnings.iter().any(|warning| {
            warning.contains("ports.json")
                && warning.contains(&key)
                && warning.contains(&binding.display().to_string())
                && warning.contains("build.prepare")
                && warning.contains("verify.base")
        }),
        "{warnings:?}"
    );

    // The binding directory is the correct path: a layer there has no
    // misplacement warning and is read.
    std::fs::create_dir_all(&binding).unwrap();
    std::fs::rename(obsolete.join("ports.json"), binding.join("ports.json")).unwrap();
    let warnings = port_declaration_warnings(Some(&project), &host);
    let misplaced_path = obsolete.join("ports.json").display().to_string();
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.contains(&misplaced_path)),
        "{warnings:?}"
    );
    assert_eq!(
        PortTable::read_for(Some(&project), &host)
            .unwrap()
            .build
            .source,
        Layer::HostProject
    );
}

/// Spec: docs/specs/cli.md#warnings-are-typed-values
#[test]
fn an_older_schema_warning_uses_the_sink_and_names_the_file_and_change() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".provenant")).unwrap();
    let file = project.path().join(PORTS_CONFIG);
    std::fs::write(&file, r#"{"schema_version":2}"#).unwrap();

    let (_, warnings) =
        capture(|| PortTable::read_for_inspection(Some(project.path()), project.path()).unwrap());
    let warning = warnings
        .iter()
        .find(|warning| warning.code.as_ref() == CONFIG_MALFORMED.as_str())
        .expect("typed schema warning");
    let path = file.display().to_string();
    assert!(warning.message.contains(&path), "{:?}", warning.message);
    assert!(warning.message.contains("found 2"), "{:?}", warning.message);
    let repair = warning.repair.as_deref().expect("repair");
    assert!(repair.contains(&path), "{repair}");
    assert!(
        repair.contains(&format!("schema_version\": {PORTS_SCHEMA_VERSION}")),
        "{repair}"
    );
}

/// Spec: docs/specs/state.md#format-changes-migrate-once-at-install
#[test]
fn a_retired_source_spelling_is_refused_with_the_new_spelling() {
    for (json, key) in [
        (
            r#"{"schema_version":3,"verify":{"source":"builtin"}}"#,
            "verify.source",
        ),
        (
            r#"{"schema_version":3,"build":{"leaf_sources":{"build.strategy":"builtin"}}}"#,
            "build.leaf_sources.build.strategy",
        ),
    ] {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".provenant")).unwrap();
        let file = project.path().join(PORTS_CONFIG);
        std::fs::write(&file, json).unwrap();

        let (_, warnings) = capture(|| {
            PortTable::read_for_inspection(Some(project.path()), project.path()).unwrap()
        });
        let warning = warnings
            .iter()
            .find(|warning| warning.message.contains("builtin"))
            .expect("the retired spelling was refused with a typed warning");
        let path = file.display().to_string();
        assert!(warning.message.contains(&path), "{:?}", warning.message);
        assert!(warning.message.contains(key), "{:?}", warning.message);
        assert!(
            warning
                .message
                .contains(&format!("\"{}\"", Layer::Default.as_str())),
            "{:?}",
            warning.message
        );
        assert_eq!(warning.code.as_ref(), CONFIG_MALFORMED.as_str());
    }
}
