use super::*;

#[test]
fn model_policy_config_accepts_layered_rules() -> Result<(), Box<dyn std::error::Error>> {
    let config = serde_json::from_value::<RoutingConfig>(serde_json::json!({
        "model_policy": {"rules": [{"model": "codex/gpt-6.1-terra", "account": "work",
            "schedule": {"expires":"1970-01-01T00:03:20Z"}, "policy": {"lane": {"stop_early_ok": false}}}]}
    }));
    assert!(
        config.is_ok(),
        "layered model policy must be accepted: {config:?}"
    );
    assert!(config?.validate().is_empty());
    Ok(())
}

#[test]
fn configured_read_only_route_refuses_writes() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let default_config = RoutingConfig::default();
    let default_context = context(&catalogue, &default_config, now()?);
    for model in [
        "agy/gemini-3.8-flash",
        "opencode/opencode/glm-5.3-flash-free",
    ] {
        let mut writer = request(model)?;
        writer.intent.writes = true;
        let resolved = resolve(&writer, &default_context);
        assert!(resolved.is_ok(), "{model}: {resolved:?}");
    }
    let mut query = request("agy/gemini-3.8-flash")?;
    query.intent.writes = true;

    let config: RoutingConfig = toml_edit::de::from_str(
        "[[model_policy.rules]]\nmodel='agy/gemini-3.8-flash'\n[model_policy.rules.policy.lane]\nread_only=true\n",
    )?;
    let context = context(&catalogue, &config, now()?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::ReadOnlyPolicy)
    ));
    Ok(())
}

#[test]
fn account_policy_filters_accounts_before_selecting_a_writer()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = toml_edit::de::from_str(
        "[[model_policy.rules]]\naccount='codex/review'\n[model_policy.rules.policy.lane]\nread_only=true\n",
    )?;
    let records = [
        AvailabilityRecord {
            account_key: Some("work".into()),
            ..availability()
        },
        AvailabilityRecord {
            account_key: Some("review".into()),
            ..availability()
        },
    ];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let context = RoutingContext {
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let mut request = request("codex/gpt-6-luna")?;
    request.intent.writes = true;
    let selected = resolve(&request, &context)?;
    assert_eq!(selected.account_key.as_deref(), Some("work"));
    assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));
    Ok(())
}

#[test]
fn spent_account_is_skipped_for_an_available_one_even_when_its_reading_is_stale()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [
        AvailabilityRecord {
            account_key: Some("work".into()),
            ..availability()
        },
        AvailabilityRecord {
            account_key: Some("review".into()),
            ..availability()
        },
    ];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let spent = |expires: i64, reset: i64| -> Result<QuotaObservation, RoutingError> {
        let mut reading = quota(
            "codex/gpt-6-luna",
            "review",
            QuotaStatus::Exhausted,
            1,
            50,
            expires,
        )?;
        reading.snapshot.windows = vec![QuotaWindow {
            pool: "secondary".into(),
            window_seconds: Some(604_800),
            used_basis_points: 10_000,
            resets_unix_seconds: Some(reset),
        }];
        Ok(reading)
    };
    let selected = |quota: &[QuotaObservation], availability: &[AvailabilityRecord]| {
        let context = RoutingContext {
            availability,
            access: &access,
            quota,
            ..context(&catalogue, &config, now()?)
        };
        let route = resolve(&request("codex/gpt-6-luna")?, &context)?;
        let ready = matches!(route.readiness, RouteReadiness::Ready { .. });
        Ok::<_, Box<dyn std::error::Error>>((route.account_key, ready))
    };
    // With no evidence the tie goes to `review`, so each skip below is the rule.
    assert_eq!(selected(&[], &records)?, (Some("review".into()), true));
    // Fresh or stale, a spent reading loses to an account with no such evidence.
    for reading in [spent(120, 10_000)?, spent(60, 10_000)?] {
        assert_eq!(selected(&[reading], &records)?, (Some("work".into()), true));
    }
    // Once the spent window has reset, the reading no longer counts.
    assert_eq!(
        selected(&[spent(60, 90)?], &records)?,
        (Some("review".into()), true)
    );
    // Stale evidence never refuses the only account; fresh evidence does.
    assert_eq!(
        selected(&[spent(60, 10_000)?], &records[1..])?,
        (Some("review".into()), true)
    );
    assert_eq!(
        selected(&[spent(120, 10_000)?], &records[1..])?,
        (Some("review".into()), false)
    );
    Ok(())
}

