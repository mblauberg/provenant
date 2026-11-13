use super::*;

#[test]
fn every_alternative_validates_its_own_effort() -> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.models.push(ModelEntry {
        free: false,
        model: ModelId::parse("codex/test")?,
        efforts: vec!["high".into()],
        evidence_ref: "test".into(),
        family: None,
        trains_on_prompts: None,
        monthly_allowance_usd: None,
        quota_pool: None,
    });
    catalogue.models.push(ModelEntry {
        free: false,
        model: ModelId::parse("codex/no-effort")?,
        efforts: vec![],
        evidence_ref: "test".into(),
        family: None,
        trains_on_prompts: None,
        monthly_allowance_usd: None,
        quota_pool: None,
    });
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let mut query = request("codex/test")?;
    query.effort = Some("high".into());
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse(
        "codex/no-effort",
    )?])?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnsupportedEffort { .. })
    ));
    Ok(())
}

#[test]
fn stale_availability_is_selectable_but_current_account_wins()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let stale = AvailabilityRecord {
        account_key: Some("stale".into()),
        expires_unix_seconds: 90,
        ..availability()
    };
    let current = AvailabilityRecord {
        account_key: Some("current".into()),
        ..availability()
    };
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let records = [current, stale.clone()];
    let mut context = RoutingContext {
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let selected = resolve(&request("codex/gpt-6-luna")?, &context)?;
    assert_eq!(selected.account_key.as_deref(), Some("current"));
    let only_stale = [stale];
    context.availability = &only_stale;
    let selected = resolve(&request("codex/gpt-6-luna")?, &context)?;
    assert_eq!(selected.account_key.as_deref(), Some("stale"));
    assert!(
        matches!(selected.readiness, RouteReadiness::Ready { extra, .. } if extra.contains_key("availability_warning"))
    );
    let records = ["current", "stale"].map(|key| AvailabilityRecord {
        account_key: Some(key.into()),
        ..availability()
    });
    let readings = [
        quota(
            "codex/gpt-6-luna",
            "current",
            provenant_quota::QuotaStatus::Available,
            1,
            50,
            120,
        )?,
        quota(
            "codex/gpt-6-luna",
            "stale",
            provenant_quota::QuotaStatus::Available,
            1,
            50,
            90,
        )?,
    ];
    let lookup = readiness_over(vec![]);
    let context = RoutingContext {
        availability: &records,
        quota: &readings,
        readiness: Some(&lookup),
        access: &access,
        ..context
    };
    assert_eq!(
        resolve(&request("codex/gpt-6-luna")?, &context)?
            .account_key
            .as_deref(),
        Some("current")
    );
    Ok(())
}

#[test]
fn all_exhausted_accounts_have_individual_remedies() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = ["one", "two"].map(|key| AvailabilityRecord {
        account_key: Some(key.into()),
        ..availability()
    });
    let readings = ["one", "two"].map(|key| {
        let mut reading =
            quota("codex/gpt-6-luna", key, QuotaStatus::Exhausted, 1, 50, 120).unwrap();
        reading.snapshot.windows = vec![QuotaWindow {
            pool: "secondary".into(),
            window_seconds: Some(604_800),
            used_basis_points: 10_000,
            resets_unix_seconds: Some(10_000),
        }];
        reading
    });
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let context = RoutingContext {
        availability: &records,
        quota: &readings,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let selected = resolve(&request("codex/gpt-6-luna")?, &context)?;
    let details = selected.extra["account_exclusions"].to_string();
    assert!(
        details.contains("one")
            && details.contains("two")
            && details.contains("exhausted")
            && details.contains("reset")
    );
    assert!(!details.contains("UnknownAvailability"));
    Ok(())
}

