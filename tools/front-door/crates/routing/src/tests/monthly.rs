use super::*;
use provenant_quota::AllowanceLedger;

// Spec: docs/specs/routing.md#monthly-allowance-ledger
#[test]
fn monthly_admission_caps_next_launch_and_constrains_missing_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let model = ModelId::parse("cursor/composer-2.5")?;
    let q = monthly_quota(model.as_str(), "a", 900);
    let assess = |observation: Option<&QuotaObservation>| {
        monthly_allowance_assessment(
            &catalogue,
            &config,
            now().unwrap(),
            &model,
            Some("a"),
            observation,
        )
        .unwrap()
    };
    assert!(!assess(Some(&q)).constrained); // 9% plus the default 1% launch fits 10% burst.
    let mut debt = q.clone();
    debt.allowance.record_launch("first", 85)?;
    assert!(assess(Some(&debt)).constrained);
    debt.snapshot.observed_unix_seconds = 90; // A fresh but unchanged provider report.
    debt.allowance.observe(&debt.snapshot);
    assert!(assess(Some(&debt)).constrained);
    assert!(assess(None).constrained);
    for mutate in [
        |q: &mut QuotaObservation| q.expires_unix_seconds = 100,
        |q: &mut QuotaObservation| q.snapshot.observed_unix_seconds = 101,
        |q: &mut QuotaObservation| q.snapshot.windows.clear(),
        |q: &mut QuotaObservation| q.snapshot.windows[0].window_seconds = None,
        |q: &mut QuotaObservation| q.snapshot.windows[0].resets_unix_seconds = None,
        |q: &mut QuotaObservation| q.snapshot.account.account_key = "other".into(),
        |q: &mut QuotaObservation| {
            q.allowance.launches = 1;
            q.allowance.windows.clear();
        },
    ] {
        let mut invalid = q.clone();
        mutate(&mut invalid);
        assert!(assess(Some(&invalid)).constrained);
    }
    // Elapsed time, not probe frequency, releases more budget.
    let mut later = debt.clone();
    later.expires_unix_seconds = 30 * 86400;
    let day = provenant_core::WallTime::from_unix_seconds(86400)?;
    assert!(
        !monthly_allowance_assessment(&catalogue, &config, day, &model, Some("a"), Some(&later))
            .unwrap()
            .constrained
    );
    let fourth_day = provenant_core::WallTime::from_unix_seconds(4 * 86400)?;
    for (used, constrained) in [(2200, false), (2300, true)] {
        let mut monthly = monthly_quota(model.as_str(), "a", used);
        monthly.expires_unix_seconds = 30 * 86400;
        assert_eq!(
            monthly_allowance_assessment(
                &catalogue,
                &config,
                fourth_day,
                &model,
                Some("a"),
                Some(&monthly)
            )
            .unwrap()
            .constrained,
            constrained
        );
    }
    let cycle_end = provenant_core::WallTime::from_unix_seconds(30 * 86400)?;
    later.expires_unix_seconds = 60 * 86400;
    assert!(
        monthly_allowance_assessment(
            &catalogue,
            &config,
            cycle_end,
            &model,
            Some("a"),
            Some(&later)
        )
        .unwrap()
        .constrained
    );
    // Another provider's declared monthly window uses the same rule.
    let mut other = monthly_quota("claude/claude-haiku-5-5", "a", 900);
    other.allowance.record_launch("first", 85)?;
    assert!(
        monthly_allowance_assessment(
            &catalogue,
            &config,
            now()?,
            &other.model,
            Some("a"),
            Some(&other)
        )
        .unwrap()
        .constrained
    );
    // A grouped provider's other allowance must not constrain this model.
    let mut grouped = monthly_quota("agy/claude-sonnet-4-6", "a", 9000);
    grouped.snapshot.windows[0].pool = "gemini.monthly".into();
    grouped.allowance = AllowanceLedger::default();
    grouped.allowance.observe(&grouped.snapshot);
    assert!(
        monthly_allowance_assessment(
            &catalogue,
            &config,
            now()?,
            &grouped.model,
            Some("a"),
            Some(&grouped)
        )
        .is_none()
    );
    Ok(())
}

// Spec: docs/specs/routing.md#monthly-allowance-ledger
#[test]
fn monthly_pacing_is_account_eligibility_with_sourced_overrides()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let model = "cursor/composer-2.5";
    let mut config = RoutingConfig::default();
    config.pools.insert(
        "monthly-test".into(),
        vec![RoutePoolMember {
            model: ModelId::parse(model)?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        }],
    );
    let accounts = ["a", "b"].map(|key| AvailabilityRecord {
        adapter: "cursor".into(),
        account_key: Some(key.into()),
        observed_models: vec!["composer-2.5".into()],
        ..availability()
    });
    let access = [AccessContext {
        adapter: "cursor",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut observations = [
        monthly_quota(model, "a", 3000),
        monthly_quota(model, "b", 0),
    ];
    observations[0].allowance = AllowanceLedger::default();
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &accounts;
    ctx.access = &access;
    ctx.quota = &observations;
    let selected = resolve(&request(model)?, &ctx)?;
    assert_eq!(selected.account_key.as_deref(), Some("b"));
    let held = resolve_for_account(&request(model)?, &ctx, Some("a"))?;
    assert!(matches!(
        held.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::PacingReserve,
            ..
        }
    ));
    ctx.quota = &observations[..1];
    let mut pooled = request(model)?;
    pooled.choice = RouteChoice::NamedRoute(RouteName::parse("monthly-test")?);
    let held = resolve(&pooled, &ctx)?;
    assert!(matches!(held.readiness, RouteReadiness::Unavailable { .. })); // No standby escape.
    for (value, allowed, origin) in [
        (
            serde_json::json!({"monthly":{"burst_percent":40}}),
            true,
            "configuration",
        ),
        (
            serde_json::json!({"accounts":{"cursor/a":{"monthly":{"enabled":false}}}}),
            true,
            "account configuration",
        ),
        (
            serde_json::json!({"accounts":{"cursor/a":{"policy":"spend"}}}),
            true,
            "bundled default",
        ),
        (
            serde_json::json!({"accounts":{"cursor/a":{"policy":"pace"}}}),
            false,
            "bundled default",
        ),
    ] {
        config.pacing = serde_json::from_value(value)?;
        let assessment = monthly_allowance_assessment(
            &catalogue,
            &config,
            now()?,
            &ModelId::parse(model)?,
            Some("a"),
            Some(&observations[0]),
        )
        .unwrap();
        assert_eq!(!assessment.constrained, allowed);
        assert!(assessment.detail.contains(origin));
    }
    for bad in [
        serde_json::json!({"burst_percent":101}),
        serde_json::json!({"initial_launch_cost_basis_points":0}),
    ] {
        let cfg: RoutingConfig =
            serde_json::from_value(serde_json::json!({"pacing":{"monthly":bad}}))?;
        assert!(!cfg.validate().is_empty());
    }
    Ok(())
}
