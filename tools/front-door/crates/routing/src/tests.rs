use super::*;
use provenant_config::{DomainConfig, DomainSections};
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{ResolvedRoute, RouteEligibility, RouteExclusionReason, RouteReadiness};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn resolve_for_account(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
) -> Result<ResolvedRoute, RoutingError> {
    resolve_for_account_with_seed(request, context, account, "route-preview", 0)
}

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

fn now() -> Result<provenant_core::WallTime, Box<dyn std::error::Error>> {
    Ok(provenant_core::WallTime::from_micros(100_000_000)?)
}
fn context<'a>(
    catalogue: &'a Catalogue,
    config: &'a RoutingConfig,
    now: provenant_core::WallTime,
) -> RoutingContext<'a> {
    RoutingContext {
        policy_account_hint: None,
        choice_context: None,
        catalogue,
        config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        launch_specs: None,
        readiness: None,
        access: &[],
        now,
        host: "test-host",
    }
}
fn request(model: &str) -> Result<RouteRequest, RoutingError> {
    Ok(RouteRequest {
        intent: Default::default(),
        choice: RouteChoice::Model(ModelId::parse(model)?),
        effort: None,
        confidential: false,
        fallback: Fallback::None,
    })
}
#[test]
fn effort_errors_report_recorded_controls_and_nearest_models()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    for (model, expected) in [
        ("codex/gpt-6.1-luna", "codex/gpt-6-luna"),
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

/// Zen serves the free OpenCode routes and Go the paid flash routes.
fn opencode_accounts() -> [AvailabilityRecord; 2] {
    ["zen", "go"].map(|key| AvailabilityRecord {
        adapter: "opencode".into(),
        account_key: Some(key.into()),
        ..availability()
    })
}
fn opencode_access() -> [AccessContext<'static>; 1] {
    [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }]
}
/// The junior pool resolved under a range of seeds, so every member gets drawn.
fn junior_draws(
    query: &RouteRequest,
    ctx: &RoutingContext<'_>,
) -> Result<Vec<ResolvedRoute>, Box<dyn std::error::Error>> {
    let mut routes = Vec::new();
    for task_index in 0..16 {
        routes.push(resolve_with_seed(query, ctx, "junior-free", task_index)?);
    }
    Ok(routes)
}
fn picked_model(route: &ResolvedRoute) -> Result<ModelId, RoutingError> {
    ModelId::parse(&format!("{}/{}", route.adapter, route.model))
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
            Err("unsupported effort"),
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
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let mut query = request("codex/test")?;
    query.effort = Some("high".into());
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("agy/unknown")?])?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnsupportedEffort { .. })
    ));
    Ok(())
}
fn availability() -> AvailabilityRecord {
    AvailabilityRecord {
        schema_version: 1,
        adapter: "codex".into(),
        executable: "/fixture/codex".into(),
        version: "fixture".into(),
        last_update: None,
        fixture_ref: "test".into(),
        catalogue_revision: Catalogue::bundled().unwrap().revision,
        account_key: Some("opaque-account".into()),
        observed_models: vec!["test".into()],
        observed_unix_seconds: 80,
        expires_unix_seconds: 120,
    }
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
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nread_only=true\nmax_continuations=9\n",
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\n[domains.routing.model_policy.rules.policy.lane]\nread_only=true\nmax_continuations=9\n",
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
        assert_ne!(policy.lane.max_continuations, Some(9), "{text}");
        assert!(!policy.permits(write), "{text}");
        assert_eq!(config.warnings.len(), 1, "{text}");
        assert!(
            config.warnings[0]
                .message
                .contains(".lane.max_continuations:"),
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

fn reason(route: &ResolvedRoute) -> Option<&RouteExclusionReason> {
    match &route.readiness {
        RouteReadiness::Unavailable { reason, .. } => Some(reason),
        _ => None,
    }
}
fn quota(
    model: &str,
    account: &str,
    status: provenant_quota::QuotaStatus,
    revision: u64,
    observed: i64,
    expires: i64,
) -> Result<QuotaObservation, RoutingError> {
    Ok(QuotaObservation {
        model: ModelId::parse(model)?,
        expires_unix_seconds: expires,
        snapshot: provenant_quota::QuotaSnapshot {
            schema_version: 1,
            account: provenant_quota::AccountScope {
                adapter: "codex".into(),
                account_key: account.into(),
                label: "test".into(),
            },
            revision,
            status,
            observed_unix_seconds: observed,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![],
        },
    })
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

#[test]
fn past_deadline_account_stands_by_for_an_available_account()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let zone = jiff::tz::TimeZone::system();
    let local = |day| {
        zone.to_ambiguous_zoned(jiff::civil::date(2026, 10, day).at(12, 0, 0, 0))
            .compatible()
            .map(|z| z.timestamp().as_second())
    };
    let now_seconds = local(30)?; // Friday, after Thursday's deadline.
    let reset = local(31)? + 86_400;
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(
        serde_json::json!({"pacing":{"accounts":{"michael":{"policy":"pace","windows":["seven_day"],"deadline":{"reset_of":"anese"}},"anese":{"policy":"pace","windows":["seven_day"],"deadline":{"reset_of":"michael"}}}}}),
    )?;
    let mut michael = availability();
    michael.adapter = "claude".into();
    michael.account_key = Some("michael".into());
    michael.observed_unix_seconds = now_seconds - 10;
    michael.expires_unix_seconds = now_seconds + 100;
    let mut anese = michael.clone();
    anese.account_key = Some("anese".into());
    let records = [michael, anese];
    let model = "claude/claude-haiku-5-5";
    let mut observations = Vec::new();
    for account in ["michael", "anese"] {
        let mut q = quota(
            model,
            account,
            QuotaStatus::Available,
            1,
            now_seconds - 10,
            now_seconds + 100,
        )?;
        q.snapshot.account.adapter = "claude".into();
        q.snapshot.windows = vec![QuotaWindow {
            pool: "seven_day".into(),
            window_seconds: Some(604_800),
            used_basis_points: 2_000,
            resets_unix_seconds: Some(if account == "anese" {
                now_seconds - 5
            } else {
                reset
            }),
        }];
        observations.push(q);
    }
    let registry = [provenant_providers::claude::REGISTRATION];
    let access = [AccessContext {
        adapter: "claude",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut ctx = context(
        &catalogue,
        &config,
        provenant_core::WallTime::from_micros(now_seconds * 1_000_000)?,
    );
    ctx.registry = &registry;
    ctx.availability = &records;
    ctx.access = &access;
    ctx.quota = &observations;
    assert_eq!(
        resolve(&request(model)?, &ctx)?.account_key.as_deref(),
        Some("anese")
    );
    ctx.availability = &records[..1];
    assert_eq!(
        resolve(&request(model)?, &ctx)?.account_key.as_deref(),
        Some("michael")
    );
    Ok(())
}

#[test]
fn pace_reserves_account_headroom_across_windows() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let registry = [provenant_providers::codex::REGISTRATION];
    let availability = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let window = |seconds, used, remaining: i64| QuotaWindow {
        pool: "test".into(),
        window_seconds: Some(seconds),
        used_basis_points: used,
        resets_unix_seconds: Some(100 + remaining),
    };
    for (windows, excluded) in [
        (vec![window(18_000, 9_000, 9_000)], true),
        (
            vec![
                window(18_000, 1_000, 9_000),
                window(604_800, 9_500, 302_400),
            ],
            true,
        ),
        (vec![window(18_000, 5_000, 9_000)], false),
    ] {
        for policy in ["pace", "spend", "max"] {
            let config: RoutingConfig =
                serde_json::from_value(serde_json::json!({"pacing": {"codex": policy}}))?;
            let mut observation = quota(
                "codex/test",
                "opaque-account",
                QuotaStatus::Available,
                1,
                80,
                120,
            )?;
            observation.snapshot.windows = windows.clone();
            let observations = [observation];
            let mut ctx = context(&catalogue, &config, now()?);
            ctx.registry = &registry;
            ctx.availability = &availability;
            ctx.access = &access;
            ctx.quota = &observations;
            let route = resolve(&request("codex/test")?, &ctx)?;
            assert_eq!(
                reason(&route) == Some(&RouteExclusionReason::PacingReserve),
                excluded && policy == "pace"
            );
            if excluded && policy == "pace" {
                assert_eq!(
                    route.exclusions[0].reason,
                    RouteExclusionReason::PacingReserve
                );
                assert!(serde_json::to_string(&route)?.contains("pacing_reserve"));
            }
        }
    }
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing": {"codex": "pace"},
        "pacing_reserve": {"codex": {"five_hour_percent": 5}}
    }))?;
    let mut observation = quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Available,
        1,
        80,
        120,
    )?;
    observation.snapshot.windows = vec![window(18_000, 9_000, 9_000)];
    let observations = [observation];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.registry = &registry;
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.quota = &observations;
    assert!(reason(&resolve(&request("codex/test")?, &ctx)?).is_none());
    let invalid: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing_reserve": {"codex": {"five_hour_percent": 101}}
    }))?;
    assert!(!provenant_config::DomainConfig::validate(&invalid).is_empty());
    Ok(())
}
#[test]
fn availability_access_and_scoped_quota_are_independent_gates()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaStatus;
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let mut context = context(&catalogue, &config, now()?);
    let registry = [provenant_providers::Registration {
        quota_source: None,
        ..provenant_providers::codex::REGISTRATION
    }];
    context.registry = &registry;
    let query = request("codex/test")?;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::UnknownAvailability)
    );
    let mut records = [availability()];
    records[0].expires_unix_seconds = 100;
    context.availability = &records;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::DisclosureDenied)
    );
    let records = [availability()];
    context.availability = &records;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::DisclosureDenied)
    );
    let disclosure = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "",
    }];
    context.access = &disclosure;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::ConfinementUnavailable)
    );
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    context.access = &access;
    let route = resolve(&query, &context)?;
    assert!(reason(&route).is_none());
    assert_eq!(route.confidentiality.evidence_ref, "policy");
    assert_eq!(route.account_key.as_deref(), Some("opaque-account"));
    assert!(matches!(
        resolve(&request("codex/not-observed")?, &context)?.readiness,
        RouteReadiness::Ready { .. }
    ));
    let stale = [quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Exhausted,
        1,
        80,
        99,
    )?];
    context.quota = &stale;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let unknown = [quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Unknown,
        1,
        80,
        120,
    )?];
    context.quota = &unknown;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let other_account = [quota(
        "codex/test",
        "other-account",
        QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?];
    context.quota = &other_account;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let other_model = [quota(
        "codex/other",
        "opaque-account",
        QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?];
    context.quota = &other_model;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let current = [quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?];
    context.quota = &current;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::Exhausted)
    );
    for (expires, expected) in [(120, Some(RouteExclusionReason::AuthRequired)), (99, None)] {
        let auth = [quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::AuthRequired,
            1,
            80,
            expires,
        )?];
        let auth_context = RoutingContext {
            quota: &auth,
            ..context
        };
        assert_eq!(reason(&resolve(&query, &auth_context)?), expected.as_ref());
    }
    let newer_unknown = [
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Exhausted,
            1,
            80,
            120,
        )?,
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Unknown,
            2,
            90,
            120,
        )?,
    ];
    context.quota = &newer_unknown;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let expired_newest = [
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Exhausted,
            1,
            80,
            120,
        )?,
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Exhausted,
            2,
            90,
            99,
        )?,
    ];
    context.quota = &expired_newest;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::Exhausted)
    );
    let reset = [
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Exhausted,
            1,
            80,
            120,
        )?,
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Available,
            2,
            90,
            120,
        )?,
    ];
    context.quota = &reset;
    assert!(
        reason(&resolve(&query, &context)?).is_none(),
        "a newer comparable observation supersedes exhausted"
    );
    Ok(())
}

#[test]
fn exhausted_refusal_lapses_after_every_full_window_resets()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let mut context = context(&catalogue, &config, now()?);
    let records = [availability()];
    context.availability = &records;
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    context.access = &access;
    let query = request("codex/test")?;
    let window = |reset| provenant_quota::QuotaWindow {
        pool: "five_hour".into(),
        window_seconds: None,
        used_basis_points: 10_000,
        resets_unix_seconds: Some(reset),
    };
    let observation = |reset| {
        let mut o = quota(
            "codex/test",
            "opaque-account",
            provenant_quota::QuotaStatus::Exhausted,
            1,
            80,
            10_000,
        )?;
        o.snapshot.windows = vec![window(reset)];
        Ok::<_, RoutingError>(o)
    };
    let at = 100;
    let pending = [observation(at + 60)?];
    context.quota = &pending;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::Exhausted)
    );
    let lapsed = [observation(at - 1)?];
    context.quota = &lapsed;
    assert!(reason(&resolve(&query, &context)?).is_none());
    Ok(())
}

#[test]
fn context_validation_precedes_filtering_and_candidate_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [availability(), availability()];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    let query = request("invented/id")?;
    for result in [
        list(&RouteListQuery::default(), &context).map(|_| ()),
        resolve(&query, &context).map(|_| ()),
        resolve_with_seed(&query, &context, "operation", 0).map(|_| ()),
        resolve_for_account(&query, &context, None).map(|_| ()),
        resolve_for_account(&query, &context, Some("not-registered")).map(|_| ()),
    ] {
        assert!(matches!(result, Err(RoutingError::Availability)));
    }
    for account in ["unknown", "", "bad account", &"x".repeat(65)] {
        assert!(matches!(
            resolve_for_account(&query, &context, Some(account)),
            Err(RoutingError::InvalidValue)
        ));
    }
    Ok(())
}

#[test]
fn distinct_accounts_of_one_adapter_are_valid_resolution_inputs()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let mut second = availability();
    second.account_key = Some("second-account".into());
    let records = [availability(), second];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    let query = request("codex/test")?;
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    context.access = &access;
    let observations = [quota(
        "codex/test",
        "opaque-account",
        provenant_quota::QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?];
    context.quota = &observations;
    // Without an override, the exhausted first registration is skipped.
    let resolved = resolve(&query, &context)?;
    assert_eq!(resolved.account_key.as_deref(), Some("second-account"));
    assert!(matches!(resolved.readiness, RouteReadiness::Ready { .. }));
    let selected = resolve_for_account(&query, &context, Some("second-account"))?;
    assert_eq!(selected.account_key.as_deref(), Some("second-account"));
    assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));
    let back = resolve_for_account(&query, &context, Some("opaque-account"))?;
    assert!(matches!(
        back.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::Exhausted,
            ..
        }
    ));
    let missing = resolve_for_account(&query, &context, Some("not-registered"))?;
    assert!(matches!(
        missing.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability,
            ..
        }
    ));
    Ok(())
}