#[test]
fn resolves_an_available_provider_supplied_only_by_the_registry()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::{
        AccountDetection, Adapter, Continuation, ExitObservation, LaunchSpec, PrepareInput,
        ProviderError, ProviderEvent, ProviderOutcome, RouteProvenance, StreamState,
        VerifiedLineage,
    };
    struct Fixture;
    impl Adapter for Fixture {
        fn id(&self) -> &'static str {
            "fixture-provider"
        }
        fn account(&self, _: &StreamState) -> Result<AccountDetection, ProviderError> {
            Ok(AccountDetection::Unknown)
        }
        fn prepare(&self, _: PrepareInput<'_>) -> Result<LaunchSpec, ProviderError> {
            Err(ProviderError::UnsupportedPolicy)
        }
        fn stream(&self, _: Option<&Continuation>) -> Result<StreamState, ProviderError> {
            Err(ProviderError::UnsupportedPolicy)
        }
        fn observe(
            &self,
            _: &mut StreamState,
            _: &[u8],
        ) -> Result<Vec<ProviderEvent>, ProviderError> {
            Err(ProviderError::UnsupportedPolicy)
        }
        fn finish(
            &self,
            _: StreamState,
            _: ExitObservation,
        ) -> Result<ProviderOutcome, ProviderError> {
            Err(ProviderError::UnsupportedPolicy)
        }
        fn continuation(
            &self,
            _: &ProviderOutcome,
            _: &provenant_records::ResolvedRoute,
            _: &str,
        ) -> Result<Continuation, ProviderError> {
            Err(ProviderError::UnsupportedPolicy)
        }
        fn validate_provenance(
            &self,
            _: &RouteProvenance<'_>,
        ) -> Result<VerifiedLineage, ProviderError> {
            Err(ProviderError::UnsupportedPolicy)
        }
    }
    static ADAPTER: Fixture = Fixture;
    let registry = [provenant_providers::Registration {
        adapter: &ADAPTER,
        quota_source: None,
        ..provenant_providers::codex::REGISTRATION
    }];
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [AvailabilityRecord {
        adapter: "fixture-provider".into(),
        executable: "/fixture/provider".into(),
        ..availability()
    }];
    let access = [AccessContext {
        adapter: "fixture-provider",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let context = RoutingContext {
        registry: &registry,
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let resolved = resolve(&request("fixture-provider/test")?, &context)?;
    assert_eq!(
        probe_adapters(
            &request("fixture-provider/test")?,
            &catalogue,
            &config,
            &registry
        )?,
        ["fixture-provider"]
    );
    assert_eq!(resolved.adapter, "fixture-provider");
    assert_eq!(resolved.account_key.as_deref(), Some("opaque-account"));
    assert!(matches!(resolved.readiness, RouteReadiness::Ready { .. }));
    Ok(())
}

#[test]
fn availability_gates_disclosure_and_rejects_duplicates() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [availability()];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    // A disclosure denial needs an explicit policy refusal, not absent access.
    let denied = ["codex", "cursor"].map(|adapter| AccessContext {
        adapter,
        disclosure_ref: "",
        confinement_ref: "grant",
    });
    context.access = &denied;
    // Enabled and available, Codex reaches the next gate: disclosure.
    assert!(matches!(
        resolve(&request("codex/test")?, &context)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::DisclosureDenied,
            ..
        }
    ));
    // Cursor also reaches the disclosure gate with a fresh availability record.
    let mut cursor = availability();
    cursor.adapter = "cursor".into();
    let records = [cursor];
    context.availability = &records;
    assert!(matches!(
        resolve(&request("cursor/test")?, &context)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::DisclosureDenied,
            ..
        }
    ));
    let duplicate = [availability(), availability()];
    context.availability = &duplicate;
    assert!(matches!(
        list(&RouteListQuery::default(), &context),
        Err(RoutingError::Availability)
    ));
    let mut invalid = availability();
    invalid.account_key = Some("email@example.com".into());
    assert!(validate_availability(&invalid, "codex").is_err());
    invalid = availability();
    invalid.expires_unix_seconds = invalid.observed_unix_seconds;
    assert!(validate_availability(&invalid, "codex").is_err());
    Ok(())
}