#[test]
fn valid_list_contains_only_resolvable_recorded_pairs() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let listed = list(
        &RouteListQuery {
            valid: true,
            adapter: Some("codex".into()),
            pool: None,
        },
        &context,
    )?;
    assert!(!listed.valid_pairs.is_empty());
    assert!(listed.models.is_empty() && listed.pools.is_empty() && listed.named_routes.is_empty());
    assert!(
        !listed
            .valid_pairs
            .iter()
            .any(|pair| pair.model.as_str() == "codex/gpt-6.1-luna")
    );
    for pair in listed.valid_pairs {
        assert_eq!(pair.model.adapter(), "codex");
        let mut request = request(pair.model.as_str())?;
        request.effort = pair.effort;
        assert_eq!(
            resolve(&request, &context)?.eligibility,
            RouteEligibility::Eligible
        );
    }
    Ok(())
}

#[test]
fn read_heavy_routing_records_cache_preference_without_forcing_edit_work()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = toml_edit::de::from_str(
        "[pools]\ncachetest=[{model='opencode/opencode-go/deepseek-v4.1-flash',weight=1},{model='codex/gpt-6-luna',weight=1}]\n",
    )?;
    let records: Vec<_> = [
        ("codex", "gpt-6-luna"),
        ("opencode", "opencode-go/deepseek-v4.1-flash"),
    ]
    .into_iter()
    .map(|(adapter, model)| AvailabilityRecord {
        adapter: adapter.into(),
        observed_models: vec![model.into()],
        ..availability()
    })
    .collect();
    let access = [
        AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        },
        AccessContext {
            adapter: "opencode",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        },
    ];
    let context = RoutingContext {
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    for (workload, expected) in [
        (Workload::General, 10_000),
        (Workload::ReadHeavy, 30_000),
        (Workload::Review, 30_000),
        (Workload::EditHeavy, 10_000),
    ] {
        let request = RouteRequest {
            intent: RouteIntent {
                workload,
                writes: false,
                ..Default::default()
            },
            choice: RouteChoice::NamedRoute(RouteName::parse("cachetest")?),
            effort: None,
            confidential: false,
            fallback: Fallback::None,
        };
        let route = resolve_with_seed(&request, &context, "cache-test", 0)?;
        let receipt = route.pool_selection.ok_or("pool receipt")?;
        assert_eq!(receipt.candidates[0].factor_basis_points, expected);
        assert_eq!(receipt.candidates[1].factor_basis_points, 10_000);
        assert_eq!(receipt.total_weight, u64::from(expected) + 10_000);
        if expected == 30_000 {
            assert!(
                receipt.candidates[0]
                    .pacing_detail
                    .as_deref()
                    .is_some_and(|reason| reason.contains("cheap-cache read-heavy preference"))
            );
        }
    }
    Ok(())
}

