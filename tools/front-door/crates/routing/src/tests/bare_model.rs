use super::*;

#[test]
fn bare_name_resolves_to_the_one_provider_that_offers_it() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    assert_eq!(
        catalogue.resolve_bare_model("claude-sonnet-5-5")?.as_str(),
        "claude/claude-sonnet-5-5"
    );
    assert_eq!(
        catalogue.resolve_bare_model("gpt-6.1-terra")?.as_str(),
        "codex/gpt-6.1-terra"
    );
    Ok(())
}

#[test]
fn bare_name_offered_by_several_providers_names_each_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let mut row = catalogue
        .models
        .iter()
        .find(|row| row.model.as_str() == "codex/gpt-6.1-terra")
        .cloned()
        .ok_or("missing codex row")?;
    row.model = ModelId::parse("cursor/gpt-6.1-terra")?;
    catalogue.models.push(row);
    assert_eq!(
        catalogue.resolve_bare_model("gpt-6.1-terra"),
        Err(BareModelError::Ambiguous {
            candidates: vec!["codex/gpt-6.1-terra".into(), "cursor/gpt-6.1-terra".into()],
        })
    );
    Ok(())
}

#[test]
fn bare_name_no_provider_offers_is_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    assert_eq!(
        catalogue.resolve_bare_model("no-such-model-9"),
        Err(BareModelError::Unknown)
    );
    // A provider-qualified name never resolves as a bare name.
    assert_eq!(
        catalogue.resolve_bare_model("claude/claude-sonnet-5-5"),
        Err(BareModelError::Unknown)
    );
    Ok(())
}

#[test]
fn bundled_pools_exclude_high_terra_but_exact_and_user_pool_allow_it()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let available = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let ctx = RoutingContext {
        availability: &available,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let terra = ModelId::parse("codex/gpt-6.1-terra")?;
    for writes in [false, true] {
        for pool in catalogue.pools.keys() {
            for effort in ["high", "xhigh", "max"] {
                let mut query = request(terra.as_str())?;
                query.choice = RouteChoice::NamedRoute(RouteName::parse(pool)?);
                query.intent.writes = writes;
                query.effort = Some(effort.into());
                match resolve_with_seed(&query, &ctx, "terra-removal", 0) {
                    Ok(route) => {
                        assert_ne!(picked_model(&route)?, terra, "{pool} at {effort}");
                        let selection = route.pool_selection.ok_or("missing pool receipt")?;
                        for candidate in selection
                            .candidates
                            .iter()
                            .filter(|c| c.model == terra.as_str())
                        {
                            assert_eq!(candidate.exclusion.as_deref(), Some("effort"));
                        }
                    }
                    Err(RoutingError::NoEligiblePoolMember { .. }) => {
                        assert!(
                            pool != "senior" || effort != "high",
                            "senior high must resolve"
                        );
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let mut exact = request(terra.as_str())?;
        exact.intent.writes = writes;
        exact.effort = Some("high".into());
        let route = resolve(&exact, &ctx)?;
        assert_eq!(picked_model(&route)?, terra);
        assert_eq!(route.effort.as_deref(), Some("high"));
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));

        let mut custom = RoutingConfig::default();
        custom.pools.insert(
            "senior".into(),
            vec![RoutePoolMember {
                model: terra.clone(),
                weight: 1,
                effort: Some("high".into()),
                efforts: vec!["high".into()],
                free: false,
            }],
        );
        exact.choice = RouteChoice::NamedRoute(RouteName::parse("senior")?);
        let custom_ctx = RoutingContext {
            config: &custom,
            ..ctx
        };
        let route = resolve(&exact, &custom_ctx)?;
        assert_eq!(picked_model(&route)?, terra);
        assert_eq!(route.effort.as_deref(), Some("high"));
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    Ok(())
}
