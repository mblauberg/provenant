use super::*;
use std::collections::BTreeSet;

#[test]
fn outcome_priors_and_exploration_rank_eligible_members() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    let mut config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "capability_policy": {"senior": {"mode":"disabled"}},
        "pools": {"senior": [
            {"model":"codex/gpt-6-luna","weight":1},
            {"model":"codex/gpt-6.1-sol","weight":1},
            {"model":"codex/gpt-6.1-terra","weight":1}
        ]}
    }))?;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("senior")?);
    query.intent.workload = Workload::Review;
    let accounts = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let evidence = SelectionEvidence {
        role: Some("review".into()),
        source: "fixture:route_outcomes".into(),
        priors: vec![
            RoutePrior {
                model: ModelId::parse("codex/gpt-6.1-sol")?,
                pool: "senior".into(),
                role: "review".into(),
                successes: 100,
                failures: 0,
                source: "fixture:attributed".into(),
            },
            RoutePrior {
                model: ModelId::parse("codex/gpt-6.1-terra")?,
                pool: "senior".into(),
                role: "review".into(),
                successes: 0,
                failures: 100,
                source: "fixture:attributed".into(),
            },
        ],
        ..Default::default()
    };
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &accounts;
    ctx.access = &access;
    ctx.selection_evidence = Some(&evidence);
    let mut picks = BTreeMap::<String, u64>::new();
    let mut explorations = 0;
    for seed in 0..4096 {
        let route = resolve_with_seed(&query, &ctx, "quality-invariant", seed)?;
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
        *picks.entry(route.model.clone()).or_default() += 1;
        let receipt = route.pool_selection.ok_or("receipt")?;
        explorations += u64::from(
            receipt.extra["exploration"]["selected"]
                .as_bool()
                .ok_or("exploration")?,
        );
        assert!(
            receipt
                .candidates
                .iter()
                .all(|candidate| candidate.exclusion.is_none())
        );
        assert!(receipt.candidates.iter().all(|candidate| {
            candidate.extra["ranking_factor_basis_points"]
                .as_u64()
                .is_some_and(|factor| factor > 0)
        }));
    }
    assert!(picks["gpt-6.1-sol"] > picks["gpt-6-luna"]);
    assert!(
        picks["gpt-6.1-terra"] > 0,
        "Terra remains eligible through exploration"
    );
    assert!(
        (300..520).contains(&explorations),
        "configured 10% exploration share: {explorations}"
    );
    // A single below-preference candidate must still serve every draw.
    config
        .pools
        .get_mut("senior")
        .ok_or("pool")?
        .retain(|member| member.model.as_str() == "codex/gpt-6-luna");
    config.outcome_priors.enabled = false;
    config
        .capability_preferences
        .insert("review".into(), BTreeMap::new());
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &accounts;
    ctx.access = &access;
    ctx.selection_evidence = Some(&evidence);
    for seed in 0..32 {
        let route = resolve_with_seed(&query, &ctx, "only-general", seed)?;
        let receipt = route.pool_selection.ok_or("receipt")?;
        assert_eq!(
            receipt.candidates[0].extra["outcome_prior"]["factor_basis_points"],
            10_000
        );
        assert_eq!(
            receipt.candidates[0].extra["capability_preference"]["source"],
            "config"
        );
        assert_eq!(receipt.extra["exploration"]["selected"], false);
    }
    Ok(())
}

#[test]
fn account_ranking_uses_reset_adjusted_headroom_and_live_load_independent_of_order()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let accounts = ["fast", "slow"].map(|key| AvailabilityRecord {
        account_key: Some(key.into()),
        ..availability()
    });
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let readings = [("fast", 8_000, 100), ("slow", 2_000, 1000)]
        .into_iter()
        .map(|(key, used, eta)| {
            let mut observation = quota(
                "codex/test",
                key,
                provenant_quota::QuotaStatus::Available,
                1,
                80,
                120,
            )?;
            observation.snapshot.windows = vec![provenant_quota::QuotaWindow {
                pool: "primary".into(),
                window_seconds: Some(18_000),
                used_basis_points: used,
                resets_unix_seconds: Some(100 + eta),
            }];
            Ok(observation)
        })
        .collect::<Result<Vec<_>, RoutingError>>()?;
    let mut evidence = SelectionEvidence {
        live_lanes_known: true,
        source: "fixture:host_active_lanes".into(),
        ..Default::default()
    };
    for reversed in [false, true] {
        let mut ordered = accounts.clone();
        if reversed {
            ordered.reverse();
        }
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &ordered;
        ctx.access = &access;
        ctx.quota = &readings;
        ctx.selection_evidence = Some(&evidence);
        let route = resolve(&request("codex/test")?, &ctx)?;
        assert_eq!(route.account_key.as_deref(), Some("fast"));
        assert_eq!(route.extra["account_load"]["reset_eta_seconds"], 100);
        assert!(
            route.extra["account_load"]["source"]
                .as_str()
                .is_some_and(|s| s.contains("host_active_lanes"))
        );
    }
    evidence
        .live_lanes
        .insert(("codex".into(), "fast".into()), 4);
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &accounts;
    ctx.access = &access;
    ctx.quota = &readings;
    ctx.selection_evidence = Some(&evidence);
    assert_eq!(
        resolve(&request("codex/test")?, &ctx)?
            .account_key
            .as_deref(),
        Some("slow")
    );
    Ok(())
}

