use super::*;

#[test]
fn reset_reference_standby_and_continue_after_observed_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let own = QuotaWindow {
        extra: Default::default(),
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut reference = quota(model, "anese", QuotaStatus::Available, 1, 80, 120)?;
    reference.snapshot.account.adapter = "claude".into();
    reference.snapshot.windows = vec![QuotaWindow {
        extra: Default::default(),
        resets_unix_seconds: Some(90),
        ..own.clone()
    }];
    for (after, factor) in [("standby", 0), ("continue", 5_000)] {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"claude/michael":{"policy":"pace", "windows":["seven_day"],
                "deadline":{"reset_of":"claude/anese", "window":"seven_day"}, "after_deadline":after}}}
        }))?;
        let route = profile_preview(
            config,
            model,
            "michael",
            vec![own.clone()],
            vec![QuotaObservation {
                model: reference.model.clone(),
                expires_unix_seconds: reference.expires_unix_seconds,
                snapshot: reference.snapshot.clone(),
            }],
        )?;
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        assert_eq!(candidate.factor_basis_points, factor);
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    Ok(())
}

#[test]
fn selected_windows_cover_pro_cursor_agy_and_free_profiles()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let catalogue = Catalogue::bundled()?;
    let claude_model = catalogue
        .models
        .iter()
        .find(|m| m.model.adapter() == "claude")
        .map(|m| m.model.as_str())
        .ok_or("catalogue Claude model")?;
    let free = catalogue
        .models
        .iter()
        .find(|m| m.model.adapter() == "opencode" && catalogue.is_free_model(&m.model))
        .map(|m| m.model.as_str())
        .ok_or("catalogue OpenCode free model")?;

    for (model, account, profile, windows, factor) in [
        (
            claude_model,
            "pro",
            serde_json::json!({"policy":"pace", "windows":["five_hour"]}),
            vec![
                QuotaWindow {
                    extra: Default::default(),
                    pool: "five_hour".into(),
                    window_seconds: Some(18_000),
                    used_basis_points: 4_000,
                    resets_unix_seconds: Some(18_100),
                },
                QuotaWindow {
                    extra: Default::default(),
                    pool: "seven_day".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 9_500,
                    resets_unix_seconds: Some(604_900),
                },
            ],
            6_000,
        ),
        (
            free,
            "zen",
            serde_json::json!({"policy":"max"}),
            vec![],
            10_000,
        ),
    ] {
        let config: RoutingConfig = if account == "zen" {
            serde_json::from_value(serde_json::json!({"pacing":{"free":{"opencode":profile}}}))?
        } else {
            let adapter = ModelId::parse(model)?.adapter().to_owned();
            let key = format!("{adapter}/{account}");
            serde_json::from_value(serde_json::json!({"pacing":{"accounts":{key:profile}}}))?
        };
        let route = profile_preview(config, model, account, windows, vec![])?;
        assert!(
            matches!(route.readiness, RouteReadiness::Ready { .. }),
            "{model}"
        );
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        assert_eq!(candidate.factor_basis_points, factor, "{model}");
        assert!(candidate.pacing_detail.is_some(), "{model}");
    }
    Ok(())
}

