use super::*;

#[test]
fn capacity_cooldowns_apply_only_to_draws_and_expire() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model":"codex/gpt-6-luna", "weight":1, "efforts":["high"]},
            {"model":"codex/gpt-6.1-sol", "weight":1, "efforts":["high"]}
        ]},
        "pacing": {"accounts": {"opaque-account": {"policy":"pace"}}}
    }))?;
    let registry = [provenant_providers::Registration {
        quota_source: None,
        ..provenant_providers::codex::REGISTRATION
    }];
    let availability = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let cooldown = provenant_quota::CapacityCooldown {
        extra: Default::default(),
        account: provenant_quota::AccountScope {
            adapter: "codex".into(),
            account_key: "opaque-account".into(),
            label: "test".into(),
        },
        model: "gpt-6-luna".into(),
        observed_unix_seconds: 90,
        expires_unix_seconds: 110,
    };
    let cooldowns = [cooldown.clone()];
    let mut observation = quota(
        "codex/gpt-6-luna",
        "opaque-account",
        provenant_quota::QuotaStatus::Available,
        1,
        80,
        120,
    )?;
    observation.snapshot.windows = vec![provenant_quota::QuotaWindow {
        pool: "primary".into(),
        window_seconds: Some(18_000),
        used_basis_points: 5_000,
        resets_unix_seconds: Some(10_000),
    }];
    let observations = [observation];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.quota = &observations;
    ctx.registry = &registry;
    ctx.availability = &availability;
    ctx.access = &access;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let baseline = resolve(&query, &ctx)?
        .pool_selection
        .ok_or("receipt")?
        .candidates[0]
        .clone();
    assert!(baseline.pacing_detail.is_some());
    assert_ne!(baseline.factor_basis_points, 10_000);
    ctx.capacity_cooldowns = &cooldowns;
    query.choice = RouteChoice::Model(ModelId::parse("codex/gpt-6-luna")?);
    let explicit = resolve(&query, &ctx)?;
    assert_eq!(explicit.model, "gpt-6-luna");
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let selected = resolve(&query, &ctx)?;
    assert_eq!(selected.model, "gpt-6.1-sol");
    let receipt = selected.pool_selection.ok_or("receipt")?;
    let skipped = &receipt.candidates[0];
    assert_eq!(skipped.exclusion.as_deref(), Some("capacity cooldown"));
    assert_eq!(skipped.factor_basis_points, baseline.factor_basis_points);
    assert_eq!(skipped.pacing_detail, baseline.pacing_detail);
    let both = [
        cooldown,
        provenant_quota::CapacityCooldown {
            extra: Default::default(),
            account: provenant_quota::AccountScope {
                adapter: "codex".into(),
                account_key: "opaque-account".into(),
                label: "test".into(),
            },
            model: "gpt-6.1-sol".into(),
            observed_unix_seconds: 90,
            expires_unix_seconds: 110,
        },
    ];
    ctx.capacity_cooldowns = &both;
    let fallback = resolve(&query, &ctx)?;
    let receipt = fallback.pool_selection.unwrap();
    assert_eq!(
        receipt.reason.as_deref(),
        Some("all eligible candidates cooling, normal selection")
    );
    ctx.now = provenant_core::WallTime::from_micros(110_000_000)?;
    let expired = resolve(&query, &ctx)?.pool_selection.unwrap();
    assert!(expired.candidates.iter().all(|c| c.exclusion.is_none()));
    Ok(())
}

