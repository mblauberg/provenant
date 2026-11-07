use super::*;

/// Whether the account's reading says a window is spent and has not reset.
/// Admission gates only models with a quota pool; native providers surface a
/// spent allowance as an exhausted snapshot, which a preview reads directly.

pub(super) fn spent(record: &provenant_quota::AccountRecord, now: i64) -> bool {
    record.snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.status == provenant_quota::QuotaStatus::Exhausted
            && snapshot.observed_unix_seconds <= now
            && snapshot.windows.iter().any(|window| {
                window.used_basis_points >= 10_000
                    && window.resets_unix_seconds.is_none_or(|reset| reset > now)
            })
    })
}

pub fn route_blocked(alias: &str, exclude: &[String], roots: &Roots) -> Option<String> {
    let query = RouteResolve {
        writes: false,
        account: None,
        name: None,
        request: RouteRequest {
            read_heavy: false,
            model: None,
            route: Some(alias.into()),
            effort: None,
            confidential: false,
            fallback: Vec::new(),
            exclude: exclude.to_vec(),
        },
    };
    let blocked = with_context(
        roots,
        REGISTRY,
        &[],
        &[],
        "local-preview",
        &[],
        false,
        |context, records, policies, _| {
            let request = resolve_request(&query, context)?;
            let rules = effective_exclusion_rules(exclude, roots)?;
            let now = context.now.as_timestamp().as_second();
            // Dispatch retries the next pool member when one is refused, so a
            // route is blocked only when every member is. The resolver picks
            // each candidate in turn; a refused member is excluded from the
            // next pick, exactly as dispatch narrows its own retry.
            let mut excluded: Vec<ModelId> = Vec::new();
            let mut refusals: Vec<String> = Vec::new();
            loop {
                let route = match provenant_routing::resolve_for_account_with_seed_excluding(
                    &request,
                    context,
                    None,
                    "route-preview",
                    0,
                    &excluded,
                    &rules,
                ) {
                    Ok(route) => route,
                    Err(error) if refusals.is_empty() => return Err(error.into()),
                    // Every member has been judged and refused.
                    Err(_) => return Ok::<_, EdgeError>(Some(refusals.join("; "))),
                };
                let model = ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
                // The resolver ignores exclusions for a route that names one
                // model, and returns it again: every candidate has been seen.
                if excluded.contains(&model) {
                    return Ok(Some(refusals.join("; ")));
                }
                let pools = context.catalogue.admission_pools(&model);
                let admission = provenant_quota::Admission {
                    policies,
                    now_unix_seconds: now,
                    records,
                    pools: &pools,
                };
                // Judge each cached account of the member's provider as if its
                // provider were ready: a spent, refused or unauthenticated
                // account cannot serve. With no cached account, nothing is
                // known and the member may run once dispatch probes it.
                let mut member = Vec::new();
                for record in records
                    .iter()
                    .filter(|record| record.account.adapter == route.adapter)
                {
                    let mut probe = route.clone();
                    probe.account_key = Some(record.account.account_key.clone());
                    probe.readiness = RouteReadiness::Ready {
                        extra: Default::default(),
                        availability_ref: "preview".into(),
                        expires_unix_seconds: now + 60,
                    };
                    match admission.check_with_partial_headroom(
                        &probe,
                        context.config.allow_unknown_partial_headroom,
                    ) {
                        Ok(()) if !spent(record, now) => return Ok(None),
                        Ok(()) => member.push("allowance exhausted".to_owned()),
                        Err(error) => member.push(error.to_string()),
                    }
                }
                if member.is_empty() {
                    return Ok(None);
                }
                refusals.push(format!(
                    "{}/{}: {}",
                    route.adapter,
                    route.model,
                    member.join("; ")
                ));
                // Dispatch narrows only a pool; a single model has no next one.
                if route.pool_selection.is_none() {
                    return Ok(Some(refusals.join("; ")));
                }
                excluded.push(model);
            }
        },
    );
    match blocked {
        Ok(reason) => reason,
        Err(error) => Some(error.to_string()),
    }
}
pub(super) fn readiness_text(readiness: &RouteReadiness) -> String {
    use provenant_records::RouteExclusionReason;
    match readiness {
        RouteReadiness::Ready {
            expires_unix_seconds,
            ..
        } => format!("ready (observation expires at {expires_unix_seconds})"),
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability,
            ..
        } => "unknown (not probed; run `provenant quota tick` to refresh quota)".into(),
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::StaleAvailability,
            ..
        } => "unknown (observation expired; run `provenant quota tick` to refresh quota)".into(),
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownPartialHeadroom,
            ..
        } => "unknown (partial headroom is uncalibrated; run `provenant quota tick` to refresh quota)".into(),
        RouteReadiness::Unavailable { reason, .. } => {
            let label = serde_json::to_value(reason)
                .ok()
                .and_then(|value| value.as_str().map(|text| text.replace('_', " ")))
                .unwrap_or_else(|| "unknown reason".into());
            format!("unavailable ({label})")
        }
    }
}

