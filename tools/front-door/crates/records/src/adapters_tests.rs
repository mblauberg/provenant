//! Unit and invariant tests for the adapter registry in `adapters.rs`.
use super::*;

mod tests {
    use super::*;

    #[test]
    fn account_and_session_facts_are_consistent() {
        for spec in ADAPTER_SPECS {
            assert!(
                !spec.retention.transcript_account_root || spec.accounts.registered,
                "{}",
                spec.identity.name
            );
            assert!(
                spec.accounts.directory_prefix.is_none() || spec.accounts.registered,
                "{}",
                spec.identity.name
            );
            assert!(
                !spec.accounts.shared_settings
                    || (spec.accounts.registered && spec.accounts.config_dir),
                "{}",
                spec.identity.name
            );
            assert!(
                !spec.accounts.isolated_login_keychain || spec.login.local,
                "{}",
                spec.identity.name
            );
        }
    }

    #[test]
    fn host_native_declares_a_fresh_named_writer_contract() {
        let spec = AdapterSpec::get("host-native").unwrap();
        assert!(spec.identity.binary.is_empty() && !spec.accounts.registered);
        assert_eq!(
            spec.capabilities.launch.default_network,
            NetworkAccess::Open
        );
        assert!(
            spec.check_launch(&LaunchRequest {
                write: true,
                network: Some(NetworkAccess::Open),
                ..Default::default()
            })
            .gaps
            .is_empty()
        );
        for (request, field) in [
            (
                LaunchRequest {
                    model: Some("claude/model"),
                    ..Default::default()
                },
                LaunchField::Model,
            ),
            (
                LaunchRequest {
                    effort: Some("high"),
                    ..Default::default()
                },
                LaunchField::Effort,
            ),
            (
                LaunchRequest {
                    resume: true,
                    ..Default::default()
                },
                LaunchField::Resume,
            ),
        ] {
            assert_eq!(spec.check_launch(&request).gaps[0].field, field);
        }
    }

    #[test]
    fn managed_harness_capabilities_are_sourced_and_coherent() {
        for spec in ADAPTER_SPECS {
            let Some(install) = spec.install else {
                continue;
            };
            assert!(!install.home.is_empty(), "{}", spec.identity.name);
            assert!(!install.config.is_empty(), "{}", spec.identity.name);
            assert!(!install.source.is_empty(), "{}", spec.identity.name);
            assert_eq!(
                install.native.hooks.value,
                install.hook_file.is_some(),
                "{}",
                spec.identity.name
            );
            assert_eq!(
                install.native.instructions.value,
                install.instruction_file.is_some(),
                "{}",
                spec.identity.name
            );
            for support in [
                install.native.hooks,
                install.native.instructions,
                install.native.answer_delivery,
                install.native.precompact_context,
            ] {
                if !support.value {
                    let AdapterFact { reason, source, .. } = support;
                    assert!(!reason.trim().is_empty(), "{}", spec.identity.name);
                    assert!(!source.trim().is_empty(), "{}", spec.identity.name);
                }
            }
            assert_eq!(
                spec.capabilities.launch.default_network,
                NetworkAccess::Open,
                "{}",
                spec.identity.name
            );
            for network in ALL_NETWORK {
                assert!(
                    spec.capabilities.launch.network.contains(network),
                    "{}",
                    spec.identity.name
                );
            }
            assert!(
                spec.capabilities.launch.write.value,
                "managed providers share worktree-write capability"
            );
        }
    }

    #[test]
    fn adapter_specs_are_unique_and_match_typed_identities() {
        for spec in ADAPTER_SPECS {
            assert_eq!(
                ADAPTER_SPECS
                    .iter()
                    .filter(|row| row.identity.name == spec.identity.name)
                    .count(),
                1
            );
            assert!(std::ptr::eq(spec.id.spec(), spec));
            assert!(std::ptr::eq(
                AdapterSpec::get(spec.identity.name).unwrap(),
                spec
            ));
            assert_eq!(serde_json::to_value(spec.id).unwrap(), spec.identity.name);
            assert_eq!(
                serde_json::from_value::<Harness>(serde_json::json!(spec.identity.name)).unwrap(),
                spec.id
            );
        }
        assert!(AdapterSpec::get("unknown").is_none());
    }
}

mod account_tests {
    use super::*;

    #[test]
    fn only_registered_adapters_declare_config_directory_accounts() {
        for spec in ADAPTER_SPECS {
            assert!(
                !spec.accounts.config_dir || spec.accounts.registered,
                "{}",
                spec.identity.name
            );
        }
        assert!(AdapterSpec::get("codex").unwrap().accounts.config_dir);
        assert!(!AdapterSpec::get("agy").unwrap().accounts.config_dir);
    }