#[test]
fn cooling_ready_candidate_beats_unavailable_candidate() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model":"codex/gpt-6-luna", "weight":1, "efforts":["high"]},
            {"model":"claude/claude-sonnet-5-5", "weight":1, "efforts":["high"]}
        ]}
    }))?;
    let registry = [
        provenant_providers::Registration {
            quota_source: None,
            ..provenant_providers::codex::REGISTRATION
        },
        provenant_providers::Registration {
            quota_source: None,
            ..provenant_providers::claude::REGISTRATION
        },
    ];
    let availability = [availability()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let cooldowns = [provenant_quota::CapacityCooldown {
        extra: Default::default(),
        account: provenant_quota::AccountScope {
            adapter: "codex".into(),
            account_key: "opaque-account".into(),
            label: "test".into(),
        },
        model: "gpt-6-luna".into(),
        observed_unix_seconds: 90,
        expires_unix_seconds: 110,
    }];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.registry = &registry;
    ctx.availability = &availability;
    ctx.access = &access;
    ctx.capacity_cooldowns = &cooldowns;
    let mut query = request("codex/gpt-6-luna")?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let selected = resolve(&query, &ctx)?;
    assert_eq!(selected.model, "gpt-6-luna");
    assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(
        selected.pool_selection.unwrap().reason.as_deref(),
        Some("cooling ready candidate selected over unavailable candidate")
    );
    Ok(())
}

#[test]
fn agy_models_are_paced_and_reserved_by_their_own_quota_pool_only()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaWindow;
    let window = |pool: &str, used| QuotaWindow {
        pool: pool.into(),
        window_seconds: Some(604_800),
        used_basis_points: used,
        resets_unix_seconds: Some(604_900),
    };
    for (model, gemini, claude, factor, detail) in [
        (
            "agy/claude-sonnet-4-6",
            10_000,
            0,
            10_000,
            "quota_pool=claude",
        ),
        (
            "agy/claude-opus-4-6-thinking",
            10_000,
            0,
            10_000,
            "quota_pool=claude",
        ),
        ("agy/claude-sonnet-4-6", 0, 9_900, 2_500, "claude.weekly"),
        (
            "agy/gemini-3.8-flash",
            0,
            10_000,
            10_000,
            "quota_pool=gemini",
        ),
        ("agy/gemini-3.8-flash", 9_900, 0, 2_500, "gemini.weekly"),
        ("agy/gemini-3.8-flash", 10_000, 0, 10_000, "exhausted"),
    ] {
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pacing":{"accounts":{"agy-host":{"policy":"pace", "windows":["seven_day"]}}},
            "pacing_reserve":{"agy":{"seven_day_percent":5}}
        }))?;
        let result = profile_preview(
            config,
            model,
            "agy-host",
            vec![
                window("gemini.weekly", gemini),
                window("claude.weekly", claude),
            ],
            vec![],
        );
        if detail == "exhausted" {
            assert!(!agy_profile_ready(result, model)?);
            continue;
        }
        let route = result?;
        let candidate = &route.pool_selection.as_ref().ok_or("receipt")?.candidates[0];
        let text = candidate.pacing_detail.as_deref().ok_or("detail")?;
        assert_eq!(candidate.factor_basis_points, factor, "{model} {text}");
        assert!(text.contains(detail), "{model} {text}");
        let own_spent = if model.contains("claude") {
            claude
        } else {
            gemini
        } >= 9_500;
        assert_eq!(
            text.contains("reserve exclusion"),
            own_spent,
            "{model} {text}"
        );
    }
    Ok(())
}

#[test]
fn quota_pool_is_validated_and_limited_to_grouped_adapters()
-> Result<(), Box<dyn std::error::Error>> {
    let bundled = Catalogue::bundled()?;
    assert_eq!(
        bundled.quota_pool(&ModelId::parse("agy/claude-sonnet-4-6")?),
        Some("claude")
    );
    assert_eq!(
        bundled.quota_pool(&ModelId::parse("agy/gemini-3.8-flash")?),
        Some("gemini")
    );
    // Uncatalogued models take the pool their family names, or none.
    for (model, pool) in [
        ("agy/gemini-9-pro", Some("gemini")),
        ("agy/claude-opus-9", Some("claude")),
        ("agy/other-1", None),
        ("codex/gemini-1", None),
    ] {
        assert_eq!(bundled.quota_pool(&ModelId::parse(model)?), pool, "{model}");
    }
    for (model, pool, valid) in [
        ("agy/claude-sonnet-4-6", Some("claude"), true),
        ("agy/claude-sonnet-4-6", None, true),
        ("agy/claude-sonnet-4-6", Some(""), false),
        ("agy/claude-sonnet-4-6", Some("bad pool"), false),
        ("codex/gpt-6-luna", Some("gemini"), false),
    ] {
        let mut catalogue = Catalogue::bundled()?;
        let id = ModelId::parse(model)?;
        let row = catalogue
            .models
            .iter_mut()
            .find(|row| row.model == id)
            .ok_or(model)?;
        row.quota_pool = pool.map(Into::into);
        assert_eq!(catalogue.validate().is_ok(), valid, "{model} {pool:?}");
    }
    Ok(())
}

