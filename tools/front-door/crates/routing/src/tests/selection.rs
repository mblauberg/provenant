use super::*;

#[test]
fn table_driven_weighted_pools_record_effort_readiness_roll_and_seed()
-> Result<(), Box<dyn std::error::Error>> {
    let access = [
        AccessContext {
            adapter: "codex",
            disclosure_ref: "policy",
            confinement_ref: "grant",
        },
        AccessContext {
            adapter: "claude",
            disclosure_ref: "policy",
            confinement_ref: "grant",
        },
    ];
    let catalogue = Catalogue::bundled()?;
    let mut base_availability = vec![availability()];
    let mut record = availability();
    record.adapter = "claude".into();
    base_availability.push(record);

    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {},
        "pools": { "trial": [
            {"model":"codex/gpt-6-luna", "weight":4, "effort":"high", "efforts":["high"]},
            {"model":"claude/claude-haiku-5-5", "weight":6, "effort":"high", "efforts":["high"]},
            {"model":"agy/gemini-3.8-flash", "weight":0, "effort":"medium", "efforts":["low"]}
        ]}
    }))?;

    let mut ctx = context(&catalogue, &config, now()?);
    ctx.registry = REGISTRY;
    ctx.availability = &base_availability;
    ctx.access = &access;

    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    query.effort = Some("high".into());

    let eligible_models = ["gpt-6-luna", "claude-haiku-5-5"];

    for seed in 0..5 {
        let route = resolve_with_seed(&query, &ctx, "op-test", seed)?;
        let receipt = route
            .pool_selection
            .as_ref()
            .ok_or("pool selection receipt")?;
        assert!(
            eligible_models.contains(&route.model.as_str()),
            "pick {} not in eligible set",
            route.model
        );
        assert_ne!(
            route.model, "gemini-3.8-flash",
            "zero weight / non-ready model forbidden"
        );

        // Reproducibility invariant: identical inputs and seed produce identical results
        let repeated = resolve_with_seed(&query, &ctx, "op-test", seed)?;
        let repeated_receipt = repeated.pool_selection.as_ref().ok_or("repeated receipt")?;
        assert_eq!(repeated.model, route.model);
        assert_eq!(repeated_receipt.roll, receipt.roll);
        assert_eq!(repeated_receipt.pick, receipt.pick);
    }
    Ok(())
}

#[test]
fn confidential_pool_excludes_training_models() -> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.pools.insert(
        "free-check".into(),
        vec![
            RoutePoolMember {
                model: ModelId::parse("codex/gpt-6-luna")?,
                weight: 1,
                effort: Some("low".into()),
                efforts: vec!["low".into()],
                free: false,
            },
            RoutePoolMember {
                model: ModelId::parse("opencode/opencode/deepseek-v4.1-flash-free")?,
                weight: 1,
                effort: None,
                efforts: vec![],
                free: true,
            },
        ],
    );
    catalogue
        .models
        .iter_mut()
        .find(|row| row.model.as_str() == "codex/gpt-6-luna")
        .expect("codex member is catalogued")
        .trains_on_prompts = Some(true);
    let config = RoutingConfig::default();
    let ctx = context(&catalogue, &config, now()?);
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("free-check")?);
    query.confidential = true;
    let resolved = resolve(&query, &ctx)?;
    let receipt = resolved.pool_selection.as_ref().ok_or("pool receipt")?;
    assert_eq!(
        receipt
            .candidates
            .iter()
            .find(|candidate| candidate.model == "codex/gpt-6-luna")
            .ok_or("training candidate")?
            .exclusion
            .as_deref(),
        Some("confidential")
    );
    let free = receipt
        .candidates
        .iter()
        .find(|candidate| candidate.model == "opencode/opencode/deepseek-v4.1-flash-free")
        .ok_or("free candidate")?;
    assert_eq!(free.exclusion, None);
    assert!(matches!(
        free.readiness,
        Some(RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability,
            ..
        })
    ));
    Ok(())
}

#[test]
fn junior_free_routes_are_selected_without_opt_in() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = opencode_accounts();
    let access = opencode_access();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("junior")?);
    let mut free_picks = 0;
    for route in junior_draws(&query, &ctx)? {
        if catalogue.is_free_model(&picked_model(&route)?) {
            free_picks += 1;
            assert_eq!(route.account_key.as_deref(), Some("zen"));
            assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
        }
    }
    assert!(free_picks > 0, "no seed selected a free junior route");
    Ok(())
}

#[test]
fn confidential_junior_requests_accept_free_routes() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = opencode_accounts();
    let access = opencode_access();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("junior")?);
    query.confidential = true;
    assert!(
        junior_draws(&query, &ctx)?
            .iter()
            .any(|route| catalogue.is_free_model(&picked_model(route).unwrap()))
    );
    for model in [
        "opencode/opencode/deepseek-v4.1-flash-free",
        "opencode/opencode/glm-5.3-flash-free",
    ] {
        query.choice = RouteChoice::Model(ModelId::parse(model)?);
        assert!(resolve(&query, &ctx).is_ok());
    }
    Ok(())
}