#[test]
fn fixed_deadlines_use_injected_utc_for_weekly_monthly_and_absolute_times()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let zone = jiff::tz::TimeZone::UTC;
    let local = |month, day| {
        zone.to_ambiguous_zoned(jiff::civil::date(1970, month, day).at(0, 0, 0, 0))
            .compatible()
            .map(|z| z.timestamp().as_second())
    };
    for (deadline, selector, duration, reset) in [
        ("sun 00:00", "seven_day", 604_800_u64, local(1, 7)?),
        ("day 25", "monthly", 2_678_400, local(2, 1)?),
        (
            "1970-01-03T00:00", // timebomb: allow fixed epoch fixture, not the live clock
            "seven_day",
            604_800,
            local(1, 7)?,
        ),
    ] {
        let windows = vec![QuotaWindow {
            extra: Default::default(),
            pool: selector.into(),
            window_seconds: Some(duration),
            used_basis_points: 5_000,
            resets_unix_seconds: Some(reset),
        }];
        let pacing = |with_deadline: bool| -> Result<RoutingConfig, serde_json::Error> {
            let mut profile = serde_json::json!({"policy":"pace", "windows":[selector]});
            if with_deadline {
                profile["deadline"] = serde_json::json!(deadline);
            }
            serde_json::from_value(serde_json::json!({
                "pacing":{"accounts":{"claude/michael":profile}}
            }))
        };
        let factor = |config: RoutingConfig| -> Result<u32, Box<dyn std::error::Error>> {
            let route = profile_preview(
                config,
                "claude/claude-haiku-5-5",
                "michael",
                windows.clone(),
                vec![],
            )?;
            Ok(route.pool_selection.ok_or("receipt")?.candidates[0].factor_basis_points)
        };
        let with_deadline = factor(pacing(true)?)?;
        let without_deadline = factor(pacing(false)?)?;
        assert!(
            with_deadline > without_deadline,
            "configured deadline before reset should increase pacing for {deadline}"
        );
    }
    Ok(())
}

#[test]
fn refreshed_peer_reset_preserves_handover_in_the_current_account_cycle()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"claude/michael":{"policy":"pace", "windows":["seven_day"],
            "deadline":{"reset_of":"claude/anese"}}}}
    }))?;
    // Michael's cycle spans [-302300, 302500]. Anese's newly observed next
    // reset is 432100, so the handover in Michael's cycle was at -172700.
    let own = QuotaWindow {
        extra: Default::default(),
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(302_500),
    };
    let mut peer = quota(model, "anese", QuotaStatus::Available, 1, 80, 120)?;
    peer.snapshot.account.adapter = "claude".into();
    peer.snapshot.windows = vec![QuotaWindow {
        extra: Default::default(),
        resets_unix_seconds: Some(432_100),
        ..own.clone()
    }];
    let route = profile_preview(config, model, "michael", vec![own], vec![peer])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, 0);
    assert!(
        candidate
            .pacing_detail
            .as_deref()
            .ok_or("detail")?
            .contains("standby")
    );
    Ok(())
}

#[test]
fn account_profile_and_overrides_govern_free_models() -> Result<(), Box<dyn std::error::Error>> {
    let model = "opencode/opencode/glm-5.3-flash-free";
    let blocked = |config: serde_json::Value| -> Result<bool, Box<dyn std::error::Error>> {
        match profile_preview(
            serde_json::from_value(config)?,
            model,
            "zen",
            vec![],
            vec![],
        ) {
            Ok(route) => Ok(!matches!(route.readiness, RouteReadiness::Ready { .. })),
            Err(error) => match error.downcast_ref::<RoutingError>() {
                Some(RoutingError::NoEligiblePoolMember { exclusions })
                    if exclusions.iter().all(|e| e.ends_with(": disabled")) =>
                {
                    Ok(true)
                }
                _ => Err(error),
            },
        }
    };
    // An account set to off cannot serve a free model, whatever the free profile says.
    assert!(blocked(serde_json::json!({"pacing":{
        "accounts":{"opencode/zen":{"policy":"off"}}, "free":{"opencode":{"policy":"max"}}}}))?);
    // Without an account profile, the free profile is the provider default.
    assert!(blocked(
        serde_json::json!({"pacing":{"free":{"opencode":{"policy":"off"}}}})
    )?);
    assert!(!blocked(serde_json::json!({"pacing":{
        "accounts":{"opencode/zen":{"policy":"max"}}, "free":{"opencode":{"policy":"off"}}}}))?);
    // An active override outranks the account profile; an expired one does not.
    let override_row = |expires: i64| serde_json::json!([{"account":"zen", "policy":"spend", "schedule":{"expires":provenant_core::WallTime::from_unix_seconds(expires).map(|at| at.to_string()).ok()}}]);
    assert!(!blocked(serde_json::json!({
        "pacing":{"accounts":{"opencode/zen":{"policy":"off"}}}, "pacing_overrides": override_row(1_000_000)
    }))?);
    assert!(blocked(serde_json::json!({
        "pacing":{"accounts":{"opencode/zen":{"policy":"off"}}}, "pacing_overrides": override_row(1)
    }))?);
    Ok(())
}