#[test]
fn catalogue_tiers_enforce_declared_capability_eligibility()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    assert_eq!(
        catalogue.pools.keys().collect::<BTreeSet<_>>(),
        catalogue.capability_policy.keys().collect::<BTreeSet<_>>()
    );
    let config = RoutingConfig::default();
    let mut records = Vec::new();
    for registration in REGISTRY {
        let spec =
            provenant_records::AdapterSpec::get(registration.adapter.id()).ok_or("adapter")?;
        let plans: Vec<_> = if spec.account_model_plans.is_empty() {
            vec![None]
        } else {
            spec.account_model_plans
                .iter()
                .map(|plan| Some(plan.plan))
                .collect()
        };
        for plan in plans {
            records.push(AvailabilityRecord {
                adapter: spec.name.into(),
                plan: plan.map(str::to_owned),
                account_key: Some(format!("fixture-{}", plan.unwrap_or("account"))),
                observed_models: vec![],
                ..availability()
            });
        }
    }
    let access: Vec<_> = records
        .iter()
        .map(|record| AccessContext {
            adapter: &record.adapter,
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        })
        .collect();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    for (tier, policy) in &catalogue.capability_policy {
        let mut query = request(catalogue.pools[tier][0].model.as_str())?;
        query.choice = RouteChoice::NamedRoute(RouteName::parse(tier)?);
        for seed in 0..64 {
            let route = resolve_with_seed(&query, &ctx, "capability-tiers", seed)?;
            let picked = catalogue
                .model(&picked_model(&route)?)
                .ok_or("picked model")?;
            assert!(
                policy
                    .minimum()
                    .is_none_or(|minimum| picked.capability >= minimum)
            );
            let receipt = route.pool_selection.ok_or("receipt")?;
            let decision = &receipt.extra["capability_policy"];
            assert_eq!(decision["relaxed"], false);
            assert_eq!(
                serde_json::from_value::<CapabilityPolicy>(decision["policy"].clone())?,
                *policy
            );
            assert!(
                decision["source"]
                    .as_str()
                    .is_some_and(|s| s.contains(tier))
            );
            let excluded: BTreeSet<_> = decision["excluded"]
                .as_array()
                .ok_or("excluded")?
                .iter()
                .map(|row| row["model"].as_str().ok_or("model"))
                .collect::<Result<_, _>>()?;
            for member in &catalogue.pools[tier] {
                let capability = catalogue.model(&member.model).ok_or("member")?.capability;
                let below = policy.minimum().is_some_and(|minimum| capability < minimum);
                assert_eq!(excluded.contains(member.model.as_str()), below);
                if below {
                    assert!(
                        receipt
                            .candidates
                            .iter()
                            .find(|c| c.model == member.model.as_str())
                            .ok_or("candidate")?
                            .exclusion
                            .is_some()
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn capability_overrides_change_eligibility_and_empty_floor_warns()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let mut config = RoutingConfig::default();
    let members: Vec<_> = catalogue
        .models()
        .iter()
        .filter(|m| m.model.adapter() == "codex")
        .map(|m| RoutePoolMember {
            model: m.model.clone(),
            weight: 1,
            effort: None,
            efforts: m.efforts.clone(),
            free: m.free,
        })
        .collect();
    config.pools.insert("senior".into(), members.clone());
    config.preference_sources.insert(
        "domains.routing.capability_policy.senior.mode".into(),
        "fixture:user".into(),
    );
    let records = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let mut query = request(members[0].model.as_str())?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("senior")?);
    for policy in [
        CapabilityPolicy::Minimum {
            minimum: Capability::Strong,
            fallback: CapabilityFallback::WarnRelax,
        },
        CapabilityPolicy::Minimum {
            minimum: Capability::General,
            fallback: CapabilityFallback::WarnRelax,
        },
        CapabilityPolicy::Disabled,
    ] {
        config.capability_policy.insert("senior".into(), policy);
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &records;
        ctx.access = &access;
        let route = resolve_with_seed(&query, &ctx, "capability-override", 0)?;
        let receipt = route.pool_selection.ok_or("receipt")?;
        let decision = &receipt.extra["capability_policy"];
        assert_eq!(decision["relaxed"], false);
        assert!(
            decision["source"]
                .as_str()
                .is_some_and(|s| s.contains("fixture:user"))
        );
        for member in &members {
            let below = policy.minimum().is_some_and(|min| {
                catalogue
                    .model(&member.model)
                    .is_some_and(|m| m.capability < min)
            });
            let candidate = receipt
                .candidates
                .iter()
                .find(|c| c.model == member.model.as_str())
                .ok_or("candidate")?;
            assert_eq!(candidate.exclusion.is_some(), below);
        }
    }
    let weakest = members
        .iter()
        .min_by_key(|m| catalogue.model(&m.model).map(|m| m.capability))
        .ok_or("member")?
        .clone();
    config.pools.insert("senior".into(), vec![weakest.clone()]);
    config.capability_policy.insert(
        "senior".into(),
        CapabilityPolicy::Minimum {
            minimum: Capability::Frontier,
            fallback: CapabilityFallback::WarnRelax,
        },
    );
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    let route = resolve_with_seed(&query, &ctx, "capability-fallback", 0)?;
    assert_eq!(route.model, weakest.model.model());
    let receipt = route.pool_selection.ok_or("receipt")?;
    assert_eq!(receipt.extra["capability_policy"]["relaxed"], true);
    assert!(
        receipt.extra["capability_policy"]["warning"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        receipt.extra["capability_policy"]["excluded"]
            .as_array()
            .ok_or("exclusions")?
            .iter()
            .any(|row| row["model"] == weakest.model.as_str())
    );
    Ok(())
}