#[test]
fn cooling_free_route_falls_back_to_another_junior_member() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = opencode_accounts();
    let access = opencode_access();
    let cooldowns = [provenant_quota::CapacityCooldown {
        extra: Default::default(),
        account: provenant_quota::AccountScope {
            adapter: "opencode".into(),
            account_key: "zen".into(),
            label: "test".into(),
        },
        model: "opencode/deepseek-v4.1-flash-free".into(),
        observed_unix_seconds: 90,
        expires_unix_seconds: 110,
    }];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.capacity_cooldowns = &cooldowns;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("junior")?);
    let cooling = "opencode/opencode/deepseek-v4.1-flash-free";
    for route in junior_draws(&query, &ctx)? {
        assert_ne!(picked_model(&route)?.as_str(), cooling);
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
        let receipt = route.pool_selection.ok_or("pool receipt")?;
        let skipped = receipt
            .candidates
            .iter()
            .find(|candidate| candidate.model == cooling)
            .ok_or("cooling candidate")?;
        assert_eq!(skipped.exclusion.as_deref(), Some("capacity cooldown"));
    }
    Ok(())
}

#[test]
fn usage_limited_free_route_falls_back_to_another_junior_member()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = opencode_accounts();
    let access = opencode_access();
    let scope = provenant_quota::AccountScope {
        adapter: "opencode".into(),
        account_key: "zen".into(),
        label: "test".into(),
    };
    let refused = [provenant_quota::record_refusal(
        None,
        &scope,
        "opencode/opencode/deepseek-v4.1-flash-free",
        Some("opencode-free"),
        90,
        None,
    )?
    .replacement
    .ok_or("record")?];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.accounts = &refused;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("junior")?);
    for route in junior_draws(&query, &ctx)? {
        assert!(!catalogue.is_free_model(&picked_model(&route)?));
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    Ok(())
}

#[test]
fn alternatives_are_bounded_distinct_and_keep_the_callers_order()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let mut query = request("codex/primary")?;
    query.fallback = Fallback::Models(BoundedModels::new(vec![
        ModelId::parse("agy/second")?,
        ModelId::parse("claude/third")?,
    ])?);
    let route = resolve(&query, &context)?;
    assert_eq!(route.adapter, "codex");
    assert_eq!(
        route
            .alternatives
            .as_slice()
            .iter()
            .map(|a| a.model.as_str())
            .collect::<Vec<_>>(),
        ["second", "third"]
    );
    assert_eq!(route.exclusions.len(), 3);
    assert!(
        route
            .alternatives
            .as_slice()
            .iter()
            .all(|a| matches!(a.readiness, RouteReadiness::Unavailable { .. }))
    );
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("codex/primary")?])?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::Alternatives)
    ));
    assert!(
        BoundedModels::<2>::new(vec![ModelId::parse("codex/a")?, ModelId::parse("codex/a")?])
            .is_err()
    );
    assert!(
        serde_json::from_str::<BoundedModels<2>>(r#"["codex/a","codex/b","codex/c"]"#).is_err()
    );
    let (_, warnings) = RouteRequest::from_json(
        "request.json",
        br#"{"choice":{"kind":"model","value":"codex/a"},"effort":null,"pool":"principal"}"#,
    )?;
    assert_eq!(warnings[0].key, "pool");
    Ok(())
}

