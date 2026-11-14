use super::*;

/// Best-effort bounded evidence. Store outages affect preference, never admission.
pub(crate) fn selection_evidence(
    roots: &Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
    role: Option<&str>,
) -> provenant_routing::SelectionEvidence {
    use provenant_core::CancelToken;
    use provenant_run as run;
    let mut evidence = provenant_routing::SelectionEvidence {
        role: role.map(str::to_owned),
        source: "run_store:unavailable".into(),
        ..Default::default()
    };
    let read = |evidence: &mut provenant_routing::SelectionEvidence| -> Result<(), String> {
        let identity = crate::fabric::Context::open_read_only(roots.clone(), home, cwd)
            .map_err(|e| e.to_string())?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let mut projects =
            provenant_config::registered_projects(roots).map_err(|e| e.to_string())?;
        if projects.len() > 128 {
            return Err("project evidence bound".into());
        }
        projects.sort_by_key(|p| p.project_id());
        projects.dedup_by_key(|p| p.project_id());
        let mut outcomes = Vec::new();
        for project in projects {
            let context = run::ExecutionContext {
                state_root: identity.roots.state.clone(),
                project_id: project.project_id(),
                actor_id: identity.identity.inbox.actor,
                read_prompts: false,
            };
            let Some(store) =
                run::RunStore::open_read_only(&context, &call).map_err(|e| e.to_string())?
            else {
                continue;
            };
            // Account capacity is host-wide, so count active lanes across projects.
            let mut after = 0;
            for page_index in 0..100 {
                let page = store
                    .lanes(
                        &run::LaneQuery {
                            targets: Vec::new(),
                            all: false,
                            limit: 100,
                            after,
                            newest_first: false,
                            owner: None,
                            unseen_after: None,
                        },
                        &call,
                    )
                    .map_err(|e| e.to_string())?;
                for lane in page.lanes {
                    if matches!(lane.state, run::AttemptState::Terminal { .. }) {
                        continue;
                    }
                    if let Some(route) =
                        lane.applied_route.as_ref().or(lane.resolved_route.as_ref())
                        && let (Some(adapter), Some(account)) =
                            (&route.provider, &route.account_key)
                    {
                        *evidence
                            .live_lanes
                            .entry((adapter.clone(), account.clone()))
                            .or_default() += 1;
                    }
                }
                match page.next {
                    None => break,
                    Some(next) if page_index < 99 => after = next,
                    Some(_) => return Err("live lane evidence bound".into()),
                }
            }
            if project.project_id() != identity.identity.project {
                continue;
            }
            // Priors stay within the caller's project, pool and role. Read only
            // the newest 1,000 retained attempts, without prompts or requests.
            let mut after = 0;
            for _ in 0..10 {
                let page = store
                    .lanes(
                        &run::LaneQuery {
                            targets: Vec::new(),
                            all: true,
                            limit: 100,
                            after,
                            newest_first: true,
                            owner: None,
                            unseen_after: None,
                        },
                        &call,
                    )
                    .map_err(|e| e.to_string())?;
                for lane in page.lanes {
                    if !matches!(lane.state, run::AttemptState::Terminal { .. })
                        || !matches!(
                            lane.terminal_evidence,
                            Some(run::TerminalEvidence::ProcessExit)
                        )
                    {
                        continue;
                    }
                    let pool = lane
                        .resolved_route
                        .as_ref()
                        .and_then(|route| route.pool_selection.as_ref())
                        .filter(|receipt| {
                            provenant_telemetry::route_outcomes::hash_assigned(receipt)
                        })
                        .map(|receipt| receipt.pool.clone());
                    let partial = !lane.partial_signals.is_empty();
                    let fact = run::fact(lane).map_err(|e| e.to_string())?;
                    outcomes.push(provenant_telemetry::route_outcomes::Evidence {
                        fact,
                        pool,
                        partial,
                        access: Some("stored_launch".into()),
                    });
                }
                match page.next {
                    None => break,
                    Some(next) => after = next,
                }
            }
        }
        evidence.priors = provenant_telemetry::route_outcomes::priors(outcomes)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|prior| {
                Some(provenant_routing::RoutePrior {
                    model: ModelId::parse(&prior.model).ok()?,
                    pool: prior.pool,
                    role: prior.role,
                    successes: prior.successes,
                    failures: prior.failures,
                    source: format!(
                        "{}:project/{}:recent/1000",
                        prior.source, identity.identity.project
                    ),
                })
            })
            .collect();
        evidence.live_lanes_known = true;
        evidence.source = "run_store:host_active_lanes;project_recent_1000_outcomes".into();
        Ok(())
    };
    if let Err(error) = read(&mut evidence) {
        evidence.live_lanes_known = false;
        evidence.source = format!("run_store:unknown ({error})");
    }
    evidence
}