    #[test]
    fn unregistered_local_accounts_follow_registered_local_login_facts() {
        for spec in ADAPTER_SPECS {
            let has_local_account = spec.accounts.registered && spec.login.local;
            for (key, expected) in [("local", has_local_account), ("fixture-account", false)] {
                assert_eq!(
                    spec.is_unregistered_local(key),
                    expected,
                    "{}: {key}",
                    spec.identity.name
                );
            }
        }
    }
}

mod account_plan_tests {
    #[test]
    fn registered_plan_partitions_follow_the_adapter_declaration() {
        for spec in super::ADAPTER_SPECS {
            for plan in spec.accounts.model_plans {
                let model = format!("{}fixture", plan.model_prefix);
                assert!(spec.account_serves_model(Some(plan.plan), &model));
                assert_eq!(
                    spec.is_free_plan(Some(plan.plan)),
                    plan.quota == super::AccountQuota::ObservedRefusals
                );
                assert!(spec.account_serves_model(None, &model));
                assert!(spec.account_serves_model(Some("unrecognised"), &model));
                for other in spec
                    .accounts
                    .model_plans
                    .iter()
                    .filter(|other| other.plan != plan.plan)
                {
                    assert!(!spec.account_serves_model(Some(other.plan), &model));
                }
            }
        }
    }
}

mod launch_tests {
    use super::*;

    #[test]
    fn command_has_no_model_effort_session_or_write_capability() {
        let command = AdapterSpec::get("command").unwrap();
        assert!(!command.accounts.registered);
        assert_eq!(command.quota.probe, QuotaProbeKind::None);
        assert_eq!(
            command.capabilities.launch.default_network,
            NetworkAccess::Open
        );
        assert!(
            command
                .check_launch(&LaunchRequest::default())
                .gaps
                .is_empty()
        );
        let launch = command.capabilities.launch;
        for declaration in [
            launch.model,
            launch.effort,
            launch.image_input,
            launch.resume,
            launch.write,
        ] {
            let name = command.identity.name;
            assert!(!declaration.value, "{name}");
            let AdapterFact { reason, source, .. } = declaration;
            assert!(!reason.trim().is_empty(), "{name}");
            assert!(!source.trim().is_empty(), "{name}");
        }
        assert_eq!(
            command.capabilities.usage_attribution.value,
            UsageAttribution::Attempt
        );
        for (request, field) in [
            (
                LaunchRequest {
                    model: Some("placeholder"),
                    ..Default::default()
                },
                LaunchField::Model,
            ),
            (
                LaunchRequest {
                    effort: Some("high"),
                    ..Default::default()
                },
                LaunchField::Effort,
            ),
            (
                LaunchRequest {
                    resume: true,
                    ..Default::default()
                },
                LaunchField::Resume,
            ),
            (
                LaunchRequest {
                    write: true,
                    ..Default::default()
                },
                LaunchField::Write,
            ),
        ] {
            let check = command.check_launch(&request);
            let decision = &check.gaps[0];
            assert_eq!(decision.field, field);
            assert_eq!(decision.action, field.default_action());
        }
    }

    #[test]
    fn launch_check_reports_all_gaps_in_field_order() {
        let spec = AdapterSpec {
            launch: AdapterLaunch {
                skill_location: None,
                ..ADAPTER_SPECS[0].launch
            },
            capabilities: Capabilities {
                launch: LaunchCapabilities {
                    model: AdapterFact::unsupported("no model", ADAPTER_ROW_SOURCE),
                    effort: AdapterFact::unsupported("no effort", ADAPTER_ROW_SOURCE),
                    image_input: AdapterFact::unsupported("no images", ADAPTER_ROW_SOURCE),
                    resume: AdapterFact::unsupported("no resume", ADAPTER_ROW_SOURCE),
                    network: &[],
                    write: AdapterFact::unsupported("no write", ADAPTER_ROW_SOURCE),
                    ..ADAPTER_SPECS[0].capabilities.launch
                },
                ..ADAPTER_SPECS[0].capabilities
            },
            ..ADAPTER_SPECS[0]
        };
        let check = spec.check_launch(&LaunchRequest {
            model: Some("model"),
            effort: Some("high"),
            image_input: true,
            resume: true,
            network: Some(NetworkAccess::Open),
            write: true,
            skill_location: true,
            gap_policy: GapPolicy::default(),
        });
        let expected = [
            LaunchField::Model,
            LaunchField::Effort,
            LaunchField::ImageInput,
            LaunchField::Resume,
            LaunchField::Network,
            LaunchField::Write,
            LaunchField::SkillLocation,
        ];
        assert_eq!(
            check.gaps.iter().map(|d| d.field).collect::<Vec<_>>(),
            expected
        );
        for decision in &check.gaps {
            assert_eq!(decision.action, decision.field.default_action());
            assert!(!decision.reason.is_empty() && !decision.source.is_empty());
        }
        assert_eq!(check.refusal().map(|d| d.field), Some(LaunchField::Resume));
        assert_eq!(
            check.degraded(LaunchField::ImageInput).map(|d| d.field),
            Some(LaunchField::ImageInput)
        );
    }