#[test]
fn account_override_selects_a_pool_member_and_keeps_other_provider_fallbacks()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    for id in ["codex/test", "claude/test"] {
        catalogue.models.push(ModelEntry {
            free: false,
            model: ModelId::parse(id)?,
            efforts: vec![],
            evidence_ref: "synthetic-controls".into(),
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        });
    }
    catalogue.pools.insert(
        "identity-test".into(),
        vec![
            RoutePoolMember {
                model: ModelId::parse("codex/test")?,
                weight: 1,
                effort: None,
                efforts: vec![],
                free: false,
            },
            RoutePoolMember {
                model: ModelId::parse("claude/test")?,
                weight: 1,
                effort: None,
                efforts: vec![],
                free: false,
            },
        ],
    );
    let config = RoutingConfig::default();
    let mut second = availability();
    second.account_key = Some("second-account".into());
    let mut other = availability();
    other.adapter = "claude".into();
    other.account_key = Some("other-provider".into());
    let records = [availability(), second, other];
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
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    context.access = &access;
    let mut query = request("codex/test")?;
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("claude/test")?])?);
    let selected = resolve_for_account(&query, &context, Some("second-account"))?;
    assert_eq!(selected.account_key.as_deref(), Some("second-account"));
    assert_eq!(
        selected.alternatives.as_slice()[0].account_key.as_deref(),
        Some("other-provider")
    );
    query.choice = RouteChoice::NamedRoute(RouteName::parse("identity-test")?);
    query.fallback = Fallback::None;
    let selected = resolve_for_account(&query, &context, Some("second-account"))?;
    assert_eq!(selected.adapter, "codex");
    assert_eq!(selected.account_key.as_deref(), Some("second-account"));
    let selected = resolve_for_account(&query, &context, Some("other-provider"))?;
    assert_eq!(selected.adapter, "claude");
    assert_eq!(selected.account_key.as_deref(), Some("other-provider"));
    Ok(())
}

#[test]
fn probe_adapters_include_only_requested_routes_pools_and_fallbacks()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let mut config = RoutingConfig::default();
    let mut query = request("codex/fixture")?;
    let names = |query: &RouteRequest, config: &RoutingConfig| {
        probe_adapters(query, &catalogue, config, REGISTRY)
    };
    assert_eq!(names(&query, &config)?, ["codex"]);
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("agy/fixture")?])?);
    assert_eq!(names(&query, &config)?, ["codex", "agy"]);
    config.routes.insert(
        "fixture".into(),
        NamedRoute {
            model: ModelId::parse("cursor/fixture")?,
            effort: None,
        },
    );
    query.choice = RouteChoice::NamedRoute(RouteName::parse("fixture")?);
    assert_eq!(names(&query, &config)?, ["cursor", "agy"]);
    // Configured pools take precedence over a named route and bundled pools.
    let member = |name: &str| -> Result<RoutePoolMember, RoutingError> {
        Ok(RoutePoolMember {
            model: ModelId::parse(name)?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        })
    };
    config.pools.insert(
        "fixture".into(),
        vec![member("claude/fixture")?, member("claude/other")?],
    );
    assert_eq!(names(&query, &config)?, ["claude", "agy"]);
    query.fallback = Fallback::None;
    let pool = catalogue
        .pools
        .keys()
        .next()
        .ok_or("bundled pool missing")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse(pool)?);
    let expected: std::collections::BTreeSet<_> = catalogue.pools[pool]
        .iter()
        .map(|member| member.model.adapter())
        .collect();
    assert_eq!(
        names(&query, &config)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    config
        .pools
        .insert(pool.clone(), vec![member("opencode/fixture")?]);
    assert_eq!(names(&query, &config)?, ["opencode"]);
    query.choice = RouteChoice::NamedRoute(RouteName::parse("absent")?);
    assert!(matches!(
        probe_adapters(&query, &catalogue, &config, REGISTRY),
        Err(RoutingError::UnknownRoute)
    ));
    query = request("unknown/fixture")?;
    assert!(names(&query, &config)?.is_empty());
    Ok(())
}

#[test]
fn qualified_pacing_and_reset_references_respect_adapter() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let window = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut matching = quota(model, "shared", QuotaStatus::Available, 1, 80, 120)?;
    matching.snapshot.account.adapter = "claude".into();
    matching.snapshot.windows = vec![QuotaWindow {
        resets_unix_seconds: Some(90),
        ..window.clone()
    }];
    let mut other = quota(model, "shared", QuotaStatus::Available, 1, 90, 120)?;
    other.snapshot.account.adapter = "codex".into();
    other.snapshot.windows = vec![window.clone()];
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({"pacing":{"accounts":{
        "claude/michael":{"policy":"pace", "windows":["seven_day"], "deadline":{"reset_of":"claude/shared", "window":"seven_day"}, "after_deadline":"standby"},
        "codex/michael":{"policy":"off"}
    }}}))?;
    let route = profile_preview(
        config,
        model,
        "michael",
        vec![window],
        vec![matching, other],
    )?;
    assert_eq!(
        route.pool_selection.ok_or("receipt")?.candidates[0].factor_basis_points,
        0
    );
    Ok(())
}

/// Exercise profile decisions through the public resolver and its receipt.
fn profile_preview(
    config: RoutingConfig,
    model: &str,
    account: &str,
    windows: Vec<provenant_quota::QuotaWindow>,
    references: Vec<QuotaObservation>,
) -> Result<ResolvedRoute, Box<dyn std::error::Error>> {
    profile_preview_with_status(
        config,
        model,
        account,
        windows,
        references,
        provenant_quota::QuotaStatus::Available,
    )
}
fn profile_preview_with_status(
    mut config: RoutingConfig,
    model: &str,
    account: &str,
    windows: Vec<provenant_quota::QuotaWindow>,
    mut references: Vec<QuotaObservation>,
    status: provenant_quota::QuotaStatus,
) -> Result<ResolvedRoute, Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let id = ModelId::parse(model)?;
    config.pools.insert(
        "profile-test".into(),
        vec![RoutePoolMember {
            model: id.clone(),
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        }],
    );
    let mut record = availability();
    record.adapter = id.adapter().into();
    record.account_key = Some(account.into());
    let records = [record];
    let access = [AccessContext {
        adapter: id.adapter(),
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut observation = quota(model, account, status, 1, 80, 120)?;
    observation.snapshot.account.adapter = id.adapter().into();
    observation.snapshot.windows = windows;
    references.push(observation);
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.quota = &references;
    let mut query = request(model)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("profile-test")?);
    Ok(resolve(&query, &ctx)?)
}

#[test]
fn reset_reference_uses_fresh_reset_and_reports_unknown_or_stale_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let config = || {
        serde_json::from_value::<RoutingConfig>(serde_json::json!({
            "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["seven_day"],
                "deadline":{"reset_of":"anese"}}}}
        }))
    };
    let own = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut reference = quota(model, "anese", QuotaStatus::Available, 1, 80, 120)?;
    reference.snapshot.account.adapter = "claude".into();
    reference.snapshot.windows = vec![QuotaWindow {
        resets_unix_seconds: Some(302_500),
        ..own.clone()
    }];
    for (references, factor, fallback) in [
        (
            vec![QuotaObservation {
                model: reference.model.clone(),
                expires_unix_seconds: reference.expires_unix_seconds,
                snapshot: reference.snapshot.clone(),
            }],
            10_000,
            false,
        ),
        (vec![], 5_000, true),
        (
            {
                let mut stale = reference;
                stale.expires_unix_seconds = 100;
                vec![stale]
            },
            5_000,
            true,
        ),
    ] {
        let route = profile_preview(config()?, model, "michael", vec![own.clone()], references)?;
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        assert_eq!(candidate.factor_basis_points, factor);
        assert_eq!(
            candidate
                .pacing_detail
                .as_deref()
                .ok_or("detail")?
                .contains("falling back"),
            fallback
        );
    }
    Ok(())
}

#[test]
fn reset_reference_standby_and_continue_after_observed_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let own = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut reference = quota(model, "anese", QuotaStatus::Available, 1, 80, 120)?;
    reference.snapshot.account.adapter = "claude".into();
    reference.snapshot.windows = vec![QuotaWindow {
        resets_unix_seconds: Some(90),
        ..own.clone()
    }];
    for (after, factor) in [("standby", 0), ("continue", 5_000)] {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["seven_day"],
                "deadline":{"reset_of":"anese", "window":"seven_day"}, "after_deadline":after}}}
        }))?;
        let route = profile_preview(
            config,
            model,
            "michael",
            vec![own.clone()],
            vec![QuotaObservation {
                model: reference.model.clone(),
                expires_unix_seconds: reference.expires_unix_seconds,
                snapshot: reference.snapshot.clone(),
            }],
        )?;
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        assert_eq!(candidate.factor_basis_points, factor);
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    Ok(())
}

#[test]
fn selected_windows_cover_pro_cursor_agy_and_free_profiles()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    for (model, account, profile, windows, factor, detail) in [
        (
            "claude/claude-haiku-5-5",
            "pro",
            serde_json::json!({"policy":"pace", "windows":["five_hour"]}),
            vec![
                QuotaWindow {
                    pool: "five_hour".into(),
                    window_seconds: Some(18_000),
                    used_basis_points: 4_000,
                    resets_unix_seconds: Some(18_100),
                },
                QuotaWindow {
                    pool: "seven_day".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 9_500,
                    resets_unix_seconds: Some(604_900),
                },
            ],
            6_000,
            "five_hour",
        ),
        (
            "cursor/grok-4.7",
            "cursor",
            serde_json::json!({"policy":"pace", "windows":["monthly"]}),
            vec![QuotaWindow {
                pool: "billing_cycle".into(),
                window_seconds: Some(2_592_000),
                used_basis_points: 4_000,
                resets_unix_seconds: Some(2_592_100),
            }],
            6_000,
            "billing_cycle",
        ),
        (
            "agy/claude-sonnet-4-6",
            "agy-host",
            serde_json::json!({"policy":"pace", "windows":["seven_day"]}),
            vec![
                QuotaWindow {
                    pool: "gemini.weekly".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 3_000,
                    resets_unix_seconds: Some(604_900),
                },
                QuotaWindow {
                    pool: "claude.weekly".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 4_000,
                    resets_unix_seconds: Some(604_900),
                },
            ],
            6_000,
            "claude.weekly",
        ),
        (
            "opencode/opencode/glm-5.3-flash-free",
            "zen",
            serde_json::json!({"policy":"max"}),
            vec![],
            10_000,
            "source=free",
        ),
    ] {
        let config: RoutingConfig = if account == "zen" {
            serde_json::from_value(serde_json::json!({"pacing":{"free":{"opencode":profile}}}))?
        } else {
            serde_json::from_value(serde_json::json!({"pacing":{"accounts":{account:profile}}}))?
        };
        let route = profile_preview(config, model, account, windows, vec![])?;
        assert!(
            matches!(route.readiness, RouteReadiness::Ready { .. }),
            "{model}"
        );
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        assert_eq!(candidate.factor_basis_points, factor, "{model}");
        assert!(
            candidate
                .pacing_detail
                .as_deref()
                .ok_or("detail")?
                .contains(detail),
            "{model}"
        );
    }
    Ok(())
}

#[test]
fn fixed_deadlines_use_host_local_weekly_monthly_and_absolute_times()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let zone = jiff::tz::TimeZone::system();
    let local = |month, day| {
        zone.to_ambiguous_zoned(jiff::civil::date(1970, month, day).at(0, 0, 0, 0))
            .compatible()
            .map(|z| z.timestamp().as_second())
    };
    for (deadline, selector, duration, reset, target) in [
        (
            "sun 00:00",
            "seven_day",
            604_800_u64,
            local(1, 7)?,
            local(1, 4)?,
        ),
        ("day 25", "monthly", 2_678_400, local(2, 1)?, local(1, 25)?),
        (
            "1970-01-03T00:00", // timebomb: allow fixed epoch fixture, not the live clock
            "seven_day",
            604_800,
            local(1, 7)?,
            local(1, 3)?,
        ),
    ] {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"michael":{"policy":"pace", "windows":[selector],
                "deadline":deadline}}}
        }))?;
        let route = profile_preview(
            config,
            "claude/claude-haiku-5-5",
            "michael",
            vec![QuotaWindow {
                pool: selector.into(),
                window_seconds: Some(duration),
                used_basis_points: 5_000,
                resets_unix_seconds: Some(reset),
            }],
            vec![],
        )?;
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        let expected = (5_000 * duration / u64::try_from(target - 100)?).clamp(2_500, 20_000);
        assert_eq!(
            u64::from(candidate.factor_basis_points),
            expected,
            "{deadline}"
        );
        assert!(
            candidate
                .pacing_detail
                .as_deref()
                .ok_or("detail")?
                .contains(&format!("deadline={target}")),
            "{deadline}"
        );
    }
    Ok(())
}

#[test]
fn refreshed_peer_reset_preserves_handover_in_the_current_account_cycle()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["seven_day"],
            "deadline":{"reset_of":"anese"}}}}
    }))?;
    // Michael's cycle spans [-302300, 302500]. Anese's newly observed next
    // reset is 432100, so the handover in Michael's cycle was at -172700.
    let own = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(302_500),
    };
    let mut peer = quota(model, "anese", QuotaStatus::Available, 1, 80, 120)?;
    peer.snapshot.account.adapter = "claude".into();
    peer.snapshot.windows = vec![QuotaWindow {
        resets_unix_seconds: Some(432_100),
        ..own.clone()
    }];
    let route = profile_preview(config, model, "michael", vec![own], vec![peer])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, 0);
    assert!(
        candidate
            .pacing_detail
            .as_deref()
            .ok_or("detail")?
            .contains("standby")
    );
    Ok(())
}