#[test]
fn agy_exhaustion_follows_the_models_serving_windows() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let window = |pool: &str, used, reset| QuotaWindow {
        pool: pool.into(),
        window_seconds: Some(604_800),
        used_basis_points: used,
        resets_unix_seconds: Some(reset),
    };
    let ready = |model: &str, status, windows| -> Result<bool, Box<dyn std::error::Error>> {
        let config: RoutingConfig = serde_json::from_value(
            serde_json::json!({"pacing":{"accounts":{"agy-host":{"policy":"max"}}}}),
        )?;
        agy_profile_ready(
            profile_preview_with_status(config, model, "agy-host", windows, vec![], status),
            model,
        )
    };
    let (claude, gemini) = ("agy/claude-sonnet-4-6", "agy/gemini-3.8-flash");
    // An exhausted ungrouped window serves every model.
    for model in [claude, gemini] {
        let windows = vec![
            window("gemini.weekly", 0, 604_900),
            window("claude.weekly", 0, 604_900),
            window("quota", 10_000, 604_900),
        ];
        assert!(!ready(model, QuotaStatus::Available, windows)?, "{model}");
    }
    // A spent Gemini group leaves Claude ready, and the reverse.
    let split = |g, c| {
        vec![
            window("gemini.weekly", g, 604_900),
            window("claude.weekly", c, 604_900),
        ]
    };
    assert!(ready(claude, QuotaStatus::Available, split(10_000, 0))?);
    assert!(!ready(gemini, QuotaStatus::Available, split(10_000, 0))?);
    assert!(ready(gemini, QuotaStatus::Available, split(0, 10_000))?);
    assert!(!ready(claude, QuotaStatus::Available, split(0, 10_000))?);
    // Exhaustion lapses per model: Claude's reset has passed, Gemini's has not.
    let lapsed = vec![
        window("gemini.weekly", 10_000, 604_900),
        window("claude.weekly", 10_000, 50),
    ];
    assert!(ready(claude, QuotaStatus::Exhausted, lapsed.clone())?);
    assert!(!ready(gemini, QuotaStatus::Exhausted, lapsed)?);
    Ok(())
}

#[test]
fn agy_shared_claude_pool_exhaustion_skips_only_the_claude_models()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{QuotaStatus, QuotaWindow};
    let window = |pool: &str, used, reset| QuotaWindow {
        pool: pool.into(),
        window_seconds: Some(604_800),
        used_basis_points: used,
        resets_unix_seconds: Some(reset),
    };
    // The reported registration: the gemini windows have headroom while the
    // shared Claude pool is spent, so gemini lanes must stay ready and only
    // the agy Claude models may be skipped.
    let windows = vec![
        window("gemini.weekly", 2_256, 604_900),
        window("gemini.5h", 1_701, 4_900),
        window("claude.weekly", 10_000, 604_900),
    ];
    let ready = |model: &str| -> Result<bool, Box<dyn std::error::Error>> {
        let config: RoutingConfig = serde_json::from_value(
            serde_json::json!({"pacing":{"accounts":{"agy-host":{"policy":"max"}}}}),
        )?;
        agy_profile_ready(
            profile_preview_with_status(
                config,
                model,
                "agy-host",
                windows.clone(),
                vec![],
                QuotaStatus::Available,
            ),
            model,
        )
    };
    assert!(ready("agy/gemini-3.8-flash")?);
    assert!(!ready("agy/claude-sonnet-4-6")?);
    assert!(!ready("agy/claude-opus-4-6-thinking")?);
    Ok(())
}

