use super::FacadeError;

#[test]
fn relay_causes_survive_diagnostics_and_wire_round_trips() -> Result<(), Box<dyn std::error::Error>>
{
    for cause in [
        super::GraphRelayCause::CredentialValue,
        super::GraphRelayCause::WriterParentRequired,
        super::GraphRelayCause::WriterWorktree,
        super::GraphRelayCause::ChildDepth,
        super::GraphRelayCause::TargetOutsideGraph,
    ] {
        let error = FacadeError::GraphRelayRefused {
            refusal_code: "refused_child".into(),
            detail: Some(super::InvalidInputDetail::GraphRelayCause(cause.clone())),
        };
        assert_eq!(error.diagnostic().detail.as_deref(), Some(cause.text()));
        let restored: FacadeError = serde_json::from_value(serde_json::to_value(&error)?)?;
        assert_eq!(restored, error);
        assert_eq!(restored.diagnostic(), error.diagnostic());
    }
    Ok(())
}

#[test]
fn graph_reason_wire_preserves_text() -> Result<(), Box<dyn std::error::Error>> {
    let reason = "node review: parser rejected value [fixture]";
    let error = FacadeError::invalid(super::InvalidInputDetail::graph_reason(reason));
    let wire = serde_json::to_value(&error)?;
    assert_eq!(wire["detail"], reason);
    assert_eq!(wire["detail_fact"]["value"], reason);
    assert_eq!(error.diagnostic().detail.as_deref(), Some(reason));
    Ok(())
}

#[test]
fn writer_busy_preserves_holder_and_reads_prior_frames() -> Result<(), Box<dyn std::error::Error>> {
    let old: FacadeError = serde_json::from_value(serde_json::json!({"code":"writer_busy"}))?;
    assert_eq!(old, FacadeError::WriterBusy { holder: None });
    let holder = provenant_core::AttemptId::from_bytes([23; 16]);
    let current = FacadeError::WriterBusy {
        holder: Some(holder),
    };
    assert!(current.to_string().contains(&holder.to_string()));
    assert!(
        current
            .diagnostic()
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&holder.to_string()))
    );
    assert_eq!(
        serde_json::from_value::<FacadeError>(serde_json::to_value(&current)?)?,
        current
    );
    Ok(())
}

#[test]
fn writer_lock_busy_names_the_holder_in_its_diagnostic() -> Result<(), Box<dyn std::error::Error>> {
    let old: FacadeError = serde_json::from_value(serde_json::json!({"code":"writer_lock_busy"}))?;
    assert_eq!(old, FacadeError::WriterLockBusy { holder: None });
    assert!(
        old.diagnostic()
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("holder unknown"))
    );
    let holder = provenant_core::AttemptId::from_bytes([24; 16]);
    let current = FacadeError::WriterLockBusy {
        holder: Some(holder),
    };
    assert!(current.to_string().contains(&holder.to_string()));
    assert!(
        current
            .diagnostic()
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&holder.to_string()))
    );
    assert_eq!(current.code(), "writer_lock_busy");
    assert_eq!(
        serde_json::from_value::<FacadeError>(serde_json::to_value(&current)?)?,
        current
    );
    Ok(())
}

#[test]
fn corrupt_store_diagnostic_exposes_safe_decode_kind() {
    let error = provenant_fabric_store::Error::Decode(provenant_records::DecodeError::Typed {
        line: 2,
        column: 3,
        field: None,
        schema_version: None,
    });
    let facade = FacadeError::from(&error);
    assert_eq!(facade.code(), "state_corrupt");
    assert!(
        facade
            .to_string()
            .contains("fabric store record: decode error")
    );
    let wire = serde_json::to_value(&facade).unwrap();
    assert!(wire["detail"].as_str().unwrap().contains("record type"));
}

#[test]
fn future_schema_requires_upgrade_with_version_diagnostics() {
    use provenant_fabric_store::Error;
    let errors = [
        Error::SchemaVersion(99),
        Error::Decode(provenant_records::DecodeError::UnsupportedVersion {
            found: 2,
            line: 1,
            column: 1,
        }),
        Error::Decode(
            provenant_records::DecodeError::UnsupportedVersion {
                found: 3,
                line: 1,
                column: 1,
            }
            .with_context("/state/attempts/example/launch.json", "LaunchRecord"),
        ),
    ];
    for error in errors {
        let facade = FacadeError::from(&error);
        assert_eq!(facade.code(), "upgrade_required");
        let message = facade.to_string();
        assert!(message.contains(env!("CARGO_PKG_VERSION")));
        assert!(message.contains("writer release unknown"));
    }
    assert_eq!(
        FacadeError::from(&Error::SchemaVersion(1)),
        FacadeError::StateCorruptDetail {
            detail: "fabric store: unsupported database schema version 1".into()
        }
    );
}