#[test]
fn account_profile_and_overrides_govern_free_models() -> Result<(), Box<dyn std::error::Error>> {
    let model = "opencode/opencode/glm-5.3-flash-free";
    let blocked = |config: serde_json::Value| -> Result<bool, Box<dyn std::error::Error>> {
        match profile_preview(
            serde_json::from_value(config)?,
            model,
            "zen",
            vec![],
            vec![],
        ) {
            Ok(route) => Ok(!matches!(route.readiness, RouteReadiness::Ready { .. })),
            Err(error) => match error.downcast_ref::<RoutingError>() {
                Some(RoutingError::NoEligiblePoolMember { exclusions })
                    if exclusions.iter().all(|e| e.ends_with(": disabled")) =>
                {
                    Ok(true)
                }
                _ => Err(error),
            },
        }
    };
    // An account set to off cannot serve a free model, whatever the free profile says.
    assert!(blocked(serde_json::json!({"pacing":{
        "accounts":{"zen":{"policy":"off"}}, "free":{"opencode":{"policy":"max"}}}}))?);
    // Without an account profile, the free profile is the provider default.
    assert!(blocked(
        serde_json::json!({"pacing":{"free":{"opencode":{"policy":"off"}}}})
    )?);
    assert!(!blocked(serde_json::json!({"pacing":{
        "accounts":{"zen":{"policy":"max"}}, "free":{"opencode":{"policy":"off"}}}}))?);
    // An active override outranks the account profile; an expired one does not.
    let override_row = |expires: i64| serde_json::json!([{"account":"zen", "policy":"spend", "schedule":{"expires":provenant_core::WallTime::from_unix_seconds(expires).map(|at| at.to_string()).ok()}}]);
    assert!(!blocked(serde_json::json!({
        "pacing":{"accounts":{"zen":{"policy":"off"}}}, "pacing_overrides": override_row(1_000_000)
    }))?);
    assert!(blocked(serde_json::json!({
        "pacing":{"accounts":{"zen":{"policy":"off"}}}, "pacing_overrides": override_row(1)
    }))?);
    Ok(())
}

#[test]
fn newer_unknown_reference_supersedes_older_reset() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["seven_day"],
            "deadline":{"reset_of":"anese"}}}}
    }))?;
    let own = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut older = quota(model, "anese", QuotaStatus::Available, 1, 70, 120)?;
    older.snapshot.account.adapter = "claude".into();
    older.snapshot.windows = vec![QuotaWindow {
        resets_unix_seconds: Some(302_500),
        ..own.clone()
    }];
    let mut newer = quota(model, "anese", QuotaStatus::Unknown, 2, 90, 120)?;
    newer.snapshot.account.adapter = "claude".into();
    let route = profile_preview(config, model, "michael", vec![own], vec![older, newer])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    // The old reset (302_500) would give 10_000; the window's own reset gives 5_000.
    assert_eq!(candidate.factor_basis_points, 5_000);
    let detail = candidate.pacing_detail.as_deref().ok_or("detail")?;
    assert!(
        detail.contains("latest snapshot is Unknown") && detail.contains("falling back"),
        "{detail}"
    );
    Ok(())
}

#[test]
fn monthly_day_31_clamps_to_a_shorter_months_last_day() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let zone = jiff::tz::TimeZone::system();
    let local = |month, day| {
        zone.to_ambiguous_zoned(jiff::civil::date(1970, month, day).at(0, 0, 0, 0))
            .compatible()
            .map(|z| z.timestamp().as_second())
    };
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["monthly"],
            "deadline":"day 31"}}}
    }))?;
    let route = profile_preview(
        config,
        "claude/claude-haiku-5-5",
        "michael",
        vec![QuotaWindow {
            pool: "monthly".into(),
            window_seconds: Some(2_419_200),
            used_basis_points: 5_000,
            resets_unix_seconds: Some(local(3, 1)?),
        }],
        vec![],
    )?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    let detail = candidate.pacing_detail.as_deref().ok_or("detail")?;
    assert!(
        detail.contains(&format!("deadline={}", local(2, 28)?))
            && detail.contains("day 31 clamped to the month's last day (28)"),
        "{detail}"
    );
    Ok(())
}

#[test]
fn daily_selector_is_neutral_until_a_daily_window_is_observable()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let model = "claude/claude-haiku-5-5";
    let config = || {
        serde_json::from_value::<RoutingConfig>(serde_json::json!({
            "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["daily"]}}}
        }))
    };
    let weekly = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 9_000,
        resets_unix_seconds: Some(604_900),
    };
    let route = profile_preview(config()?, model, "michael", vec![weekly.clone()], vec![])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, 10_000);
    assert!(
        candidate
            .pacing_detail
            .as_deref()
            .ok_or("detail")?
            .contains("unobserved windows neutral")
    );
    let daily = QuotaWindow {
        pool: "daily".into(),
        window_seconds: Some(86_400),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(86_500),
    };
    let route = profile_preview(config()?, model, "michael", vec![weekly, daily], vec![])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, 5_000);
    Ok(())
}

#[test]
fn opencode_zen_and_go_use_separate_account_identities() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records: Vec<_> = ["go", "zen"]
        .into_iter()
        .map(|key| {
            let mut record = availability();
            record.adapter = "opencode".into();
            record.account_key = Some(key.into());
            record
        })
        .collect();
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    for (model, key) in [
        ("opencode/opencode/glm-5.3-flash-free", "zen"),
        ("opencode/opencode-go/glm-5.3-flash", "go"),
    ] {
        let query = request(model)?;
        let route = resolve(&query, &ctx)?;
        assert_eq!(route.account_key.as_deref(), Some(key));
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    let free = request("opencode/opencode/glm-5.3-flash-free")?;
    let route = resolve_for_account_with_seed(&free, &ctx, Some("go"), "fixture", 0)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable { .. }
    ));
    Ok(())
}

#[test]
fn go_pool_calibration_is_advisory_and_paces_the_maximum() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_quota::{AccountRecord, Calibration, QuotaDetails, QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let model = "opencode/opencode-go/deepseek-v4.1-flash";
    let mut config = RoutingConfig {
        allow_unknown_partial_headroom: true,
        ..RoutingConfig::default()
    };
    config.pacing.accounts.insert(
        "go".into(),
        PacingProfile {
            policy: Some(PacingPolicy::Pace),
            windows: Some(vec![PacingWindow::Rolling, PacingWindow::Monthly]),
            ..PacingProfile::default()
        },
    );
    config.pools.insert(
        "go-trial".into(),
        vec![RoutePoolMember {
            model: ModelId::parse(model)?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        }],
    );
    let mut available = availability();
    available.adapter = "opencode".into();
    available.account_key = Some("go".into());
    let availability = [available];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut query = request(model)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("go-trial")?);
    let mut raw = quota(model, "go", QuotaStatus::Available, 1, 80, 120)?;
    raw.snapshot.account.adapter = "opencode".into();
    let duplicate = |observation: &QuotaObservation| QuotaObservation {
        model: observation.model.clone(),
        snapshot: observation.snapshot.clone(),
        expires_unix_seconds: observation.expires_unix_seconds,
    };
    raw.snapshot.details = Some(QuotaDetails {
        source: Some("local-ledger".into()),
        coverage: Some("partial".into()),
        ..QuotaDetails::default()
    });
    raw.snapshot.windows = [
        ("five_hour", 401, 18_000),
        ("seven_day", 160, 604_800),
        ("thirty_day", 2484, 2_592_000),
    ]
    .into_iter()
    .map(|(pool, used_basis_points, seconds)| QuotaWindow {
        pool: pool.into(),
        used_basis_points,
        window_seconds: Some(seconds),
        resets_unix_seconds: None,
    })
    .collect();
    // Selection and admission treat the same uncalibrated lower bound alike.
    let mut default_config = config.clone();
    default_config.allow_unknown_partial_headroom = false;
    let observations = [duplicate(&raw)];
    let mut ctx = context(&catalogue, &default_config, now()?);
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.quota = &observations;
    let explicit = request(model)?;
    let refused = resolve(&explicit, &ctx)?;
    assert!(matches!(
        refused.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownPartialHeadroom,
            ..
        }
    ));
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.quota = &observations;
    assert!(matches!(
        resolve(&explicit, &ctx)?.readiness,
        RouteReadiness::Ready { .. }
    ));

    let mut settings = provenant_quota::Settings::default();
    settings.calibration.insert(
        "go".into(),
        Calibration {
            five_hour: Some(43),
            seven_day: Some(17),
            thirty_day: Some(59),
            expires: provenant_core::WallTime::from_unix_seconds(101)?,
        },
    );
    let apply = |observation: &QuotaObservation| {
        let mut record = AccountRecord::empty(observation.snapshot.account.clone());
        record.snapshot = Some(observation.snapshot.clone());
        let mut records = [record];
        settings.apply_calibration(&mut records, 100);
        QuotaObservation {
            snapshot: records[0].snapshot.clone().unwrap(),
            ..duplicate(observation)
        }
    };
    let calibrated = apply(&raw);
    // Selection and launch admission must interpret the same fresh scoped
    // observation and opt-in identically, including calibration expiry.
    let mut expired_for_table = duplicate(&calibrated);
    expired_for_table
        .snapshot
        .details
        .as_mut()
        .unwrap()
        .calibration_expires_unix_seconds = Some(100);
    let mut exhausted_for_table = duplicate(&raw);
    exhausted_for_table.snapshot.windows[0].used_basis_points = 10_000;
    let policy = [provenant_quota::QuotaPolicy {
        adapter: "opencode",
        timer_seconds: 300,
        panel_seconds: 120,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    }];
    for (name, observation, opt_in, expected) in [
        ("partial default", duplicate(&raw), false, false),
        ("partial opted in", duplicate(&raw), true, true),
        ("exhausted opted in", exhausted_for_table, true, false),
        ("calibrated", duplicate(&calibrated), false, true),
        ("calibration expired", expired_for_table, false, false),
    ] {
        let mut row_config = config.clone();
        row_config.allow_unknown_partial_headroom = opt_in;
        let observations = [observation];
        let mut ctx = context(&catalogue, &row_config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let selected = resolve(&explicit, &ctx)?;
        let ready = matches!(selected.readiness, RouteReadiness::Ready { .. });
        let mut admitted_route = selected.clone();
        // Check the independent admission policy with a fresh readiness grant,
        // so an earlier selection refusal cannot mask a headroom disagreement.
        admitted_route.readiness = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 120,
        };
        let mut record = AccountRecord::empty(observations[0].snapshot.account.clone());
        record.revision = observations[0].snapshot.revision;
        record.snapshot = Some(observations[0].snapshot.clone());
        let records = [record];
        let admission = provenant_quota::Admission {
            policies: &policy,
            now_unix_seconds: 100,
            records: &records,
            pools: &[],
        }
        .check_with_partial_headroom(&admitted_route, opt_in);
        assert_eq!(
            (ready, admission.is_ok()),
            (expected, expected),
            "{name}: {admission:?}"
        );
    }
    for (key, five_hour, seven_day, expected) in [
        ("go", None, None, "five_hour, seven_day"),
        (
            "different-go",
            Some(43),
            Some(17),
            "five_hour, seven_day, thirty_day",
        ),
    ] {
        let mut partial_settings = provenant_quota::Settings::default();
        partial_settings.calibration.insert(
            key.into(),
            Calibration {
                five_hour,
                seven_day,
                thirty_day: Some(59),
                expires: provenant_core::WallTime::from_unix_seconds(101)?,
            },
        );
        let mut record = AccountRecord::empty(raw.snapshot.account.clone());
        record.snapshot = Some(raw.snapshot.clone());
        let mut records = [record];
        partial_settings.apply_calibration(&mut records, 100);
        let observations = [QuotaObservation {
            snapshot: records[0].snapshot.clone().unwrap(),
            ..duplicate(&raw)
        }];
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&query, &ctx)?;
        let detail = route.pool_selection.unwrap().candidates[0]
            .pacing_detail
            .clone()
            .unwrap();
        assert!(
            detail.contains(&format!("Go calibration missing or stale: {expected}")),
            "{key}: {detail}"
        );
        assert!(
            pacing_preview(&ModelId::parse(model)?, &ctx)[0]
                .detail
                .contains(expected)
        );
    }
    let mut expired = duplicate(&calibrated);
    expired
        .snapshot
        .details
        .as_mut()
        .unwrap()
        .calibration_expires_unix_seconds = Some(100);
    let mut incomplete = duplicate(&calibrated);
    incomplete
        .snapshot
        .details
        .as_mut()
        .unwrap()
        .calibrated_windows
        .retain(|pool| pool != "seven_day");
    let mut missing_window = duplicate(&calibrated);
    missing_window
        .snapshot
        .windows
        .retain(|window| window.pool != "seven_day");
    let mut stale = duplicate(&calibrated);
    stale.expires_unix_seconds = 100;
    for (name, observations) in [
        ("missing quota", vec![]),
        ("partial ledger", vec![duplicate(&raw)]),
        ("expired calibration", vec![expired]),
        ("incomplete calibration", vec![incomplete]),
        ("missing window", vec![missing_window]),
        ("stale quota", vec![stale]),
    ] {
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&query, &ctx)?;
        assert!(
            matches!(route.readiness, RouteReadiness::Ready { .. }),
            "{name}"
        );
        let detail = route.pool_selection.unwrap().candidates[0]
            .pacing_detail
            .clone()
            .unwrap();
        assert!(
            detail.contains("Go quota source: local-ledger, coverage: partial"),
            "{name}: {detail}"
        );
        // The same local ledger remains visible for explicitly selected models.
        let explicit = resolve(&request(model)?, &ctx)?;
        assert!(
            matches!(explicit.readiness, RouteReadiness::Ready { .. }),
            "{name}"
        );
    }
    let mut higher_ledger = duplicate(&raw);
    higher_ledger.snapshot.windows[2].used_basis_points = 7000;
    let mut passed_reset = duplicate(&calibrated);
    for window in &mut passed_reset.snapshot.windows {
        window.resets_unix_seconds = Some(99);
    }
    for (observation, expected_factor) in [
        (duplicate(&raw), 10_000),
        (calibrated, 4100),
        (apply(&higher_ledger), 3000),
        (passed_reset, 4100),
    ] {
        let observations = [observation];
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&query, &ctx)?;
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
        let receipt = serde_json::to_value(&route)?["pool_selection"].clone();
        assert_eq!(
            receipt["candidates"][0]["factor_basis_points"],
            expected_factor
        );
        assert_eq!(route.account_key.as_deref(), Some("go"));
    }
    Ok(())
}