pub(crate) fn recorded_model_readiness(
    settings: &provenant_providers::accounts::Settings,
    model: &ModelId,
    pools: &[String],
    selected: Option<&str>,
    hint: Option<&str>,
    lookup: impl Fn(&provenant_quota::ReadinessScope) -> provenant_quota::ReadinessRead,
) -> Vec<RecordedReadiness> {
    let mut seen = BTreeSet::new();
    settings
        .with_derived_accounts()
        .entries
        .into_iter()
        .filter_map(|(id, entry)| {
            if entry.adapter != model.adapter()
                || selected.is_some_and(|key| key != entry.key)
                || hint.is_some_and(|hint| {
                    hint != id
                        && hint != entry.label
                        && hint != entry.key
                        && hint != format!("{}/{}", entry.adapter, entry.key)
                })
                || !seen.insert((entry.adapter.clone(), entry.key.clone()))
                || provenant_records::AdapterSpec::get(&entry.adapter)
                    .is_some_and(|spec| !spec.account_serves_model(&entry.key, model.model()))
            {
                return None;
            }
            let mut scope = provenant_quota::ReadinessScope {
                host: provenant_quota::LOCAL_HOST.into(),
                adapter: entry.adapter,
                account_key: entry.key.clone(),
                route: model.as_str().into(),
            };
            let mut observation = lookup(&scope);
            if observation.state != provenant_quota::ReadinessState::Unavailable {
                for pool in pools {
                    scope.route.clone_from(pool);
                    let pool_observation = lookup(&scope);
                    if pool_observation.state == provenant_quota::ReadinessState::Unavailable {
                        observation = pool_observation;
                        break;
                    }
                }
            }
            Some(RecordedReadiness {
                host: scope.host.clone(),
                account: entry.label,
                account_key: entry.key,
                model: model.as_str().into(),
                observation,
            })
        })
        .collect()
}

