use super::*;

fn synthetic_catalogue() -> Result<Catalogue, Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.models = vec![
        ModelEntry {
            capability: Default::default(),
            model: ModelId::parse("alpha/unique-model")?,
            free: false,
            evidence_ref: "evidence".into(),
            source: "fixture".into(),
            service_tiers: Vec::new(),
            policy_rules: Vec::new(),
            efforts: vec!["medium".into()],
            effort_suffix: false,
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        },
        ModelEntry {
            capability: Default::default(),
            model: ModelId::parse("alpha/shared-model")?,
            free: false,
            evidence_ref: "evidence".into(),
            source: "fixture".into(),
            service_tiers: Vec::new(),
            policy_rules: Vec::new(),
            efforts: vec!["medium".into()],
            effort_suffix: false,
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        },
        ModelEntry {
            capability: Default::default(),
            model: ModelId::parse("beta/shared-model")?,
            free: false,
            evidence_ref: "evidence".into(),
            source: "fixture".into(),
            service_tiers: Vec::new(),
            policy_rules: Vec::new(),
            efforts: vec!["medium".into()],
            effort_suffix: false,
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        },
    ];
    Ok(catalogue)
}

#[test]
fn bare_name_resolves_to_the_one_provider_that_offers_it() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = synthetic_catalogue()?;
    assert_eq!(
        catalogue.resolve_bare_model("unique-model")?.as_str(),
        "alpha/unique-model"
    );
    Ok(())
}

#[test]
fn bare_name_offered_by_several_providers_names_each_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = synthetic_catalogue()?;
    let err = catalogue.resolve_bare_model("shared-model").unwrap_err();
    match err {
        BareModelError::Ambiguous { mut candidates } => {
            candidates.sort();
            assert_eq!(
                candidates,
                vec![
                    "alpha/shared-model".to_string(),
                    "beta/shared-model".to_string()
                ]
            );
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    Ok(())
}

#[test]
fn bare_name_no_provider_offers_is_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = synthetic_catalogue()?;
    assert_eq!(
        catalogue.resolve_bare_model("no-such-model"),
        Err(BareModelError::Unknown)
    );
    // A provider-qualified name never resolves as a bare name.
    assert_eq!(
        catalogue.resolve_bare_model("alpha/unique-model"),
        Err(BareModelError::Unknown)
    );
    Ok(())
}

#[test]
fn missing_provider_segment_names_the_unique_catalogued_id()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let partial = ModelId::parse("opencode/deepseek-v4.1-flash-free")?;
    let full = ModelId::parse("opencode/opencode/deepseek-v4.1-flash-free")?;
    assert_eq!(catalogue.namespaced_candidates(&partial), vec![&full]);
    assert_eq!(
        catalogue.provider_segment_warning(&partial).as_deref(),
        Some(
            "model opencode/deepseek-v4.1-flash-free is missing its provider segment; did you mean opencode/opencode/deepseek-v4.1-flash-free?"
        )
    );
    // An exact ID and a genuinely unknown one need no namespace warning.
    assert!(catalogue.namespaced_candidates(&full).is_empty());
    assert!(catalogue.provider_segment_warning(&full).is_none());
    let unknown = ModelId::parse("opencode/no-such-model-3724")?;
    assert!(catalogue.provider_segment_warning(&unknown).is_none());
    Ok(())
}

#[test]
fn missing_provider_segment_lists_candidates_and_respects_namespace_declaration()
-> Result<(), Box<dyn std::error::Error>> {
    let make = |id: &str| -> Result<ModelEntry, Box<dyn std::error::Error>> {
        Ok(ModelEntry {
            capability: Default::default(),
            model: ModelId::parse(id)?,
            free: false,
            evidence_ref: "evidence".into(),
            source: "fixture".into(),
            service_tiers: Vec::new(),
            policy_rules: Vec::new(),
            efforts: vec!["medium".into()],
            effort_suffix: false,
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        })
    };
    let mut catalogue = Catalogue::bundled()?;
    catalogue.models = vec![
        make("opencode/zen/shared-3724")?,
        make("opencode/go/shared-3724")?,
        // A namespace-less adapter never receives a provider-segment hint.
        make("codex/go/shared-3724")?,
    ];
    let partial = ModelId::parse("opencode/shared-3724")?;
    let candidates: Vec<_> = catalogue
        .namespaced_candidates(&partial)
        .iter()
        .map(|id| id.as_str())
        .collect();
    assert_eq!(
        candidates,
        vec!["opencode/zen/shared-3724", "opencode/go/shared-3724"]
    );
    let warning = catalogue
        .provider_segment_warning(&partial)
        .ok_or("ambiguous provider-segment warning")?;
    assert!(warning.contains("opencode/zen/shared-3724"), "{warning}");
    assert!(warning.contains("opencode/go/shared-3724"), "{warning}");
    let non_namespaced = ModelId::parse("codex/shared-3724")?;
    assert!(catalogue.namespaced_candidates(&non_namespaced).is_empty());
    Ok(())
}