#[test]
fn newer_unknown_reference_supersedes_older_reset() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"claude/michael":{"policy":"pace", "windows":["seven_day"],
            "deadline":{"reset_of":"claude/anese"}}}}
    }))?;
    let own = QuotaWindow {
        extra: Default::default(),
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut older = quota(model, "anese", QuotaStatus::Available, 1, 70, 120)?;
    older.snapshot.account.adapter = "claude".into();
    older.snapshot.windows = vec![QuotaWindow {
        extra: Default::default(),
        resets_unix_seconds: Some(302_500),
        ..own.clone()
    }];
    let mut newer = quota(model, "anese", QuotaStatus::Unknown, 2, 90, 120)?;
    newer.snapshot.account.adapter = "claude".into();
    let baseline = profile_preview(config.clone(), model, "michael", vec![own.clone()], vec![])?;
    let baseline_factor = baseline
        .pool_selection
        .ok_or("baseline receipt")?
        .candidates[0]
        .factor_basis_points;
    let route = profile_preview(config, model, "michael", vec![own], vec![older, newer])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, baseline_factor);
    Ok(())
}

#[test]
fn monthly_day_31_clamps_to_the_declared_short_month_end() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_quota::QuotaWindow;
    let zone = jiff::tz::TimeZone::UTC;
    let local = |month, day| {
        zone.to_ambiguous_zoned(jiff::civil::date(1970, month, day).at(0, 0, 0, 0))
            .compatible()
            .map(|z| z.timestamp().as_second())
    };
    let window = QuotaWindow {
        extra: Default::default(),
        pool: "monthly".into(),
        window_seconds: Some(2_419_200),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(local(3, 1)?),
    };
    let factor = |day| -> Result<u32, Box<dyn std::error::Error>> {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"claude/michael":{"policy":"pace", "windows":["monthly"],
                "deadline":format!("day {day}")}}}
        }))?;
        let route = profile_preview(
            config,
            "claude/claude-haiku-5-5",
            "michael",
            vec![window.clone()],
            vec![],
        )?;
        Ok(route.pool_selection.ok_or("receipt")?.candidates[0].factor_basis_points)
    };
    assert_eq!(factor(31)?, factor(28)?);
    Ok(())
}

#[test]
fn daily_selector_is_neutral_until_a_daily_window_is_observable()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let model = "claude/claude-haiku-5-5";
    let config = || {
        serde_json::from_value::<RoutingConfig>(serde_json::json!({
            "pacing":{"accounts":{"claude/michael":{"policy":"pace", "windows":["daily"]}}}
        }))
    };
    let weekly = QuotaWindow {
        extra: Default::default(),
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 9_000,
        resets_unix_seconds: Some(604_900),
    };
    let route = profile_preview(config()?, model, "michael", vec![weekly.clone()], vec![])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    let unobserved_factor = candidate.factor_basis_points;
    let daily = QuotaWindow {
        extra: Default::default(),
        pool: "daily".into(),
        window_seconds: Some(86_400),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(86_500),
    };
    let route = profile_preview(config()?, model, "michael", vec![weekly, daily], vec![])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_ne!(candidate.factor_basis_points, unobserved_factor);
    Ok(())
}

#[test]
fn opencode_zen_and_go_use_separate_account_identities() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records: Vec<_> = ["go", "zen"]
        .into_iter()
        .map(|key| {
            let mut record = availability();
            record.adapter = "opencode".into();
            record.account_key = Some(key.into());
            record.plan = Some(key.into());
            record
        })
        .collect();
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    for (model, key) in [
        ("opencode/opencode/glm-5.3-flash-free", "zen"),
        ("opencode/opencode-go/glm-5.3-flash", "go"),
    ] {
        let query = request(model)?;
        let route = resolve(&query, &ctx)?;
        assert_eq!(route.account_key.as_deref(), Some(key));
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    let free = request("opencode/opencode/glm-5.3-flash-free")?;
    let route = resolve_for_account_with_seed(&free, &ctx, Some("go"), "fixture", 0)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable { .. }
    ));
    Ok(())
}