#[test]
fn effort_errors_report_recorded_controls_and_nearest_models()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    for (model, expected) in [
        ("codex/gpt-6-luna", "supported efforts: low"),
        ("opencode/opencode-go/glm-5.3-flash", "takes no effort"),
    ] {
        let mut query = request(model)?;
        query.effort = Some("unsupported".into());
        let error = resolve(&query, &context).expect_err("effort must be refused");
        assert_eq!(error.code(), "unsupported_effort");
        let RoutingError::UnsupportedEffort {
            model: refused_model,
            effort,
            support,
        } = error
        else {
            unreachable!();
        };
        assert_eq!(refused_model, model);
        assert_eq!(effort, "unsupported");
        assert!(support.contains(expected), "{support}");
    }
    assert_eq!(
        catalogue
            .nearest_models(&ModelId::parse("codex/gpt-6.1-luna")?)
            .first()
            .map(String::as_str),
        Some("codex/gpt-6-luna")
    );
    Ok(())
}

#[test]
fn pacing_evaluator_applies_overnight_weekday_and_sunday_windows()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let model = ModelId::parse("codex/gpt-6.1-terra")?;
    for (weekday, day) in [(vec![3_u8], 0_i64), (vec![6_u8], 3_i64)] {
        for hour in [23_i64, 24 + 1] {
            let now = day * 86_400 + hour * 3600;
            let config: RoutingConfig = serde_json::from_value(serde_json::json!({
                "pacing_overrides": [{"provider":"codex", "policy":"spend", "schedule":{"windows":[{"weekdays":weekday,"start_hour":23,"end_hour":2}]}}]
            }))?;
            let context = context(
                &catalogue,
                &config,
                provenant_core::WallTime::from_micros(now * 1_000_000)?,
            );
            assert_eq!(
                pacing_preview(&model, &context)[0].policy,
                PacingPolicy::Spend
            );
        }
    }
    Ok(())
}

#[test]
fn pacing_override_schedules_validate_at_load_and_need_no_expiry()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_config::DomainConfig;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing_overrides": [
            {"provider":"codex", "policy":"spend", "schedule":{"windows":[{"weekdays":[0,1,2,3,4],"start_hour":23,"end_hour":7}]}},
            {"provider":"claude", "policy":"max"},
            {"provider":"agy", "policy":"off", "schedule":{"tz":"Mars/Olympus_Mons"}},
            {"provider":"cursor", "policy":"pace", "schedule":{"starts":"1970-01-01T00:00:05Z","expires":"1970-01-01T00:00:05Z"}}
        ]
    }))?;
    let errors: Vec<_> = config
        .validate()
        .into_iter()
        .map(|error| (error.field, error.code))
        .collect();
    assert_eq!(
        errors,
        [
            ("pacing_overrides.2".to_owned(), "schedule"),
            ("pacing_overrides.3".to_owned(), "schedule"),
        ]
    );
    Ok(())
}

#[test]
fn nearest_models_respect_namespace_and_distance() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let nearest = |id: &str| -> Result<Vec<String>, RoutingError> {
        Ok(catalogue.nearest_models(&ModelId::parse(id)?))
    };
    assert_eq!(
        nearest("opencode/opencode-go/glm-5.3-flashh")?,
        vec!["opencode/opencode-go/glm-5.3-flash"]
    );
    assert!(nearest("opencode/openrouter/~openai/claude-haiku-latest")?.is_empty());
    assert!(
        nearest("opencode/opencode-go/claude-haiku-latest")?
            .iter()
            .all(|id| id.starts_with("opencode/opencode-go/"))
    );
    assert!(nearest("codex/completely-unrelated-model")?.is_empty());
    Ok(())
}

