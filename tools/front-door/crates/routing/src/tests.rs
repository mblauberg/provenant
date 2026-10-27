use super::*;
use provenant_config::{DomainConfig, DomainSections};
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{RouteEligibility, RouteExclusionReason, RouteReadiness};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn now() -> Result<provenant_core::WallTime, Box<dyn std::error::Error>> {
    Ok(provenant_core::WallTime::from_micros(100_000_000)?)
}
fn context<'a>(
    catalogue: &'a Catalogue,
    config: &'a RoutingConfig,
    now: provenant_core::WallTime,
) -> RoutingContext<'a> {
    RoutingContext {
        catalogue,
        config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        access: &[],
        now,
        host: "test-host",
    }
}
fn request(model: &str) -> Result<RouteRequest, RoutingError> {
    Ok(RouteRequest {
        choice: RouteChoice::Model(ModelId::parse(model)?),
        effort: None,
        confidential: false,
        allow_free: false,
        fallback: Fallback::None,
    })
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
        config.pools.insert(pool.into(), vec![RoutePoolMember {
            model: ModelId::parse("claude/claude-opus-5-5")?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        }]);
        let mut query = request("claude/claude-opus-5-5")?;
        query.choice = RouteChoice::NamedRoute(RouteName::parse(pool)?);
        let route = resolve(&query, &context(&catalogue, &config, now()?))?;
        let advisory = config.opus_writer_advisory(&catalogue, &route)
            .ok_or("missing advisory for Opus-only pool")?;
        assert!(advisory.contains("principal route codex/gpt-6.1-sol"), "{advisory}");
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
    let wire = r#"{"choice":{"kind":"named_route","value":"review.v1-_2"},"effort":null,"confidential":false,"allow_free":false,"fallback":{"kind":"none"}}"#;
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
fn exact_unknown_is_retained_but_never_upgraded_or_activated()
-> Result<(), Box<dyn std::error::Error>> {
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
            reason: RouteExclusionReason::UnknownAvailability
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
fn named_routes_are_exact_with_explicit_effort_override_and_no_default()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let model = ModelId::parse("codex/fixture-model")?;
    catalogue.models.push(ModelEntry {
        model: model.clone(),
        efforts: vec!["low".into(), "high".into()],
        evidence_ref: "synthetic-controls".into(),
        family: None,
        trains_on_prompts: None,
    });
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
        choice: RouteChoice::NamedRoute(RouteName::parse("review")?),
        effort: None,
        confidential: false,
        allow_free: false,
        fallback: Fallback::None,
    };
    assert_eq!(resolve(&query, &context)?.effort.as_deref(), Some("low"));
    query.effort = Some("high".into());
    assert_eq!(resolve(&query, &context)?.effort.as_deref(), Some("high"));
    query.effort = Some("max".into());
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnsupportedEffort)
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
            name: "pace clamps to maximum factor",
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
            expected_codex_factor: Some(20_000),
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
            Err(RoutingError::UnknownAdapter),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/unlisted"},"effort":"high"}"#,
            Err(RoutingError::UnsupportedEffort),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":"bad effort"}"#,
            Err(RoutingError::UnsupportedEffort),
        ),
        (
            r#"{"choice":{"kind":"model","value":"codex/gpt-6.1-sol"},"effort":null,"fallback":{"kind":"models","models":["invented/id"]}}"#,
            Err(RoutingError::UnknownAdapter),
        ),
        (
            r#"{"choice":{"kind":"named_route","value":"review"},"effort":null,"fallback":{"kind":"models","models":["codex/gpt-6-luna"]}}"#,
            Err(RoutingError::UnsupportedEffort),
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
            .map_err(|error| error.to_string());
        let expected = expected
            .map(|effort| effort.map(str::to_owned))
            .map_err(|error| error.to_string());
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
        model: ModelId::parse("codex/test")?,
        efforts: vec!["high".into()],
        evidence_ref: "test".into(),
        family: None,
        trains_on_prompts: None,
    });
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let mut query = request("codex/test")?;
    query.effort = Some("high".into());
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("agy/unknown")?])?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnsupportedEffort)
    ));
    Ok(())
}
fn availability() -> AvailabilityRecord {
    AvailabilityRecord {
        schema_version: 1,
        adapter: "codex".into(),
        executable: "/fixture/codex".into(),
        version: "fixture".into(),
        fixture_ref: "test".into(),
        catalogue_revision: "route-pools-20261027-v2".into(),
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
            reason: RouteExclusionReason::DisclosureDenied
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
            reason: RouteExclusionReason::DisclosureDenied
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
    let cfg = provenant_config::load_with_domains(
        &roots,
        Some("schema_version=1\n[domains.routing.routes.review]\nmodel='codex/exact'\n".as_bytes()),
        &sections,
    )?;
    assert_eq!(
        cfg.domains
            .routing
            .routes
            .get("review")
            .map(|r| r.model.model()),
        Some("exact")
    );
    let cfg: RoutingConfig =
        toml_edit::de::from_str("[routes.review]\nmodel = 'opencode/openrouter/vendor/model'\n")?;
    assert!(cfg.validate().is_empty());
    assert_eq!(
        cfg.routes.get("review").map(|r| r.model.model()),
        Some("openrouter/vendor/model")
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
        model: ModelId::parse("codex/claude-haiku-4-5-20251001")?,
        efforts: vec![],
        family: Some("anthropic".into()),
        evidence_ref: "test".into(),
        trains_on_prompts: None,
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
        RouteReadiness::Unavailable { reason } => Some(reason),
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
    assert_eq!(
        reason(&resolve(&request("codex/not-observed")?, &context)?),
        Some(&RouteExclusionReason::UnverifiedModel)
    );
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
            reason: RouteExclusionReason::Exhausted
        }
    ));
    let missing = resolve_for_account(&query, &context, Some("not-registered"))?;
    assert!(matches!(
        missing.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability
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
            model: ModelId::parse(id)?,
            efforts: vec![],
            evidence_ref: "synthetic-controls".into(),
            family: None,
            trains_on_prompts: None,
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