const GO_MODEL: &str = "opencode/opencode-go/deepseek-v4.1-flash";

struct GoFixture {
    catalogue: Catalogue,
    config: RoutingConfig,
    availability: [AvailabilityRecord; 1],
    access: [AccessContext<'static>; 1],
    raw: QuotaObservation,
    explicit: RouteRequest,
    pooled: RouteRequest,
}

fn go_fixture() -> Result<GoFixture, Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaDetails, QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let mut config = RoutingConfig {
        allow_unknown_partial_headroom: true,
        ..RoutingConfig::default()
    };
    config.pacing.accounts.insert(
        "opencode/go".into(),
        PacingProfile {
            policy: Some(PacingPolicy::Pace),
            windows: Some(vec![PacingWindow::Rolling, PacingWindow::Monthly]),
            ..PacingProfile::default()
        },
    );
    config.pools.insert(
        "go-trial".into(),
        vec![RoutePoolMember {
            model: ModelId::parse(GO_MODEL)?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        }],
    );
    let mut available = availability();
    available.adapter = "opencode".into();
    available.account_key = Some("go".into());
    available.plan = Some("go".into());
    let availability = [available];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut raw = quota(GO_MODEL, "go", QuotaStatus::Available, 1, 80, 120)?;
    raw.snapshot.account.adapter = "opencode".into();
    raw.snapshot.details = Some(QuotaDetails {
        extra: Default::default(),
        plan: Some("go".into()),
        source: Some("local-ledger".into()),
        coverage: Some("partial".into()),
        ..QuotaDetails::default()
    });
    raw.snapshot.windows = [
        ("five_hour", 401, 18_000),
        ("seven_day", 160, 604_800),
        ("thirty_day", 2484, 2_592_000),
    ]
    .into_iter()
    .map(|(pool, used_basis_points, seconds)| QuotaWindow {
        extra: Default::default(),
        pool: pool.into(),
        used_basis_points,
        window_seconds: Some(seconds),
        resets_unix_seconds: None,
    })
    .collect();
    let explicit = request(GO_MODEL)?;
    let mut pooled = request(GO_MODEL)?;
    pooled.choice = RouteChoice::NamedRoute(RouteName::parse("go-trial")?);
    Ok(GoFixture {
        catalogue,
        config,
        availability,
        access,
        raw,
        explicit,
        pooled,
    })
}

fn go_settings(
    key: &str,
    five_hour: Option<u64>,
    seven_day: Option<u64>,
    thirty_day: Option<u64>,
) -> Result<provenant_quota::Settings, Box<dyn std::error::Error>> {
    let mut settings = provenant_quota::Settings::default();
    settings.calibration.insert(
        key.into(),
        provenant_quota::Calibration {
            five_hour,
            seven_day,
            thirty_day,
            expires: provenant_core::WallTime::from_unix_seconds(101)?,
        },
    );
    Ok(settings)
}

fn go_duplicate(observation: &QuotaObservation) -> QuotaObservation {
    QuotaObservation {
        model: observation.model.clone(),
        snapshot: observation.snapshot.clone(),
        expires_unix_seconds: observation.expires_unix_seconds,
    }
}

fn apply_go_calibration(
    observation: &QuotaObservation,
    settings: &provenant_quota::Settings,
) -> Result<QuotaObservation, Box<dyn std::error::Error>> {
    let mut record = provenant_quota::AccountRecord::empty(observation.snapshot.account.clone());
    record.snapshot = Some(observation.snapshot.clone());
    let mut records = [record];
    settings.apply_calibration(&mut records, 100);
    Ok(QuotaObservation {
        snapshot: records[0].snapshot.clone().ok_or("calibrated snapshot")?,
        ..go_duplicate(observation)
    })
}

