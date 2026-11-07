use super::*;

#[test]
fn reset_reference_standby_and_continue_after_observed_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let own = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut reference = quota(model, "anese", QuotaStatus::Available, 1, 80, 120)?;
    reference.snapshot.account.adapter = "claude".into();
    reference.snapshot.windows = vec![QuotaWindow {
        resets_unix_seconds: Some(90),
        ..own.clone()
    }];
    for (after, factor) in [("standby", 0), ("continue", 5_000)] {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["seven_day"],
                "deadline":{"reset_of":"anese", "window":"seven_day"}, "after_deadline":after}}}
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
    for (model, account, profile, windows, factor, detail) in [
        (
            "claude/claude-haiku-5-5",
            "pro",
            serde_json::json!({"policy":"pace", "windows":["five_hour"]}),
            vec![
                QuotaWindow {
                    pool: "five_hour".into(),
                    window_seconds: Some(18_000),
                    used_basis_points: 4_000,
                    resets_unix_seconds: Some(18_100),
                },
                QuotaWindow {
                    pool: "seven_day".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 9_500,
                    resets_unix_seconds: Some(604_900),
                },
            ],
            6_000,
            "five_hour",
        ),
        (
            "cursor/grok-4.7",
            "cursor",
            serde_json::json!({"policy":"pace", "windows":["monthly"]}),
            vec![QuotaWindow {
                pool: "billing_cycle".into(),
                window_seconds: Some(2_592_000),
                used_basis_points: 4_000,
                resets_unix_seconds: Some(2_592_100),
            }],
            6_000,
            "billing_cycle",
        ),
        (
            "agy/claude-sonnet-4-6",
            "agy-host",
            serde_json::json!({"policy":"pace", "windows":["seven_day"]}),
            vec![
                QuotaWindow {
                    pool: "gemini.weekly".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 3_000,
                    resets_unix_seconds: Some(604_900),
                },
                QuotaWindow {
                    pool: "claude.weekly".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 4_000,
                    resets_unix_seconds: Some(604_900),
                },
            ],
            6_000,
            "claude.weekly",
        ),
        (
            "opencode/opencode/glm-5.3-flash-free",
            "zen",
            serde_json::json!({"policy":"max"}),
            vec![],
            10_000,
            "source=free",
        ),
    ] {
        let config: RoutingConfig = if account == "zen" {
            serde_json::from_value(serde_json::json!({"pacing":{"free":{"opencode":profile}}}))?
        } else {
            serde_json::from_value(serde_json::json!({"pacing":{"accounts":{account:profile}}}))?
        };
        let route = profile_preview(config, model, account, windows, vec![])?;
        assert!(
            matches!(route.readiness, RouteReadiness::Ready { .. }),
            "{model}"
        );
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        assert_eq!(candidate.factor_basis_points, factor, "{model}");
        assert!(
            candidate
                .pacing_detail
                .as_deref()
                .ok_or("detail")?
                .contains(detail),
            "{model}"
        );
    }
    Ok(())
}

#[test]
fn fixed_deadlines_use_host_local_weekly_monthly_and_absolute_times()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let zone = jiff::tz::TimeZone::system();
    let local = |month, day| {
        zone.to_ambiguous_zoned(jiff::civil::date(1970, month, day).at(0, 0, 0, 0))
            .compatible()
            .map(|z| z.timestamp().as_second())
    };
    for (deadline, selector, duration, reset, target) in [
        (
            "sun 00:00",
            "seven_day",
            604_800_u64,
            local(1, 7)?,
            local(1, 4)?,
        ),
        ("day 25", "monthly", 2_678_400, local(2, 1)?, local(1, 25)?),
        (
            "1970-01-03T00:00", // timebomb: allow fixed epoch fixture, not the live clock
            "seven_day",
            604_800,
            local(1, 7)?,
            local(1, 3)?,
        ),
    ] {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"michael":{"policy":"pace", "windows":[selector],
                "deadline":deadline}}}
        }))?;
        let route = profile_preview(
            config,
            "claude/claude-haiku-5-5",
            "michael",
            vec![QuotaWindow {
                pool: selector.into(),
                window_seconds: Some(duration),
                used_basis_points: 5_000,
                resets_unix_seconds: Some(reset),
            }],
            vec![],
        )?;
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        let expected = (5_000 * duration / u64::try_from(target - 100)?).clamp(2_500, 20_000);
        assert_eq!(
            u64::from(candidate.factor_basis_points),
            expected,
            "{deadline}"
        );
        assert!(
            candidate
                .pacing_detail
                .as_deref()
                .ok_or("detail")?
                .contains(&format!("deadline={target}")),
            "{deadline}"
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
        "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["seven_day"],
            "deadline":{"reset_of":"anese"}}}}
    }))?;
    // Michael's cycle spans [-302300, 302500]. Anese's newly observed next
    // reset is 432100, so the handover in Michael's cycle was at -172700.
    let own = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(302_500),
    };
    let mut peer = quota(model, "anese", QuotaStatus::Available, 1, 80, 120)?;
    peer.snapshot.account.adapter = "claude".into();
    peer.snapshot.windows = vec![QuotaWindow {
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
        "accounts":{"zen":{"policy":"off"}}, "free":{"opencode":{"policy":"max"}}}}))?);
    // Without an account profile, the free profile is the provider default.
    assert!(blocked(
        serde_json::json!({"pacing":{"free":{"opencode":{"policy":"off"}}}})
    )?);
    assert!(!blocked(serde_json::json!({"pacing":{
        "accounts":{"zen":{"policy":"max"}}, "free":{"opencode":{"policy":"off"}}}}))?);
    // An active override outranks the account profile; an expired one does not.
    let override_row = |expires: i64| serde_json::json!([{"account":"zen", "policy":"spend", "schedule":{"expires":provenant_core::WallTime::from_unix_seconds(expires).map(|at| at.to_string()).ok()}}]);
    assert!(!blocked(serde_json::json!({
        "pacing":{"accounts":{"zen":{"policy":"off"}}}, "pacing_overrides": override_row(1_000_000)
    }))?);
    assert!(blocked(serde_json::json!({
        "pacing":{"accounts":{"zen":{"policy":"off"}}}, "pacing_overrides": override_row(1)
    }))?);
    Ok(())
}