#[test]
fn opencode_zen_pool_does_not_inherit_go_pacing_quota() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let free = "opencode/opencode/deepseek-v4.1-flash-free";
    let mut config = RoutingConfig::default();
    config
        .pacing
        .providers
        .insert("opencode".into(), PacingPolicy::Pace);
    config.pacing.accounts.insert(
        "go".into(),
        PacingProfile {
            policy: Some(PacingPolicy::Off),
            windows: Some(vec![PacingWindow::Rolling]),
            ..PacingProfile::default()
        },
    );
    config.pools.insert(
        "zen-trial".into(),
        vec![RoutePoolMember {
            model: ModelId::parse(free)?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: true,
        }],
    );
    let records: Vec<_> = ["go", "zen"]
        .into_iter()
        .map(|key| {
            let mut record = availability();
            record.adapter = "opencode".into();
            record.account_key = Some(key.into());
            record
        })
        .collect();
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut observation = quota(
        "opencode/opencode-go/deepseek-v4.1-flash",
        "go",
        provenant_quota::QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?;
    observation.snapshot.account.adapter = "opencode".into();
    observation
        .snapshot
        .windows
        .push(provenant_quota::QuotaWindow {
            pool: "five_hour".into(),
            window_seconds: Some(18_000),
            used_basis_points: 10_000,
            resets_unix_seconds: Some(100 + 18_000),
        });
    let quota = [observation];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.quota = &quota;
    let mut query = request(free)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("zen-trial")?);
    let route = resolve(&query, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(route.account_key.as_deref(), Some("zen"));
    let receipt = serde_json::to_value(&route)?["pool_selection"].clone();
    assert_eq!(receipt["candidates"][0]["factor_basis_points"], 10_000);
    assert!(
        receipt["candidates"][0]["pacing_detail"]
            .as_str()
            .ok_or("pacing detail")?
            .contains("source=provider policy=Pace")
    );
    let previews = pacing_preview(&ModelId::parse(free)?, &ctx);
    assert_eq!(previews.len(), 1);
    assert_eq!(previews[0].account.as_deref(), Some("zen"));
    assert_eq!(previews[0].policy, PacingPolicy::Pace);
    Ok(())
}

#[test]
fn capacity_cooldowns_apply_only_to_draws_and_expire() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model":"codex/gpt-6-luna", "weight":1, "efforts":["high"]},
            {"model":"codex/gpt-6.1-sol", "weight":1, "efforts":["high"]}
        ]},
        "pacing": {"accounts": {"opaque-account": {"policy":"pace"}}}
    }))?;
    let registry = [provenant_providers::Registration {
        quota_source: None,
        ..provenant_providers::codex::REGISTRATION
    }];
    let availability = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let cooldown = provenant_quota::CapacityCooldown {
        extra: Default::default(),
        account: provenant_quota::AccountScope {
            adapter: "codex".into(),
            account_key: "opaque-account".into(),
            label: "test".into(),
        },
        model: "gpt-6-luna".into(),
        observed_unix_seconds: 90,
        expires_unix_seconds: 110,
    };
    let cooldowns = [cooldown.clone()];
    let mut observation = quota(
        "codex/gpt-6-luna",
        "opaque-account",
        provenant_quota::QuotaStatus::Available,
        1,
        80,
        120,
    )?;
    observation.snapshot.windows = vec![provenant_quota::QuotaWindow {
        pool: "primary".into(),
        window_seconds: Some(18_000),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(10_000),
    }];
    let observations = [observation];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.quota = &observations;
    ctx.registry = &registry;
    ctx.availability = &availability;
    ctx.access = &access;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let baseline = resolve(&query, &ctx)?
        .pool_selection
        .ok_or("receipt")?
        .candidates[0]
        .clone();
    assert!(baseline.pacing_detail.is_some());
    assert_ne!(baseline.factor_basis_points, 10_000);
    ctx.capacity_cooldowns = &cooldowns;
    query.choice = RouteChoice::Model(ModelId::parse("codex/gpt-6-luna")?);
    let explicit = resolve(&query, &ctx)?;
    assert_eq!(explicit.model, "gpt-6-luna");
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let selected = resolve(&query, &ctx)?;
    assert_eq!(selected.model, "gpt-6.1-sol");
    let receipt = selected.pool_selection.ok_or("receipt")?;
    let skipped = &receipt.candidates[0];
    assert_eq!(skipped.exclusion.as_deref(), Some("capacity cooldown"));
    assert_eq!(skipped.factor_basis_points, baseline.factor_basis_points);
    assert_eq!(skipped.pacing_detail, baseline.pacing_detail);
    let both = [
        cooldown,
        provenant_quota::CapacityCooldown {
            extra: Default::default(),
            account: provenant_quota::AccountScope {
                adapter: "codex".into(),
                account_key: "opaque-account".into(),
                label: "test".into(),
            },
            model: "gpt-6.1-sol".into(),
            observed_unix_seconds: 90,
            expires_unix_seconds: 110,
        },
    ];
    ctx.capacity_cooldowns = &both;
    let fallback = resolve(&query, &ctx)?;
    let receipt = fallback.pool_selection.unwrap();
    assert_eq!(
        receipt.reason.as_deref(),
        Some("all eligible candidates cooling, normal selection")
    );
    ctx.now = provenant_core::WallTime::from_micros(110_000_000)?;
    let expired = resolve(&query, &ctx)?.pool_selection.unwrap();
    assert!(expired.candidates.iter().all(|c| c.exclusion.is_none()));
    Ok(())
}

#[test]
fn cooling_ready_candidate_beats_unavailable_candidate() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model":"codex/gpt-6-luna", "weight":1, "efforts":["high"]},
            {"model":"claude/claude-sonnet-5-5", "weight":1, "efforts":["high"]}
        ]}
    }))?;
    let registry = [
        provenant_providers::Registration {
            quota_source: None,
            ..provenant_providers::codex::REGISTRATION
        },
        provenant_providers::Registration {
            quota_source: None,
            ..provenant_providers::claude::REGISTRATION
        },
    ];
    let availability = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let cooldowns = [provenant_quota::CapacityCooldown {
        extra: Default::default(),
        account: provenant_quota::AccountScope {
            adapter: "codex".into(),
            account_key: "opaque-account".into(),
            label: "test".into(),
        },
        model: "gpt-6-luna".into(),
        observed_unix_seconds: 90,
        expires_unix_seconds: 110,
    }];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.registry = &registry;
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.capacity_cooldowns = &cooldowns;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let selected = resolve(&query, &ctx)?;
    assert_eq!(selected.model, "gpt-6-luna");
    assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(
        selected.pool_selection.unwrap().reason.as_deref(),
        Some("cooling ready candidate selected over unavailable candidate")
    );
    Ok(())
}

#[test]
fn agy_models_are_paced_and_reserved_by_their_own_quota_pool_only()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let window = |pool: &str, used| QuotaWindow {
        pool: pool.into(),
        window_seconds: Some(604_800),
        used_basis_points: used,
        resets_unix_seconds: Some(604_900),
    };
    for (model, gemini, claude, factor, detail) in [
        (
            "agy/claude-sonnet-4-6",
            10_000,
            0,
            10_000,
            "quota_pool=claude",
        ),
        (
            "agy/claude-opus-4-6-thinking",
            10_000,
            0,
            10_000,
            "quota_pool=claude",
        ),
        ("agy/claude-sonnet-4-6", 0, 9_900, 2_500, "claude.weekly"),
        (
            "agy/gemini-3.8-flash",
            0,
            10_000,
            10_000,
            "quota_pool=gemini",
        ),
        ("agy/gemini-3.8-flash", 10_000, 0, 2_500, "gemini.weekly"),
    ] {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"agy-host":{"policy":"pace", "windows":["seven_day"]}}},
            "pacing_reserve":{"agy":{"seven_day_percent":5}}
        }))?;
        let route = profile_preview(
            config,
            model,
            "agy-host",
            vec![
                window("gemini.weekly", gemini),
                window("claude.weekly", claude),
            ],
            vec![],
        )?;
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        let text = candidate.pacing_detail.as_deref().ok_or("detail")?;
        assert_eq!(candidate.factor_basis_points, factor, "{model} {text}");
        assert!(text.contains(detail), "{model} {text}");
        let own_spent = if model.contains("claude") {
            claude
        } else {
            gemini
        } >= 9_500;
        assert_eq!(
            text.contains("reserve exclusion"),
            own_spent,
            "{model} {text}"
        );
    }
    Ok(())
}

#[test]
fn quota_pool_is_validated_and_limited_to_grouped_adapters()
-> Result<(), Box<dyn std::error::Error>> {
    let bundled = Catalogue::bundled()?;
    assert_eq!(
        bundled.quota_pool(&ModelId::parse("agy/claude-sonnet-4-6")?),
        Some("claude")
    );
    assert_eq!(
        bundled.quota_pool(&ModelId::parse("agy/gemini-3.8-flash")?),
        Some("gemini")
    );
    // Uncatalogued models take the pool their family names, or none.
    for (model, pool) in [
        ("agy/gemini-9-pro", Some("gemini")),
        ("agy/claude-opus-9", Some("claude")),
        ("agy/other-1", None),
        ("codex/gemini-1", None),
    ] {
        assert_eq!(bundled.quota_pool(&ModelId::parse(model)?), pool, "{model}");
    }
    for (model, pool, valid) in [
        ("agy/claude-sonnet-4-6", Some("claude"), true),
        ("agy/claude-sonnet-4-6", None, true),
        ("agy/claude-sonnet-4-6", Some(""), false),
        ("agy/claude-sonnet-4-6", Some("bad pool"), false),
        ("codex/gpt-6-luna", Some("gemini"), false),
    ] {
        let mut catalogue = Catalogue::bundled()?;
        let id = ModelId::parse(model)?;
        let row = catalogue
            .models
            .iter_mut()
            .find(|row| row.model == id)
            .ok_or(model)?;
        row.quota_pool = pool.map(Into::into);
        assert_eq!(catalogue.validate().is_ok(), valid, "{model} {pool:?}");
    }
    Ok(())
}

#[test]
fn agy_exhaustion_follows_the_models_serving_windows() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let window = |pool: &str, used, reset| QuotaWindow {
        pool: pool.into(),
        window_seconds: Some(604_800),
        used_basis_points: used,
        resets_unix_seconds: Some(reset),
    };
    let ready = |model: &str, status, windows| -> Result<bool, Box<dyn std::error::Error>> {
        let config: RoutingConfig = serde_json::from_value(
            serde_json::json!({"pacing":{"accounts":{"agy-host":{"policy":"max"}}}}),
        )?;
        let route =
            profile_preview_with_status(config, model, "agy-host", windows, vec![], status)?;
        Ok(matches!(route.readiness, RouteReadiness::Ready { .. }))
    };
    let (claude, gemini) = ("agy/claude-sonnet-4-6", "agy/gemini-3.8-flash");
    // An exhausted ungrouped window serves every model.
    for model in [claude, gemini] {
        let windows = vec![
            window("gemini.weekly", 0, 604_900),
            window("claude.weekly", 0, 604_900),
            window("quota", 10_000, 604_900),
        ];
        assert!(!ready(model, QuotaStatus::Available, windows)?, "{model}");
    }
    // A spent Gemini group leaves Claude ready, and the reverse.
    let split = |g, c| {
        vec![
            window("gemini.weekly", g, 604_900),
            window("claude.weekly", c, 604_900),
        ]
    };
    assert!(ready(claude, QuotaStatus::Available, split(10_000, 0))?);
    assert!(!ready(gemini, QuotaStatus::Available, split(10_000, 0))?);
    assert!(ready(gemini, QuotaStatus::Available, split(0, 10_000))?);
    assert!(!ready(claude, QuotaStatus::Available, split(0, 10_000))?);
    // Exhaustion lapses per model: Claude's reset has passed, Gemini's has not.
    let lapsed = vec![
        window("gemini.weekly", 10_000, 604_900),
        window("claude.weekly", 10_000, 50),
    ];
    assert!(ready(claude, QuotaStatus::Exhausted, lapsed.clone())?);
    assert!(!ready(gemini, QuotaStatus::Exhausted, lapsed)?);
    Ok(())
}

#[test]
fn agy_shared_claude_pool_exhaustion_skips_only_the_claude_models()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let window = |pool: &str, used, reset| QuotaWindow {
        pool: pool.into(),
        window_seconds: Some(604_800),
        used_basis_points: used,
        resets_unix_seconds: Some(reset),
    };
    // The reported registration: the gemini windows have headroom while the
    // shared Claude pool is spent, so gemini lanes must stay ready and only
    // the agy Claude models may be skipped.
    let windows = vec![
        window("gemini.weekly", 2_256, 604_900),
        window("gemini.5h", 1_701, 4_900),
        window("claude.weekly", 10_000, 604_900),
    ];
    let ready = |model: &str| -> Result<bool, Box<dyn std::error::Error>> {
        let config: RoutingConfig = serde_json::from_value(
            serde_json::json!({"pacing":{"accounts":{"agy-host":{"policy":"max"}}}}),
        )?;
        let route = profile_preview_with_status(
            config,
            model,
            "agy-host",
            windows.clone(),
            vec![],
            QuotaStatus::Available,
        )?;
        Ok(matches!(route.readiness, RouteReadiness::Ready { .. }))
    };
    assert!(ready("agy/gemini-3.8-flash")?);
    assert!(!ready("agy/claude-sonnet-4-6")?);
    assert!(!ready("agy/claude-opus-4-6-thinking")?);
    Ok(())
}
#[test]
fn unsupported_pool_effort_is_dropped_with_a_warning() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model": "codex/gpt-6-luna", "effort": "retired", "efforts": ["low", "retired"]}
        ]}
    }))?;
    assert!(config.validate().is_empty());
    let context = context(&catalogue, &config, now()?);
    let listed = list(
        &RouteListQuery {
            pool: Some("trial".into()),
            ..RouteListQuery::default()
        },
        &context,
    )?;
    let member = &listed.pools["trial"][0];
    assert_eq!(member.efforts, ["low"]);
    assert_eq!(member.effort, None);
    let warnings = config.pool_effort_warnings(&catalogue);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("domains.routing.pools.trial[0].efforts"));
    assert!(warnings[0].contains("retired"));
    Ok(())
}
#[test]
fn supported_default_effort_survives_reconciliation() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model": "codex/gpt-6-luna", "effort": "low", "efforts": ["low", "retired"]}
        ]}
    }))?;
    let context = context(&catalogue, &config, now()?);
    let listed = list(&RouteListQuery::default(), &context)?;
    let member = &listed.pools["trial"][0];
    assert_eq!(member.effort.as_deref(), Some("low"));
    assert_eq!(member.efforts, ["low"]);
    Ok(())
}
#[test]
fn malformed_duplicate_or_excess_pool_efforts_still_fail() -> Result<(), Box<dyn std::error::Error>>
{
    let fails = |member: serde_json::Value| -> Result<bool, serde_json::Error> {
        let config: RoutingConfig =
            serde_json::from_value(serde_json::json!({"pools": {"trial": [member]}}))?;
        Ok(config
            .validate()
            .iter()
            .any(|error| error.code == "unsupported_effort"))
    };
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "efforts": ["bad effort"]})
    )?);
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "efforts": ["low", "low"]})
    )?);
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "effort": "bad effort"})
    )?);
    let many: Vec<String> = (0..17).map(|n| format!("e{n}")).collect();
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "efforts": many})
    )?);
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingDomains {
    routing: RoutingConfig,
}
/// Load `text` through the config loader with only the routing section.
fn load_routing(
    text: &str,
) -> Result<
    Result<provenant_config::ResolvedConfig<RoutingDomains>, provenant_config::ConfigError>,
    Box<dyn std::error::Error>,
