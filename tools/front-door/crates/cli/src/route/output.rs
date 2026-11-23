use super::*;

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
        None,
        |context, records, policies, _| {
            let request = resolve_request(&query, context)?;
            let rules = effective_exclusion_rules(exclude, roots)?;
            let facts = super::admission::AdmissionFacts {
                freshness: &context.config.freshness,
                policy: &context.config.launch_policy,
                policies,
                now_unix_seconds: context.now.as_timestamp().as_second(),
                records,
                catalogue: context.catalogue,
            };
            // The shared resolver walks the pool in dispatch's order and
            // judges every cached account of each member. A route blocks only
            // when no member can serve, with each typed refusal in the reason.
            let admitted = super::admission::resolve_admitted(
                &request,
                &facts,
                super::admission::AdmissionOptions {
                    account: None,
                    mode: super::admission::AdmitMode::CachedAccounts,
                    initial_excluded: &[],
                    narrow_accounts: false,
                },
                |narrowing| {
                    provenant_routing::resolve_for_account_with_seed_excluding(
                        &request,
                        context,
                        None,
                        "route-preview",
                        0,
                        narrowing.excluded,
                        &rules,
                    )
                    .map_err(EdgeError::from)
                },
            )?;
            Ok(admitted.blocked_reason())
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
        if matches!(
            route.readiness,
            RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::UnknownAvailability,
                ..
            }
        ) {
            "provider availability checked at launch".to_owned()
        } else {
            readiness_text(&route.readiness)
        },
        route
            .pool_selection
            .as_ref()
            .map_or_else(String::new, |receipt| {
                let details = receipt
                    .candidates
                    .iter()
                    .map(|candidate| {
                        let prior = candidate.ranking.outcome_prior.as_ref();
                        let evidence = prior.and_then(|prior| prior.evidence.as_ref());
                        let evidence_count = prior.map_or(0, |prior| prior.evidence_count);
                        let factor = prior.map_or(10_000, |prior| prior.factor_basis_points);
                        let successes = evidence.map_or(0, |evidence| evidence.successes);
                        let failures = evidence.map_or(0, |evidence| evidence.failures);
                        let source = prior.map_or("unavailable", |prior| prior.source.as_str());
                        let capability = candidate.ranking.capability_preference.as_ref();
                        let capability_weight = capability.map_or(1, |preference| preference.weight);
                        let capability_source = capability.map_or("unavailable", |preference| preference.source.as_str());
                        let ranking = format!(
                            "\n  {}: prior={factor}/10000 successes={successes} failures={failures} evidence_count={evidence_count} source={source} capability_weight={capability_weight} capability_source={capability_source}",
                            candidate.model
                        );
                        let diagnostics = candidate
                            .exclusion
                            .iter()
                            .chain(candidate.pacing_detail.iter())
                            .map(|detail| format!("\n  {}: {detail}", candidate.model))
                            .collect::<String>();
                        format!("{ranking}{diagnostics}")
                    })
                    .collect::<String>();
                format!(
                    " pool={} roll={} pick={}{}",
                    receipt.pool, receipt.roll, receipt.pick, details
                )
            }),
        diagnostics
    );
    if let Some(warning) = route.diagnostics.effort_warning.as_ref() {
        text.push_str(&format!(
            "warning: effort {} for {} dropped; source={}\n",
            warning.requested, warning.model, warning.source
        ));
    }
    for exclusion in &route.diagnostics.account_exclusions {
        text.push_str(&format!(
            "account exclusion: {}\n",
            provenant_records::diagnostic::safe_text(exclusion)
        ));
    }
    for exclusion in &preview.offered_exclusions {
        text.push_str(&format!(
            "account exclusion: {}\n",
            provenant_records::diagnostic::safe_text(exclusion)
        ));
    }
    for warning in &route.diagnostics.launch_warnings {
        text.push_str(&format!("{warning}\n"));
    }
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
    if let Ok(model) =
        provenant_routing::ModelId::parse(&format!("{}/{}", route.adapter, route.model))
    {
        if let Some(source) = preview.catalogue_sources.model_source(&model) {
            text.push_str(&format!("catalogue model source={source}\n"));
        }
        if let Some(selection) = &route.pool_selection {
            if let Some(source) = preview
                .catalogue_sources
                .membership_source(&selection.pool, &model)
            {
                text.push_str(&format!("catalogue pool membership source={source}\n"));
            }
            if let Some(source) = preview
                .catalogue_sources
                .weight_source(&selection.pool, &model)
            {
                text.push_str(&format!("catalogue weight source={source}\n"));
            }
        }
    }
    for (name, source) in &preview.catalogue_sources.capability_preferences {
        text.push_str(&format!(
            "catalogue capability preferences {name} source={source}\n"
        ));
    }
    for warning in &preview.overlay_warnings {
        text.push_str(&format!("warning: {warning}\n"));
    }
    for note in &preview.exclusion_notes {
        text.push_str(&format!("note: {note}\n"));
    }
    match &preview.supported_efforts {
        Some(efforts) if efforts.is_empty() => text.push_str("supported efforts: none\n"),
        Some(efforts) => text.push_str(&format!("supported efforts: {}\n", efforts.join(", "))),
        None => {
            text.push_str("warning: model source=user; absent from the bundled and discovered catalogue, effort support is unrecorded\n");
            if !preview.nearest_models.is_empty() {
                text.push_str(&format!(
                    "nearest catalogued IDs: {}\n",
                    preview.nearest_models.join(", ")
                ));
            }
        }
    }
    // The knob travels as a `Sourced` value now, so print its layer with the
    // one shared layer vocabulary instead of dumping the source struct.
    text.push_str(&format!(
        "allow unknown partial headroom: {} (source: {})\n",
        preview.allow_unknown_partial_headroom.value,
        preview.allow_unknown_partial_headroom.source.layer.as_str()
    ));
    if let Some(details) = preview.quota.as_ref().and_then(|q| q.details.as_ref())
        && let (Some(source), Some(coverage)) = (&details.source, &details.coverage)
    {
        text.push_str(&format!("quota source: {source}, coverage: {coverage}\n"));
    }
    text
}
pub(super) fn monthly_list_lines(list: &provenant_routing::RouteList) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "monthly enabled={} burst_percent={} initial_launch_cost_basis_points={} source={} knob={}",
        list.monthly.values.enabled,
        list.monthly.values.burst_percent,
        list.monthly.values.initial_launch_cost_basis_points,
        list.monthly.source,
        list.monthly.knob
    ));
    lines.extend(list.monthly_allowances.iter().map(|row| {
        format!(
            "monthly {} account={} {}",
            row.model.as_str(),
            row.account.as_deref().unwrap_or("unknown"),
            row.assessment.detail
        )
    }));
    lines
}
#[allow(
    clippy::disallowed_methods,
    reason = "the binary edge's output seam writes the command result to stdout and diagnostics to stderr"
)]
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
    let mut result = match now
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
            let diagnostic = projection.as_ref().map(Into::into);
            let repair = projection
                .as_ref()
                .and_then(|diagnostic| diagnostic.next.clone())
                .unwrap_or_else(|| {
                    let next = match code {
                        "IO" => "Inspect the reported I/O error, resolve it, then retry the command.",
                        "QUOTA" => {
                            "Run `provenant quota show --json` to inspect the quota before retrying."
                        }
                        "INVALID" => "Correct the invalid argument and retry the command.",
                        "CONFIG" => "Correct the configuration named in the error, then retry.",
                        _ => "Inspect the reported error, resolve its cause, then retry the command.",
                    };
                    next.to_owned()
                });
            let mut command_error = CommandError::new(code, error.to_string(), None, repair);
            command_error.diagnostic = diagnostic;
            result.error = Some(command_error);
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
                        "{} source={} effort={}",
                        pair.model.as_str(),
                        pair.source,
                        pair.effort.as_deref().unwrap_or("unspecified")
                    )
                }));
                lines.extend(list.models.iter().map(|m| {
                    format!(
                        "{} source={} efforts=[{}]{}",
                        m.model.as_str(),
                        m.source,
                        m.efforts.join(","),
                        unobserved_model(&m.model, list)
                    )
                }));
                lines.extend(list.named_routes.iter().map(|(name, r)| {
                    format!(
                        "route {name} = {} source={}{}",
                        r.model.as_str(),
                        r.source,
                        unobserved_model(&r.model, list)
                    )
                }));
                lines.extend(monthly_list_lines(list));
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
                                "{} source={} weight={} weight_layer={} pacing={:?}{}{} membership_source={} weight_source={}",
                                member.model.as_str(),
                                member.source,
                                member.weight,
                                member.weight_layer.as_str(),
                                member.pacing,
                                member
                                    .effort
                                    .as_ref()
                                    .map_or(String::new(), |effort| format!(" effort={effort}")),
                                unobserved_model(&member.model, list),
                                member.membership_source,
                                member.weight_source,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("route pool {name} = {models}")
                }));
                for (route, source) in &list.catalogue_sources.capability_preferences {
                    lines.push(format!("capability preferences {route} source={source}"));
                }
                for warning in &list.overlay_warnings {
                    lines.push(format!("warning: {warning}"));
                }
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
                ("valid pairs", list.valid_pairs.len()),
                ("models", list.models.len()),
                ("routes", list.named_routes.len()),
                ("pools", list.pools.len()),
                ("pacing entries", list.account_pacing.len()),
                ("overrides", list.active_overrides.len()),
                ("model exclusions", list.model_exclusions.len()),
                ("exclusion notes", list.exclusion_notes.len()),
            ];
            let mut rows: Vec<_> = list
                .valid_pairs
                .iter()
                .map(|pair| {
                    format!(
                        "{} source={} effort={}",
                        pair.model.as_str(),
                        pair.source,
                        pair.effort.as_deref().unwrap_or("unspecified")
                    )
                })
                .collect();
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
                    "route {name} = {} source={}{}",
                    r.model.as_str(),
                    r.source,
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
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    let outcome =
        crate::render_command_result(&mut result, mode, publish, crate::OverflowPolicy::Refuse);
    outcome.map_or(3, |outcome| crate::exit_status(&outcome))
}
