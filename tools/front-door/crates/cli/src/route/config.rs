use super::*;

pub(super) fn list_data(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<Box<provenant_routing::RouteList>, EdgeError> {
    list_data_and_accounts(roots, query).map(|(routes, _)| routes)
}

pub(super) fn list_data_and_accounts(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<
    (
        Box<provenant_routing::RouteList>,
        provenant_providers::accounts::Settings,
    ),
    EdgeError,
> {
    with_context(
        roots,
        REGISTRY,
        &[],
        &[],
        "local-preview",
        &[],
        false,
        None,
        |context, _, _, accounts| {
            Ok((
                Box::new(provenant_routing::list(
                    &provenant_routing::RouteListQuery {
                        adapter: query.adapter.clone(),
                        pool: query.pool.clone(),
                        valid: query.valid,
                    },
                    context,
                )?),
                accounts.clone(),
            ))
        },
    )
}

pub(super) fn display_list_data(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<Box<RouteListDisplay>, EdgeError> {
    let (routes, accounts) = list_data_and_accounts(roots, query)?;
    let catalogue = Catalogue::bundled()?;
    let now = SystemClock::new().unix_seconds();
    let mut models = BTreeSet::new();
    models.extend(routes.models.iter().map(|entry| entry.model.clone()));
    models.extend(routes.valid_pairs.iter().map(|pair| pair.model.clone()));
    models.extend(
        routes
            .named_routes
            .values()
            .map(|route| route.model.clone()),
    );
    models.extend(
        routes
            .pools
            .values()
            .flatten()
            .map(|member| member.model.clone()),
    );
    // Read each account's bounded file once for this display, without probing or writes.
    let files = std::cell::RefCell::new(std::collections::BTreeMap::new());
    let lookup = |scope: &provenant_quota::ReadinessScope| {
        let key = (scope.adapter.clone(), scope.account_key.clone());
        let mut files = files.borrow_mut();
        let file = files.entry(key).or_insert_with(|| {
            crate::quota::recorded_readiness_file(
                roots.state.as_path(),
                &scope.adapter,
                &scope.account_key,
            )
            .map_err(|_| ())
        });
        match file {
            Ok(Some(file)) => file.get(scope, now),
            Ok(None) => provenant_quota::ReadinessRead::absent(),
            Err(()) => provenant_quota::ReadinessRead::unknown(
                provenant_quota::ReadinessReason::InvalidEvidence,
            ),
        }
    };
    let mut recorded_readiness = Vec::new();
    let mut readiness_truncated = false;
    for model in models {
        let rows = recorded_model_readiness(
            &accounts,
            &model,
            &catalogue.admission_pools(&model),
            None,
            None,
            lookup,
        );
        for row in rows {
            if recorded_readiness.len() == 256 {
                readiness_truncated = true;
                break;
            }
            recorded_readiness.push(row);
        }
        if readiness_truncated {
            break;
        }
    }
    Ok(Box::new(RouteListDisplay {
        routes,
        recorded_readiness,
        readiness_truncated,
    }))
}

pub(crate) fn effective_members(
    roots: &Roots,
    pool: &str,
) -> Result<Vec<RoutePoolMember>, EdgeError> {
    let query = RouteListRequest {
        valid: false,
        adapter: None,
        pool: Some(pool.to_owned()),
    };
    let listed = list_data(roots, &query)?;
    let members = listed.pools.get(pool).ok_or(RoutingError::UnknownRoute)?;
    Ok(members
        .iter()
        .map(|member| RoutePoolMember {
            model: member.model.clone(),
            weight: member.weight,
            effort: member.effort.clone(),
            efforts: member.efforts.clone(),
            free: member.free,
        })
        .collect())
}

pub(super) fn write_pool(
    document: &mut toml_edit::DocumentMut,
    pool: &str,
    members: &[RoutePoolMember],
) -> Result<(), &'static str> {
    let routing = routing_table(document)?;
    let pools = routing
        .entry("pools")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let pools = config_table(pools).ok_or("domains.routing.pools must be a table")?;
    let mut values = toml_edit::Array::new();
    for member in members {
        let mut row = toml_edit::InlineTable::new();
        row.insert("model", toml_edit::Value::from(member.model.as_str()));
        row.insert("weight", toml_edit::Value::from(i64::from(member.weight)));
        if let Some(effort) = &member.effort {
            row.insert("effort", toml_edit::Value::from(effort.as_str()));
        }
        let mut efforts = toml_edit::Array::new();
        for effort in &member.efforts {
            efforts.push(effort.as_str());
        }
        row.insert("efforts", toml_edit::Value::Array(efforts));
        row.insert("free", toml_edit::Value::from(member.free));
        values.push(toml_edit::Value::InlineTable(row));
    }
    pools.insert(
        pool,
        toml_edit::Item::Value(toml_edit::Value::Array(values)),
    );
    Ok(())
}

pub(super) fn routing_table(
    document: &mut toml_edit::DocumentMut,
) -> Result<&mut toml_edit::Table, &'static str> {
    let domains = document.entry("domains").or_insert_with(|| {
        let mut domains = toml_edit::Table::new();
        domains.set_implicit(true);
        toml_edit::Item::Table(domains)
    });
    let routing = config_table(domains)
        .ok_or("domains must be a TOML table")?
        .entry("routing")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    config_table(routing).ok_or("domains.routing must be a TOML table")
}

pub(super) fn config_table(item: &mut toml_edit::Item) -> Option<&mut toml_edit::Table> {
    if let toml_edit::Item::Value(toml_edit::Value::InlineTable(table)) = item {
        *item = toml_edit::Item::Table(std::mem::take(table).into_table());
    }
    item.as_table_mut()
}

pub(super) fn edit_config(
    roots: &Roots,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), &'static str>,
) -> Result<(), EdgeError> {
    let _ = provenant_config::edit_config::<crate::domains::Domains>(
        roots,
        &crate::config_reads::sections_for("route")?,
        &provenant_config::EditOptions::new("cli"),
        |document, _| {
            edit(document).map_err(|reason| provenant_config::ConfigError::Edit { reason })
        },
    )?;
    Ok(())
}

