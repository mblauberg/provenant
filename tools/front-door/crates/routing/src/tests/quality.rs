use super::*;

#[test]
fn catalogue_pool_members_each_have_a_prior_and_stay_eligible()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let members: Vec<_> = catalogue
        .pools
        .get("senior")
        .ok_or("catalogue senior pool")?
        .iter()
        .filter(|member| member.model.adapter() == "codex")
        .cloned()
        .collect();
    let high = members.first().ok_or("catalogue codex senior member")?;
    let low = members
        .last()
        .ok_or("second catalogue codex senior member")?;
    if high.model == low.model {
        return Err("catalogue pool needs distinct codex members".into());
    }
    let high_model = high.model.as_str().to_owned();
    let low_model = low.model.as_str().to_owned();
    let mut config = RoutingConfig::default();
    config.pools.insert("senior".into(), members.clone());
    let mut query = request(high.model.as_str())?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("senior")?);
    query.intent.workload = Workload::Review;
    let accounts = [AvailabilityRecord {
        observed_models: members
            .iter()
            .map(|member| member.model.model().to_owned())
            .collect(),
        ..availability()
    }];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let priors = catalogue
        .pools
        .iter()
        .flat_map(|(pool, entries)| {
            entries.iter().map(move |member| RoutePrior {
                extra: Default::default(),
                model: member.model.clone(),
                pool: pool.clone(),
                role: "review".into(),
                successes: if pool == "senior" && member.model == high.model {
                    100
                } else {
                    0
                },
                failures: if pool == "senior" && member.model != high.model {
                    100
                } else {
                    0
                },
                source: "fixture:attributed".into(),
            })
        })
        .collect::<Vec<_>>();
    let evidence = SelectionEvidence {
        role: None,
        source: "fixture:route_outcomes".into(),
        priors,
        ..Default::default()
    };
    assert!(
        catalogue
            .pools
            .iter()
            .all(|(pool, entries)| entries.iter().all(|member| {
                evidence.priors.iter().any(|prior| {
                    prior.model == member.model
                        && prior.pool == pool.as_str()
                        && prior.role == "review"
                })
            }))
    );
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &accounts;
    ctx.access = &access;
    ctx.selection_evidence = Some(&evidence);
    let route = resolve_with_seed(&query, &ctx, "quality-invariant", 0)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(route.selection_context.workload, query.intent.workload);
    assert_eq!(route.selection_context.writes, query.intent.writes);
    let receipt = route.pool_selection.ok_or("receipt")?;
    assert_eq!(
        receipt.exploration.percent,
        config.outcome_priors.exploration_percent
    );
    let high_candidate = receipt
        .candidates
        .iter()
        .find(|candidate| candidate.model == high_model)
        .ok_or("high-prior candidate")?;
    let low_candidate = receipt
        .candidates
        .iter()
        .find(|candidate| candidate.model == low_model)
        .ok_or("low-prior candidate")?;
    assert!(high_candidate.exclusion.is_none());
    assert!(low_candidate.exclusion.is_none());
    assert!(
        high_candidate
            .ranking
            .outcome_prior
            .as_ref()
            .ok_or("prior")?
            .factor_basis_points
            > low_candidate
                .ranking
                .outcome_prior
                .as_ref()
                .ok_or("prior")?
                .factor_basis_points
    );

    // A single below-preference candidate must still serve every draw.
    config
        .pools
        .get_mut("senior")
        .ok_or("pool")?
        .retain(|member| member.model.as_str() == high_model);
    config.outcome_priors.enabled = false;
    config
        .capability_preferences
        .insert("review".into(), BTreeMap::new());
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &accounts;
    ctx.access = &access;
    ctx.selection_evidence = Some(&evidence);
    let route = resolve_with_seed(&query, &ctx, "only-general", 0)?;
    assert_eq!(route.model, high_model);
    let receipt = route.pool_selection.ok_or("receipt")?;
    assert_eq!(
        receipt.candidates[0]
            .ranking
            .outcome_prior
            .as_ref()
            .ok_or("prior")?
            .factor_basis_points,
        10_000
    );
    assert_eq!(
        receipt.candidates[0]
            .ranking
            .capability_preference
            .as_ref()
            .ok_or("capability")?
            .source,
        provenant_records::route_selection::WeightSource::Config
    );
    assert!(!receipt.exploration.selected);
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
                extra: Default::default(),
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
        assert_eq!(
            route
                .diagnostics
                .account_load
                .as_ref()
                .ok_or("load")?
                .reset_eta_seconds,
            Some(100)
        );
        assert!(
            route
                .diagnostics
                .account_load
                .as_ref()
                .is_some_and(|load| load.source.contains("host_active_lanes"))
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