#[test]
fn opus_advisory_uses_the_applied_pool_tier() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    for (model, alternative, tier) in [
        (
            "claude/claude-opus-5-5",
            "codex/gpt-6.1-sol effort=high",
            "principal",
        ),
        (
            "agy/claude-opus-4-6-thinking",
            "codex/gpt-6.1-sol effort=medium",
            "senior",
        ),
    ] {
        let route = resolve(&request(model)?, &context)?;
        let advisory = config
            .opus_writer_advisory(&catalogue, &route)
            .ok_or("missing advisory")?;
        assert!(advisory.contains(model), "{advisory}");
        assert!(advisory.contains(alternative), "{advisory}");
        assert!(advisory.contains(tier), "{advisory}");
        let quiet = RoutingConfig {
            suppress_opus_writer_warning: true,
            ..RoutingConfig::default()
        };
        assert!(quiet.opus_writer_advisory(&catalogue, &route).is_none());
    }
    // The same Opus model belongs to principal and design. A pool receipt
    // takes precedence over the inference used for an exact-model request.
    let mut named = request("claude/claude-opus-5-5")?;
    named.choice = RouteChoice::NamedRoute(RouteName::parse("design")?);
    let mut route = resolve(&named, &context)?;
    route.adapter = "claude".into();
    route.model = "claude-opus-5-5".into();
    let advisory = config
        .opus_writer_advisory(&catalogue, &route)
        .ok_or("missing design advisory")?;
    assert!(advisory.contains("design"), "{advisory}");
    assert!(advisory.contains("claude/claude-sonnet-5-5"), "{advisory}");
    Ok(())
}

#[test]
fn opus_only_pool_still_advises_a_cheaper_tier_member() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let mut config = RoutingConfig::default();
    for pool in ["principal", "custom"] {
        config.pools.insert(
            pool.into(),
            vec![RoutePoolMember {
                model: ModelId::parse("claude/claude-opus-5-5")?,
                weight: 1,
                effort: None,
                efforts: vec![],
                free: false,
            }],
        );
        let mut query = request("claude/claude-opus-5-5")?;
        query.choice = RouteChoice::NamedRoute(RouteName::parse(pool)?);
        let route = resolve(&query, &context(&catalogue, &config, now()?))?;
        let advisory = config
            .opus_writer_advisory(&catalogue, &route)
            .ok_or("missing advisory for Opus-only pool")?;
        assert!(
            advisory.contains("principal route codex/gpt-6.1-sol"),
            "{advisory}"
        );
    }
    Ok(())
}

#[test]
fn model_selector_keeps_the_entire_provider_path() -> Result<(), RoutingError> {
    let id = ModelId::parse("opencode/openrouter/~anthropic/claude-haiku-latest")?;
    assert_eq!(id.adapter(), "opencode");
    assert_eq!(id.model(), "openrouter/~anthropic/claude-haiku-latest");
    for text in [
        "",
        "codex/",
        "/id",
        "codex",
        "codex/id\n",
        "codex/id#high",
        "codex/id,other",
    ] {
        assert!(ModelId::parse(text).is_err(), "{text:?}");
    }
    assert!(ModelId::parse(&format!("codex/{}", "x".repeat(513))).is_err());
    Ok(())
}

#[test]
fn route_names_validate_at_the_string_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let wire = r#"{"intent":{"workload":"general","writes":false},"choice":{"kind":"named_route","value":"review.v1-_2"},"effort":null,"confidential":false,"fallback":{"kind":"none"}}"#;
    let query: RouteRequest = serde_json::from_str(wire)?;
    let roundtrip: RouteRequest = serde_json::from_str(&serde_json::to_string(&query)?)?;
    assert_eq!(roundtrip, query);
    assert_eq!(query.intent.network, None);
    assert_eq!(
        query.choice,
        RouteChoice::NamedRoute(RouteName::parse("review.v1-_2")?)
    );
    let longest = "x".repeat(64);
    assert_eq!(RouteName::parse(&longest)?.as_str(), longest);
    for text in ["", "bad name", "bad/name", "bad\n", "é", &"x".repeat(65)] {
        assert_eq!(RouteName::parse(text).unwrap_err().code(), "invalid_value");
        let wire = serde_json::json!({"kind": "named_route", "value": text});
        assert!(serde_json::from_value::<RouteChoice>(wire).is_err());
    }
    for value in [
        serde_json::json!({"name": "review"}),
        serde_json::json!(12),
        serde_json::Value::Null,
    ] {
        assert!(
            serde_json::from_value::<RouteChoice>(serde_json::json!({
                "kind": "named_route", "value": value
            }))
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn exact_unknown_is_retained_but_requires_availability() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let route = resolve(&request("codex/unlisted-exact-id")?, &context)?;
    assert_eq!(route.model, "unlisted-exact-id");
    assert_eq!(route.effort, None);
    assert_eq!(route.eligibility, RouteEligibility::UnverifiedModel);
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability,
            ..
        }
    ));
    assert_eq!(route.exclusions.len(), 1);
    assert!(route.alternatives.as_slice().is_empty());
    let mut with_effort = request("codex/unlisted-exact-id")?;
    with_effort.effort = Some("high".into());
    assert_eq!(
        resolve(&with_effort, &context)?.effort.as_deref(),
        Some("high")
    );
    with_effort.effort = Some("invalid effort".into());
    assert!(matches!(
        resolve(&with_effort, &context),
        Err(RoutingError::UnsupportedEffort { .. })
    ));
    assert!(matches!(
        resolve(&request("invented/id")?, &context),
        Err(RoutingError::UnknownAdapter)
    ));
    Ok(())
}