pub(super) fn unobserved_model(model: &ModelId, list: &RouteListDisplay) -> &'static str {
    if list
        .recorded_readiness
        .iter()
        .any(|record| record.model == model.as_str())
    {
        ""
    } else if list.readiness_truncated {
        "; readiness not shown (display limit reached)"
    } else {
        "; readiness unknown (not probed; run `provenant quota tick` to refresh quota)"
    }
}

/// The human report for a resolved route preview, naming each exclusion that
/// removed a pool candidate and where it came from.
pub(crate) fn resolved_text(preview: &RoutePreview) -> String {
    let route = &preview.route;
    let diagnostics = preview
        .pacing
        .iter()
        .map(|p| {
            format!(
                "\n  account={} policy={:?} factor={} {}",
                p.account.as_deref().unwrap_or("unknown"),
                p.policy,
                p.factor_basis_points,
                p.detail
            )
        })
        .collect::<String>();
    let mut text = format!(
        "{}/{} effort={} {}{}{}\n",
        route.adapter,
        route.model,
        route.effort.as_deref().unwrap_or("unspecified"),
        readiness_text(&route.readiness),
        route
            .pool_selection
            .as_ref()
            .map_or_else(String::new, |receipt| {
                let details = receipt
                    .candidates
                    .iter()
                    .flat_map(|candidate| {
                        candidate
                            .exclusion
                            .iter()
                            .chain(candidate.pacing_detail.iter())
                            .map(|detail| format!("\n  {}: {detail}", candidate.model))
                    })
                    .collect::<String>();
                format!(
                    " pool={} roll={} pick={}{}",
                    receipt.pool, receipt.roll, receipt.pick, details
                )
            }),
        diagnostics
    );
    for record in &preview.recorded_readiness {
        text.push_str(&format!(
            "recorded readiness {} [{}] for {}: {}\n",
            record.account.escape_debug(),
            record.account_key.escape_debug(),
            record.model,
            crate::quota::readiness_words(&record.observation)
        ));
    }
    for refusal in &preview.provider_refusals {
        text.push_str(&format!("provider refusal: {refusal}\n"));
    }
    if let Some(refusal) = &preview.admission_refusal {
        text.push_str(&format!("admission: {refusal}\n"));
    }
    if let Some(diagnostic) = &preview.login_diagnostic {
        text.push_str(&format!("login: {diagnostic}\n"));
    }
    text.push_str(&format!(
        "model policy sources={}\ncompaction break-even turns (15k summary, zero rework): {:?}\n",
        preview.model_policy.sources.join(", "),
        preview.compaction_break_even_turns
    ));
    for note in &preview.exclusion_notes {
        text.push_str(&format!("note: {note}\n"));
    }
    match &preview.supported_efforts {
        Some(efforts) if efforts.is_empty() => text.push_str("supported efforts: none\n"),
        Some(efforts) => text.push_str(&format!("supported efforts: {}\n", efforts.join(", "))),
        None => {
            text.push_str("model is not in the catalogue; effort support is unrecorded\n");
            if !preview.nearest_models.is_empty() {
                text.push_str(&format!(
                    "nearest catalogued IDs: {}\n",
                    preview.nearest_models.join(", ")
                ));
            }
        }
    }
    text.push_str(&format!(
        "allow unknown partial headroom: {} (source: {:?})\n",
        preview.allow_unknown_partial_headroom.value, preview.allow_unknown_partial_headroom.source
    ));
    if let Some(details) = preview.quota.as_ref().and_then(|q| q.details.as_ref())
        && let (Some(source), Some(coverage)) = (&details.source, &details.coverage)
    {
        text.push_str(&format!("quota source: {source}, coverage: {coverage}\n"));
    }
    text
}
pub fn run(
    command: Route,
    globals: &Globals,
    roots: &Roots,
    publish: impl FnOnce(&CommandResult<Option<Data>>) -> io::Result<provenant_records::AbsolutePath>,
) -> u8 {
    let name = match &command {
        Route::List(_) => "route list",
        Route::Resolve(_) => "route resolve",
        Route::Set(_) => "route set",
        Route::Remove(_) => "route remove",
        Route::Reset(_) => "route reset",
        Route::Pacing(_) => "route pacing",
        Route::Exclude(request) => match request.command {
            RouteExcludeCommand::Add(_) => "route exclude add",
            RouteExcludeCommand::Remove(_) => "route exclude remove",
            RouteExcludeCommand::List => "route exclude list",
        },
        Route::Override(request) => match request.command {
            RouteOverrideCommand::Set(_) => "route override set",
            RouteOverrideCommand::List(_) => "route override list",
            RouteOverrideCommand::Clear(_) => "route override clear",
        },
    };
    let now = SystemClock::new().wall_now();
    let at = now.as_ref().ok().copied();
    let result = match now
        .map_err(EdgeError::from)
        .and_then(|now| execute(&command, roots, now))
    {
        Ok(data) => CommandResult::new(name, Outcome::Ok, Some(data)),
        Err(error) => {
            let (status, code) = match &error {
                EdgeError::Config(provenant_config::ConfigError::Read { .. }) => {
                    (Outcome::Error, "IO")
                }
                EdgeError::QuotaCache(crate::quota::EdgeError::Quota(_)) | EdgeError::Quota(_) => {
                    (Outcome::Invalid, "QUOTA")
                }
                EdgeError::Routing(_) | EdgeError::Unresolved { .. } | EdgeError::Pacing(_) => {
                    (Outcome::Invalid, "INVALID")
                }
                EdgeError::Config(error) | EdgeError::PacingConfig(error)
                    if error.refusal_code().is_some() =>
                {
                    (
                        Outcome::Invalid,
                        error
                            .refusal_code()
                            .unwrap_or(provenant_config::shared_settings::NEVER_SYNCED_CODE),
                    )
                }
                EdgeError::Config(_) | EdgeError::PacingConfig(_) => (Outcome::Invalid, "CONFIG"),
                EdgeError::AccountProfile { kind, .. } => (Outcome::Invalid, kind.code()),
                _ => (Outcome::Error, "IO"),
            };
            let mut result = CommandResult::new(name, status, None);
            let projection = match &error {
                EdgeError::AccountProfile { kind, .. } => kind.kind().map(|kind| kind.diagnostic()),
                EdgeError::Config(error) | EdgeError::PacingConfig(error)
                    if error.refusal_code().is_some() =>
                {
                    Some(provenant_run::ErrorKind::SharedSettingsNeverSynced.diagnostic())
                }
                _ => None,
            };
            result.error = Some(CommandError {
                diagnostic: projection.as_ref().map(Into::into),
                code: code.into(),
                message: error.to_string(),
                cause: None,
                repair: projection.and_then(|diagnostic| diagnostic.next),
            });
            result
        }
    };
    let human = if let Some(error) = &result.error {
        format!("provenant: {}: {}\n", error.code, error.message)
    } else {
        match &result.data {
            Some(Data::List(list)) => {
                let mut lines = vec![format!("catalogue {}", list.catalogue_revision)];
                lines.extend(list.valid_pairs.iter().map(|pair| {
                    format!(
                        "{} effort={}",
                        pair.model.as_str(),
                        pair.effort.as_deref().unwrap_or("unspecified")
                    )
                }));
                lines.extend(list.models.iter().map(|m| {
                    format!(
                        "{} efforts=[{}]{}",
                        m.model.as_str(),
                        m.efforts.join(","),
                        unobserved_model(&m.model, list)
                    )
                }));
                lines.extend(list.named_routes.iter().map(|(name, r)| {
                    format!(
                        "route {name} = {}{}",
                        r.model.as_str(),
                        unobserved_model(&r.model, list)
                    )
                }));
                lines.extend(list.account_pacing.iter().map(|entry| {
                    format!(
                        "account {}/{} pacing={:?} source={} profile={:?} expires={:?}",
                        entry.adapter,
                        entry.account,
                        entry.policy,
                        entry.source,
                        entry.profile,
                        entry.expires.map(|at| at.to_string()),
                    )
                }));
                lines.extend(list.configured_accounts.iter().map(|(account, profile)| {
                    format!("account {account} pacing profile={profile:?}")
                }));
                lines.extend(list.configured_free.iter().map(|(provider, profile)| {
                    format!("free {provider} pacing profile={profile:?}")
                }));
                lines.extend(
                    list.active_overrides
                        .iter()
                        .map(|override_| format!("pacing override {override_:?}")),
                );
                lines.extend(
                    list.exclusion_notes
                        .iter()
                        .map(|note| format!("note: {note}")),
                );
                if let Some(now) = at {
                    lines.extend(
                        list.model_exclusions
                            .iter()
                            .map(|row| OverrideRow::model_exclusion(row, now).line()),
                    );
                }
                if list.recorded_readiness.is_empty() {
                    lines.push("readiness unknown (not probed; run `provenant quota tick` to refresh quota)".into());
                }
                lines.extend(list.recorded_readiness.iter().map(|record| {
                    format!(
                        "recorded readiness {} [{}] for {}: {}",
                        record.account.escape_debug(),
                        record.account_key.escape_debug(),
                        record.model,
                        crate::quota::readiness_words(&record.observation)
                    )
                }));
                if list.readiness_truncated {
                    lines.push(
                        "warning: readiness display limited to 256 account/model rows".into(),
                    );
                }
                lines.extend(list.pools.iter().map(|(name, members)| {
                    let models = members
                        .iter()
                        .map(|member| {
                            format!(
                                "{} weight={} pacing={:?}{}{}",
                                member.model.as_str(),
                                member.weight,
                                member.pacing,
                                member
                                    .effort
                                    .as_ref()
                                    .map_or(String::new(), |effort| format!(" effort={effort}")),
                                unobserved_model(&member.model, list),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("route pool {name} = {models}")
                }));
                format!("{}\n", lines.join("\n"))
            }
            Some(Data::Resolved(preview)) => resolved_text(preview),
            Some(Data::Pacing(report)) => at.map(|now| report.human(now)).unwrap_or_default(),
            Some(Data::Overrides(report)) => {
                let mut lines = Vec::new();
                if let Some(row) = &report.recorded {
                    lines.push(format!("recorded: {}", row.line()));
                }
                lines.extend(
                    report
                        .cleared
                        .iter()
                        .map(|row| format!("removed: {}", row.line())),
                );
                lines.push(format!("overrides: {}", report.overrides.len()));
                lines.extend(report.overrides.iter().map(OverrideRow::line));
                lines.extend(report.notes.iter().map(|note| format!("note: {note}")));
                format!("{}\n", lines.join("\n"))
            }
            None => String::new(),
        }
    };
    let summary = match &result.data {
        Some(Data::List(list)) => {
            let counts = [
                ("models", list.models.len()),
                ("routes", list.named_routes.len()),
                ("pools", list.pools.len()),
                ("pacing entries", list.account_pacing.len()),
                ("overrides", list.active_overrides.len()),
                ("model exclusions", list.model_exclusions.len()),
                ("exclusion notes", list.exclusion_notes.len()),
            ];
            let mut rows = Vec::new();
            if list.readiness_truncated {
                rows.push("warning: readiness display limited to 256 account/model rows".into());
            }
            if list.recorded_readiness.is_empty() {
                rows.push(
                    "readiness unknown (not probed; run `provenant quota tick` to refresh quota)"
                        .into(),
                );
            }
            rows.extend(list.recorded_readiness.iter().map(|record| {
                format!(
                    "recorded readiness {} [{}] for {}: {}",
                    record.account.escape_debug(),
                    record.account_key.escape_debug(),
                    record.model,
                    crate::quota::readiness_words(&record.observation)
                )
            }));
            rows.extend(list.named_routes.iter().map(|(name, r)| {
                format!(
                    "route {name} = {}{}",
                    r.model.as_str(),
                    unobserved_model(&r.model, list)
                )
            }));
            rows.extend(list.pools.keys().map(|name| format!("pool {name}")));
            Some(crate::Summary::new(
                format!(
                    "catalogue {}: {}",
                    list.catalogue_revision,
                    crate::Summary::tally(&counts)
                ),
                rows,
                Some("provenant route resolve MODEL".to_owned()),
            ))
        }
        Some(Data::Pacing(report)) => Some(crate::Summary::new(
            format!(
                "recorded {}; {} active pacing overrides",
                report.recorded,
                report.active_overrides.len()
            ),
            Vec::new(),
            Some("provenant route override list".to_owned()),
        )),
        Some(Data::Overrides(report)) => {
            let active = report.overrides.iter().filter(|row| row.active).count();
            let tally = format!(
                "{} scheduled overrides, {active} active",
                report.overrides.len()
            );
            let headline = match (&report.recorded, report.cleared.len()) {
                (Some(row), 0) => format!("recorded {}; {tally}", row.line()),
                (Some(row), cleared) => {
                    format!("recorded {}; removed {cleared}; {tally}", row.line())
                }
                (None, _)
                    if matches!(
                        command,
                        Route::Override(RouteOverride {
                            command: RouteOverrideCommand::Clear(_)
                        })
                    ) =>
                {
                    format!("cleared {} pacing overrides; {tally}", report.cleared.len())
                }
                (None, _) => tally,
            };
            let next = if matches!(command, Route::Exclude(_)) {
                if report.overrides.is_empty() {
                    "provenant route exclude add ADAPTER/ID --until 2h"
                } else {
                    "provenant route exclude remove ADAPTER/ID"
                }
            } else if report.overrides.is_empty() {
                "provenant route override set --account ACCOUNT --policy POLICY --until 2h"
            } else if report.recorded.is_some() {
                "provenant route override list"
            } else {
                "provenant route override clear"
            };
            Some(crate::Summary::new(
                headline,
                report
                    .overrides
                    .iter()
                    .map(OverrideRow::line)
                    .chain(report.notes.iter().map(|note| format!("note: {note}")))
                    .collect(),
                Some(next.to_owned()),
            ))
        }
        _ => None,
    };
    let listing = matches!(
        command,
        Route::Override(RouteOverride {
            command: RouteOverrideCommand::List(_)
        })
    );
    let mode = globals.render_mode(
        &human,
        listing || matches!(command, Route::List(_) | Route::Resolve(_)),
        summary.as_ref(),
    );
    let outcome = crate::render_or_publish(
        &result,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        publish,
    );
    outcome.map_or(3, |outcome| crate::exit_status(&outcome))
}