#[test]
fn directory_permissions_projection_respects_destination() {
    use provenant_records::diagnostic::Destination;
    let path = std::path::Path::new("/private/a b");
    let local = FacadeError::state_directory_permissions(path, 0o755, Destination::Local);
    assert!(local.next.unwrap().contains("chmod 700 '/private/a b'"));
    let remote = FacadeError::state_directory_permissions(path, 0o755, Destination::Remote);
    let wire = serde_json::to_string(&remote).unwrap();
    assert!(wire.contains("/private/a b"));
    assert!(remote.next.is_some());
}

// Spec: docs/specs/state.md#config-never-widens-authority
#[test]
fn unsafe_project_binding_projection_is_typed_and_destination_safe() {
    use std::path::PathBuf;

    let error = provenant_config::ConfigError::ProjectBindingDirectoryUnsafe {
        path: PathBuf::from("/private/state with 'quote"),
        mode: 0o755,
        owner_matches: true,
    };
    let local = FacadeError::project_binding_directory_diagnostic(
        &error,
        provenant_records::diagnostic::Destination::Local,
    )
    .unwrap();
    assert!(local.detail.as_deref().is_some_and(|detail| {
        detail.contains("project binding state directory /private/state with 'quote")
            && detail.contains("mode 0755")
    }));
    assert!(
        local
            .next
            .as_deref()
            .is_some_and(|next| { next.contains("chmod 700 '/private/state with '\\''quote'") })
    );

    let remote = FacadeError::project_binding_directory_diagnostic(
        &error,
        provenant_records::diagnostic::Destination::Remote,
    )
    .unwrap();
    let remote_wire = serde_json::to_string(&remote).unwrap();
    assert!(remote_wire.contains("/private/state with"));
    assert!(
        remote
            .next
            .as_deref()
            .is_some_and(|next| next.contains("restrict"))
    );

    let unrelated = provenant_config::ConfigError::InvalidProjectBinding;
    assert!(
        FacadeError::project_binding_directory_diagnostic(
            &unrelated,
            provenant_records::diagnostic::Destination::Local,
        )
        .is_none()
    );
}

#[test]
fn generic_io_has_safe_action_without_source_detail() {
    let error = FacadeError::Io {
        operation: "TOKEN_SENTINEL\u{1b}".into(),
        path: Some("/private/host".into()),
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
        cause: None,
    };
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.code, "io");
    assert!(diagnostic.detail.is_none());
    assert!(diagnostic.next.is_some());
    let wire = serde_json::to_string(&diagnostic).unwrap();
    assert!(!wire.contains("TOKEN_SENTINEL"));
    assert!(!wire.contains("/private/host"));
}

#[test]
fn generic_io_discloses_its_fixed_cause_only_in_process() {
    let cause = "verified repository path or Git metadata changed";
    let error = FacadeError::io_failure("locate writer project", None, Some(&cause));
    let expected = format!("locate writer project: {cause}");
    assert_eq!(error.cause(), Some(expected.as_str()));
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.code, "io");
    assert_eq!(diagnostic.detail.as_deref(), Some(expected.as_str()));
    let wire = serde_json::to_value(&error).unwrap();
    assert!(!wire.to_string().contains(cause), "{wire}");
    let received: FacadeError = serde_json::from_value(wire).unwrap();
    assert_eq!(received.cause(), None);
    assert!(received.diagnostic().detail.is_none());
}

#[test]
fn protocol_projection_omits_untrusted_peer_name() {
    let error = FacadeError::PeerProtocolMismatch {
        peer: "/private/TOKEN_SENTINEL\u{1b}".into(),
        remote: 3,
        expected: 6,
    };
    let projected = error.diagnostic();
    let wire = serde_json::to_string(&projected).unwrap();
    assert!(!wire.contains("TOKEN_SENTINEL"));
    assert!(!wire.contains("/private"));
    let transport = serde_json::to_string(&error).unwrap();
    assert!(!transport.contains("TOKEN_SENTINEL"));
    assert!(!transport.contains("/private"));
    let host = provenant_core::HostId::from_bytes([7; 16]).to_string();
    let safe = FacadeError::PeerProtocolMismatch {
        peer: host.clone(),
        remote: 3,
        expected: 6,
    };
    assert_eq!(serde_json::to_value(safe).unwrap()["peer"], host);
    assert!(projected.detail.unwrap().contains('3'));
    assert!(projected.next.is_some());
}