#[test]
fn unresolved_model_names_the_account_instead_of_readiness()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = opencode_accounts();
    let access = opencode_access();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    let query = request("opencode/deepseek-v4.1-flash-free")?;
    let route = resolve(&query, &ctx)?;
    let (reason, skipped) = match &route.readiness {
        RouteReadiness::Unavailable {
            reason,
            skipped_accounts,
            ..
        } => (reason, skipped_accounts),
        other => panic!("expected an unavailable route, got {other:?}"),
    };
    assert_eq!(reason, &RouteExclusionReason::UnresolvedAccount);
    let detail = exclusion_detail(reason);
    assert!(
        !detail.contains("readiness has not been verified"),
        "{detail}"
    );
    assert!(detail.contains("account"), "{detail}");
    assert!(!skipped.is_empty());
    assert!(skipped.iter().any(|account| account.account == "zen"
        && account.reason == RouteExclusionReason::UnresolvedAccount));
    Ok(())
}

#[test]
fn pool_excludes_unsupported_effort_but_exact_and_user_pool_allow_it()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    // Configured pools are validated against the bundled catalogue. Select
    // a real model with both efforts rather than inventing a pool-only model.
    let target = catalogue
        .models
        .iter()
        .find(|entry| {
            let has = |effort| entry.efforts.iter().any(|value| value == effort);
            entry.model.adapter() == "codex" && has("low") && has("high")
        })
        .ok_or("bundled Codex model supporting low and high")?
        .model
        .clone();
    // Pool allows only "low"
    catalogue.pools.insert(
        "test-pool".into(),
        vec![RoutePoolMember {
            model: target.clone(),
            weight: 1,
            effort: Some("low".into()),
            efforts: vec!["low".into()],
            free: false,
        }],
    );
    let config = RoutingConfig::default();
    let available = [AvailabilityRecord {
        schema_version: 1,
        adapter: "codex".into(),
        executable: "/fixture".into(),
        version: "fixture".into(),
        fixture_ref: "test".into(),
        catalogue_revision: catalogue.revision().into(),
        account_key: Some("opaque-account".into()),
        plan: None,
        observed_models: vec![target.model().to_owned()],
        observed_unix_seconds: 0,
        expires_unix_seconds: 10_000,
        last_update: None,
    }];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let reads = vec![observation(
        "codex",
        "opaque-account",
        target.as_str(),
        provenant_quota::ReadinessState::Ready,
        None,
        (80, 120),
    )];
    let lookup = readiness_over(reads);
    let ctx = RoutingContext {
        readiness: Some(&lookup),
        availability: &available,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };

    // Requesting "high" effort in the pool that only allows "low" excludes the candidate
    let mut query = request(target.as_str())?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("test-pool")?);
    query.effort = Some("high".into());
    let error = resolve_with_seed(&query, &ctx, "test-seed", 0).unwrap_err();
    assert_eq!(error.code(), "no_eligible_pool_member");
    assert!(
        matches!(error, RoutingError::NoEligiblePoolMember { exclusions, .. } if !exclusions.is_empty())
    );

    // Exact request for the model with "high" effort resolves ready
    let mut exact = request(target.as_str())?;
    exact.effort = Some("high".into());
    let route = resolve(&exact, &ctx)?;
    assert_eq!(picked_model(&route)?, target);
    assert_eq!(route.effort.as_deref(), Some("high"));
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));

    // User-configured custom pool allowing "high" effort resolves ready
    let mut custom = RoutingConfig::default();
    custom.pools.insert(
        "custom-pool".into(),
        vec![RoutePoolMember {
            model: target.clone(),
            weight: 1,
            effort: Some("high".into()),
            efforts: vec!["high".into()],
            free: false,
        }],
    );
    exact.choice = RouteChoice::NamedRoute(RouteName::parse("custom-pool")?);
    let custom_ctx = RoutingContext {
        config: &custom,
        ..ctx
    };
    let route = resolve(&exact, &custom_ctx)?;
    assert_eq!(picked_model(&route)?, target);
    assert_eq!(route.effort.as_deref(), Some("high"));
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    Ok(())
}
