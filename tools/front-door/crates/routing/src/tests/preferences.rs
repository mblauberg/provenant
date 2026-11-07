use super::*;

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
        toml_edit::de::from_str(include_str!("../../assets/review-defaults.toml")).unwrap();
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
        let mut q = quota(model, "opaque-account", QuotaStatus::Exhausted, 1, 50, 120)?;
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
    let readings = [first, spent("codex/gpt-6.1-sol", 300)?];
    let ctx = RoutingContext {
        quota: &readings,
        ..ctx
    };
    let rejected = [ModelId::parse("codex/gpt-6.1-sol")?];
    let Err(RoutingError::NoEligiblePoolMember {
        exclusions: overlap,
    }) = resolve_for_account_with_seed_excluding(&alias, &ctx, None, "overlap", 0, &rejected, &[])
    else {
        panic!("all exhausted members must refuse")
    };
    assert!(overlap.iter().any(|e| e.contains("codex/gpt-6.1-sol")
        && e.contains("admission rejected")
        && e.contains("300")));
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

#[test]
fn alias_ranks_stale_go_allowance_last_without_refusing_it()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in 0..20 {
        let stale = go_alias_quota_case(false, false, seed)?;
        assert_eq!(stale.model, "gpt-6-luna");
        assert!(
            stale.pool_selection.as_ref().unwrap().candidates[0]
                .exclusion
                .is_none()
        );
        let fresh = go_alias_quota_case(false, true, seed)?;
        assert_eq!(fresh.model, "gpt-6-luna");
        assert!(
            fresh.pool_selection.as_ref().unwrap().candidates[0]
                .exclusion
                .as_deref()
                .is_some_and(|e| e.contains("exhausted"))
        );
    }
    Ok(())
}

#[test]
fn alias_with_only_stale_spent_members_and_no_reset_still_launches()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in 0..20 {
        let selected = go_alias_quota_case(true, false, seed)?;
        assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));
        assert!(
            selected
                .pool_selection
                .as_ref()
                .unwrap()
                .candidates
                .iter()
                .all(|c| c.exclusion.is_none())
        );
    }
    Ok(())
}