#[test]
fn legacy_mismatch_frame_without_level_is_rejected() {
    let result = serde_json::from_value::<FacadeError>(serde_json::json!({
        "code": "peer_protocol_mismatch", "peer": "w", "remote": 1
    }));
    assert_eq!(
        result.unwrap_err().classify(),
        serde_json::error::Category::Data
    );
    let current = FacadeError::PeerProtocolMismatch {
        peer: "w".into(),
        remote: 1,
        expected: 2,
    };
    assert_eq!(current.code(), "peer_version");
}

#[test]
fn resume_preserves_no_fallback_and_target() -> Result<(), Box<dyn std::error::Error>> {
    let input = serde_json::json!({
        "op": "resume",
        "target": { "attempt": "00000000000000000000000000000001" },
        "operation_id": "resume-fixture",
        "prompt": "Continue",
        "expected_generation": 0,
        "no_fallback": true
    });
    let mut legacy = input.clone();
    legacy.as_object_mut().unwrap().remove("no_fallback");
    assert!(matches!(
        serde_json::from_value::<super::RunInput>(legacy)?,
        super::RunInput::Resume {
            no_fallback: false,
            ..
        }
    ));
    let explicit: super::RunInput = serde_json::from_value(input)?;
    assert!(matches!(
        explicit,
        super::RunInput::Resume {
            no_fallback: true,
            ..
        }
    ));
    let wire = serde_json::to_value(explicit)?;
    assert_eq!(wire["no_fallback"], true);
    assert_eq!(
        wire["target"]["attempt"],
        "00000000000000000000000000000001"
    );
    Ok(())
}

#[test]
fn filesystem_diagnostic_omits_paths_with_unknown_anchors() {
    let error = provenant_os::fs::FsError::Io {
        operation: "openat read",
        path: "HEAD".into(),
        source: std::io::ErrorKind::NotFound.into(),
    };
    // The unanchored relative path is never disclosed; the operation and
    // source still reach the local detail through the one Io carrier.
    assert_eq!(
        FacadeError::filesystem(&error),
        FacadeError::Io {
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
            cause: Some("openat read: entity not found".into()),
            operation: "openat read".into(),
            path: None,
        }
    );
    let wire = serde_json::to_string(&FacadeError::filesystem(&error)).unwrap();
    assert!(!wire.contains("HEAD"), "{wire}");
}

#[test]
fn typed_delivery_stage_survives_io_wire() {
    let stage = provenant_records::run::WriterDeliveryStage::Prepare;
    let wire = serde_json::to_value(FacadeError::HostDeliveryStage { stage }).unwrap();
    assert_eq!(wire["code"], "io");
    let received: FacadeError = serde_json::from_value(wire).unwrap();
    assert!(received.diagnostic().detail.unwrap().contains("Prepare"));
}

#[test]
fn typed_delivery_limit_survives_peer_io_envelope_without_trusting_operation_text() {
    use provenant_records::{diagnostic::DeliveryLimit, run::WriterDeliveryStage};
    let fact =
        DeliveryLimit::new(WriterDeliveryStage::Prepare, "git", "worktree list", 4096).unwrap();
    let source = FacadeError::HostDeliveryLimit { fact: fact.clone() };
    let mut wire = serde_json::to_value(source).unwrap();
    assert_eq!(wire["code"], "io");
    wire["operation"] = "TOKEN_SENTINEL".into();
    let received: FacadeError = serde_json::from_value(wire).unwrap();
    let projected = received.diagnostic();
    assert_eq!(projected.code, "io");
    assert_eq!(projected.detail.as_deref(), Some(fact.detail().as_str()));
    assert!(
        !serde_json::to_string(&projected)
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );
}