fn go_route(
    fixture: &GoFixture,
    config: RoutingConfig,
    query: &RouteRequest,
    observation: Option<QuotaObservation>,
) -> Result<ResolvedRoute, Box<dyn std::error::Error>> {
    let observations = observation.into_iter().collect::<Vec<_>>();
    let mut ctx = context(&fixture.catalogue, &config, now()?);
    ctx.availability = &fixture.availability;
    ctx.access = &fixture.access;
    ctx.quota = &observations;
    Ok(resolve(query, &ctx)?)
}

fn go_factor(
    fixture: &GoFixture,
    config: RoutingConfig,
    query: &RouteRequest,
    observation: &QuotaObservation,
) -> Result<u32, Box<dyn std::error::Error>> {
    let route = go_route(fixture, config, query, Some(go_duplicate(observation)))?;
    Ok(route
        .pool_selection
        .ok_or("pool receipt")?
        .candidates
        .first()
        .ok_or("pool candidate")?
        .factor_basis_points)
}

#[test]
fn go_partial_calibration_is_advisory_to_routing_and_admission()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{Admission, QuotaStatus};
    let fixture = go_fixture()?;
    let calibrated_settings = go_settings("go", Some(43), Some(17), Some(59))?;
    let calibrated = apply_go_calibration(&fixture.raw, &calibrated_settings)?;
    let mut expired_calibration = go_duplicate(&calibrated);
    expired_calibration
        .snapshot
        .details
        .as_mut()
        .ok_or("quota details")?
        .calibration_expires_unix_seconds = Some(100);

    let mut unknown_partial_at_limit = go_duplicate(&fixture.raw);
    unknown_partial_at_limit.snapshot.status = QuotaStatus::Unknown;
    unknown_partial_at_limit.snapshot.windows[0].used_basis_points = 10_000;
    unknown_partial_at_limit.snapshot.windows[0].resets_unix_seconds = Some(99);
    let mut exhausted_partial_at_limit = go_duplicate(&fixture.raw);
    exhausted_partial_at_limit.snapshot.status = QuotaStatus::Exhausted;
    exhausted_partial_at_limit.snapshot.windows[0].used_basis_points = 10_000;
    exhausted_partial_at_limit.snapshot.windows[0].resets_unix_seconds = Some(200);

    let full_settings = go_settings("go", Some(100), Some(100), Some(100))?;
    let calibrated_at_limit = apply_go_calibration(&fixture.raw, &full_settings)?;
    let mut measured_exhaustion = go_duplicate(&fixture.raw);
    measured_exhaustion
        .snapshot
        .details
        .as_mut()
        .ok_or("quota details")?
        .coverage = Some("complete".into());
    measured_exhaustion.snapshot.windows[0].used_basis_points = 10_000;
    measured_exhaustion.snapshot.windows[0].resets_unix_seconds = Some(200);

    let policy = [provenant_quota::QuotaPolicy {
        adapter: "opencode",
        timer_seconds: 300,
        panel_seconds: 120,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    }];
    for (observation, expected_ready) in [
        (go_duplicate(&fixture.raw), true),
        (unknown_partial_at_limit, true),
        (exhausted_partial_at_limit, true),
        (go_duplicate(&calibrated), true),
        (calibrated_at_limit, true),
        (expired_calibration, true),
        (measured_exhaustion, false),
    ] {
        for allow_unknown in [false, true] {
            let mut config = fixture.config.clone();
            config.allow_unknown_partial_headroom = allow_unknown;
            let selected = go_route(
                &fixture,
                config.clone(),
                &fixture.explicit,
                Some(go_duplicate(&observation)),
            )?;
            assert_eq!(
                matches!(selected.readiness, RouteReadiness::Ready { .. }),
                expected_ready
            );
            if !expected_ready {
                assert!(matches!(
                    selected.readiness,
                    RouteReadiness::Unavailable {
                        reason: provenant_records::RouteExclusionReason::Exhausted,
                        ..
                    }
                ));
            }
            let pooled = go_route(
                &fixture,
                config.clone(),
                &fixture.pooled,
                Some(go_duplicate(&observation)),
            );
            assert_eq!(pooled.is_ok(), expected_ready);

            let mut admitted_route = selected;
            admitted_route.readiness = RouteReadiness::Ready {
                availability_warning: None,
                extra: Default::default(),
                availability_ref: "fixture".into(),
                expires_unix_seconds: 120,
            };
            let mut record =
                provenant_quota::AccountRecord::empty(observation.snapshot.account.clone());
            record.revision = observation.snapshot.revision;
            record.snapshot = Some(observation.snapshot.clone());
            let records = [record];
            let admission = Admission {
                policy: &provenant_records::policy::Policy::default(),
                policies: &policy,
                now_unix_seconds: 100,
                records: &records,
                pools: &[],
            }
            .check(&admitted_route);
            assert_eq!(admission.is_ok(), expected_ready);
        }
    }
    Ok(())
}