#[test]
fn configured_domain_is_closed_and_registered_without_config_dependency_cycles()
-> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Domains {
        routing: RoutingConfig,
    }
    static ENTRIES: [provenant_config::DomainSection; 1] = [SECTION];
    let sections = DomainSections::<Domains>::new(&ENTRIES);
    // Config's loader owns root validation; tests inject an isolated, non-I/O reader.
    struct Resolver;
    impl provenant_core::PathResolver for Resolver {
        fn resolve_existing_ancestor(
            &self,
            path: &std::path::Path,
        ) -> std::io::Result<std::path::PathBuf> {
            Ok(path.into())
        }
    }
    let mut inputs =
        provenant_core::RootInputs::new("/fixture/home".into(), "/fixture/assets".into());
    inputs.state_dir = Some("/fixture/state".into());
    let roots = inputs.resolve(&Resolver)?;
    let model_policy_config = provenant_config::load_with_domains(
        &roots,
        Some("schema_version=1\n[domains.routing.model_policy.defaults.cache]\nread_share_target=0.95\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\n[domains.routing.model_policy.rules.schedule]\nexpires='1970-01-01T00:03:20Z'\n[domains.routing.model_policy.rules.policy.lane]\nmax_turns=40\n".as_bytes()),
        &sections,
    )?;
    assert_eq!(
        model_policy_config
            .domains
            .routing
            .model_policy
            .defaults
            .cache
            .read_share_target,
        Some(0.95)
    );
    let cfg = provenant_config::load_with_domains(
        &roots,
        Some(
            "schema_version=1\n[domains.routing.routes.review]\nmodel='codex/gpt-6.1-sol'\n"
                .as_bytes(),
        ),
        &sections,
    )?;
    assert_eq!(
        cfg.domains
            .routing
            .routes
            .get("review")
            .map(|r| r.model.model()),
        Some("gpt-6.1-sol")
    );
    let layered = provenant_config::load_with_domains(
        &roots,
        Some("schema_version=1\n[domains.routing.review.shapes.quick]\nreviewers=[{pool='senior',count=1}]\n[domains.routing.review.select.by_risk]\nmedium='deep'\n".as_bytes()),
        &sections,
    )?;
    assert_eq!(
        layered.domains.routing.review.shapes["quick"].reviewers[0].pool,
        "senior"
    );
    assert!(!layered.domains.routing.review.shapes["quick"].frame);
    assert_eq!(layered.domains.routing.review.shapes["quick"].verifiers, 0);
    assert_eq!(
        layered.domains.routing.review.shapes["quick"].synthesis,
        crate::ReviewSynthesis::Reduce
    );
    assert_eq!(
        layered.domains.routing.review.shapes["standard"].reviewers[0].count,
        5
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["routine"],
        "quick"
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["medium"],
        "deep"
    );
    let customised = provenant_config::load_with_domains(
        &roots,
        Some(
            "schema_version=1\n[domains.routing.review.shapes.quick]\nsynthesis='model'\n[domains.routing.review.floors.security]\ntier='substantial'\n"
                .as_bytes(),
        ),
        &sections,
    )?;
    let schema = provenant_config::schema(&customised, &sections, String::new())?;
    let synthesis = schema
        .fields
        .iter()
        .find(|field| field.path == "domains.routing.review.shapes.quick.synthesis")
        .unwrap();
    assert_eq!(synthesis.default, "reduce");
    assert_eq!(synthesis.value, "model");
    assert_eq!(synthesis.source, provenant_config::Layer::User);
    assert_eq!(synthesis.values, ["model", "reduce"]);
    let floor = schema
        .fields
        .iter()
        .find(|field| field.path == "domains.routing.review.floors.security.tier")
        .unwrap();
    assert_eq!(floor.default, "crucial");
    assert_eq!(floor.value, "substantial");
    assert_eq!(floor.source, provenant_config::Layer::User);
    let layered = provenant_config::load_with_domains(
        &roots,
        Some("schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers=[{pool='senior',count=1}]\n[domains.routing.review.select]\nby_risk={medium='custom'}\n".as_bytes()),
        &sections,
    )?;
    assert!(layered.domains.routing.review.shapes.contains_key("custom"));
    assert!(layered.domains.routing.review.shapes["custom"].frame);
    assert_eq!(layered.domains.routing.review.shapes["custom"].verifiers, 1);
    assert_eq!(
        layered.domains.routing.review.shapes["custom"].synthesis,
        crate::ReviewSynthesis::Model
    );
    assert!(layered.domains.routing.review.shapes.contains_key("quick"));
    assert_eq!(
        layered.domains.routing.review.select.by_risk["routine"],
        "quick"
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["medium"],
        "custom"
    );
    // Review shapes are tuning, not authority: invalid input recovers to the
    // bundled shapes with a warning naming the offending key.
    for (invalid, key) in [
        (
            "schema_version=1\n[domains.routing.review.select]\ntypo=true\n",
            "domains.routing.review.select.typo",
        ),
        (
            "schema_version=1\n[domains.routing.review.shapes.quick]\ntypo=true\n",
            "domains.routing.review.shapes.quick.typo",
        ),
        (
            "schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers=[{pool='junior',count=1}]\ntypo=true\n",
            "domains.routing.review.shapes.custom.typo",
        ),
        (
            "schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers='wrong'\n",
            "domains.routing.review.shapes.custom",
        ),
    ] {
        let config =
            provenant_config::load_with_domains(&roots, Some(invalid.as_bytes()), &sections)?;
        assert!(
            config
                .warnings
                .iter()
                .any(|warning| warning.message.starts_with(key)),
            "{invalid}: {:?}",
            config.warnings
        );
        assert_eq!(
            config.domains.routing.review.select.by_risk["routine"],
            "quick"
        );
        assert_eq!(
            config.domains.routing.review.shapes["quick"].reviewers[0].pool,
            RoutingConfig::default().review.shapes["quick"].reviewers[0].pool
        );
    }

    // An enforced restriction survives an invalid sibling, and the warning
    // names the sibling, not its parent.
    let write = crate::RouteIntent {
        workload: crate::Workload::General,
        writes: true,
        ..Default::default()
    };
    let model = crate::ModelId::parse("codex/gpt-6.1-terra")?;
    for text in [
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nread_only=true\n[domains.routing.model_policy.defaults.cache]\nread_heavy_weight=11\n",
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\n[domains.routing.model_policy.rules.policy.lane]\nread_only=true\n[domains.routing.model_policy.rules.policy.cache]\nread_heavy_weight=11\n",
    ] {
        let config = provenant_config::load_with_domains(&roots, Some(text.as_bytes()), &sections)?;
        let policy = config
            .domains
            .routing
            .model_policy
            .effective(
                &model,
                None,
                provenant_core::WallTime::from_unix_seconds(0).expect("valid policy instant"),
            )
            .policy;
        assert_eq!(policy.lane.read_only, Some(true), "{text}");
        assert_ne!(policy.cache.read_heavy_weight, Some(11), "{text}");
        assert!(!policy.permits(write), "{text}");
        assert_eq!(config.warnings.len(), 1, "{text}");
        assert!(
            config.warnings[0]
                .message
                .contains(".cache.read_heavy_weight:"),
            "{}",
            config.warnings[0].message
        );
    }
    // Input whose recovery would lift an enforced restriction refuses.
    for text in [
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nmax_context=0\n",
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nmax_turns=0\n",
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nread_only='yes'\n",
        "schema_version=1\n[domains.routing.model_policy.defaults]\nlane='read_only'\n",
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nadapter='codex'\nmodel='claude/claude-opus-5-5'\n[domains.routing.model_policy.rules.policy.lane]\nread_only=true\n",
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nschedule={ starts='1970-01-01T00:00:10Z', expires='1970-01-01T00:00:05Z' }\n[domains.routing.model_policy.rules.policy.lane]\nmax_turns=4\n",
    ] {
        let error = provenant_config::load_with_domains(&roots, Some(text.as_bytes()), &sections)
            .err()
            .ok_or(text)?;
        assert!(
            error.to_string().contains("model_policy"),
            "{text}: {error}"
        );
    }
    // A malformed rule that restricts nothing is dropped whole, with a warning.
    let config = provenant_config::load_with_domains(
        &roots,
        Some("schema_version=1\n[[domains.routing.model_policy.rules]]\nschedule={ starts='1970-01-01T00:00:10Z', expires='1970-01-01T00:00:05Z' }\n[domains.routing.model_policy.rules.policy.cache]\nprice_multiplier=2.0\n".as_bytes()),
        &sections,
    )?;
    assert!(config.domains.routing.model_policy.rules.is_empty());
    assert!(
        config.warnings[0]
            .message
            .starts_with("domains.routing.model_policy.rules.0")
    );
    let cfg: RoutingConfig = toml_edit::de::from_str(
        "[routes.review]\nmodel = 'opencode/opencode/glm-5.3-flash-free'\n",
    )?;
    assert!(cfg.validate().is_empty());
    assert_eq!(
        cfg.routes.get("review").map(|r| r.model.model()),
        Some("opencode/glm-5.3-flash-free")
    );
    assert!(toml_edit::de::from_str::<RoutingConfig>("preferences = true\n[routes]").is_err());
    Ok(())
}