#[test]
fn parameter_refusal_retains_shape_only_identifiers_until_projection() {
    use crate::graph::template::{ParameterFault, ParameterRefusal};
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    for fault in [
        ParameterFault::NotTable,
        ParameterFault::Unknown {
            key: Some(secret.into()),
            accepted: vec!["brief".into(), secret.into()],
            suggestion: Some(secret.into()),
        },
        ParameterFault::Missing {
            key: secret.into(),
            keys: vec![secret.into()],
            example: None,
        },
        ParameterFault::Shape {
            key: secret.into(),
            expected: secret.into(),
        },
    ] {
        let error = FacadeError::invalid(super::InvalidInputDetail::Parameter(Box::new(
            ParameterRefusal {
                node: Some(secret.into()),
                template: secret.into(),
                fault,
            },
        )));
        let diagnostic = error.diagnostic();
        assert!(error.to_string().contains(secret));
        assert!(serde_json::to_string(&diagnostic).unwrap().contains(secret));
        let wire = serde_json::to_string(&error).unwrap();
        assert!(wire.contains(secret), "{wire}");
        let restored: FacadeError = serde_json::from_str(&wire).unwrap();
        assert_eq!(restored.diagnostic(), diagnostic);
    }
}

#[test]
fn dispatch_refusal_retains_shape_only_reason_for_destination_projection() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let error = FacadeError::DispatchRefused {
        fact: None,
        disk: None,
        reason: format!("worktree setup failed: missing pnpm; token {secret}\u{1b}"),
    };
    let projected = error.diagnostic();
    let wire = serde_json::to_string(&error).unwrap();
    let received: FacadeError = serde_json::from_str(&wire).unwrap();
    assert_eq!(received.diagnostic(), projected);
    let detail = projected.detail.unwrap();
    assert!(detail.contains("worktree setup failed: missing pnpm"));
    assert!(detail.contains(secret));
    assert!(wire.contains(secret));
    assert!(!detail.chars().any(char::is_control));
    assert!(!projected.next.unwrap().contains("receipt"));
}

#[test]
fn graph_plan_refusal_retains_token_shaped_node_identifier_as_metadata() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let error = FacadeError::DispatchRefused {
        fact: Some(super::DispatchRefusalFact::GraphPlan),
        disk: None,
        reason: format!("node {secret}: writer worktree is required"),
    };
    assert!(
        serde_json::to_string(&error.diagnostic())
            .unwrap()
            .contains(secret)
    );
    let wire = serde_json::to_string(&error).unwrap();
    assert!(wire.contains(secret));
    let received: FacadeError = serde_json::from_str(&wire).unwrap();
    let detail = received.diagnostic().detail.unwrap();
    assert!(detail.contains(secret));
    assert!(detail.contains("writer worktree"));
}

// Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
#[test]
fn graph_plan_admission_names_the_node_and_validator_reason() {
    let reason =
        "node read: route alias must be an identifier; exact models require an adapter or --model";
    let error = FacadeError::graph_admission(crate::graph::AdmissionError::Plan(reason.into()));
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.code, "dispatch_refused");
    assert_eq!(diagnostic.detail.as_deref(), Some(reason));
    assert_eq!(
        diagnostic.next.as_deref(),
        Some("fix the named graph node and validator reason, then retry")
    );
    let fact = match &error {
        FacadeError::DispatchRefused {
            fact: Some(fact), ..
        } => fact,
        other => panic!("{other:?}"),
    };
    assert_eq!(fact, &super::DispatchRefusalFact::GraphPlan);
    let wire = serde_json::to_value(&error).unwrap();
    assert_eq!(wire["reason"], serde_json::json!(reason));
}

// Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
#[test]
fn graph_admission_keeps_composite_schema_and_output_kind_reasons() {
    use crate::graph::{AdmissionError, GraphOutputField};
    for (error, reason) in [
        (
            AdmissionError::Composite("node a: loop.max_rounds must be positive".into()),
            "node a: loop.max_rounds must be positive",
        ),
        (
            AdmissionError::Schema("schema version".into()),
            "schema version",
        ),
        (
            AdmissionError::OutputKind {
                field: GraphOutputField::Input,
                reason: "node b: inputs[].outputs selects verdict, which review does not supply"
                    .into(),
            },
            "node b: inputs[].outputs selects verdict, which review does not supply",
        ),
    ] {
        let refusal = FacadeError::graph_admission(error);
        let diagnostic = refusal.diagnostic();
        assert_eq!(diagnostic.code, "dispatch_refused");
        assert_eq!(diagnostic.detail.as_deref(), Some(reason));
        let wire = serde_json::to_value(&refusal).unwrap();
        assert_eq!(wire["reason"], serde_json::json!(reason));
        assert!(!wire["fact"].is_null(), "{wire}");
    }
}