#[test]
fn unsupported_pool_effort_is_dropped_with_a_warning() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model": "codex/gpt-6-luna", "effort": "retired", "efforts": ["low", "retired"]}
        ]}
    }))?;
    assert!(config.validate().is_empty());
    let context = context(&catalogue, &config, now()?);
    let listed = list(
        &RouteListQuery {
            pool: Some("trial".into()),
            ..RouteListQuery::default()
        },
        &context,
    )?;
    let member = &listed.pools["trial"][0];
    assert_eq!(member.efforts, ["low"]);
    assert_eq!(member.effort, None);
    let warnings = config.pool_effort_warnings(&catalogue);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("domains.routing.pools.trial[0].efforts"));
    assert!(warnings[0].contains("retired"));
    Ok(())
}

#[test]
fn supported_default_effort_survives_reconciliation() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model": "codex/gpt-6-luna", "effort": "low", "efforts": ["low", "retired"]}
        ]}
    }))?;
    let context = context(&catalogue, &config, now()?);
    let listed = list(&RouteListQuery::default(), &context)?;
    let member = &listed.pools["trial"][0];
    assert_eq!(member.effort.as_deref(), Some("low"));
    assert_eq!(member.efforts, ["low"]);
    Ok(())
}

#[test]
fn malformed_duplicate_or_excess_pool_efforts_still_fail() -> Result<(), Box<dyn std::error::Error>>
{
    let fails = |member: serde_json::Value| -> Result<bool, serde_json::Error> {
        let config: RoutingConfig =
            serde_json::from_value(serde_json::json!({"pools": {"trial": [member]}}))?;
        Ok(config
            .validate()
            .iter()
            .any(|error| error.code == "unsupported_effort"))
    };
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "efforts": ["bad effort"]})
    )?);
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "efforts": ["low", "low"]})
    )?);
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "effort": "bad effort"})
    )?);
    let many: Vec<String> = (0..17).map(|n| format!("e{n}")).collect();
    assert!(fails(
        serde_json::json!({"model": "codex/gpt-6-luna", "efforts": many})
    )?);
    Ok(())
}

#[test]
fn malformed_scope_types_drop_or_refuse_the_whole_record() -> Result<(), Box<dyn std::error::Error>>
{
    let rule = "schema_version=1\n[[domains.routing.model_policy.rules]]\n";
    let lane = "[domains.routing.model_policy.rules.policy.lane]\n";
    // A rule that grants writes loses neither its expiry nor its selector:
    // the whole rule is dropped, with a warning at the rule.
    for scope in [
        "schedule={expires='bad'}\n",
        "schedule={windows=[{start_hour='x',end_hour=4}]}\n",
        "schedule={expirse=10}\n",
        "model=5\n",
        "account=7\n",
    ] {
        let text = format!("{rule}{scope}{lane}read_only=false\n");
        let config = load_routing(&text)??;
        assert!(
            config.domains.routing.model_policy.rules.is_empty(),
            "{text}"
        );
        assert!(
            config.warnings.iter().any(|w| w
                .message
                .starts_with("domains.routing.model_policy.rules.0:")),
            "{text}: {:?}",
            config.warnings
        );
    }
    // Dropping a restricting rule would lift its restriction, so it refuses.
    let text = format!("{rule}schedule={{expires='bad'}}\n{lane}read_only=true\n");
    let error = load_routing(&text)?
        .err()
        .ok_or("restricting rule loaded")?;
    assert!(
        error.to_string().contains("model_policy.rules.0"),
        "{error}"
    );
    // An override loses its provider or schedule only with the whole row.
    for row in [
        "provider=5\npolicy='off'\n",
        "provider='codex'\npolicy='off'\nschedule={expires='bad'}\n",
    ] {
        let text = format!(
            "schema_version=1\n[[domains.routing.pacing_overrides]]\n{row}[[domains.routing.pacing_overrides]]\nprovider='claude'\npolicy='spend'\n"
        );
        let config = load_routing(&text)??;
        let rows = &config.domains.routing.pacing_overrides;
        assert_eq!(rows.len(), 1, "{text}");
        assert_eq!(rows[0].provider.as_deref(), Some("claude"), "{text}");
    }
    Ok(())
}