> {
    static ENTRIES: [provenant_config::DomainSection; 1] = [SECTION];
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
    Ok(provenant_config::load_with_domains(
        &roots,
        Some(text.as_bytes()),
        &DomainSections::<RoutingDomains>::new(&ENTRIES),
    ))
}
#[test]
fn malformed_scope_types_drop_or_refuse_the_whole_record() -> Result<(), Box<dyn std::error::Error>>
{
    let rule = "schema_version=1\n[[domains.routing.model_policy.rules]]\n";
    let lane = "[domains.routing.model_policy.rules.policy.lane]\n";
    // A rule that grants writes loses neither its expiry nor its selector:
    // the whole rule is dropped, with a warning at the rule.
    for scope in [
        "schedule={expires='bad'}\n",
        "schedule={windows=[{start_hour='x',end_hour=4}]}\n",
        "schedule={expirse=10}\n",
        "model=5\n",
        "account=7\n",
    ] {
        let text = format!("{rule}{scope}{lane}read_only=false\n");
        let config = load_routing(&text)??;
        assert!(
            config.domains.routing.model_policy.rules.is_empty(),
            "{text}"
        );
        assert!(
            config.warnings.iter().any(|w| w
                .message
                .starts_with("domains.routing.model_policy.rules.0:")),
            "{text}: {:?}",
            config.warnings
        );
    }
    // Dropping a restricting rule would lift its restriction, so it refuses.
    let text = format!("{rule}schedule={{expires='bad'}}\n{lane}read_only=true\n");
    let error = load_routing(&text)?
        .err()
        .ok_or("restricting rule loaded")?;
    assert!(
        error.to_string().contains("model_policy.rules.0"),
        "{error}"
    );
    // An override loses its provider or schedule only with the whole row.
    for row in [
        "provider=5\npolicy='off'\n",
        "provider='codex'\npolicy='off'\nschedule={expires='bad'}\n",
    ] {
        let text = format!(
            "schema_version=1\n[[domains.routing.pacing_overrides]]\n{row}[[domains.routing.pacing_overrides]]\nprovider='claude'\npolicy='spend'\n"
        );
        let config = load_routing(&text)??;
        let rows = &config.domains.routing.pacing_overrides;
        assert_eq!(rows.len(), 1, "{text}");
        assert_eq!(rows[0].provider.as_deref(), Some("claude"), "{text}");
    }
    Ok(())
}
#[test]
fn invalid_override_schedules_drop_their_override_at_load() -> Result<(), Box<dyn std::error::Error>>
{
    for schedule in [
        "{starts='1970-01-01T00:00:20Z',expires='1970-01-01T00:00:10Z'}",
        "{tz='Mars/Olympus_Mons'}",
    ] {
        let text = format!(
            "schema_version=1\n[[domains.routing.pacing_overrides]]\nprovider='codex'\npolicy='off'\nschedule={schedule}\n[[domains.routing.pacing_overrides]]\nprovider='claude'\npolicy='spend'\n"
        );
        let config = load_routing(&text)??;
        let rows = &config.domains.routing.pacing_overrides;
        assert_eq!(rows.len(), 1, "{text}");
        assert_eq!(rows[0].provider.as_deref(), Some("claude"), "{text}");
        assert!(
            config
                .warnings
                .iter()
                .any(|w| w.message.starts_with("domains.routing.pacing_overrides.0:")),
            "{text}: {:?}",
            config.warnings
        );
    }
    Ok(())
}
#[test]
fn read_only_policy_model_stays_in_junior_and_senior_pools_for_read_only_work()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = toml_edit::de::from_str(
        "[[model_policy.rules]]\nmodel='agy/gemini-3.8-flash'\n[model_policy.rules.policy.lane]\nread_only=true\n",
    )?;
    let records = [AvailabilityRecord {
        adapter: "agy".into(),
        observed_models: vec!["gemini-3.8-flash".into()],
        ..availability()
    }];
    let access = [AccessContext {
        adapter: "agy",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let context = RoutingContext {
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let gemini_exclusion = |pool: &str, writes: bool, workload| {
        let request = RouteRequest {
            intent: RouteIntent {
                workload,
                writes,
                ..Default::default()
            },
            choice: RouteChoice::NamedRoute(RouteName::parse(pool)?),
            effort: None,
            confidential: false,
            fallback: Fallback::None,
        };
        let route = resolve_with_seed(&request, &context, "agy-read-only", 0)?;
        let receipt = route.pool_selection.ok_or("pool receipt")?;
        let member = receipt
            .candidates
            .iter()
            .find(|c| c.model == "agy/gemini-3.8-flash")
            .ok_or("gemini candidate")?;
        Ok::<_, Box<dyn std::error::Error>>(member.exclusion.clone())
    };
    for pool in ["junior", "senior"] {
        for workload in [Workload::General, Workload::ReadHeavy, Workload::Review] {
            assert_eq!(gemini_exclusion(pool, false, workload)?, None, "{pool}");
        }
        assert_eq!(
            gemini_exclusion(pool, true, Workload::General)?.as_deref(),
            Some("read-only model policy"),
            "{pool}"
        );
    }
    let mut query = request("agy/gemini-3.8-flash")?;
    assert!(resolve(&query, &context).is_ok());
    query.intent.writes = true;
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::ReadOnlyPolicy)
    ));
    Ok(())
}

fn account_scope(key: &str) -> provenant_quota::AccountScope {
    provenant_quota::AccountScope {
        adapter: "codex".into(),
        account_key: key.into(),
        label: "test".into(),
    }
}
fn two_accounts() -> [AvailabilityRecord; 2] {
    let mut other = availability();
    other.account_key = Some("other-account".into());
    [availability(), other]
}
const ACCESS: [AccessContext<'static>; 1] = [AccessContext {
    adapter: "codex",
    disclosure_ref: "policy",
    confinement_ref: "grant",
}];

#[test]
fn persisted_mismatch_excludes_account_and_selects_another()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let availability = two_accounts();
    let mut bad = provenant_quota::AccountRecord::empty(account_scope("opaque-account"));
    bad.last_error = Some(provenant_quota::ProbeFailure::AccountMismatch);
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &ACCESS;
    let records = [bad.clone()];
    ctx.accounts = &records;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(route.account_key.as_deref(), Some("other-account"));
    // Every serving account mismatched: the typed reason is reported.
    let mut other = provenant_quota::AccountRecord::empty(account_scope("other-account"));
    other.last_error = Some(provenant_quota::ProbeFailure::AccountMismatch);
    let both = [bad, other];
    ctx.accounts = &both;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::AccountMismatch,
            ..
        }
    ));
    Ok(())
}

#[test]
fn invalid_account_evidence_excludes_only_that_account() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let availability = two_accounts();
    let mut broken = provenant_quota::AccountRecord::empty(account_scope("opaque-account"));
    broken.refusals.push(provenant_quota::Refusal {
        extra: Default::default(),
        account: account_scope("opaque-account"),
        model: "test".into(),
        pool: None,
        observed_unix_seconds: i64::MAX,
        resets_unix_seconds: None,
    });
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &ACCESS;
    let records = [broken];
    ctx.accounts = &records;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(route.account_key.as_deref(), Some("other-account"));
    // Alone, the broken account is unavailable for a reason that names the evidence.
    let single = [availability[0].clone()];
    ctx.availability = &single;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::InvalidAccountEvidence,
            ..
        }
    ));
    Ok(())
}

#[test]
fn account_evidence_reasons_carry_their_repair_text() {
    for (reason, text) in [
        (RouteExclusionReason::AccountMismatch, "re-register"),
        (RouteExclusionReason::FreeTierRefused, "paid route"),
        (
            RouteExclusionReason::ProviderRefused,
            "scoped provider refusal",
        ),
        (RouteExclusionReason::InvalidAccountEvidence, "invalid"),
    ] {
        assert!(exclusion_detail(&reason).contains(text), "{reason:?}");
    }
    assert!(exclusion_detail(&RouteExclusionReason::Exhausted).contains("reset"));
}

#[test]
fn free_pool_refusal_excludes_the_account_for_every_free_model()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let free = "opencode/opencode/deepseek-v4.1-flash-free";
    let mut record = availability();
    record.adapter = "opencode".into();
    record.account_key = Some("zen".into());
    let records = [record];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let scope = provenant_quota::AccountScope {
        adapter: "opencode".into(),
        account_key: "zen".into(),
        label: "test".into(),
    };
    // The refusal names a different free model: the keyless pool spans models.
    let refused = [provenant_quota::record_refusal(
        None,
        &scope,
        "opencode/opencode/other-free",
        Some("opencode-free"),
        90,
        Some(200),
    )?
    .replacement
    .ok_or("record")?];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    assert!(matches!(
        resolve(&request(free)?, &ctx)?.readiness,
        RouteReadiness::Ready { .. }
    ));
    ctx.accounts = &refused;
    assert!(matches!(
        resolve(&request(free)?, &ctx)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::FreeTierRefused,
            ..
        }
    ));
    Ok(())
}

#[test]
fn live_scoped_refusal_excludes_only_the_matching_account_and_model()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let availability = two_accounts();
    let refused = |account: &str, model: &str| -> Result<_, Box<dyn std::error::Error>> {
        let scope = account_scope(account);
        Ok(provenant_quota::record_refusal(
            None,
            &scope,
            &format!("codex/{model}"),
            None,
            90,
            Some(200),
        )?
        .replacement
        .ok_or("record")?)
    };
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &ACCESS;
    let records = [refused("opaque-account", "test")?];
    ctx.accounts = &records;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert_eq!(route.account_key.as_deref(), Some("other-account"));
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    // The same account still serves an unrelated model.
    let route = resolve(&request("codex/other")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    // With no other account, the refusal is the typed reason.
    let single = [availability[0].clone()];
    ctx.availability = &single;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::ProviderRefused,
            ..
        }
    ));
    // A refusal that has lapsed no longer blocks.
    let lapsed = [provenant_quota::record_refusal(
        None,
        &account_scope("opaque-account"),
        "codex/test",
        None,
        90,
        Some(95),
    )?
    .replacement
    .ok_or("record")?];
    ctx.accounts = &lapsed;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    Ok(())
}

#[test]
fn configured_model_exclusion_applies_to_pools_until_expiry()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let member = catalogue.pools["junior"]
        .iter()
        .find(|m| m.model.as_str() == "codex/gpt-6-luna")
        .ok_or("missing member")?
        .clone();
    catalogue.pools.insert("trial".into(), vec![member.clone()]);
    let mut config = RoutingConfig::default();
    config.model_exclusions.push(ModelExclusion {
        model: member.model.clone(),
        schedule: Schedule {
            expires: Some(provenant_core::WallTime::from_micros(101_000_000)?),
            ..Schedule::default()
        },
    });
    let mut query = request(member.model.as_str())?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let active = context(&catalogue, &config, now()?);
    let error = resolve(&query, &active).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("codex/gpt-6-luna: excluded by config: model codex/gpt-6-luna")
    );
    let expired = context(
        &catalogue,
        &config,
        provenant_core::WallTime::from_micros(102_000_000)?,
    );
    assert!(resolve(&query, &expired).is_ok());
    query.choice = RouteChoice::Model(member.model);
    assert!(resolve(&query, &active).is_ok());
    Ok(())
}

/// A two-provider pool with a Claude and a Codex member, each ready on an account.
fn exclusion_fixture() -> Result<(Catalogue, Vec<AvailabilityRecord>), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let pick = |catalogue: &Catalogue, adapter: &str| {
        catalogue
            .pools
            .values()
            .flatten()
            .find(|m| m.model.adapter() == adapter)
            .cloned()
            .ok_or("missing member")
    };
    let members = vec![pick(&catalogue, "claude")?, pick(&catalogue, "codex")?];
    catalogue.pools.insert("trial".into(), members);
    let records = ["claude", "codex"]
        .map(|adapter| {
            let mut record = availability();
            record.adapter = adapter.into();
            record.account_key = Some(format!("{adapter}-key"));
            record
        })
        .to_vec();
    Ok((catalogue, records))
}
fn trial_picks(
    catalogue: &Catalogue,
    records: &[AvailabilityRecord],
    rules: &[ExclusionRule],
) -> Result<Vec<String>, RoutingError> {
    let config = RoutingConfig::default();
    let mut ctx = context(catalogue, &config, now().unwrap());
    ctx.availability = records;
    let mut query = request("codex/test").unwrap();
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let route =
        resolve_for_account_with_seed_excluding(&query, &ctx, None, "exclusion", 0, &[], rules)?;
    let receipt = route.pool_selection.ok_or(RoutingError::Catalogue)?;
    let mut adapters: Vec<String> = receipt
        .candidates
        .iter()
        .filter(|candidate| candidate.exclusion.is_none())
        .filter_map(|candidate| candidate.model.split('/').next().map(str::to_owned))
        .collect();
    adapters.dedup();
    Ok(adapters)
}
fn rule(selector: ExclusionSelector) -> ExclusionRule {
    ExclusionRule {
        selector,
        source: ExclusionSource::Flag,
    }
}

