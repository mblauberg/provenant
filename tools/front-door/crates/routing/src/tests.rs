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
        choice: RouteChoice::NamedRoute("review".into()),
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
    query.choice = RouteChoice::NamedRoute("typo".into());
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnknownRoute)
    ));
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
        all_disabled: bool,
        codex_pacing: &'static str,
        quota_used_basis_points: Option<u64>,
        quota_reset_after_seconds: Option<i64>,
        alternative_quota_used_basis_points: Option<u64>,
        include_weekly_window: bool,
        expected_codex_factor: Option<u32>,
        expect_no_eligible: bool,
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
            all_disabled: false,
            codex_pacing: "spend",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            expect_no_eligible: false,
            redraw: false,
        },
        Case {
            name: "weight zero fallback",
            effort: "high",
            weights: [4, 4, 0],
            ready_adapters: &["agy"],
            expected_pick: Some("gemini-3.8-flash"),
            expect_effort_exclusion: false,
            all_disabled: false,
            codex_pacing: "max",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            expect_no_eligible: false,
            redraw: false,
        },
        Case {
            name: "caller effort exclusion",
            effort: "xhigh",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: true,
            all_disabled: false,
            codex_pacing: "spend",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            expect_no_eligible: false,
            redraw: false,
        },
        Case {
            name: "pace clamps to minimum factor",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            all_disabled: false,
            codex_pacing: "pace",
            quota_used_basis_points: Some(9_900),
            quota_reset_after_seconds: Some(18_000),
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(2_500),
            expect_no_eligible: false,
            redraw: false,
        },
        Case {
            name: "pace clamps to maximum factor",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            all_disabled: false,
            codex_pacing: "pace",
            quota_used_basis_points: Some(5_000),
            quota_reset_after_seconds: Some(4_500),
            alternative_quota_used_basis_points: Some(9_900),
            include_weekly_window: true,
            expected_codex_factor: Some(20_000),
            expect_no_eligible: false,
            redraw: false,
        },
        Case {
            name: "disabled members are not fallback candidates",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &[],
            expected_pick: None,
            expect_effort_exclusion: false,
            all_disabled: true,
            codex_pacing: "spend",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            expect_no_eligible: true,
            redraw: false,
        },
        Case {
            name: "same-seed redraw removes the rejected member",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            all_disabled: false,
            codex_pacing: "spend",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            expect_no_eligible: false,
            redraw: true,
        },
        Case {
            name: "unknown pace snapshot is neutral",
            effort: "high",
            weights: [4, 4, 2],
            ready_adapters: &["codex", "claude", "agy"],
            expected_pick: None,
            expect_effort_exclusion: false,
            all_disabled: false,
            codex_pacing: "pace",
            quota_used_basis_points: None,
            quota_reset_after_seconds: None,
            alternative_quota_used_basis_points: None,
            include_weekly_window: false,
            expected_codex_factor: Some(10_000),
            expect_no_eligible: false,
            redraw: false,
        },
    ];
    let registry = [
        provenant_providers::Registration {
            adapter: &RECORDED_CODEX,
            quota_source: None,
        },
        provenant_providers::Registration {
            adapter: &RECORDED_CLAUDE,
            quota_source: None,
        },
        provenant_providers::Registration {
            adapter: &RECORDED_AGY,
            quota_source: None,
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
        let mut availability: Vec<_> = base_availability
            .iter()
            .filter(|record| {
                case.ready_adapters.contains(&record.adapter.as_str()) || case.all_disabled
            })
            .cloned()
            .collect();
        if case.all_disabled {
            for record in &mut availability {
                record.last_preflight_error = Some(provenant_providers::PreflightFailure::Disabled);
            }
        }
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
        query.choice = RouteChoice::NamedRoute("trial".into());
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
        if case.expect_no_eligible {
            match result {
                Err(RoutingError::NoEligiblePoolMember { exclusions }) => {
                    assert!(exclusions.iter().any(|reason| reason.contains("disabled")));
                }
                Err(error) => return Err(error.into()),
                Ok(_) => panic!("disabled-only pool member was selected"),
            }
            let mut recoverable_availability = base_availability.clone();
            for record in &mut recoverable_availability {
                record.last_preflight_error = Some(if record.adapter == "claude" {
                    provenant_providers::PreflightFailure::AuthRequired
                } else {
                    provenant_providers::PreflightFailure::Disabled
                });
            }
            let mut recoverable_context = context(&catalogue, &config, now()?);
            recoverable_context.registry = &registry;
            recoverable_context.availability = &recoverable_availability;
            recoverable_context.access = &access;
            let recoverable =
                resolve_with_seed(&query, &recoverable_context, "operation-route-pools", 2)?;
            assert_eq!(
                recoverable.adapter, "claude",
                "recoverable member beats disabled"
            );
            assert_eq!(
                reason(&recoverable),
                Some(&RouteExclusionReason::AuthRequired)
            );
            continue;
        }
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
    query.choice = RouteChoice::NamedRoute("free-check".into());
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
    // OpenCode is not activated in the bundled registry, so opting in moves the
    // free member past the free gate to the activation gate, never to fallback.
    query.allow_free = true;
    match resolve(&query, &ctx).unwrap_err() {
        RoutingError::NoEligiblePoolMember { exclusions } => {
            assert!(
                !exclusions
                    .iter()
                    .any(|reason| reason == "free model disabled")
            );
            assert!(
                exclusions
                    .iter()
                    .any(|reason| reason == "opencode/opencode/deepseek-v4.1-flash-free: disabled")
            );
        }
        error => panic!("unexpected pool error: {error}"),
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
    assert!(
        serde_json::from_str::<RouteRequest>(
            r#"{"choice":{"kind":"model","value":"codex/a"},"effort":null,"pool":"principal"}"#
        )
        .is_err()
    );
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
        catalogue_revision: "route-pools-20261025-v2".into(),
        account_key: Some("opaque-account".into()),
        observed_models: vec!["test".into()],
        last_preflight_error: None,
        observed_unix_seconds: 80,
        expires_unix_seconds: 120,
    }
}
#[test]
fn cached_claims_cannot_activate_and_age_is_exact() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [availability()];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    let listed = list(
        &RouteListQuery {
            adapter: Some("codex".into()),
            pool: None,
        },
        &context,
    )?;
    assert_eq!(listed.availability.len(), 1);
    assert_eq!(
        listed.availability.first().and_then(|a| a.age_seconds),
        Some(20)
    );
    // Enabled and available, Codex reaches the next gate: disclosure.
    assert!(matches!(
        resolve(&request("codex/test")?, &context)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::DisclosureDenied
        }
    ));
    // A fresh record cannot activate an adapter the matrix leaves disabled.
    let mut cursor = availability();
    cursor.adapter = "cursor".into();
    let records = [cursor];
    context.availability = &records;
    assert!(matches!(
        resolve(&request("cursor/test")?, &context)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::Disabled
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
        adapter: &RecordedFixture(&Claude),
        quota_source: None,
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

/// A test-only activation gate, with the provider's real mechanics otherwise
/// unchanged. No fixture activation is exported from the library or registry.
struct RecordedFixture(&'static dyn provenant_providers::Adapter);
static RECORDED_CODEX: RecordedFixture = RecordedFixture(&provenant_providers::codex::Codex);
static RECORDED_CLAUDE: RecordedFixture = RecordedFixture(&provenant_providers::claude::Claude);
static RECORDED_AGY: RecordedFixture = RecordedFixture(&provenant_providers::agy::Agy);
impl provenant_providers::Adapter for RecordedFixture {
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn execution_enabled(&self) -> bool {
        true
    }
    fn account(
        &self,
        s: &provenant_providers::StreamState,
    ) -> Result<provenant_providers::AccountDetection, provenant_providers::ProviderError> {
        self.0.account(s)
    }
    fn preflight(
        &self,
        i: &provenant_providers::PreflightInput,
    ) -> Result<provenant_providers::PreflightPlan, provenant_providers::ProviderError> {
        self.0.preflight(i)
    }
    fn decode_preflight(
        &self,
        p: &provenant_providers::PreflightPlan,
        b: &[u8],
    ) -> Result<AvailabilityRecord, provenant_providers::ProviderError> {
        self.0.decode_preflight(p, b)
    }
    fn prepare(
        &self,
        i: provenant_providers::PrepareInput<'_>,
    ) -> Result<provenant_providers::LaunchSpec, provenant_providers::ProviderError> {
        self.0.prepare(i)
    }
    fn stream(
        &self,
        c: Option<&provenant_providers::Continuation>,
    ) -> Result<provenant_providers::StreamState, provenant_providers::ProviderError> {
        self.0.stream(c)
    }
    fn observe(
        &self,
        s: &mut provenant_providers::StreamState,
        b: &[u8],
    ) -> Result<Vec<provenant_providers::ProviderEvent>, provenant_providers::ProviderError> {
        self.0.observe(s, b)
    }
    fn finish(
        &self,
        s: provenant_providers::StreamState,
        e: provenant_providers::ExitObservation,
    ) -> Result<provenant_providers::ProviderOutcome, provenant_providers::ProviderError> {
        self.0.finish(s, e)
    }
    fn continuation(
        &self,
        o: &provenant_providers::ProviderOutcome,
        r: &ResolvedRoute,
        d: &str,
    ) -> Result<provenant_providers::Continuation, provenant_providers::ProviderError> {
        self.0.continuation(o, r, d)
    }
    fn validate_provenance(
        &self,
        p: &provenant_providers::RouteProvenance<'_>,
    ) -> Result<provenant_providers::VerifiedLineage, provenant_providers::ProviderError> {
        self.0.validate_provenance(p)
    }
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
fn activation_availability_access_and_scoped_quota_are_independent_gates()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaStatus;
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let mut context = context(&catalogue, &config, now()?);
    let registry = [provenant_providers::Registration {
        adapter: &RecordedFixture(&provenant_providers::codex::Codex),
        quota_source: None,
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
    let mut error_record = availability();
    error_record.last_preflight_error = Some(provenant_providers::PreflightFailure::AuthRequired);
    let error_records = [error_record];
    context.availability = &error_records;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::AuthRequired)
    );
    context.availability = &records;
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
    let resolved = resolve(&query, &context)?;
    assert_eq!(resolved.account_key.as_deref(), Some("opaque-account"));
    assert!(matches!(
        resolved.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::Exhausted
        }
    ));
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
    query.choice = RouteChoice::NamedRoute("identity-test".into());
    query.fallback = Fallback::None;
    let selected = resolve_for_account(&query, &context, Some("second-account"))?;
    assert_eq!(selected.adapter, "codex");
    assert_eq!(selected.account_key.as_deref(), Some("second-account"));
    let selected = resolve_for_account(&query, &context, Some("other-provider"))?;
    assert_eq!(selected.adapter, "claude");
    assert_eq!(selected.account_key.as_deref(), Some("other-provider"));
    Ok(())
}