#[test]
fn go_calibration_fields_show_coverage_without_refusal() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = go_fixture()?;
    for (key, calibration) in [
        ("go", (None, None, Some(59))),
        ("different-go", (Some(43), Some(17), Some(59))),
    ] {
        let settings = go_settings(key, calibration.0, calibration.1, calibration.2)?;
        let observation = apply_go_calibration(&fixture.raw, &settings)?;
        let details = observation
            .snapshot
            .details
            .as_ref()
            .ok_or("quota details")?;
        assert_eq!(details.source.as_deref(), Some("local-ledger"));
        assert_eq!(details.coverage.as_deref(), Some("partial"));
        assert!(details.calibrated_windows.iter().all(|pool| {
            fixture
                .raw
                .snapshot
                .windows
                .iter()
                .any(|window| &window.pool == pool)
        }));
        assert!(details.calibrated_windows.len() < observation.snapshot.windows.len());
        let route = go_route(
            &fixture,
            fixture.config.clone(),
            &fixture.pooled,
            Some(observation),
        )?;
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    }
    Ok(())
}

#[test]
fn go_partial_ledger_stays_selectable_when_coverage_is_incomplete_or_stale()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = go_fixture()?;
    let settings = go_settings("go", Some(43), Some(17), Some(59))?;
    let calibrated = apply_go_calibration(&fixture.raw, &settings)?;
    let mut expired = go_duplicate(&calibrated);
    expired.expires_unix_seconds = 100;
    let mut incomplete = go_duplicate(&calibrated);
    incomplete
        .snapshot
        .details
        .as_mut()
        .ok_or("quota details")?
        .calibrated_windows
        .retain(|pool| pool != "seven_day");
    let mut missing_window = go_duplicate(&calibrated);
    missing_window
        .snapshot
        .windows
        .retain(|window| window.pool != "seven_day");
    let mut stale = go_duplicate(&fixture.raw);
    stale.expires_unix_seconds = 100;
    for observation in [
        None,
        Some(go_duplicate(&fixture.raw)),
        Some(expired),
        Some(incomplete),
        Some(missing_window),
        Some(stale),
    ] {
        let pooled = go_route(
            &fixture,
            fixture.config.clone(),
            &fixture.pooled,
            observation.clone(),
        )?;
        assert!(matches!(pooled.readiness, RouteReadiness::Ready { .. }));
        assert!(pooled.pool_selection.is_some());
        let explicit = go_route(
            &fixture,
            fixture.config.clone(),
            &fixture.explicit,
            observation,
        )?;
        assert!(matches!(explicit.readiness, RouteReadiness::Ready { .. }));
    }
    Ok(())
}