pub(super) fn set_pool(roots: &Roots, request: &RouteSet) -> Result<(), EdgeError> {
    let catalogue = Catalogue::bundled()?;
    let model = ModelId::parse(&request.model.0)?;
    let entry = catalogue.model(&model).ok_or(RoutingError::InvalidModel)?;
    if !REGISTRY
        .iter()
        .any(|provider| provider.adapter.id() == model.adapter())
    {
        return Err(RoutingError::UnknownAdapter.into());
    }
    let mut members = match effective_members(roots, &request.pool) {
        Ok(members) => members,
        Err(EdgeError::Routing(RoutingError::UnknownRoute)) => Vec::new(),
        Err(error) => return Err(error),
    };
    let existing = members.iter().position(|member| member.model == model);
    let old = existing.map(|index| &members[index]);
    let efforts = if request.efforts.is_empty() {
        old.map_or_else(|| entry.efforts.clone(), |member| member.efforts.clone())
    } else {
        request.efforts.clone()
    };
    let member = RoutePoolMember {
        model: model.clone(),
        weight: request
            .weight
            .or_else(|| old.map(|member| member.weight))
            .unwrap_or(1),
        effort: request
            .effort
            .clone()
            .or_else(|| old.and_then(|member| member.effort.clone())),
        efforts,
        free: old.map_or_else(|| catalogue.is_free_model(&model), |member| member.free),
    };
    if let Some(index) = existing {
        members[index] = member;
    } else {
        members.push(member);
    }
    edit_config(roots, |document| {
        write_pool(document, &request.pool, &members)
    })
}

pub(super) fn remove_pool_member(roots: &Roots, request: &RouteRemove) -> Result<(), EdgeError> {
    let model = ModelId::parse(&request.model.0)?;
    let mut members = effective_members(roots, &request.pool)?;
    let before = members.len();
    members.retain(|member| member.model != model);
    if members.len() == before {
        return Err(RoutingError::UnknownRoute.into());
    }
    edit_config(roots, |document| {
        write_pool(document, &request.pool, &members)
    })
}

pub(super) fn reset_pool(roots: &Roots, request: &RouteReset) -> Result<(), EdgeError> {
    let catalogue = Catalogue::bundled()?;
    if !catalogue.has_pool(&request.pool) {
        return Err(RoutingError::UnknownRoute.into());
    }
    edit_config(roots, |document| {
        if let Some(domains) = document.get_mut("domains").and_then(config_table)
            && let Some(routing) = domains.get_mut("routing").and_then(config_table)
            && let Some(pools) = routing.get_mut("pools").and_then(config_table)
        {
            pools.remove(&request.pool);
            if pools.is_empty() {
                routing.remove("pools");
            }
        }
        Ok(())
    })
}