#[test]
fn newer_unknown_reference_supersedes_older_reset() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let model = "claude/claude-haiku-5-5";
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["seven_day"],
            "deadline":{"reset_of":"anese"}}}}
    }))?;
    let own = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(604_900),
    };
    let mut older = quota(model, "anese", QuotaStatus::Available, 1, 70, 120)?;
    older.snapshot.account.adapter = "claude".into();
    older.snapshot.windows = vec![QuotaWindow {
        resets_unix_seconds: Some(302_500),
        ..own.clone()
    }];
    let mut newer = quota(model, "anese", QuotaStatus::Unknown, 2, 90, 120)?;
    newer.snapshot.account.adapter = "claude".into();
    let route = profile_preview(config, model, "michael", vec![own], vec![older, newer])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    // The old reset (302_500) would give 10_000; the window's own reset gives 5_000.
    assert_eq!(candidate.factor_basis_points, 5_000);
    let detail = candidate.pacing_detail.as_deref().ok_or("detail")?;
    assert!(
        detail.contains("latest snapshot is Unknown") && detail.contains("falling back"),
        "{detail}"
    );
    Ok(())
}

#[test]
fn monthly_day_31_clamps_to_a_shorter_months_last_day() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let zone = jiff::tz::TimeZone::system();
    let local = |month, day| {
        zone.to_ambiguous_zoned(jiff::civil::date(1970, month, day).at(0, 0, 0, 0))
            .compatible()
            .map(|z| z.timestamp().as_second())
    };
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["monthly"],
            "deadline":"day 31"}}}
    }))?;
    let route = profile_preview(
        config,
        "claude/claude-haiku-5-5",
        "michael",
        vec![QuotaWindow {
            pool: "monthly".into(),
            window_seconds: Some(2_419_200),
            used_basis_points: 5_000,
            resets_unix_seconds: Some(local(3, 1)?),
        }],
        vec![],
    )?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    let detail = candidate.pacing_detail.as_deref().ok_or("detail")?;
    assert!(
        detail.contains(&format!("deadline={}", local(2, 28)?))
            && detail.contains("day 31 clamped to the month's last day (28)"),
        "{detail}"
    );
    Ok(())
}

#[test]
fn daily_selector_is_neutral_until_a_daily_window_is_observable()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let model = "claude/claude-haiku-5-5";
    let config = || {
        serde_json::from_value::<RoutingConfig>(serde_json::json!({
            "pacing":{"accounts":{"michael":{"policy":"pace", "windows":["daily"]}}}
        }))
    };
    let weekly = QuotaWindow {
        pool: "seven_day".into(),
        window_seconds: Some(604_800),
        used_basis_points: 9_000,
        resets_unix_seconds: Some(604_900),
    };
    let route = profile_preview(config()?, model, "michael", vec![weekly.clone()], vec![])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, 10_000);
    assert!(
        candidate
            .pacing_detail
            .as_deref()
            .ok_or("detail")?
            .contains("unobserved windows neutral")
    );
    let daily = QuotaWindow {
        pool: "daily".into(),
        window_seconds: Some(86_400),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(86_500),
    };
    let route = profile_preview(config()?, model, "michael", vec![weekly, daily], vec![])?;
    let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
    assert_eq!(candidate.factor_basis_points, 5_000);
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