#[test]
fn explicit_catalogued_model_is_ready_without_provider_listing()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let mut record = availability();
    record.adapter = "opencode".into();
    record.account_key = Some("zen".into());
    record.observed_models.clear();
    let records = [record];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    context.access = &access;
    let query = request("opencode/opencode/glm-5.3-flash-free")?;
    let route = resolve(&query, &context)?;
    assert_eq!(route.model, "opencode/glm-5.3-flash-free");
    assert_eq!(route.eligibility, RouteEligibility::Eligible);
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert!(route.exclusions.is_empty());
    let mut with_effort = query.clone();
    with_effort.effort = Some("high".into());
    assert!(matches!(
        resolve(&with_effort, &context),
        Err(RoutingError::UnsupportedEffort { .. })
    ));
    let mut confidential = query;
    confidential.confidential = true;
    assert!(resolve(&confidential, &context).is_ok());
    Ok(())
}

#[test]
fn explicit_unlisted_opencode_models_and_fallbacks_are_ready()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let mut record = availability();
    record.adapter = "opencode".into();
    record.account_key = Some("zen".into());
    record.observed_models.clear();
    let mut go = record.clone();
    go.account_key = Some("go".into());
    let records = [record, go];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    context.access = &access;
    for id in [
        "opencode/opencode/beam-free",
        "opencode/opencode-go/glm-5.2",
    ] {
        let route = resolve(&request(id)?, &context)?;
        assert_eq!(format!("{}/{}", route.adapter, route.model), id);
        assert_eq!(route.eligibility, RouteEligibility::UnverifiedModel);
        assert!(
            matches!(route.readiness, RouteReadiness::Ready { .. }),
            "{id}: {:?}",
            route.readiness
        );
    }
    let mut fallback = request("codex/gpt-6-luna")?;
    fallback.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse(
        "opencode/opencode/beam-free",
    )?])?);
    let route = resolve(&fallback, &context)?;
    let alternative = &route.alternatives.as_slice()[0];
    assert_eq!(alternative.model, "opencode/beam-free");
    assert_eq!(alternative.eligibility, RouteEligibility::UnverifiedModel);
    assert!(matches!(
        alternative.readiness,
        RouteReadiness::Ready { .. }
    ));
    Ok(())
}

#[test]
fn named_routes_reject_unlisted_models_even_when_provider_observed()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig {
        routes: BTreeMap::from([(
            "review".into(),
            NamedRoute {
                model: ModelId::parse("codex/test")?,
                effort: None,
            },
        )]),
        ..RoutingConfig::default()
    };
    let records = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    context.access = &access;
    let mut query = request("codex/test")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("review")?);
    assert!(
        config
            .validate()
            .iter()
            .any(|error| { error.field == "routes.review.model" && error.code == "unknown_model" })
    );
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::InvalidValue)
    ));
    Ok(())
}

