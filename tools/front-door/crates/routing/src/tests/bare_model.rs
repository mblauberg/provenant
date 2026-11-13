use super::*;

fn synthetic_catalogue() -> Result<Catalogue, Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.models = vec![
        ModelEntry {
            model: ModelId::parse("alpha/unique-model")?,
            free: false,
            evidence_ref: "evidence".into(),
            efforts: vec!["medium".into()],
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        },
        ModelEntry {
            model: ModelId::parse("alpha/shared-model")?,
            free: false,
            evidence_ref: "evidence".into(),
            efforts: vec!["medium".into()],
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        },
        ModelEntry {
            model: ModelId::parse("beta/shared-model")?,
            free: false,
            evidence_ref: "evidence".into(),
            efforts: vec!["medium".into()],
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
        },
    ];
    Ok(catalogue)
}

#[test]
fn bare_name_resolves_to_the_one_provider_that_offers_it() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = synthetic_catalogue()?;
    assert_eq!(
        catalogue.resolve_bare_model("unique-model")?.as_str(),
        "alpha/unique-model"
    );
    Ok(())
}

#[test]
fn bare_name_offered_by_several_providers_names_each_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = synthetic_catalogue()?;
    let err = catalogue.resolve_bare_model("shared-model").unwrap_err();
    match err {
        BareModelError::Ambiguous { mut candidates } => {
            candidates.sort();
            assert_eq!(
                candidates,
                vec![
                    "alpha/shared-model".to_string(),
                    "beta/shared-model".to_string()
                ]
            );
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    Ok(())
}

#[test]
fn bare_name_no_provider_offers_is_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = synthetic_catalogue()?;
    assert_eq!(
        catalogue.resolve_bare_model("no-such-model"),
        Err(BareModelError::Unknown)
    );
    // A provider-qualified name never resolves as a bare name.
    assert_eq!(
        catalogue.resolve_bare_model("alpha/unique-model"),
        Err(BareModelError::Unknown)
    );
    Ok(())
}

#[test]
fn pool_excludes_unsupported_effort_but_exact_and_user_pool_allow_it()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let target = ModelId::parse("alpha/effort-model")?;
    catalogue.models.push(ModelEntry {
        model: target.clone(),
        free: false,
        evidence_ref: "evidence".into(),
        efforts: vec!["low".into(), "high".into()],
        family: None,
        trains_on_prompts: None,
        monthly_allowance_usd: None,
        quota_pool: None,
    });
    // Pool allows only "low"
    catalogue.pools.insert(
        "test-pool".into(),
        vec![RoutePoolMember {
            model: target.clone(),
            weight: 1,
            effort: Some("low".into()),
            efforts: vec!["low".into()],
            free: false,
        }],
    );
    let config = RoutingConfig::default();
    let available = [AvailabilityRecord {
        schema_version: 1,
        adapter: "alpha".into(),
        executable: "/fixture".into(),
        version: "fixture".into(),
        fixture_ref: "test".into(),
        catalogue_revision: catalogue.revision().into(),
        account_key: None,
        observed_models: vec!["effort-model".into()],
        observed_unix_seconds: 0,
        expires_unix_seconds: 10_000,
        last_update: None,
    }];
    let access = [AccessContext {
        adapter: "alpha",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let ctx = RoutingContext {
        availability: &available,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };

    // Requesting "high" effort in the pool that only allows "low" excludes the candidate
    let mut query = request(target.as_str())?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("test-pool")?);
    query.effort = Some("high".into());
    match resolve_with_seed(&query, &ctx, "test-seed", 0) {
        Ok(route) => {
            assert_ne!(picked_model(&route)?, target);
        }
        Err(RoutingError::NoEligiblePoolMember { .. }) => {
            // Expected when the only member is excluded by effort
        }
        Err(err) => return Err(err.into()),
    }

    // Exact request for the model with "high" effort resolves ready
    let mut exact = request(target.as_str())?;
    exact.effort = Some("high".into());
    let route = resolve(&exact, &ctx)?;
    assert_eq!(picked_model(&route)?, target);
    assert_eq!(route.effort.as_deref(), Some("high"));
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));

    // User-configured custom pool allowing "high" effort resolves ready
    let mut custom = RoutingConfig::default();
    custom.pools.insert(
        "custom-pool".into(),
        vec![RoutePoolMember {
            model: target.clone(),
            weight: 1,
            effort: Some("high".into()),
            efforts: vec!["high".into()],
            free: false,
        }],
    );
    exact.choice = RouteChoice::NamedRoute(RouteName::parse("custom-pool")?);
    let custom_ctx = RoutingContext {
        config: &custom,
        ..ctx
    };
    let route = resolve(&exact, &custom_ctx)?;
    assert_eq!(picked_model(&route)?, target);
    assert_eq!(route.effort.as_deref(), Some("high"));
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    Ok(())
}