/// Refuse a provider the registry does not know before anything is written.
pub(super) fn known_provider(id: &str) -> Result<(), EdgeError> {
    if REGISTRY.iter().any(|provider| provider.adapter.id() == id) {
        Ok(())
    } else {
        Err(RoutingError::UnknownAdapter.into())
    }
}

/// Record a provider's default pacing policy and describe what was recorded.
pub(super) fn set_pacing(roots: &Roots, request: &RoutePacing) -> Result<String, EdgeError> {
    let policy = policy_name(request.policy.into());
    known_provider(&request.provider)?;
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let pacing = routing
            .entry("pacing")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let pacing = config_table(pacing).ok_or("domains.routing.pacing must be a table")?;
        pacing.insert(&request.provider, toml_edit::value(policy));
        Ok(())
    })?;
    Ok(format!("provider {} pacing {policy}", request.provider))
}

/// An RFC 3339 timestamp, or a positive duration (s, m, h, d or w) from `now`.
pub(super) fn instant(value: &str, now: WallTime) -> Option<WallTime> {
    if let Ok(at) = WallTime::parse_rfc3339(value) {
        return Some(at);
    }
    let seconds =
        provenant_providers::duration::suffix_seconds(value.strip_prefix('+').unwrap_or(value))?;
    if seconds == 0 {
        return None;
    }
    now.checked_add(provenant_core::DurationMicros::from_seconds(seconds).ok()?)
        .ok()
}

/// Take the recorded pacing override rows as inline tables, whichever TOML
/// array form the file used.
pub(super) fn take_override_rows(
    routing: &mut toml_edit::Table,
) -> Result<Vec<toml_edit::InlineTable>, &'static str> {
    match routing.remove("pacing_overrides") {
        None => Ok(Vec::new()),
        Some(toml_edit::Item::Value(toml_edit::Value::Array(values))) => values
            .into_iter()
            .map(|value| match value {
                toml_edit::Value::InlineTable(row) => Ok(row),
                _ => Err("domains.routing.pacing_overrides must be an array of tables"),
            })
            .collect(),
        Some(toml_edit::Item::ArrayOfTables(tables)) => Ok(tables
            .into_iter()
            .map(toml_edit::Table::into_inline_table)
            .collect()),
        _ => Err("domains.routing.pacing_overrides must be an array of tables"),
    }
}

/// Rewrite the pacing overrides, removing each row `remove` selects and every
/// expired one, then appending `append`. Returns the removed rows.
pub(super) fn rewrite_overrides(
    roots: &Roots,
    now: WallTime,
    remove: impl Fn(&provenant_routing::PacingOverride) -> bool,
    append: Option<&provenant_routing::PacingOverride>,
) -> Result<Vec<provenant_routing::PacingOverride>, EdgeError> {
    let mut removed = Vec::new();
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let mut kept = toml_edit::Array::new();
        for row in take_override_rows(routing)? {
            let decoded = toml_edit::de::from_document::<provenant_routing::PacingOverride>(
                toml_edit::DocumentMut::from(row.clone().into_table()),
            )
            .ok();
            match decoded {
                Some(decoded)
                    if remove(&decoded)
                        || decoded.schedule.expires.is_some_and(|end| end <= now) =>
                {
                    removed.push(decoded);
                }
                _ => kept.push(row),
            }
        }
        if let Some(row) = append {
            kept.push(override_value(row));
        }
        if !kept.is_empty() {
            routing.insert(
                "pacing_overrides",
                toml_edit::Item::Value(toml_edit::Value::Array(kept)),
            );
        }
        Ok(())
    })?;
    Ok(removed)
}

/// Whether a recorded row has the scope given by a resolved account and a
/// provider. Recorded account names resolve the same way as the request's.
pub(super) fn same_scope(
    row: &provenant_routing::PacingOverride,
    account: Option<&str>,
    provider: Option<&str>,
    settings: &provenant_providers::accounts::Settings,
) -> bool {
    let recorded = row.account.as_deref().map(|name| {
        resolve_account_reference(name, "account", settings).unwrap_or_else(|_| name.to_owned())
    });
    recorded.as_deref() == account && row.provider.as_deref() == provider
}