#[test]
fn detached_writer_refusal_keeps_its_typed_cause_across_transport() {
    let error = FacadeError::DispatchRefused {
        reason: "writer needs a branch".into(),
        disk: None,
        fact: Some(super::DispatchRefusalFact::DetachedWriter),
    };
    let wire = serde_json::to_string(&error).unwrap();
    let received: FacadeError = serde_json::from_str(&wire).unwrap();
    let projected = received.diagnostic();
    assert_eq!(projected.code, "dispatch_refused");
    assert_eq!(projected.detail.as_deref(), Some("writer needs a branch"));
    assert!(
        projected
            .next
            .as_deref()
            .is_some_and(|next| next.contains("branch"))
    );
}

#[test]
fn disk_refusal_retains_numeric_facts_without_source_or_host_path() {
    let error = FacadeError::DispatchRefused {
        fact: None,
        reason: "TOKEN_SENTINEL /private/host".into(),
        disk: Some(super::DiskPressure {
            available: 42,
            floor: 100,
            reclaimed: 3,
            origin: provenant_records::diagnostic::Layer::Host,
        }),
    };
    let wire = serde_json::to_string(&error).unwrap();
    assert!(!wire.contains("TOKEN_SENTINEL"));
    assert!(!wire.contains("/private/host"));
    let received: FacadeError = serde_json::from_str(&wire).unwrap();
    let projection = received.diagnostic();
    assert_eq!(projection.code, "dispatch_refused");
    assert!(
        projection
            .detail
            .as_deref()
            .unwrap()
            .contains("42 bytes available, floor 100")
    );
    assert!(
        projection
            .detail
            .unwrap()
            .contains("gc.free_floor from host configuration")
    );
    assert!(projection.next.unwrap().contains("100B"));
}

#[test]
fn memory_pressure_refusal_preserves_typed_source_over_transport() {
    use provenant_records::diagnostic::{Layer, PressureLevel, PressureThreshold, ResourceFact};
    let error = FacadeError::ConfinementRefused {
        resource: Some(ResourceFact::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: Layer::Host,
        }),
    };
    let received: FacadeError =
        serde_json::from_value(serde_json::to_value(error).unwrap()).unwrap();
    let diagnostic = received.diagnostic();
    assert_eq!(diagnostic.code, "confinement_refused");
    let detail = diagnostic.detail.unwrap();
    assert!(detail.contains("critical"));
    assert!(detail.contains("warning"));
    assert!(detail.contains("host configuration"));
    assert!(diagnostic.next.unwrap().contains("memory"));
    let generic = FacadeError::ConfinementRefused { resource: None }.diagnostic();
    assert!(generic.detail.is_none());
}

#[test]
fn confinement_rule_refusal_preserves_rule_and_path_over_transport() {
    use provenant_records::diagnostic::{ConfinementRule, ResourceFact};
    let error = FacadeError::ConfinementRefused {
        resource: Some(ResourceFact::ConfinementRule(Box::new(ConfinementRule {
            rule: "peer_project_unprojectable".into(),
            path: "/fixture/scratch/project/.git".into(),
        }))),
    };
    let received: FacadeError =
        serde_json::from_value(serde_json::to_value(error).unwrap()).unwrap();
    let diagnostic = received.diagnostic();
    assert_eq!(diagnostic.code, "confinement_refused");
    let detail = diagnostic.detail.unwrap();
    assert!(detail.contains("peer_project_unprojectable"), "{detail}");
    assert!(detail.contains("/fixture/scratch/project/.git"), "{detail}");
    assert!(
        diagnostic
            .next
            .unwrap()
            .contains("named confinement rule and path")
    );
}

#[test]
fn a_named_field_refusal_discloses_the_field_it_names() {
    for field in [
        "action",
        "after",
        "commit",
        "context.node_id",
        "document",
        "graph_context",
        "host",
        "integration_branch",
        "key",
        "knobs",
        "limit",
        "max_nodes",
        "node",
        "part",
        "source.commit",
        "targets",
        "wait_seconds",
        "worktree",
    ] {
        let refusal = FacadeError::field(field);
        assert!(
            refusal
                .to_string()
                .contains(&format!("invalid input field `{field}`")),
            "{field}: {refusal}"
        );
        let received: FacadeError =
            serde_json::from_value(serde_json::to_value(&refusal).unwrap()).unwrap();
        assert_eq!(received, refusal, "{field} survives the wire");
    }
}

