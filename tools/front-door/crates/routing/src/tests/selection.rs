use super::*;

#[test]
fn table_driven_weighted_pools_record_effort_readiness_roll_and_seed()
-> Result<(), Box<dyn std::error::Error>> {
    struct Case {
        name: &'static str,
        effort: &'static str,
        weights: [u64; 3],
        ready_adapters: &'static [&'static str],
        expected_pick: Option<&'static str>,
        expect_effort_exclusion: bool,
        codex_pacing: &'static str,
        quota_used_basis_points: Option<u64>,
        quota_reset_after_seconds: Option<i64>,
        alternative_quota_used_basis_points: Option<u64>,
        include_weekly_window: bool,
        expected_codex_factor: Option<u32>,
        redraw: bool,
    }
    let cases = [
        Case {
            name: "weighted ready members",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            codex_pacing: "spend",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            redraw: false,
        },
        Case {
            name: "weight zero fallback",
            effort: "high",
            weights: [4, 4, 0],
            ready_adapters: &["agy"],
            expected_pick: Some("gemini-3.8-flash"),
            expect_effort_exclusion: false,
            codex_pacing: "max",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            redraw: false,
        },
        Case {
            name: "caller effort exclusion",
            effort: "xhigh",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: true,
            codex_pacing: "spend",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            redraw: false,
        },
        Case {
            name: "pace clamps to minimum factor",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            codex_pacing: "pace",
            quota_used_basis_points: Some(9_900),
            quota_reset_after_seconds: Some(18_000),
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(2_500),
            redraw: false,
        },
        Case {
            name: "pace keeps weight at half headroom and half time",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            codex_pacing: "pace",
            quota_used_basis_points: Some(5_000),
            quota_reset_after_seconds: Some(9_000),
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            redraw: false,
        },
        Case {
            name: "pace uses the limiting weekly factor",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            codex_pacing: "pace",
            quota_used_basis_points: Some(5_000),
            quota_reset_after_seconds: Some(4_500),
            alternative_quota_used_basis_points: Some(9_900),
            include_weekly_window: true,
            expected_codex_factor: Some(9_000),
            redraw: false,
        },
        Case {
            name: "same-seed redraw removes the rejected member",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            codex_pacing: "spend",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            redraw: true,
        },
        Case {
            name: "unknown pace snapshot is neutral",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            codex_pacing: "pace",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            redraw: false,
        },
    ];
    let registry = [
        provenant_providers::Registration {
            quota_source: None,
            ..provenant_providers::codex::REGISTRATION
        },
        provenant_providers::Registration {
            quota_source: None,
            ..provenant_providers::claude::REGISTRATION
        },
        provenant_providers::Registration {
            quota_source: None,
            ..provenant_providers::agy::REGISTRATION
        },
    ];
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
        AccessContext {
            adapter: "agy",
            disclosure_ref: "policy",
            confinement_ref: "grant",
        },
    ];
    let catalogue = Catalogue::bundled()?;
    let mut base_availability = vec![availability()];
    for adapter in ["claude", "agy"] {
        let mut record = availability();
        record.adapter = adapter.into();
        base_availability.push(record);
    }

    for case in cases {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "routes": {},
            "pools": { "trial": [
                {"model":"codex/gpt-6-luna", "weight":case.weights[0], "effort":"high", "efforts":["medium","high","xhigh"]},
                {"model":"claude/claude-haiku-5-5", "weight":case.weights[1], "effort":"high", "efforts":["medium","high","xhigh"]},
                {"model":"agy/gemini-3.8-flash", "weight":case.weights[2], "effort":"medium", "efforts":["low","medium","high"]}
            ]},
            "pacing": {"codex":case.codex_pacing}
        }))?;
        let availability: Vec<_> = base_availability
            .iter()
            .filter(|record| case.ready_adapters.contains(&record.adapter.as_str()))
            .cloned()
            .collect();
        let quota = if let (Some(used_basis_points), Some(reset_after_seconds)) =
            (case.quota_used_basis_points, case.quota_reset_after_seconds)
        {
            let mut observation = quota(
                "codex/gpt-6-luna",
                "opaque-account",
                provenant_quota::QuotaStatus::Available,
                1,
                80,
                120,
            )?;
            observation
                .snapshot
                .windows
                .push(provenant_quota::QuotaWindow {
                    pool: "five_hour".into(),
                    window_seconds: Some(18_000),
                    used_basis_points,
                    resets_unix_seconds: Some(100 + reset_after_seconds),
                });
            if case.include_weekly_window {
                observation
                    .snapshot
                    .windows
                    .push(provenant_quota::QuotaWindow {
                        pool: "seven_day".into(),
                        window_seconds: Some(604_800),
                        used_basis_points: 1_000,
                        resets_unix_seconds: Some(100 + 604_800),
                    });
            }
            let mut observations = vec![observation];
            if let Some(used_basis_points) = case.alternative_quota_used_basis_points {
                let mut alternative = quota(
                    "codex/gpt-6-luna",
                    "other-account",
                    provenant_quota::QuotaStatus::Available,
                    1,
                    80,
                    120,
                )?;
                alternative
                    .snapshot
                    .windows
                    .push(provenant_quota::QuotaWindow {
                        pool: "five_hour".into(),
                        window_seconds: Some(18_000),
                        used_basis_points,
                        resets_unix_seconds: Some(100 + 18_000),
                    });
                observations.push(alternative);
            }
            observations
        } else {
            Vec::new()
        };
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.registry = &registry;
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &quota;
        let mut query = request("codex/gpt-6-luna")?;
        query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
        query.effort = Some(case.effort.into());
        query.confidential = true;

        let first_pick = case
            .redraw
            .then(|| resolve_with_seed(&query, &ctx, "operation-route-pools", 2))
            .transpose()?;
        let excluded_model = first_pick
            .as_ref()
            .map(|route| ModelId::parse(&format!("{}/{}", route.adapter, route.model)))
            .transpose()?;
        let result = if let Some(excluded_model) = &excluded_model {
            resolve_for_account_with_seed_excluding(
                &query,
                &ctx,
                None,
                "operation-route-pools",
                2,
                std::slice::from_ref(excluded_model),
                &[],
            )
        } else {
            resolve_with_seed(&query, &ctx, "operation-route-pools", 2)
        };
        let route = result?;
        let receipt = serde_json::to_value(&route)?["pool_selection"].clone();
        assert_eq!(receipt["pool"], "trial", "{}", case.name);
        assert_eq!(receipt["weight_source"], "config");
        assert_eq!(
            receipt["selection_rule"],
            if case.name == "weight zero fallback" {
                "first_zero_weight_ready"
            } else {
                "weighted_draw"
            },
            "{}",
            case.name
        );
        let candidate_rows = receipt["candidates"].as_array().ok_or("candidates")?;
        assert_eq!(candidate_rows.len(), 3);
        assert!(
            candidate_rows
                .iter()
                .filter_map(|candidate| candidate["pacing_detail"].as_str())
                .all(|detail| detail.contains("source=") && detail.contains("policy="))
        );
        assert_eq!(
            candidate_rows
                .iter()
                .map(|candidate| candidate["weight"].as_u64().ok_or("weight"))
                .collect::<Result<Vec<_>, _>>()?,
            case.weights,
            "{}",
            case.name
        );
        assert!(receipt["roll"].as_u64().is_some());
        let mut expected_total = 0_u64;
        for candidate in candidate_rows {
            let factor = candidate["factor_basis_points"].as_u64().ok_or("factor")?;
            assert!((2_500..=20_000).contains(&factor), "{}", case.name);
            if candidate["exclusion"] == "admission rejected" {
                assert!(candidate["readiness"].is_null(), "{}", case.name);
            } else {
                assert_eq!(
                    candidate["readiness"].is_object(),
                    candidate["exclusion"] != "effort",
                    "{}",
                    case.name
                );
            }
            if candidate["exclusion"].is_null() && candidate["readiness"]["status"] == "ready" {
                expected_total += candidate["weight"].as_u64().ok_or("weight")? * factor;
            }
        }
        let codex_candidate = candidate_rows
            .iter()
            .find(|candidate| candidate["model"] == "codex/gpt-6-luna")
            .ok_or("codex candidate")?;
        assert_eq!(
            codex_candidate["factor_basis_points"].as_u64(),
            case.expected_codex_factor.map(u64::from),
            "{}",
            case.name
        );
        if let Some(excluded_model) = &excluded_model {
            let excluded_candidate = candidate_rows
                .iter()
                .find(|candidate| candidate["model"] == excluded_model.as_str())
                .ok_or("excluded candidate")?;
            assert_eq!(excluded_candidate["exclusion"], "admission rejected");
            assert_ne!(
                (&route.adapter, &route.model),
                (
                    &first_pick.as_ref().ok_or("first pick")?.adapter,
                    &first_pick.as_ref().ok_or("first pick")?.model
                )
            );
        }
        assert_eq!(
            receipt["total_weight"].as_u64(),
            Some(expected_total),
            "{}",
            case.name
        );
        assert_eq!(
            receipt["candidates"]
                .as_array()
                .ok_or("candidates")?
                .iter()
                .any(|candidate| candidate["exclusion"] == "effort"),
            case.expect_effort_exclusion,
            "{}",
            case.name
        );
        if let Some(expected) = case.expected_pick {
            assert_eq!(route.model, expected, "{}", case.name);
        } else {
            assert_eq!(
                receipt["pick"],
                format!("{}/{}", route.adapter, route.model)
            );
        }
        let picked = candidate_rows
            .iter()
            .find(|candidate| candidate["model"] == receipt["pick"])
            .ok_or("picked candidate")?;
        if expected_total == 0 {
            assert_eq!(picked["weight"], 0, "{}", case.name);
        } else {
            let roll = receipt["roll"].as_u64().ok_or("roll")?;
            let mut lower = 0_u64;
            let mut matched_interval = false;
            for candidate in candidate_rows.iter().filter(|candidate| {
                candidate["exclusion"].is_null() && candidate["readiness"]["status"] == "ready"
            }) {
                let weight = candidate["weight"].as_u64().ok_or("weight")?;
                if weight == 0 {
                    continue;
                }
                let upper =
                    lower + weight * candidate["factor_basis_points"].as_u64().ok_or("factor")?;
                if candidate["model"] == receipt["pick"] {
                    assert!((lower..upper).contains(&roll), "{}", case.name);
                    matched_interval = true;
                }
                lower = upper;
            }
            assert!(matched_interval, "{}", case.name);
        }
        if case.name == "weighted ready members" {
            let repeated = resolve_with_seed(&query, &ctx, "operation-route-pools", 2)?;
            assert_eq!(
                serde_json::to_value(&repeated)?["pool_selection"],
                receipt,
                "same operation and task index must repeat the selection"
            );
            let next_task = resolve_with_seed(&query, &ctx, "operation-route-pools", 3)?;
            assert_ne!(
                serde_json::to_value(&next_task)?["pool_selection"]["roll"],
                receipt["roll"],
                "task index participates in the roll"
            );
        }
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