    #[test]
    fn launch_table_is_checked_per_adapter() {
        // (adapter, effort, resume, network, write, refused field)
        let effort = Some("high");
        let cases = [
            ("claude", effort, None),
            ("codex", effort, None),
            ("agy", effort, None),
            ("opencode", effort, None),
            ("cursor", effort, None),
            ("cursor", None, None),
        ];
        for (name, effort, refused) in cases {
            let spec = AdapterSpec::get(name).unwrap();
            let request = LaunchRequest {
                model: None,
                effort,
                resume: true,
                network: Some(NetworkAccess::Open),
                write: true,
                ..Default::default()
            };
            assert_eq!(
                spec.check_launch(&request).gaps.first().map(|e| e.field),
                refused,
                "{name}"
            );
        }
    }

    #[test]
    fn refusal_names_the_adapter_and_field() {
        let decision = AdapterSpec::get("command")
            .unwrap()
            .check_launch(&LaunchRequest {
                effort: Some("low"),
                ..Default::default()
            })
            .gaps
            .remove(0);
        let text = decision.to_string();
        assert!(
            text.starts_with("command does not support effort: "),
            "{text}"
        );
        assert!(text.contains(decision.reason), "{text}");
        assert!(
            text.contains(&format!("source={}", decision.source)),
            "{text}"
        );
    }

    #[test]
    fn declared_gaps_decide_by_field() {
        for spec in ADAPTER_SPECS {
            let capabilities = spec.capabilities.launch;
            let cases = [
                (
                    LaunchRequest {
                        model: Some("fixture/model"),
                        ..Default::default()
                    },
                    capabilities.model.value,
                    LaunchField::Model,
                ),
                (
                    LaunchRequest {
                        effort: Some("high"),
                        ..Default::default()
                    },
                    capabilities.effort.value,
                    LaunchField::Effort,
                ),
                (
                    LaunchRequest {
                        image_input: true,
                        ..Default::default()
                    },
                    capabilities.image_input.value,
                    LaunchField::ImageInput,
                ),
                (
                    LaunchRequest {
                        resume: true,
                        ..Default::default()
                    },
                    capabilities.resume.value,
                    LaunchField::Resume,
                ),
                (
                    LaunchRequest {
                        write: true,
                        ..Default::default()
                    },
                    capabilities.write.value,
                    LaunchField::Write,
                ),
                (
                    LaunchRequest {
                        skill_location: true,
                        ..Default::default()
                    },
                    spec.launch.skill_location.is_some(),
                    LaunchField::SkillLocation,
                ),
            ];
            for (request, supported, field) in cases {
                if !supported {
                    let check = spec.check_launch(&request);
                    let decision = &check.gaps[0];
                    assert_eq!(decision.field, field, "{}", spec.identity.name);
                    assert_eq!(
                        decision.action,
                        field.default_action(),
                        "{}",
                        spec.identity.name
                    );
                    let text = decision.to_string();
                    assert!(
                        text.starts_with(&format!("{} does not support ", spec.identity.name)),
                        "{text}"
                    );
                    assert!(text.contains(field.name()), "{text}");
                    assert!(text.contains(decision.reason), "{text}");
                    assert!(
                        text.contains(&format!("source={}", decision.source)),
                        "{text}"
                    );
                }
            }
            for network in ALL_NETWORK {
                if !capabilities.network.contains(network) {
                    let decision = &spec
                        .check_launch(&LaunchRequest {
                            network: Some(*network),
                            ..Default::default()
                        })
                        .gaps[0];
                    assert_eq!(
                        decision.field,
                        LaunchField::Network,
                        "{}",
                        spec.identity.name
                    );
                    assert_eq!(
                        decision.action,
                        LaunchAction::Refuse,
                        "{}",
                        spec.identity.name
                    );
                }
            }
        }
    }
}

mod gap_policy_tests {
    use super::*;
    use crate::{
        Layer, Source,
        policy::{Policy, Strength},
    };
    use std::collections::BTreeMap;