pub(super) fn set_model_choice(
    roots: &Roots,
    request: &OverrideSet,
    model: &Model,
    now: WallTime,
) -> Result<Data, EdgeError> {
    let risk = request
        .risk_tier
        .as_deref()
        .ok_or(EdgeError::Pacing("--risk-tier is required"))?;
    let task = request
        .task_kind
        .as_deref()
        .ok_or(EdgeError::Pacing("--task-kind is required"))?;
    let expires = request
        .until
        .as_deref()
        .and_then(|value| instant(value, now))
        .filter(|end| *end > now)
        .ok_or(EdgeError::Pacing("--until must be a future expiry"))?;
    let row = provenant_routing::ModelChoiceOverride {
        dimension: "model".into(),
        risk_tier: risk.into(),
        task_kind: task.into(),
        preferred: ModelId::parse(&model.0)?,
        expires: Some(expires),
    };
    let mut validation = provenant_routing::RoutingConfig::default();
    validation.model_choices.push(row.clone());
    if !provenant_config::DomainConfig::validate(&validation).is_empty() {
        return Err(EdgeError::Pacing("invalid risk tier or task kind"));
    }
    let mut removed = Vec::new();
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let rows = match routing.remove("model_choices") {
            None => Vec::new(),
            Some(toml_edit::Item::Value(toml_edit::Value::Array(values))) => values
                .into_iter()
                .map(|v| match v {
                    toml_edit::Value::InlineTable(t) => Ok(t),
                    _ => Err("model_choices must be an array of tables"),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(toml_edit::Item::ArrayOfTables(values)) => values
                .into_iter()
                .map(toml_edit::Table::into_inline_table)
                .collect(),
            _ => return Err("model_choices must be an array of tables"),
        };
        let mut kept = toml_edit::Array::new();
        for value in rows {
            let decoded = toml_edit::de::from_document::<provenant_routing::ModelChoiceOverride>(
                toml_edit::DocumentMut::from(value.clone().into_table()),
            )
            .ok();
            if let Some(existing) = decoded
                && existing.dimension == row.dimension
                && existing.risk_tier == row.risk_tier
                && existing.task_kind == row.task_kind
            {
                removed.push(existing);
            } else {
                kept.push(value);
            }
        }
        let mut value = toml_edit::InlineTable::new();
        value.insert("dimension", row.dimension.as_str().into());
        value.insert("risk_tier", row.risk_tier.as_str().into());
        value.insert("task_kind", row.task_kind.as_str().into());
        value.insert("preferred", row.preferred.as_str().into());
        value.insert("expires", expires.to_string().into());
        kept.push(value);
        routing.insert("model_choices", toml_edit::Item::Value(kept.into()));
        Ok(())
    })?;
    Ok(Data::Overrides(Box::new(OverrideReport {
        notes: exclusion_notes(roots)?,
        recorded: Some(OverrideRow::model_choice(&row, now)),
        cleared: removed
            .iter()
            .map(|item| OverrideRow::model_choice(item, now))
            .collect(),
        overrides: list_overrides(roots, now)?,
    })))
}

pub(super) fn set_override(
    roots: &Roots,
    request: &OverrideSet,
    now: WallTime,
) -> Result<Data, EdgeError> {
    if let Some(model) = &request.model {
        return set_model_choice(roots, request, model, now);
    }
    if !request.all && request.account.is_none() && request.provider.is_none() {
        return Err(EdgeError::Pacing("a pacing scope is required"));
    }
    if let Some(provider) = &request.provider {
        known_provider(provider)?;
    }
    let starts = request
        .starts
        .as_deref()
        .map(|value| {
            instant(value, now).ok_or(EdgeError::Pacing(
                "--starts must be an RFC 3339 timestamp or positive duration",
            ))
        })
        .transpose()?;
    let expires = request
        .until
        .as_deref()
        .map(|value| {
            instant(value, now)
                .filter(|end| *end > now)
                .ok_or(EdgeError::Pacing(
                    "--until must be a future RFC 3339 timestamp or positive duration",
                ))
        })
        .transpose()?;
    let windows = request
        .window
        .iter()
        .map(|spec| parse_window(spec))
        .collect::<Option<Vec<_>>>()
        .ok_or(EdgeError::Pacing(
            "--window must be [DAYS@]START-END, such as mon-fri@9-17 or 23-2",
        ))?;
    let schedule = provenant_routing::Schedule {
        starts,
        expires,
        tz: request.tz.clone(),
        windows,
    };
    if !schedule.valid() {
        return Err(EdgeError::Pacing(
            "invalid schedule: --starts must precede --until, --tz must be an IANA time zone, windows need distinct start and end hours from 0 to 24, and at most 16 windows",
        ));
    }
    let settings = crate::config_reads::read_for(roots, "route")?
        .domains
        .accounts;
    let account = request
        .account
        .as_deref()
        .map(|name| resolve_account_reference(name, "--account", &settings))
        .transpose()?;
    let recorded = provenant_routing::PacingOverride {
        account,
        provider: request.provider.clone(),
        policy: request
            .policy
            .ok_or(EdgeError::Pacing("--policy is required"))?
            .into(),
        schedule,
    };
    let removed = rewrite_overrides(
        roots,
        now,
        |row| {
            same_scope(
                row,
                recorded.account.as_deref(),
                recorded.provider.as_deref(),
                &settings,
            )
        },
        Some(&recorded),
    )?;
    Ok(Data::Overrides(Box::new(OverrideReport {
        notes: exclusion_notes(roots)?,
        recorded: Some(OverrideRow::pacing(&recorded, now)),
        cleared: removed
            .iter()
            .map(|row| OverrideRow::pacing(row, now))
            .collect(),
        overrides: list_overrides(roots, now)?,
    })))
}