#[test]
fn go_pool_pacing_tracks_the_most_constraining_calibrated_window()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = go_fixture()?;
    let settings = go_settings("go", Some(43), Some(17), Some(59))?;
    let calibrated = apply_go_calibration(&fixture.raw, &settings)?;
    let mut higher_ledger = go_duplicate(&fixture.raw);
    higher_ledger.snapshot.windows[2].used_basis_points = 7_000;
    let higher_calibration = apply_go_calibration(&higher_ledger, &settings)?;
    let mut passed_reset = go_duplicate(&calibrated);
    for window in &mut passed_reset.snapshot.windows {
        window.resets_unix_seconds = Some(99);
    }
    let raw_factor = go_factor(
        &fixture,
        fixture.config.clone(),
        &fixture.pooled,
        &fixture.raw,
    )?;
    let calibrated_factor = go_factor(
        &fixture,
        fixture.config.clone(),
        &fixture.pooled,
        &calibrated,
    )?;
    let higher_factor = go_factor(
        &fixture,
        fixture.config.clone(),
        &fixture.pooled,
        &higher_calibration,
    )?;
    let passed_reset_factor = go_factor(
        &fixture,
        fixture.config.clone(),
        &fixture.pooled,
        &passed_reset,
    )?;
    assert!(raw_factor > calibrated_factor);
    assert!(higher_factor < calibrated_factor);
    assert_eq!(passed_reset_factor, calibrated_factor);
    Ok(())
}

#[test]
fn opencode_zen_pool_does_not_inherit_go_pacing_quota() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let free = "opencode/opencode/deepseek-v4.1-flash-free";
    let mut config = RoutingConfig::default();
    config
        .pacing
        .providers
        .insert("opencode".into(), PacingPolicy::Pace);
    config.pacing.accounts.insert(
        "opencode/go".into(),
        PacingProfile {
            policy: Some(PacingPolicy::Off),
            windows: Some(vec![PacingWindow::Rolling]),
            ..PacingProfile::default()
        },
    );
    config.pools.insert(
        "zen-trial".into(),
        vec![RoutePoolMember {
            model: ModelId::parse(free)?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: true,
        }],
    );
    let records: Vec<_> = ["go", "zen"]
        .into_iter()
        .map(|key| {
            let mut record = availability();
            record.adapter = "opencode".into();
            record.account_key = Some(key.into());
            record.plan = Some(key.into());
            record
        })
        .collect();
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut observation = quota(
        "opencode/opencode-go/deepseek-v4.1-flash",
        "go",
        provenant_quota::QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?;
    observation.snapshot.account.adapter = "opencode".into();
    observation
        .snapshot
        .windows
        .push(provenant_quota::QuotaWindow {
            extra: Default::default(),
            pool: "five_hour".into(),
            window_seconds: Some(18_000),
            used_basis_points: 10_000,
            resets_unix_seconds: Some(100 + 18_000),
        });
    let quota = [observation];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.quota = &quota;
    let mut query = request(free)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("zen-trial")?);
    let route = resolve(&query, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(route.account_key.as_deref(), Some("zen"));
    let receipt = route.pool_selection.as_ref().ok_or("receipt")?;
    assert_eq!(receipt.candidates[0].factor_basis_points, 10_000);
    let previews = pacing_preview(&ModelId::parse(free)?, &ctx);
    assert_eq!(previews.len(), 1);
    assert_eq!(previews[0].account.as_deref(), Some("zen"));
    assert_eq!(previews[0].policy, PacingPolicy::Pace);
    Ok(())
}

#[test]
fn account_level_exhausted_record_stands_by_when_the_model_windows_are_unprobed()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let mut spent = quota(
        "claude/claude-sonnet-5-5",
        "michael",
        QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?;
    spent.snapshot.account.adapter = "claude".into();
    spent.snapshot.windows = vec![QuotaWindow {
        extra: Default::default(),
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 10_000,
        resets_unix_seconds: Some(604_900),
    }];
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"claude/michael":{"policy":"pace", "windows":["seven_day"]}}}
    }))?;
    let route = profile_preview(
        config,
        "claude/claude-haiku-5-5",
        "michael",
        vec![],
        vec![spent],
    )?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, 0);
    Ok(())
}
