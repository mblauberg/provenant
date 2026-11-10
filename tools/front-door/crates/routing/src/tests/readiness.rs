use super::*;

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
    let security = review.path_floors(&["src/credentials/store.rs".into()]);
    assert_eq!(security.tier, None);
    let custom = review.path_floors(&["src/special/a.rs".into()]);
    assert_eq!(custom.tier.as_deref(), Some("crucial"));
    assert!(custom.lenses.contains(&"reliability".to_owned()));
    Ok(())
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
    let RouteReadiness::Unavailable { reason, extra } = &none.readiness else {
        return Err("expected an unavailable route".into());
    };
    assert_eq!(reason, &RouteExclusionReason::ProviderRefused);
    // Each passed-over account keeps its own reason for the refusal message.
    let mut skipped = provenant_records::SkippedAccount::read(extra);
    skipped.sort_by(|a, b| a.account.cmp(&b.account));
    assert_eq!(
        skipped,
        ["review", "work"].map(|account| provenant_records::SkippedAccount {
            account: account.into(),
            reason: RouteExclusionReason::ProviderRefused,
        })
    );
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