#[test]
fn go_pool_calibration_is_advisory_and_paces_the_maximum() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_quota::{AccountRecord, Calibration, QuotaDetails, QuotaStatus, QuotaWindow};
    let catalogue = Catalogue::bundled()?;
    let model = "opencode/opencode-go/deepseek-v4.1-flash";
    let mut config = RoutingConfig {
        allow_unknown_partial_headroom: true,
        ..RoutingConfig::default()
    };
    config.pacing.accounts.insert(
        "go".into(),
        PacingProfile {
            policy: Some(PacingPolicy::Pace),
            windows: Some(vec![PacingWindow::Rolling, PacingWindow::Monthly]),
            ..PacingProfile::default()
        },
    );
    config.pools.insert(
        "go-trial".into(),
        vec![RoutePoolMember {
            model: ModelId::parse(model)?,
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        }],
    );
    let mut available = availability();
    available.adapter = "opencode".into();
    available.account_key = Some("go".into());
    let availability = [available];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut query = request(model)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("go-trial")?);
    let mut raw = quota(model, "go", QuotaStatus::Available, 1, 80, 120)?;
    raw.snapshot.account.adapter = "opencode".into();
    let duplicate = |observation: &QuotaObservation| QuotaObservation {
        model: observation.model.clone(),
        snapshot: observation.snapshot.clone(),
        expires_unix_seconds: observation.expires_unix_seconds,
    };
    raw.snapshot.details = Some(QuotaDetails {
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
        pool: pool.into(),
        used_basis_points,
        window_seconds: Some(seconds),
        resets_unix_seconds: None,
    })
    .collect();
    // Selection and admission treat the same uncalibrated lower bound alike.
    let mut default_config = config.clone();
    default_config.allow_unknown_partial_headroom = false;
    let observations = [duplicate(&raw)];
    let mut ctx = context(&catalogue, &default_config, now()?);
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.quota = &observations;
    let explicit = request(model)?;
    let refused = resolve(&explicit, &ctx)?;
    assert!(matches!(
        refused.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownPartialHeadroom,
            ..
        }
    ));
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.quota = &observations;
    assert!(matches!(
        resolve(&explicit, &ctx)?.readiness,
        RouteReadiness::Ready { .. }
    ));

    let mut settings = provenant_quota::Settings::default();
    settings.calibration.insert(
        "go".into(),
        Calibration {
            five_hour: Some(43),
            seven_day: Some(17),
            thirty_day: Some(59),
            expires: provenant_core::WallTime::from_unix_seconds(101)?,
        },
    );
    let apply = |observation: &QuotaObservation| {
        let mut record = AccountRecord::empty(observation.snapshot.account.clone());
        record.snapshot = Some(observation.snapshot.clone());
        let mut records = [record];
        settings.apply_calibration(&mut records, 100);
        QuotaObservation {
            snapshot: records[0].snapshot.clone().unwrap(),
            ..duplicate(observation)
        }
    };
    let calibrated = apply(&raw);
    // Selection and launch admission must interpret the same fresh scoped
    // observation and opt-in identically, including calibration expiry.
    let mut expired_for_table = duplicate(&calibrated);
    expired_for_table
        .snapshot
        .details
        .as_mut()
        .unwrap()
        .calibration_expires_unix_seconds = Some(100);
    let mut exhausted_for_table = duplicate(&raw);
    exhausted_for_table.snapshot.windows[0].used_basis_points = 10_000;
    let policy = [provenant_quota::QuotaPolicy {
        adapter: "opencode",
        timer_seconds: 300,
        panel_seconds: 120,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    }];
    for (name, observation, opt_in, expected) in [
        ("partial default", duplicate(&raw), false, false),
        ("partial opted in", duplicate(&raw), true, true),
        ("exhausted opted in", exhausted_for_table, true, false),
        ("calibrated", duplicate(&calibrated), false, true),
        ("calibration expired", expired_for_table, false, false),
    ] {
        let mut row_config = config.clone();
        row_config.allow_unknown_partial_headroom = opt_in;
        let observations = [observation];
        let mut ctx = context(&catalogue, &row_config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let selected = resolve(&explicit, &ctx)?;
        let ready = matches!(selected.readiness, RouteReadiness::Ready { .. });
        let mut admitted_route = selected.clone();
        // Check the independent admission policy with a fresh readiness grant,
        // so an earlier selection refusal cannot mask a headroom disagreement.
        admitted_route.readiness = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 120,
        };
        let mut record = AccountRecord::empty(observations[0].snapshot.account.clone());
        record.revision = observations[0].snapshot.revision;
        record.snapshot = Some(observations[0].snapshot.clone());
        let records = [record];
        let admission = provenant_quota::Admission {
            policies: &policy,
            now_unix_seconds: 100,
            records: &records,
            pools: &[],
        }
        .check_with_partial_headroom(&admitted_route, opt_in);
        assert_eq!(
            (ready, admission.is_ok()),
            (expected, expected),
            "{name}: {admission:?}"
        );
    }
    for (key, five_hour, seven_day, expected) in [
        ("go", None, None, "five_hour, seven_day"),
        (
            "different-go",
            Some(43),
            Some(17),
            "five_hour, seven_day, thirty_day",
        ),
    ] {
        let mut partial_settings = provenant_quota::Settings::default();
        partial_settings.calibration.insert(
            key.into(),
            Calibration {
                five_hour,
                seven_day,
                thirty_day: Some(59),
                expires: provenant_core::WallTime::from_unix_seconds(101)?,
            },
        );
        let mut record = AccountRecord::empty(raw.snapshot.account.clone());
        record.snapshot = Some(raw.snapshot.clone());
        let mut records = [record];
        partial_settings.apply_calibration(&mut records, 100);
        let observations = [QuotaObservation {
            snapshot: records[0].snapshot.clone().unwrap(),
            ..duplicate(&raw)
        }];
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&query, &ctx)?;
        let detail = route.pool_selection.unwrap().candidates[0]
            .pacing_detail
            .clone()
            .unwrap();
        assert!(
            detail.contains(&format!("Go calibration missing or stale: {expected}")),
            "{key}: {detail}"
        );
        assert!(
            pacing_preview(&ModelId::parse(model)?, &ctx)[0]
                .detail
                .contains(expected)
        );
    }
    let mut expired = duplicate(&calibrated);
    expired
        .snapshot
        .details
        .as_mut()
        .unwrap()
        .calibration_expires_unix_seconds = Some(100);
    let mut incomplete = duplicate(&calibrated);
    incomplete
        .snapshot
        .details
        .as_mut()
        .unwrap()
        .calibrated_windows
        .retain(|pool| pool != "seven_day");
    let mut missing_window = duplicate(&calibrated);
    missing_window
        .snapshot
        .windows
        .retain(|window| window.pool != "seven_day");
    let mut stale = duplicate(&calibrated);
    stale.expires_unix_seconds = 100;
    for (name, observations) in [
        ("missing quota", vec![]),
        ("partial ledger", vec![duplicate(&raw)]),
        ("expired calibration", vec![expired]),
        ("incomplete calibration", vec![incomplete]),
        ("missing window", vec![missing_window]),
        ("stale quota", vec![stale]),
    ] {
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&query, &ctx)?;
        assert!(
            matches!(route.readiness, RouteReadiness::Ready { .. }),
            "{name}"
        );
        let detail = route.pool_selection.unwrap().candidates[0]
            .pacing_detail
            .clone()
            .unwrap();
        assert!(
            detail.contains("Go quota source: local-ledger, coverage: partial"),
            "{name}: {detail}"
        );
        // The same local ledger remains visible for explicitly selected models.
        let explicit = resolve(&request(model)?, &ctx)?;
        assert!(
            matches!(explicit.readiness, RouteReadiness::Ready { .. }),
            "{name}"
        );
    }
    let mut higher_ledger = duplicate(&raw);
    higher_ledger.snapshot.windows[2].used_basis_points = 7000;
    let mut passed_reset = duplicate(&calibrated);
    for window in &mut passed_reset.snapshot.windows {
        window.resets_unix_seconds = Some(99);
    }
    for (observation, expected_factor) in [
        (duplicate(&raw), 10_000),
        (calibrated, 4100),
        (apply(&higher_ledger), 3000),
        (passed_reset, 4100),
    ] {
        let observations = [observation];
        let mut ctx = context(&catalogue, &config, now()?);
        ctx.availability = &availability;
        ctx.access = &access;
        ctx.quota = &observations;
        let route = resolve(&query, &ctx)?;
        assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
        let receipt = serde_json::to_value(&route)?["pool_selection"].clone();
        assert_eq!(
            receipt["candidates"][0]["factor_basis_points"],
            expected_factor
        );
        assert_eq!(route.account_key.as_deref(), Some("go"));
    }
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
        "go".into(),
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
    let receipt = serde_json::to_value(&route)?["pool_selection"].clone();
    assert_eq!(receipt["candidates"][0]["factor_basis_points"], 10_000);
    assert!(
        receipt["candidates"][0]["pacing_detail"]
            .as_str()
            .ok_or("pacing detail")?
            .contains("source=provider policy=Pace")
    );
    let previews = pacing_preview(&ModelId::parse(free)?, &ctx);
    assert_eq!(previews.len(), 1);
    assert_eq!(previews[0].account.as_deref(), Some("zen"));
    assert_eq!(previews[0].policy, PacingPolicy::Pace);
    Ok(())
}