#[test]
fn provider_account_and_model_selectors_remove_only_their_members()
-> Result<(), Box<dyn std::error::Error>> {
    let (catalogue, records) = exclusion_fixture()?;
    assert_eq!(trial_picks(&catalogue, &records, &[])?, ["claude", "codex"]);
    let provider = rule(ExclusionSelector::Provider("claude".into()));
    assert_eq!(trial_picks(&catalogue, &records, &[provider])?, ["codex"]);
    // Excluding the only account of a provider removes the provider's members.
    let account = rule(ExclusionSelector::Account {
        adapter: "codex".into(),
        key: "codex-key".into(),
    });
    assert_eq!(trial_picks(&catalogue, &records, &[account])?, ["claude"]);
    // An account that shares its adapter leaves the adapter's other account.
    let mut shared = records.clone();
    let mut second = availability();
    second.adapter = "codex".into();
    second.account_key = Some("codex-spare".into());
    shared.push(second);
    let account = rule(ExclusionSelector::Account {
        adapter: "codex".into(),
        key: "codex-key".into(),
    });
    assert_eq!(
        trial_picks(&catalogue, &shared, std::slice::from_ref(&account))?,
        ["claude", "codex"]
    );
    let config = RoutingConfig::default();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &shared;
    let mut query = request("codex/test")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let route = resolve_for_account_with_seed_excluding(
        &query,
        &ctx,
        None,
        "exclusion",
        0,
        &[],
        &[account],
    )?;
    let receipt = serde_json::to_value(route.pool_selection.ok_or("receipt")?)?;
    assert_eq!(
        receipt["considered_exclusions"][0],
        "source=flag selector=account codex/codex-key"
    );
    assert!(
        receipt["candidates"]
            .as_array()
            .ok_or("candidates")?
            .iter()
            .any(|candidate| candidate["model"]
                .as_str()
                .is_some_and(|model| model.starts_with("codex/"))
                && candidate["exclusion"].is_null())
    );
    assert!(!receipt.to_string().contains("prompt"));
    assert!(!receipt.to_string().contains("credential"));
    let codex = catalogue.pools["trial"][1].model.clone();
    let model = rule(ExclusionSelector::Model(codex));
    assert_eq!(trial_picks(&catalogue, &records, &[model])?, ["claude"]);
    Ok(())
}

#[test]
fn exclusions_that_empty_a_pool_are_named_and_pins_ignore_them()
-> Result<(), Box<dyn std::error::Error>> {
    let (catalogue, records) = exclusion_fixture()?;
    let rules = [
        rule(ExclusionSelector::Provider("claude".into())),
        ExclusionRule {
            selector: ExclusionSelector::Provider("codex".into()),
            source: ExclusionSource::Graph,
        },
    ];
    let error = trial_picks(&catalogue, &records, &rules).unwrap_err();
    assert_eq!(error.code(), "no_eligible_pool_member");
    let text = error.to_string();
    assert!(text.contains("excluded by flag: provider claude"), "{text}");
    assert!(text.contains("excluded by graph: provider codex"), "{text}");
    let config = RoutingConfig::default();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    let pinned = resolve_for_account_with_seed_excluding(
        &request("claude/test")?,
        &ctx,
        None,
        "exclusion",
        0,
        &[],
        &rules,
    )?;
    assert_eq!(pinned.adapter, "claude");
    // An account rule never changes a pinned model's account or readiness, and
    // the pin's own fallback is not filtered.
    let account_rules = [
        rule(ExclusionSelector::Account {
            adapter: "claude".into(),
            key: "claude-key".into(),
        }),
        rule(ExclusionSelector::Provider("codex".into())),
    ];
    let mut pin = request("claude/test")?;
    pin.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("codex/test")?])?);
    let pinned = resolve_for_account_with_seed_excluding(
        &pin,
        &ctx,
        None,
        "exclusion",
        0,
        &[],
        &account_rules,
    )?;
    let plain =
        resolve_for_account_with_seed_excluding(&pin, &ctx, None, "exclusion", 0, &[], &[])?;
    assert_eq!(pinned.account_key, plain.account_key);
    assert_eq!(pinned.readiness, plain.readiness);
    assert_eq!(pinned.alternatives.as_slice().len(), 1);
    Ok(())
}

#[test]
fn named_route_fallback_keeps_to_the_exclusions_of_its_pool()
-> Result<(), Box<dyn std::error::Error>> {
    let (catalogue, records) = exclusion_fixture()?;
    let mut config = RoutingConfig::default();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    let mut query = request("claude/test")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("codex/test")?])?);
    let none = resolve_for_account_with_seed_excluding(&query, &ctx, None, "fb", 0, &[], &[])?;
    assert_eq!(none.alternatives.as_slice().len(), 1);
    let rules = [rule(ExclusionSelector::Provider("codex".into()))];
    let route = resolve_for_account_with_seed_excluding(&query, &ctx, None, "fb", 0, &[], &rules)?;
    assert_eq!(route.alternatives.as_slice().len(), 0);
    // A persistent model exclusion filters the fallback too.
    config.model_exclusions.push(ModelExclusion {
        model: ModelId::parse("codex/test")?,
        schedule: Schedule::default(),
    });
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    let route = resolve_for_account_with_seed_excluding(&query, &ctx, None, "fb", 0, &[], &[])?;
    assert_eq!(route.alternatives.as_slice().len(), 0);
    // The same fallback on a pinned model is left alone.
    let mut pin = request("claude/test")?;
    pin.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("codex/test")?])?);
    let route = resolve_for_account_with_seed_excluding(&pin, &ctx, None, "fb", 0, &[], &rules)?;
    assert_eq!(route.alternatives.as_slice().len(), 1);
    Ok(())
}

#[test]
fn an_excluded_account_is_named_when_another_account_cannot_serve_the_model()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let free = catalogue
        .pools
        .values()
        .flatten()
        .find(|m| m.model.as_str() == "opencode/opencode/glm-5.3-flash-free")
        .cloned()
        .ok_or("missing free member")?;
    catalogue.pools.insert("trial".into(), vec![free]);
    let records: Vec<_> = ["zen", "go"]
        .map(|key| {
            let mut record = availability();
            record.adapter = "opencode".into();
            record.account_key = Some(key.into());
            record
        })
        .to_vec();
    let config = RoutingConfig::default();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    let mut query = request("codex/test")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    // The go account cannot serve a free model, so excluding zen empties it.
    let rules = [rule(ExclusionSelector::Account {
        adapter: "opencode".into(),
        key: "zen".into(),
    })];
    let error = resolve_for_account_with_seed_excluding(&query, &ctx, None, "acct", 0, &[], &rules)
        .unwrap_err();
    assert_eq!(error.code(), "no_eligible_pool_member");
    assert!(
        error
            .to_string()
            .contains("excluded by flag: account opencode/zen"),
        "{error}"
    );
    Ok(())
}

#[test]
fn a_single_model_route_honours_exclusions_unlike_a_pin() -> Result<(), Box<dyn std::error::Error>>
{
    let (catalogue, records) = exclusion_fixture()?;
    let claude = catalogue.pools["trial"][0].model.clone();
    let mut config = RoutingConfig::default();
    config.routes.insert(
        "solo".into(),
        NamedRoute {
            model: claude.clone(),
            effort: None,
        },
    );
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    let mut query = request(claude.as_str())?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("solo")?);
    let rules = [rule(ExclusionSelector::Provider("claude".into()))];
    let error = resolve_for_account_with_seed_excluding(&query, &ctx, None, "solo", 0, &[], &rules)
        .unwrap_err();
    assert_eq!(error.code(), "no_eligible_pool_member");
    assert!(
        error
            .to_string()
            .contains("excluded by flag: provider claude"),
        "{error}"
    );
    assert!(
        resolve_for_account_with_seed_excluding(
            &request(claude.as_str())?,
            &ctx,
            None,
            "s",
            0,
            &[],
            &rules
        )
        .is_ok()
    );
    Ok(())
}

#[test]
fn unknown_exclusion_selectors_are_refused_with_the_valid_forms()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let models: Vec<_> = catalogue.models().iter().map(|e| e.model.clone()).collect();
    let account = |text: &str| -> Result<Option<(String, String)>, String> {
        Ok((text == "work").then(|| ("codex".to_owned(), "work".to_owned())))
    };
    let parse = |text: &str| ExclusionSelector::parse(text, &["claude", "codex"], &models, account);
    assert_eq!(
        parse("claude")?,
        ExclusionSelector::Provider("claude".into())
    );
    assert_eq!(
        parse("work")?,
        ExclusionSelector::Account {
            adapter: "codex".into(),
            key: "work".into()
        }
    );
    let model = models[0].as_str().to_owned();
    assert_eq!(
        parse(&model)?,
        ExclusionSelector::Model(ModelId::parse(&model)?)
    );
    let error = parse("claud").unwrap_err();
    assert_eq!(error.code(), "unknown_exclusion");
    assert!(error.to_string().contains("valid forms: a provider"));
    Ok(())
}

#[test]
fn configured_model_exclusion_removes_the_member_from_every_pool()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let model = ModelId::parse("codex/gpt-6-luna")?;
    let mut config = RoutingConfig::default();
    config.model_exclusions.push(ModelExclusion {
        model: model.clone(),
        schedule: Schedule::default(),
    });
    let ctx = context(&catalogue, &config, now()?);
    let mut seen = 0;
    for (name, members) in &catalogue.pools {
        if !members.iter().any(|member| member.model == model) {
            continue;
        }
        seen += 1;
        let mut query = request("codex/test")?;
        query.choice = RouteChoice::NamedRoute(RouteName::parse(name)?);
        let route = resolve(&query, &ctx)?;
        let receipt = route.pool_selection.ok_or("pool selection")?;
        let candidate = receipt
            .candidates
            .iter()
            .find(|candidate| candidate.model == model.as_str())
            .ok_or("candidate")?;
        assert_eq!(
            candidate.exclusion.as_deref(),
            Some("excluded by config: model codex/gpt-6-luna")
        );
        assert_ne!(receipt.pick, model.as_str());
    }
    assert!(seen > 0);
    Ok(())
}

#[test]
fn a_malformed_persisted_exclusion_fails_closed_for_its_row_only()
-> Result<(), Box<dyn std::error::Error>> {
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "model_exclusions": [
            {"model": "codex/gpt-6-luna", "schedule": {"expires": "not-a-time"}},
            {"model": "claude/unreadable", "extra": true},
            {"model": "not a model"},
            {"model": "codex/gpt-6-terra", "schedule": {"expires": "2030-01-01T00:00:00Z"}},
        ],
    }))?;
    let models: Vec<&str> = config
        .model_exclusions
        .iter()
        .map(|row| row.model.as_str())
        .collect();
    // The two rows whose model parses stay excluded; the unparseable one is
    // reported, and the valid row is untouched.
    assert_eq!(
        models,
        ["codex/gpt-6-luna", "claude/unreadable", "codex/gpt-6-terra"]
    );
    assert!(config.model_exclusions[0].schedule.expires.is_none());
    assert!(config.model_exclusions[2].schedule.expires.is_some());
    let notes = config.model_exclusions.notes();
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert!(notes[0].contains("model_exclusions[0]") && notes[0].contains("stays excluded"));
    assert!(notes[2].contains("model_exclusions[2]") && notes[2].contains("is ignored"));
    // A repaired row excludes its model in the pools.
    let catalogue = Catalogue::bundled()?;
    let ctx = context(&catalogue, &config, now()?);
    let mut query = request("codex/test")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("junior")?);
    let route = resolve(&query, &ctx)?;
    let receipt = route.pool_selection.ok_or("pool selection")?;
    assert!(receipt.candidates.iter().any(|candidate| candidate.model == "codex/gpt-6-luna"
        && candidate.exclusion.is_some()));
    Ok(())
}

#[test]
fn review_project_overrides_order_and_shape_slots_through_loader()
-> Result<(), Box<dyn std::error::Error>> {
    let config = load_routing(
        "schema_version=1\n[domains.routing.review.lenses]\norder=['flexibility','tests','reliability']\n[domains.routing.review.shapes.standard]\nreviewers=[{pool='junior',count=2}]\n",
    )??;
    let review = &config.domains.routing.review;
    let panel = review.resolve_panel(
        &crate::ReviewRequest {
            shape: Some("standard".into()),
            ..Default::default()
        },
        None,
        &[],
        false,
    )?;
    let lenses: Vec<_> = panel.slots.iter().map(|slot| slot.lens.as_str()).collect();
    assert_eq!(lenses, ["flexibility", "tests"]);
    Ok(())
}

#[test]
fn review_path_lens_rules_load_replace_or_clear_the_bundled_table_through_the_loader()
-> Result<(), Box<dyn std::error::Error>> {
    let paths = vec!["crates/x/tests/a.rs".to_owned(), "docs/e2e/b.md".to_owned()];
    for (text, expected) in [
        // No override keeps the bundled rules.
        ("schema_version=1\n", vec!["**/tests/**"]),
        // A custom table replaces the bundled table rather than extending it.
        (
            "schema_version=1\n[domains.routing.review.lenses.by_paths.\"docs/e2e/**\"]\nadd=['tests']\n",
            vec!["docs/e2e/**"],
        ),
        // An empty table clears every bundled rule.
        (
            "schema_version=1\n[domains.routing.review.lenses.by_paths]\n",
            vec![],
        ),
    ] {
        let config = load_routing(text)??;
        let review = &config.domains.routing.review;
        let plan = review.lens_plan(None, &paths, false);
        assert_eq!(plan.matched_paths, expected, "{text}");
        assert_eq!(plan.add.is_empty(), expected.is_empty(), "{text}");
    }
    Ok(())
}

#[test]
fn review_floor_policy_can_be_overridden_and_extended() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_routing(
        "schema_version=1\n[domains.routing.review.floors.security]\ntier='routine'\nlenses=[]\n[domains.routing.review.floors.custom]\npaths=['**/special/**']\ntier='crucial'\nlenses=['reliability']\n",
    )??;
    let review = &config.domains.routing.review;
    let security = review.path_floors(&["src/credentials.rs".into()]);
    assert_eq!(security.tier, None);
    let custom = review.path_floors(&["src/special/a.rs".into()]);
    assert_eq!(custom.tier.as_deref(), Some("crucial"));
    assert!(custom.lenses.contains(&"reliability".to_owned()));
    Ok(())
}

fn observation(
    adapter: &str,
    account: &str,
    route: &str,
    state: provenant_quota::ReadinessState,
    reason: Option<provenant_quota::ReadinessReason>,
    window: (i64, i64),
) -> provenant_quota::RouteReadiness {
    provenant_quota::RouteReadiness {
        schema_version: 1,
        scope: provenant_quota::ReadinessScope {
            host: provenant_quota::LOCAL_HOST.into(),
            adapter: adapter.into(),
            account_key: account.into(),
            route: route.into(),
        },
        state,
        reason,
        observed_unix_seconds: window.0,
        expires_unix_seconds: window.1,
        reset_unix_seconds: None,
        evidence_ref: Some("fixture".into()),
    }
}