pub(super) fn clear_model_choice(
    roots: &Roots,
    risk: &str,
    task: &str,
    now: WallTime,
) -> Result<Data, EdgeError> {
    let mut removed = Vec::new();
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let rows = match routing.remove("model_choices") {
            None => Vec::new(),
            Some(toml_edit::Item::Value(toml_edit::Value::Array(values))) => values
                .into_iter()
                .map(|v| match v {
                    toml_edit::Value::InlineTable(t) => Ok(t),
                    _ => Err("model_choices must be an array of tables"),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(toml_edit::Item::ArrayOfTables(values)) => values
                .into_iter()
                .map(toml_edit::Table::into_inline_table)
                .collect(),
            _ => return Err("model_choices must be an array of tables"),
        };
        let mut kept = toml_edit::Array::new();
        for value in rows {
            let decoded = toml_edit::de::from_document::<provenant_routing::ModelChoiceOverride>(
                toml_edit::DocumentMut::from(value.clone().into_table()),
            )
            .ok();
            if let Some(existing) = decoded
                && existing.dimension == "model"
                && existing.risk_tier == risk
                && existing.task_kind == task
            {
                removed.push(existing);
            } else {
                kept.push(value);
            }
        }
        if !kept.is_empty() {
            routing.insert("model_choices", toml_edit::Item::Value(kept.into()));
        }
        Ok(())
    })?;
    Ok(Data::Overrides(Box::new(OverrideReport {
        notes: exclusion_notes(roots)?,
        recorded: None,
        cleared: removed
            .iter()
            .map(|row| OverrideRow::model_choice(row, now))
            .collect(),
        overrides: list_overrides(roots, now)?,
    })))
}

pub(super) fn clear_override(
    roots: &Roots,
    request: &OverrideClear,
    now: WallTime,
) -> Result<Data, EdgeError> {
    if let (Some(risk), Some(task)) = (&request.risk_tier, &request.task_kind) {
        return clear_model_choice(roots, risk, task, now);
    }
    let settings = crate::config_reads::read_for(roots, "route")?
        .domains
        .accounts;
    let account = request
        .account
        .as_deref()
        .map(|name| resolve_account_reference(name, "--account", &settings))
        .transpose()?;
    let every = account.is_none() && request.provider.is_none();
    let removed = rewrite_overrides(
        roots,
        now,
        |row| {
            every
                || same_scope(
                    row,
                    account.as_deref(),
                    request.provider.as_deref(),
                    &settings,
                )
        },
        None,
    )?;
    Ok(Data::Overrides(Box::new(OverrideReport {
        notes: exclusion_notes(roots)?,
        recorded: None,
        cleared: removed
            .iter()
            .map(|row| OverrideRow::pacing(row, now))
            .collect(),
        overrides: list_overrides(roots, now)?,
    })))
}

