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
    for treatment in [None, Some(true)] {
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
            .trains_on_prompts = treatment;
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
    }
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
            extra: Default::default(),
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
        extra: Default::default(),
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
    let resolved_dup = resolve(&query, &context)?;
    assert!(resolved_dup.alternatives.as_slice().is_empty());

    let mut strict_policy = provenant_records::policy::Policy::default();
    strict_policy.overlay(
        &[(
            "ROUTING_ALTERNATIVES".into(),
            provenant_records::policy::Strength::Refuse,
        )]
        .into_iter()
        .collect(),
        provenant_records::policy::Source::Project,
    );
    let strict_config = RoutingConfig {
        launch_policy: strict_policy,
        ..config.clone()
    };
    let strict_context = RoutingContext {
        config: &strict_config,
        ..context
    };
    assert!(matches!(
        resolve(&query, &strict_context),
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
    enum Expected {
        Adapter,
        Effort {
            model: &'static str,
            effort: &'static str,
        },
        Resolved(Option<&'static str>),
    }
    let fixtures = [
        (
            r#"{"choice":{"kind":"model","value":"invented/id"},"effort":"bad effort"}"#,
            Expected::Adapter,
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/unlisted"},"effort":"high"}"#,
            Expected::Resolved(Some("high")),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":"bad effort"}"#,
            Expected::Effort {
                model: "codex/gpt-6.1-sol",
                effort: "bad effort",
            },
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":null,"fallback":{"kind":"models","models":["invented/id"]}}"#,
            Expected::Adapter,
        ),
        (
            // The route's own effort applies to its fallbacks: luna records no
            // `max`, so the fallback is the candidate named in the refusal.
            r#"{"choice":{"kind":"named_route","value":"review"},"effort":null,"fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Expected::Effort {
                model: "codex/gpt-6-luna",
                effort: "max",
            },
        ),
        (
            r#"{"choice":{"kind":"named_route","value":"review"},"effort":"high","fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Expected::Resolved(Some("high")),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":null,"fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Expected::Resolved(None),
        ),
        (
            r#"{"choice":{"kind":"named_route","value":"fixture-effort"},"effort":null,"fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Expected::Resolved(Some("high")),
        ),
    ];
    for (wire, expected) in fixtures {
        let query = serde_json::from_str(wire)?;
        match (resolve(&query, &context), expected) {
            (Ok(route), Expected::Resolved(effort)) => {
                for alternative in route.alternatives.as_slice() {
                    assert_eq!(alternative.effort, route.effort, "{wire}");
                }
                assert_eq!(route.effort.as_deref(), effort, "{wire}");
            }
            (Err(RoutingError::UnknownAdapter), Expected::Adapter) => {}
            (
                Err(RoutingError::UnsupportedEffort { model, effort, .. }),
                Expected::Effort {
                    model: expected_model,
                    effort: expected_effort,
                },
            ) => {
                assert_eq!(model, expected_model, "{wire}");
                assert_eq!(effort, expected_effort, "{wire}");
            }
            (Err(error), _) => panic!("unexpected routing error code {} for {wire}", error.code()),
            (Ok(_), _) => panic!("unexpected successful route for {wire}"),
        }
    }
    let mut strict_policy = provenant_records::policy::Policy::default();
    strict_policy.overlay(
        &[(
            "ROUTING_EFFORT_UNCATALOGUED".into(),
            provenant_records::policy::Strength::Refuse,
        )]
        .into_iter()
        .collect(),
        provenant_records::policy::Source::Project,
    );
    let strict_config = RoutingConfig {
        launch_policy: strict_policy,
        ..config.clone()
    };
    let strict_context = RoutingContext {
        config: &strict_config,
        ..context
    };
    let uncatalogued: RouteRequest = serde_json::from_str(
        r#"{"choice":{"kind":"model","value":"codex/unlisted"},"effort":"high"}"#,
    )?;
    assert!(matches!(
        resolve(&uncatalogued, &strict_context),
        Err(RoutingError::UnsupportedEffort { .. })
    ));
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
        AvailabilityRecord {
            observed_models: vec!["gpt-6-luna".into()],
            ..availability()
        },
        AvailabilityRecord {
            adapter: "claude".into(),
            observed_models: vec!["claude-haiku-5-5".into()],
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
        let candidates = &constrained
            .pool_selection
            .as_ref()
            .ok_or("receipt")?
            .candidates;
        let codex_candidate = candidates
            .iter()
            .find(|candidate| candidate.model.starts_with("codex/"))
            .ok_or("eligible provider candidate")?;
        let claude_candidate = candidates
            .iter()
            .find(|candidate| candidate.model.starts_with("claude/"))
            .ok_or("other-family provider candidate")?;
        assert!(codex_candidate.exclusion.is_none());
        assert!(claude_candidate.exclusion.is_some());
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
    .unwrap_err();
    assert_eq!(error.code(), "no_eligible_pool_member");
    assert!(matches!(
        error,
        RoutingError::NoEligiblePoolMember { exclusions, .. } if !exclusions.is_empty()
    ));
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

// Spec: docs/specs/routing.md#unknown-never-means-ready
#[test]
fn pools_never_draw_unresolved_or_login_required_accounts() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_quota::{ReadinessRead, ReadinessReason, ReadinessState};
    let catalogue = Catalogue::bundled()?;
    let mut config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model":"codex/gpt-6-luna", "weight":1},
            {"model":"claude/claude-haiku-5-5", "weight":0}
        ]}
    }))?;
    // Missing identity is advisory by default so adapters with a HOME login
    // can route. Exercise a genuinely unresolved account with the refusing policy.
    config.launch_policy.overlay(
        &std::collections::BTreeMap::from([(
            "QUOTA_IDENTITY_UNKNOWN".into(),
            provenant_records::policy::Strength::Refuse,
        )]),
        provenant_records::policy::Source::Dispatch,
    );
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
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    for (missing_key, stale, login) in [
        (true, false, None),
        (true, true, None),
        (false, false, Some(ReadinessReason::SignedOut)),
        (false, false, Some(ReadinessReason::LoginExpired)),
    ] {
        let mut first = availability();
        if missing_key {
            first.account_key = None;
        }
        if stale {
            first.expires_unix_seconds = 90;
        }
        let second = AvailabilityRecord {
            adapter: "claude".into(),
            ..availability()
        };
        let records = [first, second];
        let readiness = |scope: &provenant_quota::ReadinessScope, _: i64| {
            if scope.adapter != "codex" || login.is_none() {
                return ReadinessRead::absent();
            }
            ReadinessRead {
                observed_scope: Some(scope.clone()),
                state: ReadinessState::Unavailable,
                reason: login,
                observed_unix_seconds: Some(90),
                expires_unix_seconds: Some(110),
                reset_unix_seconds: None,
                evidence_ref: Some("fixture".into()),
            }
        };
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &records;
        ctx.access = &access;
        ctx.readiness = Some(&readiness);
        let selected = resolve(&query, &ctx)?;
        assert_eq!(selected.adapter, "claude");
        let receipt = selected.pool_selection.ok_or("receipt")?;
        assert!(receipt.candidates[0].exclusion.is_some());
        // No ready alternative must not resurrect a known unusable account.
        let mut only_first: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pools": {"trial": [{"model":"codex/gpt-6-luna", "weight":1}]}
        }))?;
        only_first.launch_policy = config.launch_policy.clone();
        ctx.config = &only_first;
        let expected = if missing_key {
            RouteExclusionReason::UnknownAccount
        } else if login == Some(ReadinessReason::SignedOut) {
            RouteExclusionReason::AuthRequired
        } else {
            RouteExclusionReason::LoginExpired
        };
        assert!(matches!(
            resolve(&query, &ctx),
            Err(RoutingError::NoEligiblePoolMember { cause: Some(reason), .. }) if reason == expected
        ));
    }
    // Spec: docs/specs/routing.md#unknown-never-means-ready
    // An expired sign-out and a configured exclusion still form a typed exhausted pool.
    let mixed = config.clone();
    let excluded = [rule(ExclusionSelector::Provider("claude".into()))];
    let records = [
        availability(),
        AvailabilityRecord {
            adapter: "claude".into(),
            ..availability()
        },
    ];
    let mut file = provenant_quota::ReadinessFile::default();
    let mut account = provenant_quota::AccountRecord::empty(provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: "codex".into(),
        account_key: records[0].account_key.clone().ok_or("fixture account")?,
        label: "work".into(),
    });
    file.sync_probe_result(
        &account,
        provenant_quota::LOCAL_HOST,
        10,
        30,
        Some(ReadinessReason::SignedOut),
    )?;
    let readiness = |scope: &provenant_quota::ReadinessScope, now| file.get(scope, now);
    let mut ctx = context(&catalogue, &mixed, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.readiness = Some(&readiness);
    assert!(matches!(
        resolve_for_account_with_seed_excluding(&query, &ctx, None, "mixed", 0, &[], &excluded),
        Err(RoutingError::NoEligiblePoolMember {
            preparation_failure: provenant_records::run::PreparationFailure::AdapterUnavailable,
            ..
        })
    ));
    account.auth_state = Some(provenant_quota::AuthState {
        signed_in: Some(true),
        observed_unix_seconds: Some(80),
        ..Default::default()
    });
    file.sync_probe_result(&account, provenant_quota::LOCAL_HOST, 80, 30, None)?;
    let readiness = |scope: &provenant_quota::ReadinessScope, now| file.get(scope, now);
    let mut ctx = context(&catalogue, &mixed, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.readiness = Some(&readiness);
    assert_eq!(
        resolve_for_account_with_seed_excluding(&query, &ctx, None, "mixed", 0, &[], &excluded)?
            .adapter,
        "codex"
    );
    // Different login failures still have the same preparation fallback class.
    let records = [
        availability(),
        AvailabilityRecord {
            adapter: "claude".into(),
            ..availability()
        },
    ];
    let readiness = |scope: &provenant_quota::ReadinessScope, _: i64| ReadinessRead {
        observed_scope: Some(scope.clone()),
        state: ReadinessState::Unavailable,
        reason: Some(if scope.adapter == "codex" {
            ReadinessReason::SignedOut
        } else {
            ReadinessReason::LoginExpired
        }),
        observed_unix_seconds: Some(90),
        expires_unix_seconds: Some(110),
        reset_unix_seconds: None,
        evidence_ref: Some("fixture".into()),
    };
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.readiness = Some(&readiness);
    assert!(matches!(resolve(&query, &ctx),
        Err(RoutingError::NoEligiblePoolMember { cause: Some(reason), .. })
            if crate::FallbackOutcome::from_exclusion(&reason) == crate::FallbackOutcome::AuthRequired
    ));
    Ok(())
}