    fn request(field: LaunchField) -> LaunchRequest<'static> {
        match field {
            LaunchField::Model => LaunchRequest {
                model: Some("fixture/model"),
                ..Default::default()
            },
            LaunchField::Effort => LaunchRequest {
                effort: Some("high"),
                ..Default::default()
            },
            LaunchField::ImageInput => LaunchRequest {
                image_input: true,
                ..Default::default()
            },
            _ => panic!("{field:?} is not a degradable field"),
        }
    }

    fn policy_code(field: LaunchField) -> &'static str {
        match field {
            LaunchField::Model => "LAUNCH_MODEL_UNSUPPORTED",
            LaunchField::Effort => "LAUNCH_EFFORT_UNSUPPORTED",
            LaunchField::ImageInput => "LAUNCH_IMAGE_INPUT_UNSUPPORTED",
            _ => panic!("{field:?} is not a degradable field"),
        }
    }

    fn entry(adapter: &str, field: LaunchField, action: GapAction) -> CapabilityOverlay {
        CapabilityOverlay {
            entries: vec![AdapterOverride {
                adapter: adapter.into(),
                disable: vec![],
                reason: None,
                on_gap: vec![(field, action)],
                source: Source {
                    layer: Layer::Host,
                    origin: Some("config.toml#adapter_capabilities".into()),
                },
            }],
        }
    }

    #[test]
    fn gap_actions_are_table_driven_across_policy_and_entry_origins() {
        let spec = AdapterSpec::get("command").unwrap();
        for field in [
            LaunchField::Model,
            LaunchField::Effort,
            LaunchField::ImageInput,
        ] {
            for action in [GapAction::Warn, GapAction::Off, GapAction::Refuse] {
                let strength = match action {
                    GapAction::Warn => Strength::Warn,
                    GapAction::Off => Strength::Off,
                    GapAction::Refuse => Strength::Refuse,
                };
                let mut policy = Policy::default();
                policy.overlay(
                    &BTreeMap::from([(policy_code(field).to_string(), strength)]),
                    Layer::Host,
                );
                let policy_request = LaunchRequest {
                    gap_policy: CapabilityOverlay::default().gap_policy("command", &policy),
                    ..request(field)
                };
                let entry_request = LaunchRequest {
                    gap_policy: entry("command", field, action)
                        .gap_policy("command", &Policy::default()),
                    ..request(field)
                };
                let expected_action = if action == GapAction::Refuse {
                    LaunchAction::Refuse
                } else {
                    LaunchAction::Degrade
                };
                let expected_warn =
                    expected_action == LaunchAction::Refuse || action == GapAction::Warn;
                let key = field.on_gap_key().unwrap();
                let entry_origin =
                    format!("config.toml#adapter_capabilities[adapter=command].on_gap.{key}");
                // The entry wins over the policy floor and records it as shadowed.
                assert_eq!(
                    entry_request.gap_policy.rule(field).unwrap().shadowed,
                    [crate::Sourced {
                        value: GapAction::Warn,
                        source: Source {
                            layer: Layer::Default,
                            origin: Some(format!("policy.{}", policy_code(field))),
                        },
                    }]
                );
                for (request, origin) in [
                    (policy_request, format!("policy.{}", policy_code(field))),
                    (entry_request, entry_origin),
                ] {
                    let check = spec.check_launch(&request);
                    let gap = &check.gaps[0];
                    assert_eq!(gap.field, field);
                    assert_eq!(gap.action, expected_action, "{field:?} {action:?}");
                    assert_eq!(gap.warn, expected_warn, "{field:?} {action:?}");
                    assert_eq!(
                        gap.action_source,
                        Source {
                            layer: Layer::Host,
                            origin: Some(origin.clone()),
                        },
                        "{field:?} {action:?}"
                    );
                    let text = gap.to_string();
                    assert!(
                        text.contains(&format!("action set by {origin} (host configuration)")),
                        "{text}"
                    );
                    if expected_action == LaunchAction::Refuse {
                        assert_eq!(check.refusal().map(|gap| gap.field), Some(field));
                    } else {
                        assert_eq!(check.degraded(field).map(|gap| gap.field), Some(field));
                    }
                }
            }
        }
    }

    #[test]
    fn default_policy_keeps_the_declared_actions() {
        let spec = AdapterSpec::get("command").unwrap();
        for field in [
            LaunchField::Model,
            LaunchField::Effort,
            LaunchField::ImageInput,
        ] {
            let gap = &spec
                .check_launch(&LaunchRequest {
                    gap_policy: CapabilityOverlay::default()
                        .gap_policy("command", &Policy::default()),
                    ..request(field)
                })
                .gaps[0];
            assert_eq!(gap.action, field.default_action());
            assert!(gap.warn, "{gap:?}");
            assert_eq!(gap.action_source.layer, Layer::Default);
            // A default names the two settings that can change it.
            let text = gap.to_string();
            assert!(
                text.ends_with(&format!(
                    "; warn by default; set policy.{} or [[adapter_capabilities]] on_gap.{}",
                    policy_code(field),
                    field.on_gap_key().unwrap()
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn refuse_only_fields_ignore_every_gap_policy() {
        let row = AdapterSpec::get("command").unwrap();
        let spec = AdapterSpec {
            capabilities: Capabilities {
                launch: LaunchCapabilities {
                    network: &[],
                    ..row.capabilities.launch
                },
                ..row.capabilities
            },
            ..*row
        };
        let mut policy = Policy::default();
        policy.overlay(
            &BTreeMap::from([
                ("LAUNCH_MODEL_UNSUPPORTED".to_string(), Strength::Refuse),
                ("LAUNCH_EFFORT_UNSUPPORTED".to_string(), Strength::Refuse),
                (
                    "LAUNCH_IMAGE_INPUT_UNSUPPORTED".to_string(),
                    Strength::Refuse,
                ),
            ]),
            Layer::Host,
        );
        let overlay = CapabilityOverlay {
            entries: vec![AdapterOverride {
                adapter: "command".into(),
                disable: vec![],
                reason: None,
                on_gap: vec![
                    (LaunchField::Resume, GapAction::Off),
                    (LaunchField::Network, GapAction::Off),
                    (LaunchField::Write, GapAction::Off),
                    (LaunchField::SkillLocation, GapAction::Off),
                ],
                source: Source {
                    layer: Layer::Project,
                    origin: Some("config.toml#adapter_capabilities".into()),
                },
            }],
        };
        let check = spec.check_launch(&LaunchRequest {
            resume: true,
            network: Some(NetworkAccess::Open),
            write: true,
            skill_location: true,
            gap_policy: overlay.gap_policy("command", &policy),
            ..Default::default()
        });
        assert_eq!(
            check.gaps.iter().map(|gap| gap.field).collect::<Vec<_>>(),
            [
                LaunchField::Resume,
                LaunchField::Network,
                LaunchField::Write,
                LaunchField::SkillLocation,
            ]
        );
        for gap in &check.gaps {
            assert_eq!(gap.action, LaunchAction::Refuse);
            assert!(gap.warn, "{gap:?}");
            assert_eq!(gap.action_source, Source::default());
            let text = gap.to_string();
            assert!(
                !text.contains("action set by") && !text.contains("by default; set"),
                "{text}"
            );
        }
    }

    #[test]
    fn with_overlay_disables_exactly_the_requested_facts() {
        let spec = AdapterSpec::get("codex").unwrap();
        let overlay = CapabilityOverlay {
            entries: vec![AdapterOverride {
                adapter: "codex".into(),
                disable: OverridableCapability::ALL.to_vec(),
                reason: Some("narrowed by the host".into()),
                on_gap: vec![],
                source: Source {
                    layer: Layer::Host,
                    origin: Some("config.toml#adapter_capabilities".into()),
                },
            }],
        };
        let narrowed = spec.with_overlay(&overlay);
        let launch = narrowed.capabilities.launch;
        for fact in [
            launch.model,
            launch.effort,
            launch.image_input,
            launch.resume,
            launch.write,
            narrowed.capabilities.live_input,
        ] {
            assert!(!fact.value, "{fact:?}");
            assert_eq!(fact.reason, OVERLAY_REASON);
            assert_eq!(fact.source, OVERLAY_SOURCE);
        }
        assert_eq!(launch.network, spec.capabilities.launch.network);
        assert_eq!(
            launch.default_network,
            spec.capabilities.launch.default_network
        );
        assert_eq!(narrowed.capabilities.control, spec.capabilities.control);
        assert_eq!(
            narrowed.capabilities.terminal_event,
            spec.capabilities.terminal_event
        );
        assert_eq!(
            narrowed.capabilities.answering_model,
            spec.capabilities.answering_model
        );
        assert_eq!(
            narrowed.capabilities.usage_attribution,
            spec.capabilities.usage_attribution
        );
        assert_eq!(narrowed.id, spec.id);
        assert_eq!(narrowed.identity.name, spec.identity.name);
        assert_eq!(narrowed.launch.skill_location, spec.launch.skill_location);
        assert_eq!(narrowed.retention.artifacts, spec.retention.artifacts);
        assert_eq!(
            narrowed.retention.transcript_layout,
            spec.retention.transcript_layout
        );
        assert_eq!(narrowed.install, spec.install);

        let untouched = AdapterSpec::get("claude").unwrap();
        assert_eq!(
            untouched.with_overlay(&overlay).capabilities.launch.model,
            untouched.capabilities.launch.model
        );
        assert_eq!(
            spec.with_overlay(&CapabilityOverlay::default())
                .capabilities
                .launch
                .model,
            spec.capabilities.launch.model
        );
    }
}

mod batch_invariant_tests {
    use super::*;
    use std::path::Path;

    /// A path-shaped source is checked against the workspace; a URL or a config
    /// key is documentation, not a file in this tree.
    fn source_exists(source: &str, front_door: &Path, label: &str) {
        let path = source.split('#').next().unwrap_or(source);
        if path.starts_with("crates/")
            || path.starts_with("src/")
            || path.starts_with("tests/")
            || path.starts_with("config/")
        {
            assert!(
                front_door.join(path).exists(),
                "{label}: source {path} does not exist under tools/front-door"
            );
        }
    }

    fn absent_fact<T: PartialEq>(fact: AdapterFact<T>, absent: T, label: &str, front_door: &Path) {
        if fact.value != absent {
            return;
        }
        let label = format!("{label} ({})", fact.reason);
        assert_ne!(fact.reason, ADAPTER_ROW_REASON, "{label}");
        assert!(!fact.reason.trim().is_empty(), "{label}");
        assert!(!fact.source.trim().is_empty(), "{label}");
        source_exists(fact.source, front_door, &label);
    }

    /// Invariant 1: every absent fact names a reason other than the row default
    /// and a non-empty source; path-shaped sources exist. Caveat sources too.
    #[test]
    fn absent_facts_are_sourced_and_path_sources_exist() {
        let front_door = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for spec in ADAPTER_SPECS {
            let name = spec.identity.name;
            let launch = spec.capabilities.launch;
            let caps = spec.capabilities;
            for (fact, label) in [
                (launch.model, "model"),
                (launch.effort, "effort"),
                (launch.image_input, "image input"),
                (launch.resume, "resume"),
                (launch.write, "write"),
                (caps.live_input, "live input"),
            ] {
                absent_fact(fact, false, &format!("{name}: {label}"), &front_door);
            }
            absent_fact(
                caps.terminal_event,
                TerminalEventStrength::Absent,
                &format!("{name}: terminal event"),
                &front_door,
            );
            absent_fact(
                caps.answering_model,
                AnsweringModelSource::Unreported,
                &format!("{name}: answering model"),
                &front_door,
            );
            absent_fact(
                caps.session_reference,
                SessionReference::Absent,
                &format!("{name}: session reference"),
                &front_door,
            );
            absent_fact(
                spec.retention.transcript_layout,
                false,
                &format!("{name}: transcript layout"),
                &front_door,
            );
            if let Some(install) = spec.install {
                for (fact, label) in [
                    (install.native.hooks, "hooks"),
                    (install.native.answer_delivery, "answer delivery"),
                    (install.native.precompact_context, "precompact context"),
                    (install.native.instructions, "instructions"),
                ] {
                    absent_fact(fact, false, &format!("{name}: {label}"), &front_door);
                }
            }
            for caveat in caps.caveats {
                let label = format!("{name}: caveat {}", caveat.code);
                assert!(!caveat.code.is_empty(), "{label}");
                assert!(!caveat.note.is_empty(), "{label}");
                assert!(!caveat.source.is_empty(), "{label}");
                source_exists(caveat.source, &front_door, &label);
            }
        }
    }

    /// Invariant 2: raw framing and an absent terminal event are the same set.
    #[test]
    fn raw_framing_iff_absent_terminal_event() {
        for spec in ADAPTER_SPECS {
            assert_eq!(
                spec.launch.framing == StreamFraming::Raw,
                spec.capabilities.terminal_event.value == TerminalEventStrength::Absent,
                "{}",
                spec.identity.name
            );
        }
    }

    /// Invariant 3: one gap per field in order, degrading exactly the three
    /// input fields. The table is exhaustive over `LaunchField`.
    #[test]
    fn every_absent_field_decides_by_field_in_order() {
        let expected = [
            (LaunchField::Model, LaunchAction::Degrade),
            (LaunchField::Effort, LaunchAction::Degrade),
            (LaunchField::ImageInput, LaunchAction::Degrade),
            (LaunchField::Resume, LaunchAction::Refuse),
            (LaunchField::Network, LaunchAction::Refuse),
            (LaunchField::Write, LaunchAction::Refuse),
            (LaunchField::SkillLocation, LaunchAction::Refuse),
        ];
        for (field, action) in expected {
            assert_eq!(field.default_action(), action, "{field:?}");
        }
        let row = AdapterSpec::get("command").unwrap();
        let spec = AdapterSpec {
            launch: AdapterLaunch {
                skill_location: None,
                ..row.launch
            },
            capabilities: Capabilities {
                launch: LaunchCapabilities {
                    model: AdapterFact::unsupported("no model", ADAPTER_ROW_SOURCE),
                    effort: AdapterFact::unsupported("no effort", ADAPTER_ROW_SOURCE),
                    image_input: AdapterFact::unsupported("no images", ADAPTER_ROW_SOURCE),
                    resume: AdapterFact::unsupported("no resume", ADAPTER_ROW_SOURCE),
                    write: AdapterFact::unsupported("no write", ADAPTER_ROW_SOURCE),
                    network: &[],
                    ..row.capabilities.launch
                },
                ..row.capabilities
            },
            ..*row
        };
        let check = spec.check_launch(&LaunchRequest {
            model: Some("model"),
            effort: Some("high"),
            image_input: true,
            resume: true,
            network: Some(NetworkAccess::Open),
            write: true,
            skill_location: true,
            ..Default::default()
        });
        assert_eq!(
            check.gaps.iter().map(|gap| gap.field).collect::<Vec<_>>(),
            expected.iter().map(|(field, _)| *field).collect::<Vec<_>>()
        );
        for gap in &check.gaps {
            assert_eq!(gap.action, gap.field.default_action(), "{gap:?}");
        }
    }

    /// Invariant 4: no row claims live input without a live control lane.
    #[test]
    fn live_input_requires_a_live_control_lane() {
        for spec in ADAPTER_SPECS {
            if spec.capabilities.control.lane != ControlSupport::LiveInjection {
                assert!(
                    !spec.capabilities.live_input.value,
                    "{} declares live input without a live lane",
                    spec.identity.name
                );
            }
        }
    }

    /// Invariant 5: an artifact layout is declared only with a known layout.
    #[test]
    fn retained_artifacts_require_a_known_transcript_layout() {
        for spec in ADAPTER_SPECS {
            if !spec.retention.artifacts.is_empty() {
                assert!(
                    spec.retention.transcript_layout.value,
                    "{}",
                    spec.identity.name
                );
            }
        }
    }

    /// Invariant 9: a resumable adapter exposes a provider session id.
    #[test]
    fn resume_requires_a_reported_session_reference() {
        for spec in ADAPTER_SPECS {
            if spec.capabilities.launch.resume.value {
                assert_ne!(
                    spec.capabilities.session_reference.value,
                    SessionReference::Absent,
                    "{} declares resume without a session id",
                    spec.identity.name
                );
            }
        }
    }

    /// Invariant 10: raw output never claims a source for the answering model.
    #[test]
    fn raw_framing_never_claims_an_answering_model() {
        for spec in ADAPTER_SPECS {
            if spec.launch.framing == StreamFraming::Raw {
                assert_eq!(
                    spec.capabilities.answering_model.value,
                    AnsweringModelSource::Unreported,
                    "{}",
                    spec.identity.name
                );
            }
        }
    }

    /// Invariant 11: an overlay can only narrow, never widen, any row's facts.
    #[test]
    fn an_overlay_never_turns_a_fact_true() {
        use crate::{Layer, Source};
        let overlay = CapabilityOverlay {
            entries: ADAPTER_SPECS
                .iter()
                .map(|spec| AdapterOverride {
                    adapter: spec.identity.name.to_owned(),
                    disable: OverridableCapability::ALL.to_vec(),
                    reason: Some("batch invariant".to_owned()),
                    on_gap: vec![],
                    source: Source {
                        layer: Layer::Host,
                        origin: Some(OVERLAY_SOURCE.to_owned()),
                    },
                })
                .collect(),
        };
        for spec in ADAPTER_SPECS {
            let narrowed = spec.with_overlay(&overlay);
            for (index, fact) in [
                narrowed.capabilities.launch.model,
                narrowed.capabilities.launch.effort,
                narrowed.capabilities.launch.image_input,
                narrowed.capabilities.launch.resume,
                narrowed.capabilities.launch.write,
                narrowed.capabilities.live_input,
            ]
            .into_iter()
            .enumerate()
            {
                assert!(
                    !fact.value,
                    "{}: fact {index} was widened",
                    spec.identity.name
                );
            }
        }
    }

    /// Invariant 12: no gap setting changes a refuse-only field's action.
    #[test]
    fn no_gap_setting_changes_a_refuse_only_field() {
        use crate::{
            Layer, Source,
            policy::{Policy, Strength},
        };
        use std::collections::BTreeMap;

        fn request(field: LaunchField) -> LaunchRequest<'static> {
            match field {
                LaunchField::Resume => LaunchRequest {
                    resume: true,
                    ..Default::default()
                },
                LaunchField::Network => LaunchRequest {
                    network: Some(NetworkAccess::Open),
                    ..Default::default()
                },
                LaunchField::Write => LaunchRequest {
                    write: true,
                    ..Default::default()
                },
                LaunchField::SkillLocation => LaunchRequest {
                    skill_location: true,
                    ..Default::default()
                },
                _ => panic!("{field:?} is not a refuse-only field"),
            }
        }

        let row = AdapterSpec::get("command").unwrap();
        let spec = AdapterSpec {
            capabilities: Capabilities {
                launch: LaunchCapabilities {
                    network: &[],
                    ..row.capabilities.launch
                },
                ..row.capabilities
            },
            ..*row
        };
        for action in [GapAction::Warn, GapAction::Off, GapAction::Refuse] {
            let strength = match action {
                GapAction::Warn => Strength::Warn,
                GapAction::Off => Strength::Off,
                GapAction::Refuse => Strength::Refuse,
            };
            let mut policy = Policy::default();
            policy.overlay(
                &BTreeMap::from([
                    ("LAUNCH_MODEL_UNSUPPORTED".to_string(), strength),
                    ("LAUNCH_EFFORT_UNSUPPORTED".to_string(), strength),
                    ("LAUNCH_IMAGE_INPUT_UNSUPPORTED".to_string(), strength),
                ]),
                Layer::Host,
            );
            for field in [
                LaunchField::Resume,
                LaunchField::Network,
                LaunchField::Write,
                LaunchField::SkillLocation,
            ] {
                let overlay = CapabilityOverlay {
                    entries: vec![AdapterOverride {
                        adapter: row.identity.name.to_owned(),
                        disable: vec![],
                        reason: None,
                        on_gap: vec![(field, action)],
                        source: Source {
                            layer: Layer::Project,
                            origin: Some(OVERLAY_SOURCE.to_owned()),
                        },
                    }],
                };
                let check = spec.check_launch(&LaunchRequest {
                    gap_policy: overlay.gap_policy(row.identity.name, &policy),
                    ..request(field)
                });
                let gap = check
                    .gaps
                    .iter()
                    .find(|gap| gap.field == field)
                    .unwrap_or_else(|| panic!("{field:?} produced no gap"));
                assert_eq!(gap.action, LaunchAction::Refuse, "{field:?} {action:?}");
                assert_eq!(
                    gap.action_source,
                    crate::Source::default(),
                    "{field:?} {action:?}"
                );
                assert!(gap.warn, "{gap:?}");
            }
        }
    }

    /// Invariant 13: every configurable key in the batch names a documented
    /// knob with a default and allowed values.
    #[test]
    fn every_configurable_key_is_documented_with_a_default() {
        let registry: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../config/harness-knobs.json"))
                .expect("knob registry");
        let documented = registry["documented_fields"]
            .as_array()
            .expect("documented_fields");
        // Policy knobs are documented in `policy_rules` below; the curated
        // compact reference lists the configuration keys.
        for key in [
            "providers.{provider}.version_probe",
            "providers.{provider}.version_probe_timeout_seconds",
            "domains.adapter.capabilities.measurements",
            "domains.adapter.capabilities.measurement_samples",
            "domains.adapter.capabilities.measurement_records",
        ] {
            assert!(
                documented.iter().any(|value| value.as_str() == Some(key)),
                "{key} is not a documented field"
            );
        }
        let policies = registry["policy_rules"]["settings"]
            .as_object()
            .expect("policy rules");
        for code in [
            "LAUNCH_MODEL_UNSUPPORTED",
            "LAUNCH_EFFORT_UNSUPPORTED",
            "LAUNCH_IMAGE_INPUT_UNSUPPORTED",
            "HARNESS_VERSION_UNSUPPORTED",
            "HARNESS_VERSION_BROKEN",
        ] {
            let knob = format!("policy.{code}");
            let rule = policies
                .get(&knob)
                .unwrap_or_else(|| panic!("{knob} undocumented"));
            assert_eq!(rule["default"].as_str(), Some("warn"), "{knob}");
            assert!(
                !rule["allowed"].as_str().unwrap_or_default().is_empty(),
                "{knob}"
            );
        }
        let domain = registry["domain_adapter_settings"]["settings"]
            .as_object()
            .expect("adapter settings");
        for key in ["measurements", "measurement_samples", "measurement_records"] {
            let rule = domain
                .get(key)
                .unwrap_or_else(|| panic!("{key} undocumented"));
            assert!(!rule["default"].is_null(), "{key}");
            assert!(
                !rule["allowed"].as_str().unwrap_or_default().is_empty(),
                "{key}"
            );
        }
        // Every [[adapter_capabilities]] key, including each settable on_gap
        // field, is in the registry the reference and config tools read.
        let entries = registry["adapter_capabilities_settings"]["settings"]
            .as_object()
            .expect("adapter capability entry settings");
        let keys = ["adapter", "disable", "reason"]
            .map(str::to_owned)
            .into_iter()
            .chain(
                LaunchField::DEGRADABLE
                    .iter()
                    .filter_map(|field| field.on_gap_key())
                    .map(|key| format!("on_gap.{key}")),
            );
        for key in keys {
            let rule = entries
                .get(&key)
                .unwrap_or_else(|| panic!("{key} undocumented"));
            assert!(!rule["default"].is_null(), "{key}");
            assert!(
                !rule["allowed"].as_str().unwrap_or_default().is_empty(),
                "{key}"
            );
        }
    }
}