/// What was repaired or ignored in the persisted model exclusions.
pub(super) fn exclusion_notes(roots: &Roots) -> Result<Vec<String>, EdgeError> {
    Ok(crate::config_reads::read_for(roots, "route")?
        .domains
        .routing
        .model_exclusions
        .notes()
        .to_vec())
}
/// Every recorded pacing override, then every scheduled model policy rule.
pub(super) fn list_overrides(roots: &Roots, now: WallTime) -> Result<Vec<OverrideRow>, EdgeError> {
    let config = crate::config_reads::read_for(roots, "route")?
        .domains
        .routing;
    let mut overrides: Vec<OverrideRow> = config
        .pacing_overrides
        .iter()
        .map(|row| OverrideRow::pacing(row, now))
        .collect();
    overrides.extend(
        config
            .model_choices
            .iter()
            .map(|row| OverrideRow::model_choice(row, now)),
    );
    overrides.extend(
        config
            .model_exclusions
            .iter()
            .map(|row| OverrideRow::model_exclusion(row, now)),
    );
    for (index, row) in config.model_policy.rules.iter().enumerate() {
        let scope = [
            row.adapter.as_deref().map(|v| format!("adapter {v}")),
            row.model.as_ref().map(|v| format!("model {}", v.as_str())),
            row.account.as_deref().map(|v| format!("account {v}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        overrides.push(OverrideRow {
            kind: "model_policy",
            scope: if scope.is_empty() {
                "all routes".into()
            } else {
                scope
            },
            value: format!("rules[{index}]"),
            schedule: row.schedule.clone(),
            active: row.schedule.active(now),
            remaining_seconds: row.schedule.remaining(now),
        });
    }
    Ok(overrides)
}

pub(super) fn edit_model_exclusion(
    roots: &Roots,
    model: &Model,
    until: Option<WallTime>,
    add: bool,
    now: WallTime,
) -> Result<(), EdgeError> {
    let id = ModelId::parse(&model.0)?;
    if add && Catalogue::bundled()?.model(&id).is_none() {
        return Err(RoutingError::InvalidModel.into());
    }
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let rows = match routing.remove("model_exclusions") {
            None => Vec::new(),
            Some(toml_edit::Item::Value(toml_edit::Value::Array(values))) => values
                .into_iter()
                .map(|row| match row {
                    toml_edit::Value::InlineTable(row) => Ok(row),
                    _ => Err("domains.routing.model_exclusions must be an array of tables"),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(toml_edit::Item::ArrayOfTables(rows)) => rows
                .into_iter()
                .map(toml_edit::Table::into_inline_table)
                .collect(),
            _ => return Err("domains.routing.model_exclusions must be an array of tables"),
        };
        let mut kept = toml_edit::Array::new();
        for row in rows {
            // Adding also drops the rows that have expired, so lapsed
            // exclusions cannot fill the limit. A row whose expiry is not
            // readable is kept.
            let lapsed = add
                && row
                    .get("schedule")
                    .and_then(toml_edit::Value::as_inline_table)
                    .and_then(|schedule| schedule.get("expires"))
                    .and_then(toml_edit::Value::as_str)
                    .and_then(|text| WallTime::parse_rfc3339(text).ok())
                    .is_some_and(|end| end <= now);
            if !lapsed && row.get("model").and_then(toml_edit::Value::as_str) != Some(id.as_str()) {
                kept.push(row);
            }
        }
        if add {
            let mut row = toml_edit::InlineTable::new();
            row.insert("model", id.as_str().into());
            if let Some(until) = until {
                let mut schedule = toml_edit::InlineTable::new();
                schedule.insert("expires", until.to_string().into());
                row.insert("schedule", schedule.into());
            }
            kept.push(row);
        }
        if !kept.is_empty() {
            routing.insert("model_exclusions", toml_edit::Item::Value(kept.into()));
        }
        Ok(())
    })
}

pub(super) fn execute(command: &Route, roots: &Roots, now: WallTime) -> Result<Data, EdgeError> {
    match command {
        Route::List(query) => Ok(Data::List(display_list_data(roots, query)?)),
        Route::Resolve(query) => with_context(
            roots,
            REGISTRY,
            &[],
            &[],
            "local-preview",
            &[],
            false,
            None,
            |context, records, policies, accounts| {
                let context = &RoutingContext {
                    policy_account_hint: query.account.as_deref(),
                    ..*context
                };
                let request = resolve_request(query, context)?;
                let rules = effective_exclusion_rules(&query.request.exclude, roots)?;
                let route = provenant_routing::resolve_for_account_with_seed_excluding(
                    &request,
                    context,
                    None,
                    "route-preview",
                    0,
                    &[],
                    &rules,
                )?;
                let model = ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
                let model_policy = context.config.model_policy.effective(
                    &model,
                    query.account.as_deref().or(route.account_key.as_deref()),
                    context.now,
                );
                provenant_routing::validate_intent(&model_policy.policy, request.intent)?;
                let compaction_break_even_turns = model_policy
                    .policy
                    .compaction
                    .window
                    .and_then(|window| model_policy.policy.break_even_turns(window, 15_000, 0));
                let quota = records
                    .iter()
                    .find(|r| {
                        r.account.adapter == route.adapter
                            && (route.account_key.as_deref()
                                == Some(r.account.account_key.as_str())
                                || (route.account_key.is_none()
                                    && provenant_records::AdapterSpec::get(&route.adapter)
                                        .is_some_and(|spec| spec.allowance_prefix.is_some())
                                    && snapshot_governs(
                                        &route.adapter,
                                        &r.account.account_key,
                                        &model,
                                    )))
                    })
                    .and_then(|r| r.snapshot.clone());
                let login_diagnostic = match &route.readiness {
                    RouteReadiness::Unavailable {
                        reason:
                            reason @ (provenant_records::RouteExclusionReason::AuthRequired
                            | provenant_records::RouteExclusionReason::LoginExpired
                            | provenant_records::RouteExclusionReason::AccountMismatch),
                        ..
                    } => Some(crate::quota::account_login_diagnostic(
                        accounts.entries.values(),
                        records,
                        &route.adapter,
                        route.account_key.as_deref(),
                        reason,
                    )),
                    _ => None,
                };
                let pacing = provenant_routing::pacing_preview(&model, context);
                let mut recorded_readiness = recorded_model_readiness(
                    accounts,
                    &model,
                    &context.catalogue.admission_pools(&model),
                    None,
                    query.account.as_deref(),
                    |scope| {
                        crate::quota::read_readiness(
                            roots.state.as_path(),
                            scope,
                            now.unix_seconds(),
                        )
                    },
                );
                let pools = context.catalogue.admission_pools(&model);
                let quota_evidence = quota_observations(
                    context.catalogue,
                    context.availability,
                    records,
                    policies,
                    std::slice::from_ref(&model),
                )?;
                let admission = provenant_quota::Admission {
                    policies,
                    now_unix_seconds: now.unix_seconds(),
                    records,
                    pools: &pools,
                };
                // Preview has no executable probe, so its resolved route has no
                // selected account. Assess each eligible account's quota as
                // dispatch would after provider availability is established.
                let mut refusals = Vec::new();
                let mut possible = false;
                for row in &mut recorded_readiness {
                    let mut candidate = route.clone();
                    candidate.account_key = Some(row.account_key.clone());
                    candidate.readiness = RouteReadiness::Ready {
                        extra: Default::default(),
                        availability_ref: "preview".into(),
                        expires_unix_seconds: now.unix_seconds().saturating_add(60),
                    };
                    let result = admission.check(&candidate);
                    match &result {
                        Ok(()) => possible = true,
                        Err(error) => refusals.push(format!("{}: {error}", row.account_key)),
                    }
                    if row.observation.reason == Some(provenant_quota::ReadinessReason::NotProbed)
                        && let Some(observation) = quota_evidence.iter().find(|observation| {
                            observation.model == model
                                && observation.snapshot.account.account_key == row.account_key
                                && observation.snapshot.observed_unix_seconds <= now.unix_seconds()
                                && observation.expires_unix_seconds > now.unix_seconds()
                        })
                    {
                        use provenant_quota::{
                            PartialHeadroom, QuotaStatus, ReadinessReason, ReadinessState,
                        };
                        let headroom = provenant_quota::partial_headroom(
                            &observation.snapshot,
                            &route.adapter,
                            &route.model,
                            &pools,
                            now.unix_seconds(),
                        );
                        let verdict = match (observation.snapshot.status, headroom) {
                            (QuotaStatus::AuthRequired, _) => Some((
                                ReadinessState::Unavailable,
                                Some(ReadinessReason::SignedOut),
                            )),
                            (_, PartialHeadroom::Exhausted) => Some((
                                ReadinessState::Unavailable,
                                Some(ReadinessReason::Exhausted),
                            )),
                            (_, PartialHeadroom::Unknown) => Some((
                                ReadinessState::Unknown,
                                Some(ReadinessReason::UncalibratedPartialCoverage),
                            )),
                            (
                                QuotaStatus::Available | QuotaStatus::CreditOnly,
                                PartialHeadroom::Known,
                            ) if result.is_ok() => Some((ReadinessState::Ready, None)),
                            _ => None,
                        };
                        if let Some((state, reason)) = verdict {
                            row.observation = provenant_quota::ReadinessRead {
                                observed_scope: Some(provenant_quota::ReadinessScope {
                                    host: provenant_quota::LOCAL_HOST.into(),
                                    adapter: route.adapter.clone(),
                                    account_key: row.account_key.clone(),
                                    route: model.as_str().into(),
                                }),
                                state,
                                reason,
                                observed_unix_seconds: Some(
                                    observation.snapshot.observed_unix_seconds,
                                ),
                                expires_unix_seconds: Some(observation.expires_unix_seconds),
                                reset_unix_seconds: None,
                                evidence_ref: Some(observation.snapshot.source_ref.clone()),
                            };
                        }
                    }
                }
                // A pool or explicit fallback may try another model during
                // dispatch. One member cannot decide the whole request.
                let admission_refusal = (route.pool_selection.is_none()
                    && !matches!(request.fallback, Fallback::Models(_))
                    && !possible
                    && !refusals.is_empty())
                .then(|| refusals.join("; "));
                let mut provider_refusals = Vec::new();
                for record in records
                    .iter()
                    .filter(|record| record.account.adapter == route.adapter)
                {
                    if let Some(block) = admission.account_block(
                        &route.adapter,
                        &record.account.account_key,
                        &route.model,
                    )? {
                        provider_refusals.push(format!(
                            "{}: {}",
                            record.account.account_key,
                            block.repair()
                        ));
                    }
                }
                let entry = context.catalogue.model(&model);
                let allow_unknown_partial_headroom = context
                    .config
                    .partial_headroom_source
                    .clone()
                    .ok_or(RoutingError::InvalidValue)?;
                Ok(Data::Resolved(Box::new(RoutePreview {
                    allow_unknown_partial_headroom,
                    exclusion_notes: context.config.model_exclusions.notes().to_vec(),
                    model_policy,
                    compaction_break_even_turns,
                    route,
                    supported_efforts: entry.map(|entry| entry.efforts.clone()),
                    nearest_models: if entry.is_none() {
                        context.catalogue.nearest_models(&model)
                    } else {
                        Vec::new()
                    },
                    pacing,
                    recorded_readiness,
                    login_diagnostic,
                    quota,
                    admission_refusal,
                    provider_refusals,
                })))
            },
        ),
        Route::Set(request) => {
            set_pool(roots, request)?;
            Ok(Data::List(display_list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Remove(request) => {
            remove_pool_member(roots, request)?;
            Ok(Data::List(display_list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Reset(request) => {
            reset_pool(roots, request)?;
            Ok(Data::List(display_list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Pacing(request) => {
            let recorded = set_pacing(roots, request)?;
            let list = list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: None,
                },
            )?;
            Ok(Data::Pacing(Box::new(PacingReport {
                recorded,
                active_overrides: list.active_overrides,
            })))
        }
        Route::Override(request) => match &request.command {
            RouteOverrideCommand::Set(set) => set_override(roots, set, now),
            RouteOverrideCommand::List(query) => Ok(Data::Overrides(Box::new(OverrideReport {
                notes: exclusion_notes(roots)?,
                recorded: None,
                cleared: Vec::new(),
                overrides: list_overrides(roots, now)?
                    .into_iter()
                    .filter(|row| {
                        query.all || row.schedule.expires.is_none_or(|expiry| expiry > now)
                    })
                    .collect(),
            }))),
            RouteOverrideCommand::Clear(clear) => clear_override(roots, clear, now),
        },
        Route::Exclude(request) => {
            match &request.command {
                RouteExcludeCommand::Add(add) => {
                    let until = add.until.as_deref().map(|value| instant(value, now).filter(|end| *end > now).ok_or(EdgeError::Pacing("--until must be a future RFC 3339 timestamp or positive duration"))).transpose()?;
                    edit_model_exclusion(roots, &add.model, until, true, now)?;
                }
                RouteExcludeCommand::Remove(remove) => {
                    edit_model_exclusion(roots, &remove.model, None, false, now)?
                }
                RouteExcludeCommand::List => {}
            }
            Ok(Data::Overrides(Box::new(OverrideReport {
                notes: exclusion_notes(roots)?,
                recorded: None,
                cleared: Vec::new(),
                overrides: list_overrides(roots, now)?
                    .into_iter()
                    .filter(|row| row.kind == "model_exclusion")
                    .collect(),
            })))
        }
    }
}