/// The OpenCode snapshot reports the Go plan's pools, which only govern models
/// served by the `opencode-go` provider. Free (Zen) and other configured
/// providers must not inherit its account-level exhaustion.
pub(super) fn snapshot_governs(adapter: &str, account: &str, model: &ModelId) -> bool {
    provenant_records::AdapterSpec::get(adapter).is_none_or(|spec| {
        if let Some((free_account, prefix)) = spec.free_account
            && account == free_account
        {
            return model.model().starts_with(prefix);
        }
        spec.quota_model_prefix
            .is_none_or(|prefix| model.model().starts_with(prefix))
    })
}
pub(super) fn quota_observations(
    catalogue: &Catalogue,
    availability: &[AvailabilityRecord],
    records: &[provenant_quota::AccountRecord],
    policies: &[provenant_quota::QuotaPolicy],
    requested_models: &[ModelId],
) -> Result<Vec<QuotaObservation>, EdgeError> {
    let mut observations = Vec::new();
    for record in records {
        let adapter = record.account.adapter.as_str();
        let Some(snapshot) = record.snapshot.as_ref() else {
            continue;
        };
        let Some(ttl) = provenant_quota::schedule::interval(
            policies,
            adapter,
            provenant_quota::schedule::Trigger::Timer,
            provenant_quota::schedule::Power::Ac,
        ) else {
            continue;
        };
        let expires_unix_seconds = snapshot
            .observed_unix_seconds
            .checked_add(i64::try_from(ttl).map_err(|_| provenant_quota::QuotaError::TimeRange)?)
            .ok_or(provenant_quota::QuotaError::TimeRange)?;
        let mut models: BTreeSet<ModelId> = catalogue
            .models()
            .iter()
            .filter(|entry| entry.model.adapter() == adapter)
            .map(|entry| entry.model.clone())
            .collect();
        for observed_model in availability
            .iter()
            .filter(|a| {
                a.adapter == adapter
                    && a.account_key.as_deref() == Some(record.account.account_key.as_str())
            })
            .flat_map(|a| &a.observed_models)
        {
            // Providers may list display names (with spaces) that are not
            // model ids; skip those rather than refusing the whole resolution.
            let Ok(model) = ModelId::parse(&format!("{}/{}", adapter, observed_model)) else {
                continue;
            };
            models.insert(model);
        }
        models.extend(
            requested_models
                .iter()
                .filter(|model| model.adapter() == adapter)
                .cloned(),
        );
        models.retain(|model| snapshot_governs(adapter, &record.account.account_key, model));
        if observations.len().saturating_add(models.len()) > 256 {
            return Err(provenant_quota::QuotaError::Invalid("quota observation bound").into());
        }
        for model in models {
            observations.push(QuotaObservation {
                model,
                snapshot: snapshot.clone(),
                expires_unix_seconds,
            });
        }
    }
    Ok(observations)
}
pub(super) fn resolve_account_reference(
    name: &str,
    field: &str,
    settings: &provenant_providers::accounts::Settings,
) -> Result<String, EdgeError> {
    let derived = settings.with_derived_accounts();
    let valid = derived
        .entries
        .iter()
        .map(|(id, entry)| {
            format!(
                "{} (adapter/key: {}/{}, entry: {})",
                entry.label, entry.adapter, entry.key, id
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let matches_for = |settings: &provenant_providers::accounts::Settings| {
        settings
            .entries
            .iter()
            .filter(|(id, entry)| {
                name == format!("{}/{}", entry.adapter, entry.key)
                    || *id == name
                    || entry.label == name
                    || entry.key == name
            })
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect::<Vec<_>>()
    };
    let mut matches = matches_for(settings);
    if matches.is_empty() {
        matches = matches_for(&derived);
    }
    match matches.as_slice() {
        [(_, entry)] => Ok(format!("{}/{}", entry.adapter, entry.key)),
        [] => Err(EdgeError::AccountProfile {
            kind: AccountProfileError::Unknown,
            message: format!(
                "{field}: unknown account `{name}`; valid accounts: {}",
                if valid.is_empty() {
                    "(none registered)"
                } else {
                    &valid
                }
            ),
        }),
        _ => Err(EdgeError::AccountProfile {
            kind: AccountProfileError::Ambiguous,
            message: format!(
                "{field}: account `{name}` is ambiguous: {}; use adapter/key or a unique label",
                matches
                    .iter()
                    .map(|(id, entry)| format!(
                        "{}/{} (label: {}, entry: {})",
                        entry.adapter, entry.key, entry.label, id
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

// Resolve configured labels once, so all routing paths use the same account keys.
pub(super) fn resolve_pacing_accounts(
    config: &mut provenant_routing::RoutingConfig,
    settings: &provenant_providers::accounts::Settings,
) -> Result<(), EdgeError> {
    let resolve = |name: &str, field: &str| resolve_account_reference(name, field, settings);
    let mut accounts = std::collections::BTreeMap::new();
    for (name, mut profile) in std::mem::take(&mut config.pacing.accounts) {
        let key = resolve(&name, &format!("pacing.accounts.{name}"))?;
        if let Some(provenant_routing::PacingDeadline::ResetOf(reference)) = &mut profile.deadline {
            reference.reset_of = resolve(
                &reference.reset_of,
                &format!("pacing.accounts.{name}.deadline.reset_of"),
            )?;
        }
        if accounts.insert(key.clone(), profile).is_some() {
            return Err(EdgeError::AccountProfile {
                kind: AccountProfileError::Duplicate,
                message: format!("multiple pacing profiles resolve to account key `{key}`"),
            });
        }
    }
    config.pacing.accounts = accounts;
    for (provider, profile) in &mut config.pacing.free {
        if let Some(provenant_routing::PacingDeadline::ResetOf(reference)) = &mut profile.deadline {
            reference.reset_of = resolve(
                &reference.reset_of,
                &format!("pacing.free.{provider}.deadline.reset_of"),
            )?;
        }
    }
    for (index, override_) in config.pacing_overrides.iter_mut().enumerate() {
        if let Some(account) = &mut override_.account {
            *account = resolve(account, &format!("pacing_overrides[{index}].account"))?;
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(super) fn with_context<T>(
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    requested_models: &[ModelId],
    revalidated_auth: bool,
    launch_policy: Option<&provenant_records::policy::Policy>,
    action: impl FnOnce(
        &RoutingContext<'_>,
        &[provenant_quota::AccountRecord],
        &[provenant_quota::QuotaPolicy],
        &provenant_providers::accounts::Settings,
    ) -> Result<T, EdgeError>,
) -> Result<T, EdgeError> {
    let resolved_config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?;
    let partial_headroom_source = resolved_config
        .knobs
        .get("routing.allow_unknown_partial_headroom")
        .cloned()
        .ok_or(RoutingError::InvalidValue)?;
    let allow_unknown = partial_headroom_source
        .value
        .as_bool()
        .ok_or(RoutingError::InvalidValue)?;
    let preference_sources = resolved_config
        .origins
        .iter()
        .filter(|(key, _)| key.starts_with("domains.routing."))
        .map(|(key, origin)| {
            (
                key.clone(),
                match origin {
                    provenant_config::Origin::Compiled { .. } => "default".to_owned(),
                    provenant_config::Origin::User { .. } => "config".to_owned(),
                },
            )
        })
        .collect();
    let domains = resolved_config.domains;
    let policies = provenant_providers::scheduling_policies(&domains.quota, registry);
    let mut config = domains.routing;
    config.preference_sources = preference_sources;
    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::run::preview_project_id(roots, &cwd))
        .map(|project| project.to_string())
        .unwrap_or_default();
    config.launch_policy = launch_policy.cloned().unwrap_or_else(|| {
        domains
            .policy
            .resolve_with_knobs(&project, &resolved_config.knobs, &Default::default())
    });
    config.allow_unknown_partial_headroom = allow_unknown;
    config.partial_headroom_source = Some(partial_headroom_source);
    resolve_pacing_accounts(&mut config, &domains.accounts)?;
    let catalogue = Catalogue::bundled()?;
    // A lane holds no grant on the shared quota store: route without a
    // snapshot there rather than refusing the nested dispatch.
    let mut quota_records = match crate::quota::read_cache(roots) {
        Ok(records) => records,
        Err(crate::quota::EdgeError::Fs(error))
            if error.is_permission_denied()
                && std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() =>
        {
            Vec::new()
        }
        Err(error) => return Err(error.into()),
    };
    // Cached accounts also supply reset_of references outside the probed route.
    // Quota facts never grant executable or account readiness.
    let timestamp = SystemClock::new().wall_now()?;
    let timestamp = timestamp.as_timestamp();
    domains
        .quota
        .apply_calibration(&mut quota_records, timestamp.as_second());
    // Fresh login evidence can replace older sign-out or expiry evidence.
    // Only an account-identifying check can clear an identity mismatch.
    let login_evidence = |adapter: &str, key: &str, now: i64| {
        if !revalidated_auth {
            return None;
        }
        availability
            .iter()
            .filter(|record| {
                record.adapter == adapter
                    && record.account_key.as_deref() == Some(key)
                    && record.observed_unix_seconds <= now
                    && record.expires_unix_seconds > now
            })
            .filter_map(|record| {
                let at = record.observed_unix_seconds;
                let spec = provenant_records::AdapterSpec::get(adapter)?;
                let observed = match spec.login_observation {
                    provenant_records::LoginObservation::AccountAvailability => true,
                    provenant_records::LoginObservation::Reprobe => {
                        record.fixture_ref == format!("installed/{adapter}/login-status")
                    }
                    provenant_records::LoginObservation::ExecutableOnly => {
                        record.fixture_ref == format!("installed/{adapter}/login-unverified")
                    }
                    provenant_records::LoginObservation::None => false,
                };
                observed.then_some(if spec.login_check_names_account {
                    crate::quota::LoginEvidence::Verified(at)
                } else {
                    crate::quota::LoginEvidence::Unverified(at)
                })
            })
            .max_by_key(|probe| probe.observed_at())
    };
    for record in &mut quota_records {
        let probe = login_evidence(
            &record.account.adapter,
            &record.account.account_key,
            timestamp.as_second(),
        );
        if record.last_error == Some(provenant_quota::ProbeFailure::AccountMismatch)
            && matches!(probe, Some(crate::quota::LoginEvidence::Verified(at)) if record.last_attempt_unix_seconds.is_none_or(|attempt| at >= attempt))
        {
            record.last_error = None;
        }
    }
    let quota = quota_observations(
        &catalogue,
        availability,
        &quota_records,
        &policies,
        requested_models,
    )?;
    let capacity_cooldowns: Vec<_> = quota_records
        .iter()
        .flat_map(|record| record.capacity_cooldowns.iter().cloned())
        .collect();
    let now = WallTime::from_micros(timestamp.as_microsecond())?;
    let state = roots.state.as_path();
    let readiness = |scope: &provenant_quota::ReadinessScope, now: i64| match login_evidence(
        &scope.adapter,
        &scope.account_key,
        now,
    ) {
        Some(reprobe) => crate::quota::read_readiness_after_probe(state, scope, now, Some(reprobe)),
        None => crate::quota::read_readiness(state, scope, now),
    };
    action(
        &RoutingContext {
            policy_account_hint: None,
            choice_context: None,
            selection_evidence: None,
            catalogue: &catalogue,
            config: &config,
            registry,
            availability,
            quota: &quota,
            capacity_cooldowns: &capacity_cooldowns,
            accounts: &quota_records,
            quota_policies: &policies,
            readiness: Some(&readiness),
            access,
            now,
            host,
        },
        &quota_records,
        &policies,
        &domains.accounts,
    )
}
/// Resolve one production dispatch against fresh provider facts and validated
/// task access. Account aliases must already have been mapped to registered keys.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DispatchError {
    #[error("no route outside model families {} and family separation is required{}", .0.join(", "), exclusion_suffix(.1))]
    FamilySeparation(Vec<String>, Vec<String>),
    #[error(transparent)]
    Edge(Box<EdgeError>),
    #[error(transparent)]
    Catalogue(#[from] RoutingError),
}

/// The exclusions that took part in a refusal, for its message.
pub(super) fn exclusion_suffix(rules: &[String]) -> String {
    if rules.is_empty() {
        String::new()
    } else {
        format!(" (with {})", rules.join("; "))
    }
}

impl From<EdgeError> for DispatchError {
    fn from(error: EdgeError) -> Self {
        Self::Edge(Box::new(error))
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "fixture"))]
pub(crate) fn resolve_dispatch(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<(&[String], bool, &[String])>,
    rules: &[provenant_routing::ExclusionRule],
) -> Result<provenant_records::ResolvedRoute, DispatchError> {
    resolve_dispatch_rejecting(
        request,
        roots,
        REGISTRY,
        account,
        seed,
        availability,
        access,
        host,
        excluded_families,
        rules,
        &[],
        None,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "fixture"))]
pub(crate) fn resolve_dispatch_rejecting(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<(&[String], bool, &[String])>,
    rules: &[provenant_routing::ExclusionRule],
    rejected: &[ModelId],
    choice_context: Option<(&str, &str)>,
) -> Result<provenant_records::ResolvedRoute, DispatchError> {
    resolve_dispatch_rejecting_family(
        request,
        roots,
        registry,
        account,
        seed,
        availability,
        access,
        host,
        excluded_families,
        rules,
        rejected,
        choice_context,
        None,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_dispatch_rejecting_family(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<(&[String], bool, &[String])>,
    rules: &[provenant_routing::ExclusionRule],
    rejected: &[ModelId],
    choice_context: Option<(&str, &str)>,
    required_family: Option<&str>,
    selection_evidence: Option<&provenant_routing::SelectionEvidence>,
    launch_policy: Option<&provenant_records::policy::Policy>,
) -> Result<provenant_records::ResolvedRoute, DispatchError> {
    let catalogue = Catalogue::bundled()?;
    let (excluded_families, required, preferred) = match excluded_families {
        Some((families, required, preferred)) => (Some(families), required, preferred),
        None => (None, false, &[][..]),
    };
    let excluded: Vec<ModelId> = catalogue
        .models()
        .iter()
        .filter(|model| {
            excluded_families
                .is_some_and(|families| review_family_excluded(model.family.as_deref(), families))
        })
        .map(|model| model.model.clone())
        .collect();
    let preferred_excluded: Vec<ModelId> = catalogue
        .models()
        .iter()
        .filter(|model| review_family_excluded(model.family.as_deref(), preferred))
        .map(|model| model.model.clone())
        .collect();
    let combined: Vec<ModelId> = excluded
        .iter()
        .chain(&preferred_excluded)
        .cloned()
        .collect();
    let with_rejected =
        |members: &[ModelId]| members.iter().chain(rejected).cloned().collect::<Vec<_>>();
    let selected = resolve_dispatch_excluding_family(
        request,
        roots,
        registry,
        account,
        seed,
        availability,
        access,
        host,
        &with_rejected(&combined),
        rules,
        choice_context,
        required_family,
        selection_evidence,
        launch_policy,
    );
    // Relax when the unused families have no eligible member, including fresh
    // snapshot exhaustion, or the selected account is blocked by persisted
    // mismatch or refusal evidence. Stale quota and other availability
    // uncertainty alone do not relax family separation.
    let exhausted = match &selected {
        Err(EdgeError::Routing(error)) => error.code() == "no_eligible_pool_member",
        Ok(route) => {
            !combined.is_empty()
                && matches!(
                    &route.readiness,
                    RouteReadiness::Unavailable { reason, .. } if reason.is_account_block()
                )
        }
        Err(_) => false,
    };
    let selected = if exhausted && !preferred_excluded.is_empty() {
        let relaxed = resolve_dispatch_excluding_family(
            request,
            roots,
            registry,
            account,
            seed,
            availability,
            access,
            host,
            &with_rejected(&excluded),
            rules,
            choice_context,
            required_family,
            selection_evidence,
            launch_policy,
        );
        match (relaxed, selected) {
            // Keep the preferred-stage route when relaxing would only trade one
            // unavailable route for another.
            (Ok(route), Ok(spent)) if !matches!(route.readiness, RouteReadiness::Ready { .. }) => {
                Ok(spent)
            }
            (relaxed, _) => relaxed,
        }
    } else {
        selected
    };
    let exhausted = match &selected {
        Err(EdgeError::Routing(error)) => error.code() == "no_eligible_pool_member",
        Ok(route) => {
            !excluded.is_empty()
                && matches!(&route.readiness, RouteReadiness::Unavailable { reason, .. } if reason.is_account_block())
        }
        Err(_) => false,
    };
    if required && exhausted {
        Err(DispatchError::FamilySeparation(
            excluded_families.unwrap_or_default().to_vec(),
            rules.iter().map(ToString::to_string).collect(),
        ))
    } else if !excluded.is_empty() && exhausted {
        let relaxed = resolve_dispatch_excluding_family(
            request,
            roots,
            registry,
            account,
            seed,
            availability,
            access,
            host,
            rejected,
            rules,
            choice_context,
            required_family,
            selection_evidence,
            launch_policy,
        );
        match (relaxed, selected) {
            // Relaxing must not trade a spent route for another one.
            (Ok(route), Ok(spent)) if !matches!(route.readiness, RouteReadiness::Ready { .. }) => {
                Ok(spent)
            }
            (relaxed, _) => relaxed.map_err(DispatchError::from),
        }
    } else {
        selected.map_err(DispatchError::from)
    }
}

/// Resolve a dispatch using synthetic availability and catalogue fixtures.
#[cfg(feature = "fixture")]
#[allow(clippy::too_many_arguments)]
pub fn fixture_resolve_dispatch(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<(&[String], bool, &[String])>,
    rules: &[provenant_routing::ExclusionRule],
) -> Result<provenant_records::ResolvedRoute, String> {
    resolve_dispatch(
        request,
        roots,
        account,
        seed,
        availability,
        access,
        host,
        excluded_families,
        rules,
    )
    .map_err(|error| error.to_string())
}

pub(super) fn review_family_excluded(family: Option<&str>, families: &[String]) -> bool {
    !families.is_empty() && family.is_none_or(|family| families.iter().any(|used| used == family))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_dispatch_excluding_family(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    registry: &[provenant_providers::Registration],
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    initial_exclusions: &[ModelId],
    rules: &[provenant_routing::ExclusionRule],
    choice_context: Option<(&str, &str)>,
    required_family: Option<&str>,
    selection_evidence: Option<&provenant_routing::SelectionEvidence>,
    launch_policy: Option<&provenant_records::policy::Policy>,
) -> Result<provenant_records::ResolvedRoute, EdgeError> {
    let mut requested_models = Vec::new();
    if let RouteChoice::Model(model) = &request.choice {
        requested_models.push(model.clone());
    }
    if let Fallback::Models(models) = &request.fallback {
        requested_models.extend_from_slice(models.as_slice());
    }
    with_context(
        roots,
        registry,
        availability,
        access,
        host,
        &requested_models,
        true,
        launch_policy,
        |context, records, policies, _| {
            let scoped = RoutingContext {
                choice_context,
                selection_evidence,
                ..*context
            };
            let context = &scoped;
            let mut excluded_members = initial_exclusions.to_vec();
            let mut refusals = Vec::new();
            // Resolution already skips persisted mismatch and refusals, so this
            // narrowing retry now mostly catches admission-only failures (a fresh
            // AuthRequired, an exhausted allowance).
            let mut refused_accounts: Vec<(String, String)> = Vec::new();
            let mut resolved = provenant_routing::resolve_for_account_with_seed_excluding_family(
                request,
                context,
                account,
                seed.0,
                seed.1,
                &excluded_members,
                rules,
                required_family,
            )?;
            loop {
                let model = ModelId::parse(&format!("{}/{}", resolved.adapter, resolved.model))?;
                let pools = context.catalogue.admission_pools(&model);
                let now_unix_seconds = context.now.as_timestamp().as_second();
                let admission = provenant_quota::Admission {
                    policies,
                    now_unix_seconds,
                    records,
                    pools: &pools,
                };
                let gate = if matches!(&resolved.readiness, RouteReadiness::Ready { .. }) {
                    admission.check(&resolved).map_err(EdgeError::from)
                } else {
                    Ok(())
                };
                if let Err(error) = gate {
                    // A spent account leaves the model servable by another
                    // registered account. Unless the caller pinned one, try the
                    // same model there before giving it up.
                    if account.is_none()
                        && let Some(key) = resolved.account_key.clone()
                    {
                        refused_accounts.push((resolved.adapter.clone(), key));
                        let remaining: Vec<_> = context
                            .availability
                            .iter()
                            .filter(|record| {
                                !refused_accounts.iter().any(|(adapter, key)| {
                                    record.adapter == *adapter
                                        && record.account_key.as_deref() == Some(key)
                                })
                            })
                            .cloned()
                            .collect();
                        let narrowed = RoutingContext {
                            availability: &remaining,
                            ..*context
                        };
                        if let Ok(replacement) =
                            provenant_routing::resolve_for_account_with_seed_excluding_family(
                                request,
                                &narrowed,
                                None,
                                seed.0,
                                seed.1,
                                &excluded_members,
                                rules,
                                required_family,
                            )
                            && matches!(&replacement.readiness, RouteReadiness::Ready { .. })
                            && replacement.adapter == resolved.adapter
                            && replacement.model == resolved.model
                            && replacement.account_key != resolved.account_key
                        {
                            resolved = replacement;
                            continue;
                        }
                    }
                    if !matches!(&request.choice, RouteChoice::NamedRoute(_))
                        || resolved.pool_selection.is_none()
                        || excluded_members.contains(&model)
                    {
                        return Err(error);
                    }
                    let reason = error.to_string();
                    refusals.push(reason.clone());
                    excluded_members.push(model);
                    let mut replacement = match admission_replacement(
                        provenant_routing::resolve_for_account_with_seed_excluding_family(
                            request,
                            context,
                            account,
                            seed.0,
                            seed.1,
                            &excluded_members,
                            rules,
                            required_family,
                        ),
                    )? {
                        Some(route) => route,
                        None => {
                            if refusals.len() == 1 {
                                return Err(error);
                            }
                            return Err(EdgeError::Admission(refusals.join("; ")));
                        }
                    };
                    if replacement.adapter == resolved.adapter
                        && replacement.model == resolved.model
                    {
                        return Err(error);
                    }
                    record_pool_redraw(&resolved, &mut replacement, &reason)?;
                    if let Some(redraw) = replacement
                        .pool_selection
                        .as_mut()
                        .and_then(|selection| selection.redraws.last_mut())
                    {
                        let cause = match &error {
                            EdgeError::Quota(provenant_quota::QuotaError::Refused(
                                "allowance exhausted",
                            )) => provenant_records::RouteExclusionReason::Exhausted,
                            EdgeError::Quota(provenant_quota::QuotaError::Refused(
                                "uncalibrated partial coverage",
                            )) => provenant_records::RouteExclusionReason::UnknownPartialHeadroom,
                            _ => provenant_records::RouteExclusionReason::AdmissionRefused,
                        };
                        redraw.extra.insert(
                            "cause".into(),
                            serde_json::to_value(cause).map_err(|_| RoutingError::Catalogue)?,
                        );
                    }
                    resolved = replacement;
                    continue;
                }
                break;
            }
            Ok(resolved)
        },
    )
}

/// Keep an unavailable member for preparation, which can re-probe it.
pub(super) fn admission_replacement(
    result: Result<provenant_records::ResolvedRoute, RoutingError>,
) -> Result<Option<provenant_records::ResolvedRoute>, EdgeError> {
    match result {
        Ok(route) => Ok(Some(route)),
        Err(RoutingError::NoEligiblePoolMember { .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn record_pool_redraw(
    rejected: &provenant_records::ResolvedRoute,
    replacement: &mut provenant_records::ResolvedRoute,
    reason: &str,
) -> Result<(), RoutingError> {
    let previous = rejected
        .pool_selection
        .as_ref()
        .ok_or(RoutingError::Catalogue)?;
    let current = replacement
        .pool_selection
        .as_mut()
        .ok_or(RoutingError::Catalogue)?;
    if previous.pool != current.pool {
        return Err(RoutingError::Catalogue);
    }
    for rejected_candidate in previous
        .candidates
        .iter()
        .filter(|candidate| candidate.exclusion.as_deref() == Some("admission rejected"))
    {
        if let Some(candidate) = current
            .candidates
            .iter_mut()
            .find(|candidate| candidate.model == rejected_candidate.model)
        {
            candidate.exclusion = Some("admission rejected".into());
            candidate.readiness = rejected_candidate.readiness.clone();
        } else {
            current.candidates.push(rejected_candidate.clone());
        }
    }
    if let Some(previous_candidate) = previous
        .candidates
        .iter()
        .find(|candidate| candidate.model == previous.pick)
    {
        if let Some(candidate) = current
            .candidates
            .iter_mut()
            .find(|candidate| candidate.model == previous_candidate.model)
        {
            candidate.exclusion = Some("admission rejected".into());
            candidate.readiness = previous_candidate.readiness.clone();
        } else {
            let mut candidate = previous_candidate.clone();
            candidate.exclusion = Some("admission rejected".into());
            current.candidates.push(candidate);
        }
    }
    let mut redraws = previous.redraws.clone();
    redraws.push(provenant_records::RouteSelectionRedraw {
        extra: Default::default(),
        rejected_pick: previous.pick.clone(),
        roll: previous.roll,
        total_weight: previous.total_weight,
        reason: reason.into(),
    });
    current.redraws = redraws;
    Ok(())
}
/// The account records and polling policies every account-block assessment
/// reads: resolution, the final admission recheck and settlement alternates.
/// Calibrated at `now` and dated by the configured polling intervals, so the
/// three agree on what a refusal's clearing reading and freshness mean.
pub(crate) fn account_evidence(
    settings: &provenant_quota::Settings,
    mut records: Vec<provenant_quota::AccountRecord>,
    now: i64,
    registry: &[provenant_providers::Registration],
) -> (
    Vec<provenant_quota::AccountRecord>,
    Vec<provenant_quota::QuotaPolicy>,
) {
    settings.apply_calibration(&mut records, now);
    (
        records,
        provenant_providers::scheduling_policies(settings, registry),
    )
}
/// Recheck each selected fallback against the latest persisted refusals.
pub(crate) fn check_dispatch_candidate(
    roots: &Roots,
    route: &provenant_records::ResolvedRoute,
    registry: &[provenant_providers::Registration],
) -> Result<(), String> {
    let catalogue = Catalogue::bundled().map_err(|e| e.to_string())?;
    let model =
        ModelId::parse(&format!("{}/{}", route.adapter, route.model)).map_err(|e| e.to_string())?;
    let resolved_config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?;
    let domains = resolved_config.domains;
    let pools = catalogue.admission_pools(&model);
    let now = SystemClock::new().unix_seconds();
    let records = crate::quota::read_cache(roots).map_err(|e| e.to_string())?;
    let (records, policies) = account_evidence(&domains.quota, records, now, registry);
    provenant_quota::Admission {
        policies: &policies,
        now_unix_seconds: now,
        records: &records,
        pools: &pools,
    }
    .check(route)
    .map_err(|e| e.to_string())
}

/// Check a review slot against the same pool and account policy used by dispatch.
/// Preview has no launch grants; unknown availability remains unknown until launch.
pub fn resolve_review_family(
    roots: &Roots,
    tier: &serde_json::Value,
    required_family: Option<&str>,
    exclude: &[String],
) -> Result<String, String> {
    let (_, lowered) = crate::fabric::graph_relay::tier_route("review", tier, None, true)?;
    let routes: Vec<_> = lowered
        .as_array()
        .map_or_else(|| vec![&lowered], |items| items.iter().collect());
    let mut refusals = Vec::new();
    for route in routes {
        let parsed = (|| -> Result<_, String> {
            let choice = if let Some(name) = route.get("alias").and_then(serde_json::Value::as_str)
            {
                RouteChoice::NamedRoute(RouteName::parse(name).map_err(|error| error.to_string())?)
            } else {
                let adapter = route
                    .get("adapter")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing route adapter")?;
                let model = route
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing route model")?;
                RouteChoice::Model(
                    ModelId::parse(&format!("{adapter}/{model}"))
                        .map_err(|error| error.to_string())?,
                )
            };
            let request = provenant_routing::RouteRequest {
                intent: provenant_routing::RouteIntent {
                    workload: provenant_routing::Workload::Review,
                    ..Default::default()
                },
                choice,
                effort: route
                    .get("effort")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                confidential: false,
                fallback: Fallback::None,
            };
            with_context(
                roots,
                REGISTRY,
                &[],
                &[],
                "local-preview",
                &[],
                false,
                None,
                |context, records, policies, settings| {
                    let rules = effective_exclusion_rules(exclude, roots)?;
                    // Preview has no availability probes. Account exclusions
                    // are evaluated against registered account keys below,
                    // rather than treating an empty probe set as no alternatives.
                    let route_rules: Vec<_> = rules.iter().filter(|rule| !matches!(rule.selector, provenant_routing::ExclusionSelector::Account { .. })).cloned().collect();
                    let mut excluded = Vec::new();
                    let mut refusals = Vec::new();
                    loop {
                        let selected =
                            provenant_routing::resolve_for_account_with_seed_excluding_family(
                                &request,
                                context,
                                route.get("account").and_then(serde_json::Value::as_str),
                                "review-family-preview",
                                0,
                                &excluded,
                                &route_rules,
                                required_family,
                            )
                            .map_err(|error| {
                                if refusals.is_empty() {
                                    EdgeError::from(error)
                                } else {
                                    EdgeError::Admission(format!(
                                        "{}; {error}",
                                        refusals.join("; ")
                                    ))
                                }
                            })?;
                        let model =
                            ModelId::parse(&format!("{}/{}", selected.adapter, selected.model))?;
                        if excluded.contains(&model) {
                            return Err(EdgeError::Admission(refusals.join("; ")));
                        }
                        let pools = context.catalogue.admission_pools(&model);
                        let now = context.now.as_timestamp().as_second();
                        let admission = provenant_quota::Admission {
                            policies,
                            now_unix_seconds: now,
                            records,
                            pools: &pools,
                        };
                        let selected_account = route.get("account").and_then(serde_json::Value::as_str);
                        let recorded = recorded_model_readiness(settings, &model, &pools, selected_account, None, |scope| {
                            context.readiness.map_or_else(provenant_quota::ReadinessRead::absent, |lookup| lookup(scope, now))
                        });
                        let keys: BTreeSet<_> = recorded.iter().map(|read| read.account_key.as_str())
                            .chain(records.iter().filter(|record| record.account.adapter == selected.adapter).map(|record| record.account.account_key.as_str()))
                            .filter(|key| selected_account.is_none_or(|selected| selected == *key))
                            .collect();
                        let mut accounts = Vec::new();
                        for key in keys {
                            if matches!(request.choice, RouteChoice::NamedRoute(_))
                                && let Some(rule) = rules.iter().find(|rule| matches!(&rule.selector, provenant_routing::ExclusionSelector::Account { adapter, key: excluded } if adapter == &selected.adapter && excluded == key))
                            {
                                accounts.push(rule.to_string());
                                continue;
                            }
                            if let Some(read) = recorded.iter().find(|read| read.account_key == key) {
                                let observation = &read.observation;
                                let current = observation.observed_scope.is_some()
                                    && observation.observed_unix_seconds.is_some_and(|at| at <= now)
                                    && observation.expires_unix_seconds.is_some_and(|expiry| expiry > now);
                                if observation.state == provenant_quota::ReadinessState::Unavailable
                                    || (current && matches!(observation.reason,
                                        Some(provenant_quota::ReadinessReason::SignedOut | provenant_quota::ReadinessReason::LoginExpired | provenant_quota::ReadinessReason::AccountMismatch)))
                                {
                                    accounts.push(format!("account {key}: {:?}", observation.reason));
                                    continue;
                                }
                            }
                            let mut probe = selected.clone();
                            probe.account_key = Some(key.to_owned());
                            probe.readiness = RouteReadiness::Ready {
                                extra: Default::default(), availability_ref: "preview".into(), expires_unix_seconds: now + 60,
                            };
                            let spent = records.iter().any(|record| record.account.adapter == selected.adapter && record.account.account_key == key && super::output::spent(record, now));
                            match admission.check(&probe) {
                                Ok(()) if !spent => { accounts.clear(); break; }
                                Ok(()) => accounts.push(format!("account {key}: allowance exhausted")),
                                Err(error) => accounts.push(format!("account {key}: {error}")),
                            }
                        }
                        if accounts.is_empty() {
                            return provenant_records::AdapterSpec::get(&selected.adapter)
                                .map(|adapter| adapter.family.to_owned())
                                .ok_or_else(|| RoutingError::UnknownAdapter.into());
                        }
                        refusals.push(format!("{}: {}", model.as_str(), accounts.join("; ")));
                        excluded.push(model);
                    }
                },
            )
            .map_err(|error| error.to_string())
        })();
        match parsed {
            Ok(family) => return Ok(family),
            Err(reason) => refusals.push(reason),
        }
    }
    Err(refusals.join("; "))
}

/// What the depletion ladder reads for a stopped attempt. Readiness comes from
/// the availability the stopped attempt's preparation probed; every adapter
/// there is offered under the stopped task's authority digest, because the
/// ladder only proposes a route and preparation establishes confinement again
/// before any launch. `tried` are the node's other attempts.
pub fn depletion_facts(
    roots: &Roots,
    stopped: &provenant_records::run::AttemptRecord,
    tried: &[provenant_records::run::AttemptRecord],
    availability: &[AvailabilityRecord],
) -> Result<provenant_fabric_runtime::graph::DepletionFacts, String> {
    use provenant_routing::{HostNativeRoute, Stopped, Target};
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let resolved = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|error| text(&error))?;
    let catalogue = Catalogue::bundled().map_err(|error| text(&error))?;
    let route = stopped
        .applied_route
        .as_ref()
        .ok_or("the stopped attempt has no applied route")?;
    let model = ModelId::parse(
        route
            .model
            .as_deref()
            .ok_or("the stopped route has no model")?,
    )
    .map_err(|error| text(&error))?;
    let stopped_route = Stopped {
        family: catalogue
            .model(&model)
            .and_then(|entry| entry.family.clone()),
        model: model.clone(),
        account: route.account_key.clone(),
        effort: route.effort.clone(),
        tier: route
            .pool_selection
            .as_ref()
            .map(|selection| selection.pool.clone()),
    };
    let tried = std::iter::once(stopped)
        .chain(tried)
        .filter_map(|record| record.applied_route.as_ref())
        .filter_map(|route| match route.extra.get("host_native") {
            Some(name) => Some(Target::HostNative {
                name: name.as_str()?.to_owned(),
            }),
            None => Some(Target::Route {
                model: ModelId::parse(route.model.as_deref()?).ok()?,
                account: route.account_key.clone(),
            }),
        })
        .collect();
    let host_native = resolved
        .domains
        .fabric
        .host_native
        .iter()
        .map(|(name, native)| match native {
            provenant_fabric_runtime::config::HostNativeConfig::ClaudeAgent { model, .. } => {
                HostNativeRoute {
                    name: name.clone(),
                    model: ModelId::parse(model)
                        .or_else(|_| ModelId::parse(&format!("claude/{model}")))
                        .ok(),
                }
            }
        })
        .collect();
    let profile = &stopped.profile;
    let intent = provenant_routing::RouteIntent {
        workload: if matches!(
            stopped.requested_route,
            provenant_records::run::RequestedRoute::Review { .. }
        ) {
            provenant_routing::Workload::Review
        } else {
            match profile
                .knobs
                .get("routing.workload")
                .and_then(serde_json::Value::as_str)
            {
                Some("read_heavy") => provenant_routing::Workload::ReadHeavy,
                Some("edit_heavy") => provenant_routing::Workload::EditHeavy,
                Some("review") => provenant_routing::Workload::Review,
                _ => provenant_routing::Workload::General,
            }
        },
        writes: !stopped.write_grants.is_empty(),
        network: None,
        image_input: false,
        skill_location: profile.require_skill_location,
    };
    let exclusions = task_exclusion_rules(profile, roots).map_err(|error| text(&error))?;
    let digest = stopped.authority_digest.to_string();
    let adapters: BTreeSet<&str> = availability
        .iter()
        .map(|record| record.adapter.as_str())
        .collect();
    let access: Vec<_> = adapters
        .into_iter()
        .map(|adapter| AccessContext {
            adapter,
            disclosure_ref: &digest,
            confinement_ref: &digest,
        })
        .collect();
    let host = provenant_config::bind_host(roots)
        .map_err(|error| text(&error))?
        .to_string();
    let (readiness, now) = with_context(
        roots,
        REGISTRY,
        availability,
        &access,
        &host,
        std::slice::from_ref(&model),
        true,
        None,
        |context, _, _, _| {
            Ok((
                provenant_routing::depletion_facts(context, intent, std::slice::from_ref(&model))?,
                context.now,
            ))
        },
    )
    .map_err(|error| text(&error))?;
    Ok(provenant_fabric_runtime::graph::DepletionFacts {
        wait_bound_seconds: provenant_routing::depletion_wait_bound(&resolved.knobs),
        config: resolved.domains.routing,
        catalogue,
        stopped: stopped_route,
        readiness,
        tried,
        host_native,
        exclusions,
        intent,
        confidential: profile
            .extra
            .get("confidential")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        now,
    })
}