#[test]
fn invalid_override_schedules_drop_their_override_at_load() -> Result<(), Box<dyn std::error::Error>>
{
    for schedule in [
        "{starts='1970-01-01T00:00:20Z',expires='1970-01-01T00:00:10Z'}",
        "{tz='Mars/Olympus_Mons'}",
    ] {
        let text = format!(
            "schema_version=1\n[[domains.routing.pacing_overrides]]\nprovider='codex'\npolicy='off'\nschedule={schedule}\n[[domains.routing.pacing_overrides]]\nprovider='claude'\npolicy='spend'\n"
        );
        let config = load_routing(&text)??;
        let rows = &config.domains.routing.pacing_overrides;
        assert_eq!(rows.len(), 1, "{text}");
        assert_eq!(rows[0].provider.as_deref(), Some("claude"), "{text}");
        assert!(
            config
                .warnings
                .iter()
                .any(|w| w.message.starts_with("domains.routing.pacing_overrides.0:")),
            "{text}: {:?}",
            config.warnings
        );
    }
    Ok(())
}

#[test]
fn read_only_policy_model_stays_in_junior_and_senior_pools_for_read_only_work()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = toml_edit::de::from_str(
        "[[model_policy.rules]]\nmodel='agy/gemini-3.8-flash'\n[model_policy.rules.policy.lane]\nread_only=true\n",
    )?;
    let records = [AvailabilityRecord {
        adapter: "agy".into(),
        observed_models: vec!["gemini-3.8-flash".into()],
        ..availability()
    }];
    let access = [AccessContext {
        adapter: "agy",
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    }];
    let context = RoutingContext {
        availability: &records,
        access: &access,
        ..context(&catalogue, &config, now()?)
    };
    let gemini_exclusion = |pool: &str, writes: bool, workload| {
        let request = RouteRequest {
            intent: RouteIntent {
                workload,
                writes,
                ..Default::default()
            },
            choice: RouteChoice::NamedRoute(RouteName::parse(pool)?),
            effort: None,
            confidential: false,
            fallback: Fallback::None,
        };
        let route = resolve_with_seed(&request, &context, "agy-read-only", 0)?;
        let receipt = route.pool_selection.ok_or("pool receipt")?;
        let member = receipt
            .candidates
            .iter()
            .find(|c| c.model == "agy/gemini-3.8-flash")
            .ok_or("gemini candidate")?;
        Ok::<_, Box<dyn std::error::Error>>(member.exclusion.clone())
    };
    for pool in ["junior", "senior"] {
        for workload in [Workload::General, Workload::ReadHeavy, Workload::Review] {
            assert_eq!(gemini_exclusion(pool, false, workload)?, None, "{pool}");
        }
        assert_eq!(
            gemini_exclusion(pool, true, Workload::General)?.as_deref(),
            Some("read-only model policy"),
            "{pool}"
        );
    }
    let mut query = request("agy/gemini-3.8-flash")?;
    assert!(resolve(&query, &context).is_ok());
    query.intent.writes = true;
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::ReadOnlyPolicy)
    ));
    Ok(())
}

#[test]
fn persisted_mismatch_excludes_account_and_selects_another()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let availability = two_accounts();
    let mut bad = provenant_quota::AccountRecord::empty(account_scope("opaque-account"));
    bad.last_error = Some(provenant_quota::ProbeFailure::AccountMismatch);
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &ACCESS;
    let records = [bad.clone()];
    ctx.accounts = &records;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(route.account_key.as_deref(), Some("other-account"));
    // Every serving account mismatched: the typed reason is reported.
    let mut other = provenant_quota::AccountRecord::empty(account_scope("other-account"));
    other.last_error = Some(provenant_quota::ProbeFailure::AccountMismatch);
    let both = [bad, other];
    ctx.accounts = &both;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::AccountMismatch,
            ..
        }
    ));
    Ok(())
}

