use super::*;
use provenant_config::{DomainConfig, DomainSections};
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{RouteEligibility, RouteExclusionReason, RouteReadiness};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[test]
fn default_pools_do_not_include_free_routes() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let listed = list(&RouteListQuery::default(), &context)?;
    assert!(listed.pools.values().flatten().all(|member| !member.free));
    Ok(())
}

#[test]
fn model_policy_config_accepts_layered_rules() -> Result<(), Box<dyn std::error::Error>> {
    let config = serde_json::from_value::<RoutingConfig>(serde_json::json!({
        "model_policy": {"rules": [{"model": "codex/gpt-6.1-terra", "account": "work",
            "schedule": {"expires": 200}, "policy": {"lane": {"stop_early_ok": false}}}]}
    }));
    assert!(
        config.is_ok(),
        "layered model policy must be accepted: {config:?}"
    );
    assert!(config?.validate().is_empty());
    Ok(())
}

#[test]
fn restricted_routes_refuse_writes() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let mut query = request("agy/gemini-3.8-flash")?;
    query.intent.writes = true;
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
            },
            choice: RouteChoice::NamedRoute(RouteName::parse("cachetest")?),
            effort: None,
            confidential: false,
            allow_free: false,
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
        catalogue,
        config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
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
        allow_free: false,
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
        ("opencode/opencode-go/glm-5.2", "takes no effort"),
    ] {
        let mut query = request(model)?;
        query.effort = Some("unsupported".into());
        let message = resolve(&query, &context)
            .expect_err("effort must be refused")
            .to_string();
        assert!(message.contains(model), "{message}");
        assert!(message.contains("unsupported"), "{message}");
        assert!(message.contains(expected), "{message}");
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
fn legacy_pacing_override_loads_and_evaluates_like_schedule_form()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let now = jiff::Timestamp::now().as_second();
    let expires = now + 3600;
    let legacy: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing_overrides": [{"provider":"codex", "policy":"spend", "starts_unix_seconds":now-60, "expires_unix_seconds":expires, "windows":[]}]
    }))?;
    let canonical: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing_overrides": [{"provider":"codex", "policy":"spend", "schedule":{"starts":now-60,"expires":expires,"windows":[]}}]
    }))?;
    let time = provenant_core::WallTime::from_micros(now * 1_000_000)?;
    let old_context = context(&catalogue, &legacy, time);
    let new_context = context(&catalogue, &canonical, time);
    let model = ModelId::parse("codex/gpt-6.1-terra")?;
    assert_eq!(
        pacing_preview(&model, &old_context)[0].policy,
        PacingPolicy::Spend
    );
    assert_eq!(
        pacing_preview(&model, &old_context)[0].policy,
        pacing_preview(&model, &new_context)[0].policy
    );
    assert!(
        serde_json::to_value(&legacy)?["pacing_overrides"][0]["schedule"]["expires"].is_number()
    );
    let expired: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing_overrides": [{"provider":"codex", "policy":"spend", "expires_unix_seconds":now-1}]
    }))?;
    assert!(expired.pacing_overrides.is_empty());
    Ok(())
}
#[test]
fn nearest_models_respect_namespace_and_distance() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let nearest = |id: &str| -> Result<Vec<String>, RoutingError> {
        Ok(catalogue.nearest_models(&ModelId::parse(id)?))
    };
    assert_eq!(
        nearest("opencode/openrouter/~anthropic/claude-haiku-latestt")?,
        vec!["opencode/openrouter/~anthropic/claude-haiku-latest"]
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
    let wire = r#"{"intent":{"workload":"general","writes":false},"choice":{"kind":"named_route","value":"review.v1-_2"},"effort":null,"confidential":false,"allow_free":false,"fallback":{"kind":"none"}}"#;
    let query: RouteRequest = serde_json::from_str(wire)?;
    assert_eq!(serde_json::to_string(&query)?, wire);
    assert_eq!(
        query.choice,
        RouteChoice::NamedRoute(RouteName::parse("review.v1-_2")?)
    );
    let longest = "x".repeat(64);
    assert_eq!(RouteName::parse(&longest)?.as_str(), longest);
    for text in ["", "bad name", "bad/name", "bad\n", "é", &"x".repeat(65)] {
        assert!(matches!(
            RouteName::parse(text),
            Err(RoutingError::InvalidValue)
        ));
        let wire = serde_json::json!({"kind": "named_route", "value": text});
        let error = serde_json::from_value::<RouteChoice>(wire)
            .err()
            .ok_or("invalid name accepted")?;
        assert!(
            error
                .to_string()
                .contains(&RoutingError::InvalidValue.to_string())
        );
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
fn explicit_unlisted_model_is_ready_without_provider_listing()
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
    let query = request("opencode/opencode/beam-free")?;
    let route = resolve(&query, &context)?;
    assert_eq!(route.model, "opencode/beam-free");
    assert_eq!(route.eligibility, RouteEligibility::UnverifiedModel);
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
    assert!(matches!(
        resolve(&confidential, &context),
        Err(RoutingError::ConfidentialModel)
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
        allow_free: false,
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
    let codex_models: Vec<_> = catalogue
        .models()
        .iter()
        .filter(|entry| entry.model.adapter() == "codex")
        .map(|entry| entry.model.model())
        .collect();
    assert_eq!(codex_models, ["gpt-6-luna", "gpt-6.1-terra", "gpt-6.1-sol"]);
    for (pool, model, weight, default_effort, allowed) in [
        ("junior", "gpt-6.1-terra", 2, "low", vec!["low", "medium"]),
        (
            "senior",
            "gpt-6.1-sol",
            4,
            "medium",
            vec!["low", "medium", "high"],
        ),
        (
            "senior",
            "gpt-6.1-terra",
            4,
            "high",
            vec!["low", "medium", "high"],
        ),
        (
            "principal",
            "gpt-6.1-sol",
            1,
            "high",
            vec!["medium", "high", "xhigh"],
        ),
    ] {
        let member = catalogue.pools[pool]
            .iter()
            .find(|member| member.model.as_str() == format!("codex/{model}"))
            .ok_or("Codex pool member missing")?;
        assert_eq!(member.model.model(), model);
        assert_eq!(member.weight, weight);
        assert_eq!(member.efforts, allowed);
        assert_eq!(member.effort.as_deref(), Some(default_effort));
        // Check this member's effort policy independently of the weighted draw.
        let mut isolated = catalogue.clone();
        isolated.pools.insert(pool.into(), vec![member.clone()]);
        let mut record = availability();
        record.observed_models = vec![model.into()];
        let records = [record];
        let mut context = context(&isolated, &config, now()?);
        context.availability = &records;
        context.access = &access;
        let entry = catalogue.model(&member.model).ok_or("model missing")?;
        assert_eq!(entry.family.as_deref(), Some("openai"));
        assert_eq!(entry.trains_on_prompts, Some(false));
        assert!(entry.evidence_ref.contains("models_cache.json"));
        assert_eq!(entry.efforts, ["low", "medium", "high", "xhigh", "max"]);
        let mut query = request(&format!("codex/{model}"))?;
        query.confidential = true;
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            query.effort = Some(effort.into());
            let exact = resolve(&query, &context)?;
            assert_eq!(exact.effort.as_deref(), Some(effort));
            assert!(matches!(exact.readiness, RouteReadiness::Ready { .. }));
        }
        query.choice = RouteChoice::NamedRoute(RouteName::parse(pool)?);
        query.effort = None;
        let resolved = resolve_with_seed(&query, &context, "codex-tier", 0)?;
        assert_eq!(resolved.model, model);
        assert_eq!(resolved.effort.as_deref(), Some(default_effort));
        for effort in &allowed {
            query.effort = Some((*effort).into());
            let resolved = resolve_with_seed(&query, &context, "codex-tier", 0)?;
            assert_eq!(resolved.model, model);
            assert_eq!(resolved.effort.as_deref(), Some(*effort));
        }
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            if allowed.contains(&effort) {
                continue;
            }
            query.effort = Some(effort.into());
            match resolve_with_seed(&query, &context, "codex-tier", 0) {
                Ok(resolved) => assert_ne!(resolved.model, model),
                Err(RoutingError::NoEligiblePoolMember { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    for entry in catalogue.models() {
        assert!(entry.efforts.iter().all(|effort| {
            ["low", "medium", "high", "xhigh", "max"].contains(&effort.as_str())
        }));
    }
    for members in catalogue.pools.values() {
        for member in members {
            assert!(!member.efforts.iter().any(|effort| effort == "max"));
            assert_ne!(member.effort.as_deref(), Some("max"));
        }
    }
    let principal = &catalogue.pools["principal"];
    assert_eq!(principal.len(), 2);
    assert_eq!(principal[0].model.as_str(), "claude/claude-opus-5-5");
    assert_eq!(principal[0].weight, 1);
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
            )
        } else {
            resolve_with_seed(&query, &ctx, "operation-route-pools", 2)
        };
        let route = result?;
        let receipt = serde_json::to_value(&route)?["pool_selection"].clone();
        assert_eq!(receipt["pool"], "trial", "{}", case.name);
        let candidate_rows = receipt["candidates"].as_array().ok_or("candidates")?;
        assert_eq!(candidate_rows.len(), 3);
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
fn free_pool_members_require_opt_in_and_confidential_models_are_excluded()
-> Result<(), Box<dyn std::error::Error>> {
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
    match resolve(&query, &ctx).unwrap_err() {
        RoutingError::NoEligiblePoolMember { exclusions } => {
            assert!(
                exclusions
                    .iter()
                    .any(|reason| reason == "confidential model")
            );
            assert!(
                exclusions
                    .iter()
                    .any(|reason| reason == "free model disabled")
            );
        }
        error => panic!("unexpected pool error: {error}"),
    }
    // Opting in passes the free gate; availability is still required.
    query.allow_free = true;
    let resolved = resolve(&query, &ctx)?;
    assert_eq!(resolved.adapter, "opencode");
    assert_eq!(
        reason(&resolved),
        Some(&RouteExclusionReason::UnknownAvailability)
    );
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
    assert!(
        serde_json::from_str::<RouteRequest>(
            r#"{"choice":{"kind":"model","value":"codex/a"},"effort":null,"pool":"principal"}"#
        )
        .is_err()
    );
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
        Some("schema_version=1\n[domains.routing.model_policy.defaults.cache]\nread_share_target=0.95\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\n[domains.routing.model_policy.rules.schedule]\nexpires=200\n[domains.routing.model_policy.rules.policy.lane]\nmax_turns=40\n".as_bytes()),
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
    assert_eq!(
        layered.domains.routing.review.shapes["standard"].reviewers[0].count,
        2
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["low"],
        "quick"
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["medium"],
        "deep"
    );
    let layered = provenant_config::load_with_domains(
        &roots,
        Some("schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers=[{pool='senior',count=1}]\n[domains.routing.review.select]\nby_risk={medium='custom'}\n".as_bytes()),
        &sections,
    )?;
    assert!(layered.domains.routing.review.shapes.contains_key("custom"));
    assert!(layered.domains.routing.review.shapes.contains_key("quick"));
    assert_eq!(
        layered.domains.routing.review.select.by_risk["low"],
        "quick"
    );
    assert_eq!(
        layered.domains.routing.review.select.by_risk["medium"],
        "custom"
    );
    for invalid in [
        "schema_version=1\n[domains.routing.review.select]\ntypo=true\n",
        "schema_version=1\n[domains.routing.review.shapes.quick]\ntypo=true\n",
        "schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers=[{pool='junior',count=1}]\ntypo=true\n",
        "schema_version=1\n[domains.routing.review.shapes.custom]\nreviewers='wrong'\n",
    ] {
        assert!(
            provenant_config::load_with_domains(&roots, Some(invalid.as_bytes()), &sections)
                .is_err(),
            "{invalid}"
        );
    }
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
            serde_json::json!([{"account":"codex/opaque-account", "policy":"spend", "schedule":{"expires":101}}]),
            false,
        ),
        (
            "pace",
            serde_json::json!([{"account":"claude/opaque-account", "policy":"spend", "schedule":{"expires":101}}]),
            true,
        ),
        (
            "pace",
            serde_json::json!([{"account":"opaque-account", "policy":"spend", "schedule":{"expires":100}}]),
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
        "pools": {"trial": [{"model":"opencode/opencode-go/claude-haiku-5-5", "weight":1, "efforts":[]}]},
        "pacing": {"accounts": {"go": {"policy":"pace", "windows":["rolling", "seven_day", "monthly"], "reserve":{"rolling":0}}}}
    }))?;
    let model = "opencode/opencode-go/claude-haiku-5-5";
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
    query.allow_free = true;
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
        Some(&RouteExclusionReason::StaleAvailability)
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
        probe_adapters(query, &catalogue, config)
            .map(|specs| specs.into_iter().map(|spec| spec.name).collect::<Vec<_>>())
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
        probe_adapters(&query, &catalogue, &config),
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
    query.allow_free = true;
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
    let override_row = |expires| serde_json::json!([{"account":"zen", "policy":"spend", "schedule":{"expires":expires}}]);
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
        ("opencode/opencode/beam-free", "zen"),
        ("opencode/opencode-go/glm-5.1", "go"),
    ] {
        let query = request(model)?;
        let route = resolve(&query, &ctx)?;
        assert_eq!(route.account_key.as_deref(), Some(key));
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    let free = request("opencode/opencode/beam-free")?;
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
    let mut config = RoutingConfig::default();
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
    let mut settings = provenant_quota::Settings::default();
    settings.calibration.insert(
        "go".into(),
        Calibration {
            five_hour: Some(43),
            seven_day: Some(17),
            thirty_day: Some(59),
            expires: "1970-01-01T00:01:41Z".into(), // timebomb: allow fixed epoch fixture, not the live clock
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
                expires: "1970-01-01T00:01:41Z".into(), // timebomb: allow fixed epoch fixture, not the live clock
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
        (duplicate(&raw), 7516),
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
    query.allow_free = true;
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