/// A caller-built lookup over fixed observations, as the CLI builds one over
/// the readiness files on disk.
fn readiness_over(
    records: Vec<provenant_quota::RouteReadiness>,
) -> impl Fn(&provenant_quota::ReadinessScope, i64) -> provenant_quota::ReadinessRead {
    move |scope, now| {
        provenant_quota::ReadinessFile {
            schema_version: 1,
            login: None,
            records: records
                .iter()
                .filter(|r| {
                    r.scope.adapter == scope.adapter && r.scope.account_key == scope.account_key
                })
                .cloned()
                .collect(),
        }
        .get(scope, now)
    }
}

fn readiness_access() -> [AccessContext<'static>; 2] {
    ["codex", "agy"].map(|adapter| AccessContext {
        adapter,
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    })
}

#[test]
fn expired_login_observation_warns_and_remains_selectable() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_quota::{
        ACCOUNT_ROUTE, LOCAL_HOST, LoginObservation, ReadinessFile, ReadinessScope,
    };
    let file = ReadinessFile {
        login: Some(LoginObservation {
            scope: ReadinessScope {
                host: LOCAL_HOST.into(),
                adapter: "codex".into(),
                account_key: "opaque-account".into(),
                route: ACCOUNT_ROUTE.into(),
            },
            observed_unix_seconds: 80,
            valid_until_unix_seconds: 100,
            expires_unix_seconds: Some(85),
        }),
        ..Default::default()
    };
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [availability()];
    let access = readiness_access();
    for cached in [false, true] {
        let lookup = |scope: &ReadinessScope, time| file.get(scope, if cached { 90 } else { time });
        let context = RoutingContext {
            availability: &records,
            access: &access,
            readiness: Some(&lookup),
            ..context(&catalogue, &config, now()?)
        };
        let selected = resolve(&request("codex/gpt-6-luna")?, &context)?;
        assert!(
            matches!(selected.readiness, RouteReadiness::Ready { extra, .. } if extra.contains_key("availability_warning"))
        );
    }
    Ok(())
}

fn agy_account(key: &str) -> AvailabilityRecord {
    AvailabilityRecord {
        adapter: "agy".into(),
        account_key: Some(key.into()),
        ..availability()
    }
}

#[test]
fn readiness_states_and_expiry_decide_selection() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{ReadinessReason as Why, ReadinessState as State};
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [availability()];
    let access = readiness_access();
    let fresh = (80, 120);
    let status = |observed: Vec<provenant_quota::RouteReadiness>| {
        let lookup = readiness_over(observed);
        let context = RoutingContext {
            availability: &records,
            access: &access,
            readiness: Some(&lookup),
            ..context(&catalogue, &config, now()?)
        };
        Ok::<_, Box<dyn std::error::Error>>(
            resolve(&request("codex/gpt-6-luna")?, &context)?.readiness,
        )
    };
    let route = "codex/gpt-6-luna";
    let one = |state, reason, window| {
        vec![observation(
            "codex",
            "opaque-account",
            route,
            state,
            reason,
            window,
        )]
    };
    // No record, a ready record and an unknown record all dispatch as before.
    for observed in [
        vec![],
        one(State::Ready, None, fresh),
        one(State::Unknown, Some(Why::UnreachablePeer), fresh),
        one(State::Unavailable, Some(Why::CommandFailure), fresh),
    ] {
        assert!(
            matches!(status(observed.clone())?, RouteReadiness::Ready { .. }),
            "{observed:?}"
        );
    }
    // Unavailable excludes with the projected reason.
    for (why, expected) in [
        (
            Why::MissingExecutable,
            RouteExclusionReason::MissingExecutable,
        ),
        (Why::AccountMismatch, RouteExclusionReason::AccountMismatch),
        (Why::SignedOut, RouteExclusionReason::AuthRequired),
        (Why::LoginExpired, RouteExclusionReason::LoginExpired),
        (Why::Exhausted, RouteExclusionReason::ProviderRefused),
        (Why::ProviderRefusal, RouteExclusionReason::ProviderRefused),
    ] {
        match status(one(State::Unavailable, Some(why), fresh))? {
            RouteReadiness::Unavailable { reason, .. } => assert_eq!(reason, expected, "{why:?}"),
            other => panic!("{why:?} must exclude: {other:?}"),
        }
    }
    for (why, expected) in [
        (Why::SignedOut, RouteExclusionReason::AuthRequired),
        (Why::LoginExpired, RouteExclusionReason::LoginExpired),
        (Why::AccountMismatch, RouteExclusionReason::AccountMismatch),
    ] {
        assert!(
            matches!(status(one(State::Unknown, Some(why), fresh))?, RouteReadiness::Unavailable { reason, .. } if reason == expected)
        );
    }
    let account_wide = observation(
        "codex",
        "opaque-account",
        "*",
        State::Unknown,
        Some(Why::SignedOut),
        fresh,
    );
    assert!(matches!(
        status(vec![account_wide])?,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::AuthRequired,
            ..
        }
    ));
    // At or beyond expiry, and before the observation, it reads unknown.
    for window in [(80, 100), (80, 90), (101, 200)] {
        assert!(matches!(
            status(one(State::Unavailable, Some(Why::SignedOut), window))?,
            RouteReadiness::Ready { .. }
        ));
    }
    // An unavailable record for another account, adapter or model is not this route's.
    for other in [
        observation(
            "codex",
            "someone-else",
            "*",
            State::Unavailable,
            Some(Why::SignedOut),
            fresh,
        ),
        observation(
            "codex",
            "opaque-account",
            "codex/gpt-6.1-terra",
            State::Unavailable,
            Some(Why::SignedOut),
            fresh,
        ),
    ] {
        assert!(matches!(status(vec![other])?, RouteReadiness::Ready { .. }));
    }
    // Without a lookup nothing is read.
    let context = RoutingContext {
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    assert!(matches!(
        resolve(&request(route)?, &context)?.readiness,
        RouteReadiness::Ready { .. }
    ));
    Ok(())
}

#[test]
fn unavailable_account_moves_selection_to_an_alternate_account()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{ReadinessReason as Why, ReadinessState as State};
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
    let access = readiness_access();
    let pick = |observed: Vec<provenant_quota::RouteReadiness>| {
        let lookup = readiness_over(observed);
        let context = RoutingContext {
            availability: &records,
            access: &access,
            readiness: Some(&lookup),
            ..context(&catalogue, &config, now()?)
        };
        Ok::<_, Box<dyn std::error::Error>>(resolve(&request("codex/gpt-6-luna")?, &context)?)
    };
    let refused = |account: &str, route: &str| {
        observation(
            "codex",
            account,
            route,
            State::Unavailable,
            Some(Why::ProviderRefusal),
            (80, 120),
        )
    };
    // The same account is chosen with no record; refusing it moves to the other.
    for first in ["work", "review"] {
        let other = if first == "work" { "review" } else { "work" };
        for route in ["*", "codex/gpt-6-luna"] {
            let chosen = pick(vec![refused(first, route)])?;
            assert_eq!(
                chosen.account_key.as_deref(),
                Some(other),
                "{first} {route}"
            );
            assert!(matches!(chosen.readiness, RouteReadiness::Ready { .. }));
        }
        let expired = observation(
            "codex",
            first,
            "*",
            State::Unavailable,
            Some(Why::LoginExpired),
            (80, 120),
        );
        let known = observation(
            "codex",
            other,
            "codex/gpt-6-luna",
            State::Ready,
            None,
            (80, 120),
        );
        let chosen = pick(vec![expired, known])?;
        assert_eq!(chosen.account_key.as_deref(), Some(other));
        assert!(matches!(chosen.readiness, RouteReadiness::Ready { .. }));
    }
    // A refusal for one model does not cover the account's other models.
    let other_model = refused("work", "codex/gpt-6.1-terra");
    let chosen = pick(vec![other_model.clone()])?;
    assert!(matches!(chosen.readiness, RouteReadiness::Ready { .. }));
    // Every account refused leaves the route unavailable with the refusal.
    let none = pick(vec![refused("work", "*"), refused("review", "*")])?;
    assert!(matches!(
        none.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::ProviderRefused,
            ..
        }
    ));
    Ok(())
}

#[test]
fn pool_refusal_covers_its_pool_and_a_model_refusal_only_its_model()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{ReadinessReason as Why, ReadinessState as State};
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [agy_account("work")];
    let access = readiness_access();
    let refused = |route: &str| {
        observation(
            "agy",
            "work",
            route,
            State::Unavailable,
            Some(Why::ProviderRefusal),
            (80, 120),
        )
    };
    let ready = |observed: Vec<provenant_quota::RouteReadiness>, model: &str| {
        let lookup = readiness_over(observed);
        let context = RoutingContext {
            availability: &records,
            access: &access,
            readiness: Some(&lookup),
            ..context(&catalogue, &config, now()?)
        };
        Ok::<_, Box<dyn std::error::Error>>(matches!(
            resolve(&request(model)?, &context)?.readiness,
            RouteReadiness::Ready { .. }
        ))
    };
    let (sonnet, opus, gemini) = (
        "agy/claude-sonnet-4-6",
        "agy/claude-opus-4-6-thinking",
        "agy/gemini-3.8-flash",
    );
    // The verified `claude` pool covers both of its models and nothing else.
    let pool = vec![refused("claude")];
    assert!(!ready(pool.clone(), sonnet)?);
    assert!(!ready(pool.clone(), opus)?);
    assert!(ready(pool, gemini)?);
    // A refusal without a pool covers its model only.
    let model = vec![refused(sonnet)];
    assert!(!ready(model.clone(), sonnet)?);
    assert!(ready(model.clone(), opus)?);
    assert!(ready(model, gemini)?);
    // An account refusal covers every route of the account.
    let account = vec![refused("*")];
    assert!(!ready(account.clone(), sonnet)?);
    assert!(!ready(account, gemini)?);
    Ok(())
}

#[test]
fn unavailable_pool_member_is_drawn_around() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{ReadinessReason as Why, ReadinessState as State};
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = toml_edit::de::from_str(
        "[pools]\nmixed=[{model='agy/claude-sonnet-4-6',weight=1},{model='agy/gemini-3.8-flash',weight=1}]\n",
    )?;
    let records = [agy_account("work")];
    let access = readiness_access();
    let request = RouteRequest {
        intent: Default::default(),
        choice: RouteChoice::NamedRoute(RouteName::parse("mixed")?),
        effort: None,
        confidential: false,
        fallback: Fallback::None,
    };
    let drawn = |observed: Vec<provenant_quota::RouteReadiness>| {
        let lookup = readiness_over(observed);
        let context = RoutingContext {
            availability: &records,
            access: &access,
            readiness: Some(&lookup),
            ..context(&catalogue, &config, now()?)
        };
        (0..16)
            .map(|index| {
                Ok::<_, Box<dyn std::error::Error>>(
                    resolve_with_seed(&request, &context, "readiness", index)?
                        .model
                        .to_string(),
                )
            })
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
    };
    // Unknown does not narrow the pool: both members are drawn.
    assert_eq!(drawn(vec![])?.len(), 2);
    // An unavailable `claude` pool leaves only the Gemini member.
    let refused = observation(
        "agy",
        "work",
        "claude",
        State::Unavailable,
        Some(Why::ProviderRefusal),
        (80, 120),
    );
    assert_eq!(
        drawn(vec![refused])?,
        ["gemini-3.8-flash".to_owned()].into_iter().collect()
    );
    Ok(())
}

#[test]
fn routing_default_matches_bundled_asset() {
    let asset: RoutingConfig =
        toml_edit::de::from_str(include_str!("../assets/review-defaults.toml")).unwrap();
    assert_eq!(
        serde_json::to_value(RoutingConfig::default()).unwrap(),
        serde_json::to_value(asset).unwrap()
    );
}

#[test]
fn mixed_capability_pool_filters_before_drawing_and_explicit_routes_refuse()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::{ADAPTER_SPECS, AdapterSpec, LaunchCapabilities, run::NetworkAccess};
    let catalogue = Catalogue::bundled()?;
    let mut config = RoutingConfig::default();
    config.pools.insert(
        "fixture-capabilities".into(),
        vec![
            RoutePoolMember {
                model: ModelId::parse("cursor/gpt-5.3-codex-low")?,
                weight: 1,
                effort: None,
                efforts: vec![],
                free: false,
            },
            RoutePoolMember {
                model: ModelId::parse("codex/gpt-6-luna")?,
                weight: 0,
                effort: None,
                efforts: vec![],
                free: false,
            },
        ],
    );
    let narrow = AdapterSpec {
        skills: None,
        launch: LaunchCapabilities {
            write: false,
            network: &[NetworkAccess::Provider],
            ..ADAPTER_SPECS[2].launch
        },
        ..ADAPTER_SPECS[2]
    };
    let lookup = |adapter: &str| {
        if adapter == "cursor" {
            Some(narrow)
        } else {
            AdapterSpec::get(adapter).copied()
        }
    };
    let mut cursor = availability();
    cursor.adapter = "cursor".into();
    let availability = [cursor, availability()];
    let access = [
        AccessContext {
            adapter: "cursor",
            disclosure_ref: "policy",
            confinement_ref: "grant",
        },
        AccessContext {
            adapter: "codex",
            disclosure_ref: "policy",
            confinement_ref: "grant",
        },
    ];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.launch_specs = Some(&lookup);
    for (writes, network, skill_location, expected) in [
        (false, NetworkAccess::Provider, false, "cursor"),
        (true, NetworkAccess::Provider, false, "codex"),
        (false, NetworkAccess::Open, false, "codex"),
        (false, NetworkAccess::Provider, true, "codex"),
    ] {
        let mut query = request("cursor/gpt-5.3-codex-low")?;
        query.choice = RouteChoice::NamedRoute(RouteName::parse("fixture-capabilities")?);
        query.intent.writes = writes;
        query.intent.network = Some(network);
        query.intent.skill_location = skill_location;
        assert_eq!(
            query
                .intent
                .check_launch(&narrow, "gpt-5.3-codex-low", None)
                .is_ok(),
            expected == "cursor"
        );
        let selected = resolve(&query, &ctx)?;
        assert_eq!(
            selected.adapter, expected,
            "writes={writes} network={network:?}"
        );
        if expected == "codex" {
            assert!(
                selected
                    .pool_selection
                    .unwrap()
                    .candidates
                    .iter()
                    .any(|candidate| candidate.model.starts_with("cursor/")
                        && candidate.exclusion.is_some())
            );
        }
        let mut explicit = request("cursor/gpt-5.3-codex-low")?;
        explicit.intent = query.intent;
        assert_eq!(
            matches!(
                resolve(&explicit, &ctx),
                Err(RoutingError::UnsupportedLaunch(_))
            ),
            expected == "codex"
        );
    }
    Ok(())
}