#[test]
fn family_requires_provider_answer_evidence_at_the_same_revision()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::{
        Adapter, ExitObservation, FamilyEntry, RouteProvenance, claude::Claude,
    };
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let route = resolve(&request("claude/claude-haiku-4-5-20251001")?, &context)?;
    let mut stream = Claude.stream(None)?;
    Claude.observe(
        &mut stream,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/providers/claude/direct-model.jsonl"
        ))
        .as_bytes(),
    )?;
    let outcome = Claude.finish(
        stream,
        ExitObservation {
            code: Some(0),
            timed_out: false,
        },
    )?;
    let families = [FamilyEntry {
        adapter: "claude".into(),
        model: route.model.clone(),
        family: "anthropic".into(),
    }];
    let lineage = Claude.validate_provenance(&RouteProvenance {
        route: &route,
        outcome: &outcome,
        evidence_ref: "recording",
        evidence_catalogue_revision: &catalogue.revision,
        catalogue: &families,
    })?;
    let forged_rows = [FamilyEntry {
        adapter: "claude".into(),
        model: route.model.clone(),
        family: "fabricated-family".into(),
    }];
    let forged = Claude.validate_provenance(&RouteProvenance {
        route: &route,
        outcome: &outcome,
        evidence_ref: "recording",
        evidence_catalogue_revision: &catalogue.revision,
        catalogue: &forged_rows,
    })?;
    assert_eq!(
        FamilyFact::from_verified(
            ModelId::parse("claude/claude-haiku-4-5-20251001")?,
            &forged,
            80,
            120,
            &catalogue
        )?
        .record()
        .family,
        None
    );
    let fact = FamilyFact::from_verified(
        ModelId::parse("claude/claude-haiku-4-5-20251001")?,
        &lineage,
        80,
        120,
        &catalogue,
    )?;
    assert_eq!(fact.record().family.as_deref(), Some("anthropic"));
    let wrong = FamilyFact::from_verified(
        ModelId::parse("claude/claude-sonnet-5")?,
        &lineage,
        80,
        120,
        &catalogue,
    )?;
    assert_eq!(wrong.record().family, None);
    let mut broker = catalogue.clone();
    broker.models.push(ModelEntry {
        free: false,
        model: ModelId::parse("codex/claude-haiku-4-5-20251001")?,
        efforts: vec![],
        family: Some("anthropic".into()),
        evidence_ref: "test".into(),
        trains_on_prompts: None,
        monthly_allowance_usd: None,
        quota_pool: None,
    });
    assert_eq!(
        FamilyFact::from_verified(
            ModelId::parse("codex/claude-haiku-4-5-20251001")?,
            &lineage,
            80,
            120,
            &broker
        )?
        .record()
        .family,
        None
    );
    let mut changed = catalogue.clone();
    changed.revision = "new-revision".into();
    let wrong = FamilyFact::from_verified(
        ModelId::parse("claude/claude-haiku-4-5-20251001")?,
        &lineage,
        80,
        120,
        &changed,
    )?;
    assert_eq!(wrong.record().family, None);
    let demand = ReviewDemand {
        family_slots: BTreeMap::from([("anthropic".into(), 1)]),
    };
    let facts = [fact];
    let requests = [request("claude/claude-haiku-4-5-20251001")?];
    assert!(
        !review_availability(&RoutingSnapshot::new(&requests, &context, &facts)?, &demand)?
            .satisfied()
    );
    let registry = [provenant_providers::Registration {
        quota_source: None,
        ..provenant_providers::claude::REGISTRATION
    }];
    let mut record = availability();
    record.adapter = "claude".into();
    record.observed_models = vec!["claude-haiku-4-5-20251001".into()];
    let records = [record];
    let access = [AccessContext {
        adapter: "claude",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut context = context;
    context.registry = &registry;
    context.availability = &records;
    context.access = &access;
    let duplicated = [requests[0].clone(), requests[0].clone()];
    let snapshot = RoutingSnapshot::new(&duplicated, &context, &facts)?;
    assert!(review_availability(&snapshot, &demand)?.satisfied());
    let double = ReviewDemand {
        family_slots: BTreeMap::from([("anthropic".into(), 2)]),
    };
    assert!(!review_availability(&snapshot, &double)?.satisfied());
    context.now = provenant_core::WallTime::from_micros(120_000_000)?;
    assert!(
        !review_availability(&RoutingSnapshot::new(&requests, &context, &facts)?, &demand)?
            .satisfied()
    );
    Ok(())
}

#[test]
fn account_spend_and_expiring_override_ignore_reserve() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let registry = [provenant_providers::codex::REGISTRATION];
    let records = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut observation = quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Available,
        1,
        80,
        120,
    )?;
    observation.snapshot.windows = vec![QuotaWindow {
        pool: "primary".into(),
        window_seconds: Some(18_000),
        used_basis_points: 9_500,
        resets_unix_seconds: Some(10_000),
    }];
    let observations = [observation];
    for (profile, overrides, excluded) in [
        ("spend", serde_json::json!([]), false),
        (
            "pace",
            serde_json::json!([{"account":"codex/opaque-account", "policy":"spend", "schedule":{"expires":"1970-01-01T00:01:41Z"}}]),
            false,
        ),
        (
            "pace",
            serde_json::json!([{"account":"claude/opaque-account", "policy":"spend", "schedule":{"expires":"1970-01-01T00:01:41Z"}}]),
            true,
        ),
        (
            "pace",
            serde_json::json!([{"account":"opaque-account", "policy":"spend", "schedule":{"expires":"1970-01-01T00:01:40Z"}}]),
            true,
        ),
    ] {
        let config: RoutingConfig = serde_json::from_value(
            serde_json::json!({"pacing": {"accounts": {"opaque-account": {"policy":profile}}}, "pacing_overrides":overrides}),
        )?;
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.registry = &registry;
        ctx.availability = &records;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&request("codex/test")?, &ctx)?;
        assert_eq!(
            reason(&route) == Some(&RouteExclusionReason::PacingReserve),
            excluded,
            "profile={profile}, overrides={overrides}"
        );
    }
    Ok(())
}