#[test]
fn worktree_missing_names_path_and_node_on_message_diagnostic_and_wire() {
    let path = "/repo/.worktrees/graph-task--slices--one";
    let error = FacadeError::WorktreeMissing {
        path: Some(path.into()),
        node: None,
    }
    .with_node("task--slices--one");
    assert!(error.to_string().contains(path), "{error}");
    assert!(error.to_string().contains("task--slices--one"), "{error}");
    let diagnostic = error.diagnostic();
    let detail = diagnostic.detail.clone().unwrap_or_default();
    assert!(detail.contains(path), "{detail}");
    assert!(detail.contains("task--slices--one"), "{detail}");
    let wire = serde_json::to_value(&error).unwrap();
    assert_eq!(wire["code"], "worktree_missing");
    assert_eq!(wire["path"], path);
    assert_eq!(wire["node"], "task--slices--one");
    let received: FacadeError = serde_json::from_value(wire).unwrap();
    assert_eq!(received, error);
}

#[test]
fn invalid_input_preserves_typed_fields() {
    let bare = FacadeError::InvalidInput { detail: None };
    assert_eq!(bare.to_string(), "invalid input");
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        serde_json::json!({"code": "invalid_input"})
    );
    let detailed = FacadeError::invalid(super::InvalidInputDetail::Schema);
    assert_eq!(detailed.code(), "invalid_input");
    assert_eq!(
        detailed.to_string(),
        "invalid input: graph document schema_version is invalid"
    );
    let wire = serde_json::to_value(&detailed).unwrap();
    assert_eq!(wire["detail"], "graph document schema_version is invalid");
    assert_eq!(wire["detail_fact"]["kind"], "schema");
    assert!(!wire.to_string().contains("31m"));
    let oversized = FacadeError::invalid(super::InvalidInputDetail::GraphDocumentTooLarge);
    let received: FacadeError =
        serde_json::from_value(serde_json::to_value(&oversized).unwrap()).unwrap();
    assert_eq!(received, oversized);
    assert_eq!(received.code(), "invalid_input");
    assert!(received.diagnostic().next.is_some());
    let untrusted: FacadeError = serde_json::from_value(serde_json::json!({
        "code": "invalid_input", "detail": "schema version TOKEN_SENTINEL\u{1b}"
    }))
    .unwrap();
    assert!(
        !serde_json::to_string(&untrusted)
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );
    assert!(!untrusted.to_string().contains("TOKEN_SENTINEL"));
    let forged: FacadeError = serde_json::from_value(serde_json::json!({
        "code": "invalid_input", "detail": "safe-looking text",
        "detail_fact": {"kind": "field", "value": "TOKEN_SENTINEL"}
    }))
    .unwrap();
    assert_eq!(
        forged.diagnostic().detail.as_deref(),
        Some("invalid input field `TOKEN_SENTINEL`")
    );
    for input in [
        format!("schema version TOKEN_SENTINEL\u{1b}{}", "é".repeat(2048)),
        "invalid input field `scope.paths` TOKEN_SENTINEL".to_owned(),
    ] {
        let received: FacadeError = serde_json::from_value(serde_json::json!({
            "code": "invalid_input", "detail": input
        }))
        .unwrap();
        let diagnostic = received.diagnostic();
        let wire = serde_json::to_string(&diagnostic).unwrap();
        assert!(!wire.contains("TOKEN_SENTINEL"));
        assert!(wire.len() < 4096);
        assert!(diagnostic.next.is_some());
    }
    let fake_field = FacadeError::InvalidInput {
        detail: Some(super::InvalidInputDetail::Field("secret_token".into())),
    };
    assert!(
        serde_json::to_string(&fake_field)
            .unwrap()
            .contains("secret_token")
    );
    let from_field = FacadeError::from(&super::super::CoordinationError::Input(
        super::super::input::InputError::new("scope.paths"),
    ));
    assert!(
        from_field.to_string().contains("scope.paths"),
        "{from_field}"
    );
    let received: FacadeError =
        serde_json::from_value(serde_json::to_value(&from_field).unwrap()).unwrap();
    assert_eq!(received.diagnostic(), from_field.diagnostic());
}

#[test]
fn io_operation_display_escapes_controls() {
    let error = FacadeError::io("read\n\u{1b}[31m\"peer");
    let display = error.to_string();
    assert!(!display.contains('\n'), "{display:?}");
    assert!(!display.contains('\u{1b}'), "{display:?}");
    assert!(display.contains("\\n"), "{display:?}");
    assert!(display.contains("\\\"peer"), "{display:?}");
}