#[test]
fn image_input_preference_keeps_pool_weights_and_falls_back()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::AdapterSpec;
    let catalogue = Catalogue::bundled()?;
    for (cursor_image, codex_image, cursor_weight, codex_weight, preferred) in [
        (true, false, 1, 50, Some("cursor")),
        (false, true, 50, 0, Some("codex")),
        (false, false, 1, 0, Some("cursor")),
        (true, true, 1, 1, None),
    ] {
        let mut config = RoutingConfig::default();
        config.pools.insert(
            "image-fixture".into(),
            vec![
                RoutePoolMember {
                    model: ModelId::parse("cursor/gpt-5.3-codex-low")?,
                    weight: cursor_weight,
                    effort: None,
                    efforts: vec![],
                    free: false,
                },
                RoutePoolMember {
                    model: ModelId::parse("codex/gpt-6-luna")?,
                    weight: codex_weight,
                    effort: None,
                    efforts: vec![],
                    free: false,
                },
            ],
        );
        let lookup = |adapter: &str| {
            let mut spec = *AdapterSpec::get(adapter)?;
            spec.launch.image_input = match adapter {
                "cursor" => cursor_image,
                "codex" => codex_image,
                _ => false,
            };
            Some(spec)
        };
        let mut cursor = availability();
        cursor.adapter = "cursor".into();
        let available = [cursor, availability()];
        let access = [
            AccessContext {
                adapter: "cursor",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
            AccessContext {
                adapter: "codex",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
        ];
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &available;
        ctx.access = &access;
        ctx.launch_specs = Some(&lookup);
        let mut query = request("cursor/gpt-5.3-codex-low")?;
        query.choice = RouteChoice::NamedRoute(RouteName::parse("image-fixture")?);
        query.intent.image_input = true;
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..32 {
            let result = resolve_with_seed(&query, &ctx, "image", seed)?;
            if let Some(expected) = preferred {
                assert_eq!(result.adapter, expected);
            }
            if cursor_image == codex_image {
                let mut without_image = query.clone();
                without_image.intent.image_input = false;
                assert_eq!(
                    result.adapter,
                    resolve_with_seed(&without_image, &ctx, "image", seed)?.adapter
                );
            }
            seen.insert(result.adapter);
        }
        if preferred.is_none() {
            assert_eq!(seen.len(), 2);
        }
    }
    Ok(())
}

/// A snapshot can only be built by resolving against current, validated inputs;
/// deserialized ResolvedRoute projections cannot assert fresh availability.
struct RoutingSnapshot<'a> {
    catalogue_revision: String,
    now_unix_seconds: i64,
    routes: Vec<ResolvedRoute>,
    families: &'a [FamilyFact],
}
impl<'a> RoutingSnapshot<'a> {
    pub fn new(
        requests: &[crate::RouteRequest],
        context: &crate::RoutingContext<'_>,
        families: &'a [FamilyFact],
    ) -> Result<Self, RoutingError> {
        if requests.len() > 256 || families.len() > 256 {
            return Err(RoutingError::ReviewDemand);
        }
        let routes = requests
            .iter()
            .map(|r| crate::resolve(r, context))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            catalogue_revision: context.catalogue.revision.clone(),
            now_unix_seconds: context.now.as_timestamp().as_second(),
            routes,
            families,
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewDemand {
    pub family_slots: BTreeMap<String, u64>,
}
#[derive(Debug, Serialize)]
struct ReviewAvailability {
    catalogue_revision: String,
    expires_unix_seconds: Option<i64>,
    families: BTreeMap<String, u64>,
    blockers: Vec<ReviewBlocker>,
    satisfied: bool,
}
impl ReviewAvailability {
    pub fn satisfied(&self) -> bool {
        self.satisfied
    }
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReviewBlocker {
    Route {
        adapter: String,
        model: String,
        reason: RouteExclusionReason,
    },
    UnknownFamily {
        model: String,
    },
    FamilySlots {
        family: String,
        required: u64,
        available: u64,
    },
}
fn review_availability(
    snapshot: &RoutingSnapshot<'_>,
    demand: &ReviewDemand,
) -> Result<ReviewAvailability, RoutingError> {
    if demand.family_slots.is_empty()
        || demand.family_slots.len() > 16
        || demand
            .family_slots
            .iter()
            .any(|(f, n)| !crate::request::identifier(f) || !(1..=64).contains(n))
        || snapshot.routes.len() > 256
        || snapshot.families.len() > 256
    {
        return Err(RoutingError::ReviewDemand);
    }
    let mut families = BTreeMap::<String, u64>::new();
    let mut blockers = Vec::new();
    let mut expiry = None::<i64>;
    let mut seen = std::collections::BTreeSet::new();
    for route in &snapshot.routes {
        if !seen.insert((&route.adapter, &route.model)) {
            continue;
        }
        let ready_until = match &route.readiness {
            RouteReadiness::Ready {
                expires_unix_seconds,
                ..
            } if route.catalogue_revision == snapshot.catalogue_revision
                && *expires_unix_seconds > snapshot.now_unix_seconds =>
            {
                *expires_unix_seconds
            }
            _ => {
                let reason = match &route.readiness {
                    RouteReadiness::Unavailable { reason, .. } => reason.clone(),
                    _ => RouteExclusionReason::StaleAvailability,
                };
                blockers.push(ReviewBlocker::Route {
                    adapter: route.adapter.clone(),
                    model: route.model.clone(),
                    reason,
                });
                continue;
            }
        };
        let fact = snapshot.families.iter().map(|f| f.record()).find(|f| {
            f.model.adapter() == route.adapter
                && f.model.model() == route.model
                && f.catalogue_revision == snapshot.catalogue_revision
                && f.observed_unix_seconds <= snapshot.now_unix_seconds
                && f.expires_unix_seconds > snapshot.now_unix_seconds
                && f.family.is_some()
        });
        if let Some(fact) = fact {
            if let Some(family) = &fact.family {
                *families.entry(family.clone()).or_default() += 1;
                let until = ready_until.min(fact.expires_unix_seconds);
                expiry = Some(expiry.map_or(until, |current| current.min(until)));
            }
        } else {
            blockers.push(ReviewBlocker::UnknownFamily {
                model: format!("{}/{}", route.adapter, route.model),
            });
        }
    }
    let mut satisfied = true;
    for (family, required) in &demand.family_slots {
        let available = families.get(family).copied().unwrap_or(0);
        if available < *required {
            satisfied = false;
            blockers.push(ReviewBlocker::FamilySlots {
                family: family.clone(),
                required: *required,
                available,
            });
        }
    }
    Ok(ReviewAvailability {
        catalogue_revision: snapshot.catalogue_revision.clone(),
        expires_unix_seconds: expiry,
        families,
        blockers,
        satisfied,
    })
}

#[test]
fn model_choice_preferences_expire_and_can_be_revoked() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = RoutingConfig::default();
    let mut query = request("codex/gpt-6-luna")?;
    let pinned = query.clone();
    query.choice = RouteChoice::NamedRoute(RouteName::parse("senior")?);
    let now = provenant_core::WallTime::from_unix_seconds(100)?;
    let expires = provenant_core::WallTime::from_unix_seconds(200)?;
    config.model_choices.push(ModelChoiceOverride {
        dimension: "model".into(),
        risk_tier: "substantial".into(),
        task_kind: "repair".into(),
        preferred: ModelId::parse("codex/example")?,
        expires: Some(expires),
    });
    assert_eq!(
        config
            .preferred_model(&query, "substantial", "repair", now)
            .map(ModelId::as_str),
        Some("codex/example")
    );
    assert!(
        config
            .preferred_model(&query, "routine", "repair", now)
            .is_none()
    );
    assert!(
        config
            .preferred_model(&query, "substantial", "repair", expires)
            .is_none()
    );
    assert!(
        config
            .preferred_model(&pinned, "substantial", "repair", now)
            .is_none()
    );
    config.model_choices.clear();
    assert!(
        config
            .preferred_model(&query, "substantial", "repair", now)
            .is_none()
    );
    Ok(())
}

#[test]
fn malformed_model_choice_does_not_survive_without_expiry() -> Result<(), Box<dyn std::error::Error>>
{
    let text = "schema_version=1\n[[domains.routing.model_choices]]\ndimension='model'\nrisk_tier='substantial'\ntask_kind='repair'\npreferred='codex/gpt-6-luna'\nexpires='bad'\n[[domains.routing.model_choices]]\ndimension='model'\nrisk_tier='routine'\ntask_kind='repair'\npreferred='codex/gpt-6-luna'\nexpires='2030-01-01T00:00:00Z'\n";
    let config = load_routing(text)??;
    assert_eq!(config.domains.routing.model_choices.len(), 1);
    assert_eq!(config.domains.routing.model_choices[0].risk_tier, "routine");
    assert!(!config.warnings.is_empty());
    Ok(())
}

#[test]
fn model_choice_selects_only_eligible_pool_members_before_expiry()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let mut config: RoutingConfig = toml_edit::de::from_str(
        "[pools]\nchoice=[{model='codex/gpt-6-luna',weight=1},{model='codex/gpt-6.1-sol',weight=1}]\n",
    )?;
    let expires = provenant_core::WallTime::from_unix_seconds(200)?;
    config.model_choices.push(ModelChoiceOverride {
        dimension: "model".into(),
        risk_tier: "substantial".into(),
        task_kind: "repair".into(),
        preferred: ModelId::parse("codex/gpt-6.1-sol")?,
        expires: Some(expires),
    });
    let records = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let mut ctx = RoutingContext {
        choice_context: Some(("substantial", "repair")),
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("choice")?);
    for seed in 0..8 {
        let route = resolve_with_seed(&query, &ctx, "preference", seed)?;
        assert_eq!(route.model, "gpt-6.1-sol");
        assert_eq!(
            route
                .pool_selection
                .unwrap()
                .extra
                .get("selection_rule")
                .and_then(serde_json::Value::as_str),
            Some("model_choice_override")
        );
    }
    let excluded = [ModelId::parse("codex/gpt-6.1-sol")?];
    let route = resolve_for_account_with_seed_excluding(
        &query,
        &ctx,
        None,
        "preference",
        0,
        &excluded,
        &[],
    )?;
    assert_eq!(route.model, "gpt-6-luna");
    ctx.now = expires;
    assert_ne!(
        resolve_with_seed(&query, &ctx, "preference", 0)?
            .pool_selection
            .unwrap()
            .extra
            .get("selection_rule")
            .and_then(serde_json::Value::as_str),
        Some("model_choice_override")
    );
    query.choice = RouteChoice::Model(ModelId::parse("codex/gpt-6-luna")?);
    ctx.now = now()?;
    assert_eq!(resolve(&query, &ctx)?.model, "gpt-6-luna");
    Ok(())
}

#[test]
fn model_choice_outside_route_obeys_restrictions() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let mut config: RoutingConfig = toml_edit::de::from_str(
        "[routes.choice]\nmodel='codex/gpt-6-luna'\n[[model_policy.rules]]\nmodel='codex/gpt-6.1-sol'\n[model_policy.rules.policy.lane]\nread_only=true\n",
    )?;
    config.model_choices.push(ModelChoiceOverride {
        dimension: "model".into(),
        risk_tier: "substantial".into(),
        task_kind: "repair".into(),
        preferred: ModelId::parse("codex/gpt-6.1-sol")?,
        expires: Some(provenant_core::WallTime::from_unix_seconds(200)?),
    });
    let records = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let ctx = RoutingContext {
        choice_context: Some(("substantial", "repair")),
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("choice")?);
    assert_eq!(resolve(&query, &ctx)?.model, "gpt-6.1-sol");
    query.intent.writes = true;
    assert_eq!(resolve(&query, &ctx)?.model, "gpt-6-luna");
    Ok(())
}

#[test]
fn alias_skips_known_spent_members_and_refuses_all_with_resets()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({"pools":{"trial":[
        {"model":"codex/gpt-6-luna","weight":100},
        {"model":"codex/gpt-6.1-sol","weight":1}
    ]}}))?;
    let records = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let spent = |model: &str, reset| -> Result<QuotaObservation, RoutingError> {
        let mut q = quota(model, "opaque-account", QuotaStatus::Exhausted, 1, 50, 60)?;
        q.snapshot.windows.push(QuotaWindow {
            pool: "secondary".into(),
            window_seconds: Some(1000),
            used_basis_points: 10_000,
            resets_unix_seconds: Some(reset),
        });
        Ok(q)
    };
    let first = spent("codex/gpt-6-luna", 400)?;
    let readings = [spent("codex/gpt-6-luna", 400)?];
    let ctx = RoutingContext {
        availability: &records,
        access: &access,
        quota: &readings,
        ..context(&catalogue, &config, now()?)
    };
    let mut alias = request("codex/gpt-6-luna")?;
    alias.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    assert_eq!(resolve(&alias, &ctx)?.model, "gpt-6.1-sol");
    assert_eq!(
        resolve(&request("codex/gpt-6-luna")?, &ctx)?.model,
        "gpt-6-luna"
    );
    let mut partial = spent("codex/gpt-6-luna", 400)?;
    partial.snapshot.status = QuotaStatus::Available;
    let partial_readings = [partial];
    let partial_ctx = RoutingContext {
        quota: &partial_readings,
        ..ctx
    };
    assert_eq!(resolve(&alias, &partial_ctx)?.model, "gpt-6.1-sol");
    let readings = [first, spent("codex/gpt-6.1-sol", 300)?];
    let ctx = RoutingContext {
        quota: &readings,
        ..ctx
    };
    let Err(RoutingError::NoEligiblePoolMember { exclusions }) = resolve(&alias, &ctx) else {
        panic!("all spent alias must refuse")
    };
    assert!(
        exclusions
            .iter()
            .any(|e| e.contains("codex/gpt-6-luna") && e.contains("400"))
    );
    assert!(
        exclusions
            .iter()
            .any(|e| e.contains("codex/gpt-6.1-sol") && e.contains("300"))
    );
    Ok(())
}