#[test]
fn rolling_window_limits_go_despite_monthly_headroom() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [{"model":"opencode/opencode-go/glm-5.3-flash", "weight":1, "efforts":[]}]},
        "pacing": {"accounts": {"go": {"policy":"pace", "windows":["rolling", "seven_day", "monthly"], "reserve":{"rolling":0}}}}
    }))?;
    let model = "opencode/opencode-go/glm-5.3-flash";
    let mut record = availability();
    record.adapter = "opencode".into();
    record.account_key = Some("go".into());
    let records = [record];
    let registry = [provenant_providers::opencode::REGISTRATION];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut observation = quota(model, "go", QuotaStatus::Available, 1, 80, 120)?;
    observation.snapshot.account.adapter = "opencode".into();
    observation.snapshot.details = Some(provenant_quota::QuotaDetails {
        source: Some("fixture-authoritative".into()),
        coverage: Some("complete".into()),
        ..provenant_quota::QuotaDetails::default()
    });
    observation.snapshot.windows = vec![
        QuotaWindow {
            pool: "five_hour".into(),
            window_seconds: Some(18_000),
            used_basis_points: 7_000,
            resets_unix_seconds: Some(18_100),
        },
        QuotaWindow {
            pool: "seven_day".into(),
            window_seconds: Some(604_800),
            used_basis_points: 2_000,
            resets_unix_seconds: Some(604_900),
        },
        QuotaWindow {
            pool: "thirty_day".into(),
            window_seconds: Some(2_592_000),
            used_basis_points: 1_000,
            resets_unix_seconds: Some(2_592_100),
        },
    ];
    let observations = [observation];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.registry = &registry;
    ctx.availability = &records;
    ctx.access = &access;
    ctx.quota = &observations;
    let mut query = request(model)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let route = resolve_with_seed(&query, &ctx, "rolling", 0)?;
    let receipt = serde_json::to_value(route)?;
    assert_eq!(
        receipt["pool_selection"]["candidates"][0]["factor_basis_points"],
        3_000
    );
    assert!(receipt.to_string().contains("five_hour"));
    Ok(())
}
