use super::*;

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
        serde_json::json!({"pacing":{"accounts":{"claude/michael":{"policy":"pace","windows":["seven_day"],"deadline":{"reset_of":"claude/anese"}},"claude/anese":{"policy":"pace","windows":["seven_day"],"deadline":{"reset_of":"claude/michael"}}}}}),
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
fn pacing_reserve_warns_without_excluding_accounts() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    use provenant_records::diagnostic::PacingReserveSetting;
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
    for (windows, expected_setting) in [
        (
            vec![window(18_000, 9_000, 9_000)],
            Some(PacingReserveSetting::AdapterFiveHour),
        ),
        (
            vec![
                window(18_000, 1_000, 9_000),
                window(604_800, 9_500, 302_400),
            ],
            Some(PacingReserveSetting::AdapterSevenDay),
        ),
        (vec![window(18_000, 5_000, 9_000)], None),
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
            assert!(reason(&route).is_none());
            assert_eq!(
                route.extra["account_load"]["warnings"]
                    .as_array()
                    .is_some_and(|warnings| !warnings.is_empty()),
                expected_setting.is_some() && policy == "pace"
            );
            if expected_setting.is_some() && policy == "pace" {
                assert!(route.exclusions.is_empty());
                assert!(serde_json::to_string(&route)?.contains("pacing_reserve"));
                let fact: provenant_records::diagnostic::ResourceFact =
                    serde_json::from_value(route.extra["pacing_reserve_fact"].clone())?;
                assert!(matches!(
                    fact,
                    provenant_records::diagnostic::ResourceFact::PacingReserve {
                        setting,
                        origin: provenant_records::diagnostic::PacingSettingSource::ShippedDefault,
                        ..
                    } if Some(setting) == expected_setting
                ));
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
    for (configuration, expected_setting, expected_origin, expected_percent) in [
        (
            serde_json::json!({"pacing": {"codex": "pace"}, "pacing_reserve": {"codex": {"five_hour_percent": 20}}}),
            provenant_records::diagnostic::PacingReserveSetting::AdapterFiveHour,
            provenant_records::diagnostic::PacingSettingSource::Unresolved,
            20,
        ),
        (
            serde_json::json!({"pacing": {"accounts": {"codex/opaque-account": {"policy": "pace", "reserve": {"five_hour": 25}}}}}),
            provenant_records::diagnostic::PacingReserveSetting::Account,
            provenant_records::diagnostic::PacingSettingSource::Configuration,
            25,
        ),
    ] {
        let config: RoutingConfig = serde_json::from_value(configuration)?;
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.registry = &registry;
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&request("codex/test")?, &ctx)?;
        let fact: provenant_records::diagnostic::ResourceFact =
            serde_json::from_value(route.extra["pacing_reserve_fact"].clone())?;
        assert_eq!(
            fact,
            provenant_records::diagnostic::ResourceFact::PacingReserve {
                percent: expected_percent,
                setting: expected_setting,
                origin: expected_origin
            }
        );
    }
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
        Some(&RouteExclusionReason::ConfinementUnavailable)
    );
    let records = [availability()];
    context.availability = &records;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::ConfinementUnavailable)
    );
    let denied = [AccessContext {
        adapter: "codex",
        disclosure_ref: "",
        confinement_ref: "grant",
    }];
    context.access = &denied;
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
    let refresh_pending = [quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::RefreshPending,
        1,
        80,
        120,
    )?];
    let pending_context = RoutingContext {
        quota: &refresh_pending,
        ..context
    };
    assert!(reason(&resolve(&query, &pending_context)?).is_none());
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
            capability: Default::default(),
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

#[test]
fn reset_reference_uses_fresh_reset_and_reports_unknown_or_stale_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let config = || {
        serde_json::from_value::<RoutingConfig>(serde_json::json!({
            "pacing":{"accounts":{"claude/michael":{"policy":"pace", "windows":["seven_day"],
                "deadline":{"reset_of":"claude/anese"}}}}
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