#[test]
fn candidate_errors_and_fallback_effort_match_request_fixtures()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.pools.insert(
        "fixture-effort".into(),
        vec![RoutePoolMember {
            model: ModelId::parse("codex/gpt-6.1-sol")?,
            weight: 1,
            effort: Some("high".into()),
            efforts: vec!["high".into()],
            free: false,
        }],
    );
    let config = RoutingConfig {
        routes: BTreeMap::from([(
            "review".into(),
            NamedRoute {
                model: ModelId::parse("codex/gpt-6.1-sol")?,
                effort: Some("max".into()),
            },
        )]),
        ..RoutingConfig::default()
    };
    let context = context(&catalogue, &config, now()?);
    let fixtures = [
        (
            r#"{"choice":{"kind":"model","value":"invented/id"},"effort":"bad effort"}"#,
            Err("unknown adapter"),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/unlisted"},"effort":"high"}"#,
            Ok(Some("high")),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":"bad effort"}"#,
            Err("unsupported effort"),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":null,"fallback":{"kind":"models","models":["invented/id"]}}"#,
            Err("unknown adapter"),
        ),
        (
            r#"{"choice":{"kind":"named_route","value":"review"},"effort":null,"fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Err("unsupported effort"),
        ),
        (
            r#"{"choice":{"kind":"named_route","value":"review"},"effort":"high","fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Ok(Some("high")),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":null,"fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Ok(None),
        ),
        (
            r#"{"choice":{"kind":"named_route","value":"fixture-effort"},"effort":null,"fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Ok(Some("high")),
        ),
    ];
    for (wire, expected) in fixtures {
        let query = serde_json::from_str(wire)?;
        let actual = resolve(&query, &context)
            .map(|route| {
                for alternative in route.alternatives.as_slice() {
                    assert_eq!(alternative.effort, route.effort, "{wire}");
                }
                route.effort
            })
            .map_err(|error| match error {
                RoutingError::UnsupportedEffort { .. } => "unsupported effort".to_owned(),
                error => error.to_string(),
            });
        let expected = expected
            .map(|effort| effort.map(str::to_owned))
            .map_err(str::to_owned);
        assert_eq!(actual, expected, "{wire}");
    }
    let context = RoutingContext {
        registry: &[],
        ..context
    };
    let query: RouteRequest = serde_json::from_str(
        r#"{"choice":{"kind":"named_route","value":"fixture-effort"},"effort":null}"#,
    )?;
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnknownAdapter)
    ));
    assert!(matches!(
        list(&RouteListQuery::default(), &context),
        Err(RoutingError::UnknownAdapter)
    ));
    Ok(())
}

#[test]
fn required_provider_family_filters_pool_and_records_exclusions()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"family-trial": [
            {"model":"codex/gpt-6-luna", "weight":1},
            {"model":"claude/claude-haiku-5-5", "weight":99}
        ]}
    }))?;
    let records = [
        availability(),
        AvailabilityRecord {
            adapter: "claude".into(),
            ..availability()
        },
    ];
    let access = ["codex", "claude"].map(|adapter| AccessContext {
        adapter,
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    });
    let ctx = RoutingContext {
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("family-trial")?);
    for seed in 0..20 {
        let constrained = resolve_for_account_with_seed_excluding_family(
            &query,
            &ctx,
            None,
            "family",
            seed,
            &[],
            &[],
            Some("openai"),
        )?;
        assert_eq!(constrained.adapter, "codex");
        assert!(
            constrained
                .pool_selection
                .unwrap()
                .candidates
                .iter()
                .any(|candidate| candidate.model.starts_with("claude/")
                    && candidate
                        .exclusion
                        .as_deref()
                        .is_some_and(|reason| reason.contains("required family openai count 1")))
        );
        let old =
            resolve_for_account_with_seed_excluding(&query, &ctx, None, "family", seed, &[], &[])?;
        let unchanged = resolve_for_account_with_seed_excluding_family(
            &query,
            &ctx,
            None,
            "family",
            seed,
            &[],
            &[],
            None,
        )?;
        assert_eq!(serde_json::to_value(old)?, serde_json::to_value(unchanged)?);
    }
    let missing = RoutingContext {
        availability: &records[1..],
        ..ctx
    };
    // Preview has no availability/confinement snapshot; family selection keeps
    // ordinary unknown-readiness semantics until the dispatch probes the host.
    let preview = resolve_for_account_with_seed_excluding_family(
        &query,
        &missing,
        None,
        "family",
        0,
        &[],
        &[],
        Some("openai"),
    )?;
    assert_eq!(preview.adapter, "codex");
    assert!(matches!(
        preview.readiness,
        RouteReadiness::Unavailable { .. }
    ));
    let rejected = [ModelId::parse("codex/gpt-6-luna")?];
    let error = resolve_for_account_with_seed_excluding_family(
        &query,
        &ctx,
        None,
        "family",
        0,
        &rejected,
        &[],
        Some("openai"),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("openai"), "{error}");
    assert!(error.contains("count 1"), "{error}");
    assert!(error.contains("admission rejected"), "{error}");
    assert!(error.contains("candidate family anthropic"), "{error}");
    // Exact routes are constrained too, and cannot escape through a fallback.
    query.choice = RouteChoice::Model(ModelId::parse("codex/gpt-6-luna")?);
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse(
        "claude/claude-haiku-5-5",
    )?])?);
    let pinned = resolve_for_account_with_seed_excluding_family(
        &query,
        &ctx,
        None,
        "family",
        0,
        &[],
        &[],
        Some("openai"),
    )?;
    assert!(pinned.alternatives.as_slice().is_empty());
    query.fallback = Fallback::None;
    query.choice = RouteChoice::Model(ModelId::parse("claude/claude-haiku-5-5")?);
    assert!(
        resolve_for_account_with_seed_excluding_family(
            &query,
            &ctx,
            None,
            "family",
            0,
            &[],
            &[],
            Some("openai"),
        )
        .is_err()
    );
    Ok(())
}