#[test]
fn model_default_effort_uses_agreeing_configured_routes() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    let model = ModelId::parse("codex/gpt-6.1-terra")?;
    let mut config = RoutingConfig::default();
    config.routes.insert(
        "writer".into(),
        NamedRoute {
            model: model.clone(),
            effort: Some("high".into()),
        },
    );
    let effort = catalogue.model_default_effort(&config, &model);
    assert_eq!(effort.as_deref(), Some("high"));
    let mut query = request("codex/gpt-6.1-terra")?;
    query.effort = effort;
    assert_eq!(
        resolve(&query, &context(&catalogue, &config, now()?))?
            .effort
            .as_deref(),
        Some("high")
    );
    config.routes.insert(
        "other".into(),
        NamedRoute {
            model: model.clone(),
            effort: Some("low".into()),
        },
    );
    assert_eq!(catalogue.model_default_effort(&config, &model), None);
    Ok(())
}

#[test]
fn named_routes_are_exact_with_explicit_effort_override_and_no_default()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let model = ModelId::parse("codex/gpt-6.1-sol")?;
    let config = RoutingConfig {
        routes: BTreeMap::from([(
            "review".into(),
            NamedRoute {
                model,
                effort: Some("low".into()),
            },
        )]),
        ..RoutingConfig::default()
    };
    let context = context(&catalogue, &config, now()?);
    let mut query = RouteRequest {
        intent: Default::default(),
        choice: RouteChoice::NamedRoute(RouteName::parse("review")?),
        effort: None,
        confidential: false,
        fallback: Fallback::None,
    };
    assert_eq!(resolve(&query, &context)?.effort.as_deref(), Some("low"));
    query.effort = Some("high".into());
    assert_eq!(resolve(&query, &context)?.effort.as_deref(), Some("high"));
    query.effort = Some("invented".into());
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnsupportedEffort { .. })
    ));
    query.choice = RouteChoice::NamedRoute(RouteName::parse("typo")?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnknownRoute)
    ));
    Ok(())
}

#[test]
fn bundled_codex_tiers_resolve_with_pool_specific_effort_controls()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut checked = 0;
    for (pool, members) in &catalogue.pools {
        for member in members
            .iter()
            .filter(|member| member.model.adapter() == "codex")
        {
            checked += 1;
            if let Some(default) = &member.effort {
                assert!(
                    member.efforts.contains(default),
                    "{pool}: {}",
                    member.model.as_str()
                );
            }
            let entry = catalogue.model(&member.model).ok_or("model missing")?;
            assert!(
                member
                    .efforts
                    .iter()
                    .all(|effort| entry.efforts.contains(effort))
            );
            let mut isolated = catalogue.clone();
            isolated.pools.insert(pool.clone(), vec![member.clone()]);
            let mut record = availability();
            record.observed_models = vec![member.model.model().into()];
            let records = [record];
            let mut context = context(&isolated, &config, now()?);
            context.availability = &records;
            context.access = &access;
            let mut query = request(member.model.as_str())?;
            query.choice = RouteChoice::NamedRoute(RouteName::parse(pool)?);
            query.effort = None;
            let resolved = resolve_with_seed(&query, &context, "codex-tier", 0)?;
            assert_eq!(resolved.model, member.model.model());
            assert_eq!(resolved.effort.as_ref(), member.effort.as_ref());
            for effort in &member.efforts {
                query.effort = Some(effort.clone());
                let resolved = resolve_with_seed(&query, &context, "codex-tier", 0)?;
                assert_eq!(resolved.model, member.model.model());
                assert_eq!(resolved.effort.as_deref(), Some(effort.as_str()));
            }
        }
    }
    assert!(checked > 0, "bundled pools should contain Codex models");
    Ok(())
}
