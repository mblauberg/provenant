use super::*;

#[test]
fn every_alternative_validates_its_own_effort() -> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.models.push(ModelEntry {
        capability: Default::default(),
        free: false,
        model: ModelId::parse("codex/test")?,
        efforts: vec!["high".into()],
        effort_suffix: false,
        evidence_ref: "test".into(),
        source: "fixture".into(),
        service_tiers: Vec::new(),
        policy_rules: Vec::new(),
        family: None,
        trains_on_prompts: None,
        monthly_allowance_usd: None,
        quota_pool: None,
    });
    catalogue.models.push(ModelEntry {
        capability: Default::default(),
        free: false,
        model: ModelId::parse("codex/no-effort")?,
        efforts: vec![],
        effort_suffix: false,
        evidence_ref: "test".into(),
        source: "fixture".into(),
        service_tiers: Vec::new(),
        policy_rules: Vec::new(),
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
    assert!(matches!(
        selected.readiness,
        RouteReadiness::Ready {
            availability_warning: Some(_),
            ..
        }
    ));
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
            extra: Default::default(),
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
    let RouteReadiness::Unavailable {
        reason: RouteExclusionReason::Exhausted,
        skipped_accounts,
        ..
    } = &selected.readiness
    else {
        return Err("all exhausted accounts project to an exhausted route".into());
    };
    let skipped = skipped_accounts
        .iter()
        .map(|account| account.account.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(skipped, ["one", "two"].into_iter().collect());
    assert!(
        skipped_accounts
            .iter()
            .all(|account| account.reason == RouteExclusionReason::Exhausted)
    );
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
        fn stream(
            &self,
            _: Option<&Continuation>,
            _: &provenant_records::policy::Policy,
        ) -> Result<StreamState, ProviderError> {
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
fn routing_domain_loads_model_policy_and_named_routes() -> Result<(), Box<dyn std::error::Error>> {
    let model_policy_config = load_routing(
        "schema_version=1\n[domains.routing.model_policy.defaults.cache]\nread_share_target=0.95\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\n[domains.routing.model_policy.rules.schedule]\nexpires='1970-01-01T00:03:20Z'\n[domains.routing.model_policy.rules.policy.lane]\nmax_turns=40\n",
    )??;
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
    let cfg = load_routing(
        "schema_version=1\n[domains.routing.routes.review]\nmodel='codex/gpt-6.1-sol'\n",
    )??;
    assert_eq!(
        cfg.domains
            .routing
            .routes
            .get("review")
            .map(|r| r.model.model()),
        Some("gpt-6.1-sol")
    );
    Ok(())
}

#[test]
fn review_configuration_inherits_and_overrides_shipped_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let shipped = {
        #[derive(Deserialize)]
        struct Defaults {
            review: crate::ReviewConfig,
        }
        toml_edit::de::from_str::<Defaults>(include_str!("../../assets/review-defaults.toml"))?
            .review
    };
    let layered = load_routing(
        "schema_version=1\n[domains.routing.review.shapes.quick]\nreviewers=[{pool='senior',count=1}]\n[domains.routing.review.select.by_risk]\nmedium='deep'\n",
    )??;
    assert_eq!(
        layered.domains.routing.review.shapes["quick"].reviewers[0].pool,
        "senior"
    );
    assert_eq!(
        layered.domains.routing.review.shapes["quick"].frame,
        shipped.shapes["quick"].frame
    );
    assert_eq!(
        layered.domains.routing.review.shapes["quick"].verifiers,
        shipped.shapes["quick"].verifiers
    );
    assert_eq!(
        layered.domains.routing.review.shapes["quick"].synthesis,
        shipped.shapes["quick"].synthesis
    );
    assert_eq!(
        layered.domains.routing.review.shapes["standard"].reviewers[0].count,
        shipped.shapes["standard"].reviewers[0].count
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["routine"],
        shipped.select.by_risk["routine"]
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["medium"],
        "deep"
    );

    let customised = load_routing(
        "schema_version=1\n[domains.routing.review.shapes.quick]\nsynthesis='model'\n[domains.routing.review.floors.security]\ntier='substantial'\n",
    )??;
    static SECTIONS: [provenant_config::DomainSection; 1] = [SECTION];
    let sections = DomainSections::<RoutingDomains>::new(&SECTIONS);
    let schema = provenant_config::schema(&customised, &sections, String::new())?;
    let synthesis = schema
        .fields
        .iter()
        .find(|field| field.path == "domains.routing.review.shapes.quick.synthesis")
        .ok_or("review synthesis schema field")?;
    assert_eq!(
        synthesis.default,
        format!("{:?}", shipped.shapes["quick"].synthesis).to_lowercase()
    );
    assert_eq!(synthesis.value, "model");
    assert_eq!(synthesis.source, provenant_config::Layer::User);
    assert_eq!(synthesis.values, ["model", "reduce"]);
    let floor = schema
        .fields
        .iter()
        .find(|field| field.path == "domains.routing.review.floors.security.tier")
        .ok_or("security floor schema field")?;
    assert_eq!(floor.default, shipped.floors["security"].tier);
    assert_eq!(floor.value, "substantial");
    assert_eq!(floor.source, provenant_config::Layer::User);

    let extensible = load_routing(
        "schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers=[{pool='senior',count=1}]\n[domains.routing.review.select]\nby_risk={medium='custom'}\n",
    )??;
    assert!(
        extensible
            .domains
            .routing
            .review
            .shapes
            .contains_key("custom")
    );
    assert!(extensible.domains.routing.review.shapes["custom"].frame);
    assert_eq!(
        extensible.domains.routing.review.shapes["custom"].verifiers,
        1
    );
    assert_eq!(
        extensible.domains.routing.review.shapes["custom"].synthesis,
        crate::ReviewSynthesis::Model
    );
    assert!(
        extensible
            .domains
            .routing
            .review
            .shapes
            .contains_key("quick")
    );
    assert_eq!(
        extensible.domains.routing.review.select.by_risk["routine"],
        "quick"
    );
    assert_eq!(
        extensible.domains.routing.review.select.by_risk["medium"],
        "custom"
    );
    Ok(())
}

#[test]
fn unknown_routing_keys_warn_once_with_the_user_config_source()
-> Result<(), Box<dyn std::error::Error>> {
    for (text, key) in [
        (
            "schema_version=1\n[domains.routing]\npreferences=true\n",
            "domains.routing.preferences",
        ),
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
    ] {
        let config = load_routing(text)??;
        assert_eq!(config.warnings.len(), 1, "{text}");
        let warning = &config.warnings[0];
        assert_eq!(warning.rule, "unknown_key", "{text}");
        assert_eq!(warning.path.as_path_text(), Some("config.toml"), "{text}");
        assert!(warning.message.contains(key), "{text}");
        assert_eq!(
            config.domains.routing.review.select.by_risk["routine"],
            "quick"
        );
    }
    Ok(())
}

#[test]
fn invalid_review_value_recovers_from_the_shipped_default() -> Result<(), Box<dyn std::error::Error>>
{
    let config = load_routing(
        "schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers='wrong'\n",
    )??;
    assert!(!config.warnings.is_empty());
    assert_eq!(
        config.domains.routing.review.shapes["quick"].reviewers[0].pool,
        RoutingConfig::default().review.shapes["quick"].reviewers[0].pool
    );
    Ok(())
}

#[test]
fn invalid_policy_siblings_cannot_lift_enforced_restrictions()
-> Result<(), Box<dyn std::error::Error>> {
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
        let config = load_routing(text)??;
        let policy = config
            .domains
            .routing
            .model_policy
            .effective(
                &crate::Catalogue::bundled().unwrap(),
                &model,
                None,
                provenant_core::WallTime::from_unix_seconds(0)?,
            )
            .policy;
        assert_eq!(policy.lane.read_only, Some(true), "{text}");
        assert_ne!(policy.cache.read_heavy_weight, Some(11), "{text}");
        assert!(!policy.permits(write), "{text}");
        assert!(
            config
                .warnings
                .iter()
                .any(|warning| { warning.message.contains(".cache.read_heavy_weight:") })
        );
    }
    Ok(())
}

#[test]
fn invalid_model_policy_inputs_that_can_lift_a_restriction_are_refused()
-> Result<(), Box<dyn std::error::Error>> {
    for text in [
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nmax_context=0\n",
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nmax_turns=0\n",
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nread_only='yes'\n",
        "schema_version=1\n[domains.routing.model_policy.defaults]\nlane='read_only'\n",
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nadapter='codex'\nmodel='claude/claude-opus-5-5'\n[domains.routing.model_policy.rules.policy.lane]\nread_only=true\n",
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nschedule={ starts='1970-01-01T00:00:10Z', expires='1970-01-01T00:00:05Z' }\n[domains.routing.model_policy.rules.policy.lane]\nmax_turns=4\n",
    ] {
        let error = load_routing(text)?.err().ok_or(text)?;
        // Refused at the offending key, whether the value is out of range
        // (a domain error) or of the wrong type.
        assert!(
            error.to_string().contains("model_policy"),
            "{text}: {error}"
        );
    }

    let malformed_rule = load_routing(
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nschedule={ starts='1970-01-01T00:00:10Z', expires='1970-01-01T00:00:05Z' }\n[domains.routing.model_policy.rules.policy.cache]\nprice_multiplier=2.0\n",
    )??;
    assert!(malformed_rule.domains.routing.model_policy.rules.is_empty());
    assert!(!malformed_rule.warnings.is_empty());
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
    let mut stream = Claude.stream(None, &provenant_records::policy::Policy::default())?;
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
        capability: Default::default(),
        free: false,
        model: ModelId::parse("codex/claude-haiku-4-5-20251001")?,
        efforts: vec![],
        effort_suffix: false,
        family: Some("anthropic".into()),
        evidence_ref: "test".into(),
        source: "fixture".into(),
        service_tiers: Vec::new(),
        policy_rules: Vec::new(),
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
        extra: Default::default(),
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
        let mut config: RoutingConfig = serde_json::from_value(
            serde_json::json!({"pacing": {"accounts": {"codex/opaque-account": {"policy":profile}}}, "pacing_overrides":overrides}),
        )?;
        config.launch_policy.overlay(
            &std::collections::BTreeMap::from([
                (
                    "PACING_ADAPTER".into(),
                    provenant_records::policy::Strength::Warn,
                ),
                (
                    "PACING_ACCOUNT".into(),
                    provenant_records::policy::Strength::Warn,
                ),
                (
                    "PACING_FREE".into(),
                    provenant_records::policy::Strength::Warn,
                ),
            ]),
            provenant_records::policy::Source::Dispatch,
        );
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.registry = &registry;
        ctx.availability = &records;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&request("codex/test")?, &ctx)?;
        assert!(reason(&route).is_none());
        assert_eq!(
            route
                .diagnostics
                .account_load
                .as_ref()
                .is_some_and(|load| !load.warnings.is_empty()),
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
        "pacing": {"accounts": {"opencode/go": {"policy":"pace", "windows":["rolling", "seven_day", "monthly"], "reserve":{"rolling":0}}}}
    }))?;
    let model = "opencode/opencode-go/glm-5.3-flash";
    let mut record = availability();
    record.adapter = "opencode".into();
    record.account_key = Some("go".into());
    record.plan = Some("go".into());
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
        extra: Default::default(),
        plan: Some("go".into()),
        source: Some("fixture-authoritative".into()),
        coverage: Some("complete".into()),
        ..provenant_quota::QuotaDetails::default()
    });
    observation.snapshot.windows = vec![
        QuotaWindow {
            extra: Default::default(),
            pool: "five_hour".into(),
            window_seconds: Some(18_000),
            used_basis_points: 7_000,
            resets_unix_seconds: Some(18_100),
        },
        QuotaWindow {
            extra: Default::default(),
            pool: "seven_day".into(),
            window_seconds: Some(604_800),
            used_basis_points: 2_000,
            resets_unix_seconds: Some(604_900),
        },
        QuotaWindow {
            extra: Default::default(),
            pool: "thirty_day".into(),
            window_seconds: Some(2_592_000),
            used_basis_points: 1_000,
            resets_unix_seconds: Some(2_592_100),
        },
    ];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.registry = &registry;
    ctx.availability = &records;
    ctx.access = &access;
    let mut query = request(model)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let high_observations = [observation.clone()];
    ctx.quota = &high_observations;
    let high_route = resolve_with_seed(&query, &ctx, "rolling", 0)?;
    let high_pressure =
        high_route.pool_selection.ok_or("receipt")?.candidates[0].factor_basis_points;
    observation.snapshot.windows[0].used_basis_points = 1_000;
    let lower_observations = [observation];
    ctx.quota = &lower_observations;
    let lower_route = resolve_with_seed(&query, &ctx, "rolling", 0)?;
    let lower_pressure =
        lower_route.pool_selection.ok_or("receipt")?.candidates[0].factor_basis_points;
    assert!(lower_pressure > high_pressure);
    Ok(())
}