#[test]
fn invalid_account_evidence_excludes_only_that_account() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let availability = two_accounts();
    let mut broken = provenant_quota::AccountRecord::empty(account_scope("opaque-account"));
    broken.refusals.push(provenant_quota::Refusal {
        extra: Default::default(),
        account: account_scope("opaque-account"),
        model: "test".into(),
        pool: None,
        observed_unix_seconds: i64::MAX,
        resets_unix_seconds: None,
    });
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &ACCESS;
    let records = [broken];
    ctx.accounts = &records;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    assert_eq!(route.account_key.as_deref(), Some("other-account"));
    // Alone, the broken account is unavailable for a reason that names the evidence.
    let single = [availability[0].clone()];
    ctx.availability = &single;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::InvalidAccountEvidence,
            ..
        }
    ));
    Ok(())
}

#[test]
fn account_evidence_reasons_carry_their_repair_text() {
    for (reason, text) in [
        (RouteExclusionReason::AccountMismatch, "re-register"),
        (RouteExclusionReason::FreeTierRefused, "paid route"),
        (
            RouteExclusionReason::ProviderRefused,
            "scoped provider refusal",
        ),
        (RouteExclusionReason::InvalidAccountEvidence, "invalid"),
        (
            RouteExclusionReason::UnknownAvailability,
            "provenant quota tick",
        ),
        (RouteExclusionReason::StaleAvailability, "stale"),
        (RouteExclusionReason::AuthRequired, "sign in"),
    ] {
        assert!(exclusion_detail(&reason).contains(text), "{reason:?}");
    }
    let exhausted = exclusion_detail(&RouteExclusionReason::Exhausted);
    assert!(exhausted.contains("exhausted"));
    assert!(exhausted.contains("wait"));
    assert!(exhausted.contains("reset"));
}

#[test]
fn free_pool_refusal_excludes_the_account_for_every_free_model()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let free = "opencode/opencode/deepseek-v4.1-flash-free";
    let mut record = availability();
    record.adapter = "opencode".into();
    record.account_key = Some("zen".into());
    let records = [record];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let scope = provenant_quota::AccountScope {
        adapter: "opencode".into(),
        account_key: "zen".into(),
        label: "test".into(),
    };
    // The refusal names a different free model: the keyless pool spans models.
    let refused = [provenant_quota::record_refusal(
        None,
        &scope,
        "opencode/opencode/other-free",
        Some("opencode-free"),
        90,
        Some(200),
    )?
    .replacement
    .ok_or("record")?];
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    assert!(matches!(
        resolve(&request(free)?, &ctx)?.readiness,
        RouteReadiness::Ready { .. }
    ));
    ctx.accounts = &refused;
    assert!(matches!(
        resolve(&request(free)?, &ctx)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::FreeTierRefused,
            ..
        }
    ));
    Ok(())
}

#[test]
fn live_scoped_refusal_excludes_only_the_matching_account_and_model()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let availability = two_accounts();
    let refused = |account: &str, model: &str| -> Result<_, Box<dyn std::error::Error>> {
        let scope = account_scope(account);
        Ok(provenant_quota::record_refusal(
            None,
            &scope,
            &format!("codex/{model}"),
            None,
            90,
            Some(200),
        )?
        .replacement
        .ok_or("record")?)
    };
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &availability;
    ctx.access = &ACCESS;
    let records = [refused("opaque-account", "test")?];
    ctx.accounts = &records;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert_eq!(route.account_key.as_deref(), Some("other-account"));
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    // The same account still serves an unrelated model.
    let route = resolve(&request("codex/other")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    // With no other account, the refusal is the typed reason.
    let single = [availability[0].clone()];
    ctx.availability = &single;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::ProviderRefused,
            ..
        }
    ));
    // A refusal that has lapsed no longer blocks.
    let lapsed = [provenant_quota::record_refusal(
        None,
        &account_scope("opaque-account"),
        "codex/test",
        None,
        90,
        Some(95),
    )?
    .replacement
    .ok_or("record")?];
    ctx.accounts = &lapsed;
    let route = resolve(&request("codex/test")?, &ctx)?;
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    Ok(())
}
