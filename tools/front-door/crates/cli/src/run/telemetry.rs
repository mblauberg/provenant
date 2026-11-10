use super::*;

/// Usage and cost over this project's terminal attempts, priced from the
/// reviewed table shipped with the product.
pub fn stats(command: Stats, edge: &Edge<'_>) -> Reply {
    reply("telemetry stats", stats_inner(command, edge, None))
}

/// Stats grouped by the host's merged pull request snapshot and contribution store.
pub fn stats_by_pr(
    command: Stats,
    edge: &Edge<'_>,
    merged_prs: Vec<provenant_run::PullRequest>,
) -> Reply {
    reply(
        "telemetry stats",
        stats_inner(command, edge, Some(merged_prs)),
    )
}

pub(super) fn cache_kpi_rows(
    facts: &[&provenant_records::AttemptFact],
    domains: &crate::domains::Domains,
    now: provenant_core::WallTime,
) -> Value {
    let mut grouped: BTreeMap<(Option<String>, Option<String>), Vec<&provenant_records::Usage>> =
        BTreeMap::new();
    let share = |usage: &provenant_records::Usage| {
        provenant_routing::cache_read_share(
            usage.uncached_input,
            usage.cache_read,
            usage.cache_write,
        )
    };
    let lanes: Vec<_> = facts.iter().map(|fact| {
        let route = fact.applied_route.as_ref();
        let model = route.and_then(|route| route.model.clone());
        let account = route.and_then(|route| route.account_label.clone());
        grouped.entry((model.clone(), account.clone())).or_default().push(&fact.usage);
        json!({"attempt_id": fact.attempt_id, "model": model, "account": account, "share": share(&fact.usage)})
    }).collect();
    let routes: Vec<_> = grouped.into_iter().map(|((model, account), usages)| {
        let sum = |select: fn(&provenant_records::Usage) -> Option<u64>| usages.iter().try_fold(0u64, |sum, usage| sum.checked_add(select(usage)?));
        let target = model.as_deref().and_then(|model| ModelId::parse(model).ok()).and_then(|model| {
            let key = account.as_deref().and_then(|label| domains.accounts.entries.values().find(|entry| entry.adapter == model.adapter() && (entry.label == label || entry.key == label))).map(|entry| entry.key.as_str()).or(account.as_deref());
            domains.routing.model_policy.effective(&model, key, now).policy.cache.read_share_target
        });
        json!({"model": model, "account": account, "share": provenant_routing::cache_read_share(sum(|u| u.uncached_input), sum(|u| u.cache_read), sum(|u| u.cache_write)), "target": target})
    }).collect();
    json!({"scope": "project terminal attempts", "routes": routes, "lanes": lanes})
}

/// Attempts still queued or running in the project, or in one run. The window
/// cannot count them: they have no finish time yet.
pub(super) fn unfinished_attempts(
    store: Option<&run::RunStore>,
    run_id: Option<RunId>,
    call: &run::Request<'_>,
) -> Result<u64, Fail> {
    let Some(store) = store else {
        return Ok(0);
    };
    let mut count = 0;
    let mut after = 0;
    loop {
        let page = store.lanes(
            &run::LaneQuery {
                targets: run_id
                    .map(|run| {
                        vec![run::LaneTarget {
                            run,
                            task: None,
                            attempt: None,
                        }]
                    })
                    .unwrap_or_default(),
                all: false,
                limit: 100,
                after,
                newest_first: false,
                owner: None,
                unseen_after: None,
            },
            call,
        )?;
        // A run selection also returns its terminal lanes, so filter on state.
        count += page
            .lanes
            .iter()
            .filter(|lane| !matches!(lane.state, run::AttemptState::Terminal { .. }))
            .count() as u64;
        match page.next {
            Some(next) => after = next,
            None => return Ok(count),
        }
    }
}

/// Names the project the figures cover, and what the window leaves out.
pub(super) fn stats_scope_line(project: provenant_core::ProjectId, unfinished: u64) -> String {
    format!(
        "scope: project {project}, finished attempts only ({unfinished} queued or running not counted); other projects are excluded, so run stats from their checkout\n"
    )
}

/// Reasoning tokens get a line only where a provider reported them.
pub(super) fn reasoning_line(totals: &provenant_telemetry::Totals) -> Option<String> {
    let zero = provenant_telemetry::TokenTotal::default();
    (totals.usage.reasoning.known > 0).then(|| {
        format!(
            "reasoning tokens: {}\n",
            grouped_token_count(&totals.usage.reasoning, &zero, &zero)
        )
    })
}

pub(super) fn stats_inner(
    command: Stats,
    edge: &Edge<'_>,
    merged_prs: Option<Vec<provenant_run::PullRequest>>,
) -> Result<(Value, String), Fail> {
    use run::AttemptReader;
    let path = edge.assets.join("config/telemetry-prices.toml");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        Fail::new(
            Status::Error,
            ErrorKind::Prices,
            format!("{}: {error}", path.display()),
        )
    })?;
    let prices = toml_edit::de::from_str::<provenant_telemetry::ConfiguredPrices>(&text)
        .map_err(|error| error.to_string())
        .and_then(|config| {
            provenant_telemetry::validate_prices(&config).map_err(|error| error.to_string())
        })
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Prices, error))?;
    if let Some(path) = command.claude_subagents.as_deref() {
        let native =
            provenant_telemetry::claude_subagents(path, &prices).map_err(|error| match error {
                provenant_telemetry::NativeError::Input(message) => {
                    Fail::new(Status::Invalid, ErrorKind::Invalid, message)
                }
                provenant_telemetry::NativeError::Io(message) => {
                    Fail::new(Status::Error, ErrorKind::Io, message)
                }
            })?;
        let mut human = format!("{} native Claude subagents\n", native.agents);
        if native.skipped_incomplete_lines > 0 {
            human.push_str(&format!(
                "{} incomplete final lines skipped\n",
                native.skipped_incomplete_lines
            ));
        }
        for row in &native.models {
            let cost = row.cost_usd.as_ref().map_or_else(
                || "unknown".to_owned(),
                |cost| format!("est. API-equivalent ${}", cost.usd),
            );
            let billing = match row.price_table_billing {
                Some(provenant_telemetry::BillingKind::Subscription) => {
                    " (price-table subscription model)"
                }
                _ => "",
            };
            human.push_str(&format!(
                "  {}: {} agents, {} messages, cost {}{}\n",
                plain_text(&row.model, usize::MAX),
                row.agents,
                row.messages,
                cost,
                billing
            ));
        }
        let mut data = json!({ "native_claude_subagents": native });
        attach_freshness(&prices, &mut data, &mut human)?;
        return Ok((data, human));
    }
    let instant = |time: Option<jiff::Timestamp>| {
        time.map(|t| WallTime::from_micros(t.as_microsecond()))
            .transpose()
            .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let (context, store) = open_read_only(edge, &call)?;
    let selected_run = command
        .run_id
        .as_ref()
        .map(|target| targets::run_id(edge, target))
        .transpose()?;
    let window = provenant_telemetry::Window {
        since: instant(command.since)?,
        until: instant(command.until)?,
        project: Some(context.project_id),
        detail: command.detail,
    };
    let facts = store
        .as_ref()
        .map(|store| {
            store
                .attempts(
                    run::AttemptFilter {
                        run_id: selected_run,
                        task_id: None,
                    },
                    &call,
                )?
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let unfinished = unfinished_attempts(store.as_ref(), selected_run, &call)?;
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::config(Status::Invalid, error))?;
    let selected: Vec<_> = facts
        .iter()
        .filter(|fact| {
            window.since.is_none_or(|s| fact.finished_at >= s)
                && window.until.is_none_or(|u| fact.finished_at < u)
        })
        .collect();
    let graph_quality = graph_quality_rows(
        &selected,
        store.as_ref(),
        &call,
        &edge.roots.state,
        context.project_id,
    )?;
    let partial = partial_signal_counts(&selected, store.as_ref(), &call);
    let mut route_evidence = Vec::new();
    let mut requests = BTreeMap::new();
    for fact in &selected {
        let record = store
            .as_ref()
            .and_then(|store| store.record(fact.attempt_id, &call).ok());
        let access = record.as_ref().and_then(|record| {
            let store = store.as_ref()?;
            let request = requests
                .entry(record.run_id)
                .or_insert_with(|| store.request(record.run_id, &call).ok());
            let task = request
                .as_ref()?
                .tasks
                .iter()
                .find(|task| task.task_id == fact.task_id)?;
            Some(
                if task.writer.is_some() {
                    "worktree-write"
                } else if !task.access.write_roots.is_empty() {
                    "scratch-write"
                } else {
                    "read-only"
                }
                .to_owned(),
            )
        });
        let pool = record
            .as_ref()
            .and_then(|record| record.resolved_route.as_ref())
            .and_then(|route| route.pool_selection.as_ref())
            .filter(|selection| provenant_telemetry::route_outcomes::hash_assigned(selection))
            .map(|selection| selection.pool.clone());
        route_evidence.push(provenant_telemetry::route_outcomes::Evidence {
            fact: (**fact).clone(),
            access,
            pool,
            // Missing evidence cannot certify a complete success.
            partial: record
                .as_ref()
                .is_none_or(|record| !record.partial_signals.is_empty()),
        });
    }
    let route_outcomes = provenant_telemetry::route_outcomes::report(route_evidence, &prices)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
    let mut chair_bindings = BTreeMap::new();
    // This is the explicit real-time deadline of the read-only Fabric boundary.
    #[allow(clippy::disallowed_methods)]
    let chair_index = provenant_fabric_runtime::chairs::ChairIndex::open_read_only(
        &edge.roots.state,
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    );
    if let Ok(Some(index)) = chair_index {
        let index_call = provenant_fabric_runtime::chairs::Request {
            clock: call.clock,
            cancel: call.cancel,
        };
        let runs: std::collections::BTreeSet<_> =
            selected.iter().filter_map(|fact| fact.trace.run).collect();
        for run in runs {
            if let Ok(Some(binding)) = index.read(context.project_id, run, &index_call)
                && let Some(session) = binding.chair.and_then(|chair| chair.session)
            {
                chair_bindings.insert(run, session);
            }
        }
    }
    let attempt_context = provenant_telemetry::context::report(&selected, &chair_bindings, &prices)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
    let task_context = super::telemetry_context::report(&selected, &graph_quality, edge, &prices)?;
    let decision_ids = command.detail.then(|| {
        selected
            .iter()
            .map(|fact| fact.attempt_id)
            .collect::<Vec<_>>()
    });
    let cache_kpis = cache_kpi_rows(
        &selected,
        &config.domains,
        call.clock.wall_now().map_err(|_| {
            provenant_run::RunError::Admission(provenant_run::AdmissionError::Clock)
        })?,
    );
    let pipeline = if command.outcomes || command.suggest {
        Some(pipeline_report(
            &selected,
            &prices,
            &command.pipeline,
            edge,
            context.project_id,
        )?)
    } else {
        None
    };
    // Chair sessions span projects, so the host's whole window is read. They are
    // best effort: an unreadable usage log must not hide the lane figures.
    let chair_window = (
        window
            .since
            .map_or(jiff::Timestamp::MIN, |t| t.as_timestamp()),
        window
            .until
            .map_or(jiff::Timestamp::MAX, |t| t.as_timestamp()),
    );
    let (chairs, chair_error) = match crate::quota::chair_sessions_with_usage(
        edge.roots,
        Some(edge.home),
        chair_window.0,
        chair_window.1,
    ) {
        Ok(chairs) => (chairs, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let by = command.by;
    let mut unknown_pr_weights = 0u64;
    let mut merged_pr_count = 0u64;
    let external_labels = if by == Some(crate::StatsBy::Pr) {
        let merged_prs = merged_prs.ok_or_else(|| {
            Fail::new(
                Status::Invalid,
                ErrorKind::Unsupported,
                "--by pr requires merged PR state",
            )
        })?;
        merged_pr_count = merged_prs.len() as u64;
        let contributions =
            provenant_run::Contributions::open_read_only(edge.roots.state.as_path(), &call)?;
        let mut labels =
            BTreeMap::<provenant_core::AttemptId, Vec<provenant_telemetry::ExternalGroup>>::new();
        let mut contribution_count = 0u64;
        if let Some(contributions) = contributions {
            for pr in merged_prs {
                for contribution in contributions.for_pr(&pr, &call)? {
                    contribution_count += 1;
                    if contribution_count > provenant_telemetry::MAX_ATTEMPTS {
                        return Err(Fail::new(
                            Status::Invalid,
                            ErrorKind::Invalid,
                            "PR contributions exceed 100,000 rows; narrow the time window",
                        ));
                    }
                    if contribution.weight_millionths.is_none() {
                        unknown_pr_weights += 1;
                    }
                    labels.entry(contribution.attempt).or_default().push(
                        provenant_telemetry::ExternalGroup {
                            value: format!("{}#{}", pr.repository, pr.number),
                            weight_millionths: contribution.weight_millionths,
                        },
                    );
                }
            }
        }
        Some(labels)
    } else {
        None
    };
    let by_name = match by {
        Some(crate::StatsBy::Route) => "route",
        Some(crate::StatsBy::Pr) => "pr",
        Some(crate::StatsBy::Issue) => "issue",
        Some(crate::StatsBy::Graph) => "graph",
        Some(crate::StatsBy::Node) => "node",
        Some(crate::StatsBy::Pattern) => "pattern",
        Some(crate::StatsBy::Preset) => "preset",
        Some(crate::StatsBy::Role) => "role",
        None => "all",
    };
    let report = if by == Some(crate::StatsBy::Pr) {
        let labels = external_labels.as_ref().ok_or_else(|| {
            Fail::new(
                Status::Invalid,
                ErrorKind::Unsupported,
                "missing PR contribution labels",
            )
        })?;
        provenant_telemetry::stats_by_external_key(facts, &prices, window, labels)
    } else if let Some(by) = by {
        let key = match by {
            crate::StatsBy::Route => provenant_telemetry::ContextKey::Route,
            crate::StatsBy::Issue => provenant_telemetry::ContextKey::Issue,
            crate::StatsBy::Graph => provenant_telemetry::ContextKey::GraphId,
            crate::StatsBy::Node => provenant_telemetry::ContextKey::NodeId,
            crate::StatsBy::Pattern => provenant_telemetry::ContextKey::Pattern,
            crate::StatsBy::Preset => provenant_telemetry::ContextKey::Preset,
            crate::StatsBy::Role => provenant_telemetry::ContextKey::Role,
            crate::StatsBy::Pr => unreachable!("handled above"),
        };
        provenant_telemetry::stats_by_context(facts, &prices, window, key)
    } else {
        provenant_telemetry::stats_with_chair(facts, chairs, &prices, window)
    }
    .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
    let mut human = stats_scope_line(context.project_id, unfinished);
    human.push_str(&stats_summary(&report));
    if let Some(line) = reasoning_line(&report.totals) {
        human.push_str(&line);
    }
    if let Some(groups) = &report.context_groups {
        human.push_str(&format!("by {by_name}: {} groups\n", groups.len()));
        for group in groups {
            let label = group.value.as_deref().unwrap_or("unattributed");
            let input = grouped_token_count(
                &group.totals.usage.uncached_input,
                &group.totals.usage.cache_read,
                &group.totals.usage.cache_write,
            );
            let output = grouped_token_count(
                &group.totals.usage.output_excluding_reasoning,
                &group.totals.usage.reasoning,
                &provenant_telemetry::TokenTotal::default(),
            );
            human.push_str(&format!(
                "{}  input={input} output={output}  {}\n",
                plain_text(label, 256),
                cost_summary(&group.totals).trim()
            ));
        }
        human.push_str("next: inspect group totals with --json\n");
    }
    for graph in &graph_quality {
        if graph["checkpoint"] == "read" {
            human.push_str(&format!(
                "graph {}: verdicts={} findings={} initial_rounds={} repair_rounds={} partial_signals={}\n",
                graph["graph_id"].as_str().unwrap_or("unknown"),
                graph["verdicts"],
                graph["findings"].as_u64().unwrap_or(0),
                graph["initial_rounds"].as_u64().unwrap_or(0),
                graph["repair_rounds"].as_u64().unwrap_or(0),
                graph["partial_signals"]
            ));
        }
    }
    let total_share = totals_cache_read_share(&report.totals);
    human.push_str(&format!(
        "cache-read share: {}\n",
        total_share.map_or_else(
            || "unknown (incomplete input categories)".into(),
            |share| format!("{:.1}%", share * 100.0)
        )
    ));
    let mut data = serde_json::to_value(&report)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    data["attempt_context"] = json!(attempt_context);
    data["task_context"] = task_context;
    human.push_str("context coverage: inspect attempt_context and task_context with --json; missing observations remain unknown\n");
    human.push_str("comparability: release, harness experiments, task difficulty and user activity cannot be separated when observations are missing or change together\n");
    human.push_str(&format!(
        "accepted outcomes: {} (distinct delivery IDs with recorded accepted workflow receipts among selected attempts; cost includes linked attempts only)\n",
        data["task_context"]["accepted_outcomes"]["denominator"]
    ));
    match provenant_telemetry::usage::ergonomics(edge.roots, command.since, command.until, None) {
        Ok(ergonomics) => {
            data["cli_ergonomics"] = json!({
                "scope": "host demand log in selected time window; lossy best-effort capture",
                "counts": ergonomics,
            });
        }
        Err(error) => {
            data["cli_ergonomics"] = json!({"unavailable": error.to_string()});
        }
    }
    // Native rows are a host-wide breakdown of chair usage already included
    // above; expose the attribution without adding it to totals a second time.
    if selected_run.is_none() {
        let native = provenant_telemetry::native_attempts::read(edge.roots);
        let mut rows = provenant_telemetry::ingest::read_rows(
            edge.roots,
            &provenant_telemetry::ingest::RowFilter::default(),
        );
        if let Ok(rows) = &mut rows {
            rows.retain(|row| {
                row.timestamp
                    .parse::<jiff::Timestamp>()
                    .ok()
                    .is_some_and(|at| {
                        command.since.is_none_or(|since| at >= since)
                            && command.until.is_none_or(|until| at < until)
                    })
            });
            let thinking = provenant_telemetry::signals::thinking(rows);
            let unknown_reasoning: u64 =
                thinking.values().map(|group| group.reasoning_unknown).sum();
            human.push_str(&format!(
                    "thinking per trigger (host native replies): provider-reported reasoning tokens; {} replies without reasoning counts; output_tokens_proxy is separate\n",
                    unknown_reasoning,
                ));
            for (trigger, group) in &thinking {
                let label = serde_json::to_value(trigger)
                    .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
                human.push_str(&format!(
                        "  {}: {} reasoning tokens, {} known / {} unknown replies; {} output_tokens_proxy\n",
                        label.as_str().unwrap_or("unknown"), group.reasoning_tokens,
                        group.reasoning_known, group.reasoning_unknown, group.output_tokens_proxy,
                    ));
            }
            data["reply_context"] = json!({
                "scope": "host native replies in selected time window; breakdown only, never added to project totals",
                "reasoning_unit": "provider-reported reasoning tokens",
                "proxy_unit": "output tokens (output_tokens_proxy); not reasoning",
                "thinking_per_trigger": thinking,
                "trigger_labels": "inferred labels; recorded structural events remain distinct in ingest rows",
                "compaction_coverage": {"known": rows.iter().filter(|row| row.reply.is_some()).count(), "unknown": rows.iter().filter(|row| row.reply.is_none()).count()},
            });
            // Spend by the context each reply sent: cost is context times
            // turns, so this shows how much is spent at large contexts.
            // Compare windows with --since and --until.
            let bands = provenant_telemetry::context_bands(rows, &prices).map_err(|error| {
                Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string())
            })?;
            human.push_str(
                "spend by context band (host native replies; context = input + cached reads + cache writes):\n",
            );
            for band in &bands {
                let range = match band.below_tokens {
                    Some(below) => format!("{}k-{}k", band.min_tokens / 1000, below / 1000),
                    None => format!("{}k+", band.min_tokens / 1000),
                };
                let source = serde_json::to_value(band.source)
                    .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
                human.push_str(&format!(
                    "  {} {range}: {} replies  {}\n",
                    source.as_str().unwrap_or("unknown"),
                    band.totals.attempts,
                    cost_summary(&band.totals).trim()
                ));
            }
            data["context_bands"] = json!({
                "scope": "host native replies in selected time window; breakdown only, never added to project totals",
                "context": "uncached input + cache reads + cache writes of one reply, in tokens",
                "floors": provenant_telemetry::CONTEXT_BAND_FLOORS,
                "bands": bands,
            });
        }
        match (native, rows) {
            (Ok(attempts), Ok(rows)) => {
                let native = provenant_telemetry::native_attempts::report(attempts, &rows);
                if !native.attempts.is_empty() || !native.unlinked.is_empty() {
                    human.push_str("native sub-agent usage (host; may overlap chair usage):\n");
                    for item in &native.attempts {
                        if item.models.is_empty() {
                            human.push_str(&format!(
                                "  {} {}: no linked messages\n",
                                item.attempt.alias, item.attempt.model
                            ));
                        }
                        for (model, buckets) in &item.models {
                            human.push_str(&format!(
                                "  {} {}: input={} output={}\n",
                                item.attempt.alias,
                                model,
                                buckets[..4].iter().copied().fold(0u64, u64::saturating_add),
                                buckets[4]
                            ));
                        }
                    }
                    for item in &native.unlinked {
                        human.push_str(&format!(
                            "  unlinked {}: {} messages, input={} output={}\n",
                            item.model,
                            item.messages,
                            item.buckets[..4]
                                .iter()
                                .copied()
                                .fold(0u64, u64::saturating_add),
                            item.buckets[4]
                        ));
                    }
                    data["native_subagents"] = json!(native);
                }
            }
            (Err(error), _) => {
                human.push_str(&format!(
                    "native attempt attribution unavailable: {error}\n"
                ));
                data["native_subagents_error"] = json!(error.to_string());
            }
            (_, Err(error)) => {
                human.push_str(&format!("native usage unavailable: {error}\n"));
                data["native_subagents_error"] = json!(error.to_string());
                data["reply_context_error"] = json!(error.to_string());
            }
        }
    }
    if let Some(ids) = decision_ids {
        let mut decisions = Vec::new();
        if let Some(store) = store.as_ref() {
            for id in ids {
                let record = store.record(id, &call)?;
                if let Some(route) = record.resolved_route {
                    decisions.push(route_decision(id, &route));
                }
            }
        }
        human.push_str(&format!(
            "route decisions: {} attempts; inspect receipts with --json\n",
            decisions.len()
        ));
        data["route_decisions"] = json!(decisions);
    }
    data["cache_read_share"] = cache_kpis;
    data["cache_read_share"]["overall"] = json!(total_share);
    let plan_value =
        plan_value_rows(&report, &config.domains.accounts, edge).unwrap_or_else(|cause| {
            data["plan_value_unavailable"] = json!(cause);
            Vec::new()
        });
    if !plan_value.is_empty() {
        human.push_str(&format!("plan value: {} accounts\n", plan_value.len()));
        for row in &plan_value {
            let account = row["account"].as_str().unwrap_or("unknown");
            let price = &row["monthly_price"];
            let amount = price["amount"].as_f64().unwrap_or(0.0);
            let currency = price["currency"].as_str().unwrap_or("USD");
            let value = row["api_equivalent_usd_in_selected_window"]
                .as_str()
                .map_or_else(|| "unknown".to_owned(), |value| format!("${value}"));
            let bound = if row["api_equivalent_complete"] == true {
                ""
            } else {
                " at least"
            };
            human.push_str(&format!(
                "{}: plan {} {:.2} {}/mo; API-equivalent{bound} {value} in selected window\n",
                plain_text(account, 128),
                row["plan"].as_str().unwrap_or("unknown"),
                amount,
                currency
            ));
        }
        human.push_str("next: compare a full billing period with quota history\n");
    }
    data["plan_value"] = json!(plan_value);
    data["graph_quality"] = json!(graph_quality);
    for row in &route_outcomes {
        let number = |value: Option<f64>| {
            value
                .map(|n| format!("{n:.3}"))
                .unwrap_or_else(|| "unknown".into())
        };
        human.push_str(&format!(
            "outcomes {} access={} effort={} n={} successes={} success_rate={} cost/success_microdollars={} time/success_ms={} comparison={} pool={} cost_basis={}\n",
            plain_text(&row.route, 256), row.access, row.effort,
            row.samples, row.successes, number(row.success_rate),
            number(row.cost_per_success_microdollars), number(row.time_per_success_ms),
            row.comparison, row.pool.as_deref().unwrap_or("none"), row.cost_basis,
        ));
    }
    data["route_outcomes"] = json!(route_outcomes);

    // Every grouping reports partial finishes, not only graph groups.
    match &partial {
        Ok(partial) if partial.is_empty() => {}
        Ok(partial) => human.push_str(&format!(
            "partial signals: {}\n",
            partial
                .iter()
                .map(|(name, count)| format!("{name}={count}"))
                .collect::<Vec<_>>()
                .join(" ")
        )),
        Err(cause) => human.push_str(&format!("partial signals: unavailable ({cause})\n")),
    }
    data["partial_signals"] = partial_signals_value(partial);
    data["unfinished_attempts_excluded"] = json!(unfinished);
    if let Some(pipeline) = pipeline {
        human.push_str(&format!(
            "post-merge outcomes: CI {}; {} unattributed rows; {} suggestions\n",
            pipeline["outcomes"]["ci"]
                .as_str()
                .unwrap_or("not_measured"),
            pipeline["outcomes"]["unattributed_rows"],
            pipeline["ranking"]["suggestions"]
                .as_array()
                .map_or(0, Vec::len)
        ));
        human.push_str(
            "suggestions are advisory; inspect counts, intervals and expiry with --json\n",
        );
        data["pipeline"] = pipeline;
    }
    attach_freshness(&prices, &mut data, &mut human)?;
    if by == Some(crate::StatsBy::Pr) {
        data["pr_attribution"] = json!({
            "merged_prs_read": merged_pr_count,
            "unknown_allocation_weights": unknown_pr_weights,
            "allocation": "contribution_weight_millionths",
        });
    }
    if let Some(error) = chair_error {
        human.push_str(&format!(
            "chair sessions not read: {}\n",
            plain_text(&error, usize::MAX)
        ));
        data["chair_error"] = Value::String(error);
    }
    Ok((data, human))
}

/// Read retained facts and checkpoints only. Source findings receive verifier
/// decisions by graph-local finding identity; synthesis copies never add value.
pub(super) fn pipeline_report(
    facts: &[&provenant_records::AttemptFact],
    prices: &provenant_telemetry::PriceTable,
    options: &crate::PipelineOptions,
    edge: &Edge<'_>,
    project: provenant_core::ProjectId,
) -> Result<Value, Fail> {
    use provenant_fabric_runtime::graph::FileJournal;
    use provenant_telemetry::outcome::Evidence;
    let invalid = |message: String| Fail::new(Status::Invalid, ErrorKind::Invalid, message);
    let mut checkpoints = BTreeMap::new();
    let mut unreadable_graphs = std::collections::BTreeSet::new();
    for fact in facts {
        if let Some(graph) = fact.context.as_ref().and_then(|c| c.graph_id.as_ref())
            && !checkpoints.contains_key(graph)
        {
            let checkpoint = match FileJournal::read_materialised_checkpoint(
                &edge.roots.state,
                project,
                graph,
            ) {
                Ok(checkpoint) => checkpoint,
                Err(_) => {
                    unreadable_graphs.insert(graph.clone());
                    None
                }
            };
            checkpoints.insert(graph.clone(), checkpoint);
        }
    }
    let mut nodes =
        BTreeMap::<(Option<String>, String), Vec<provenant_records::AttemptFact>>::new();
    for fact in facts {
        let context = fact.context.as_ref();
        let graph = context.and_then(|c| c.graph_id.clone());
        let node = context
            .filter(|_| graph.is_some())
            .and_then(|c| c.node_id.clone())
            .unwrap_or_else(|| fact.attempt_id.to_string());
        nodes
            .entry((graph, node))
            .or_default()
            .push((*fact).clone());
    }
    let mut evidence = Vec::new();
    for ((graph, node), mut facts) in nodes {
        facts.sort_by_key(|f| (f.finished_at, f.attempt_number));
        let latest = facts
            .last()
            .ok_or_else(|| invalid("empty node evidence".into()))?;
        let context = latest.context.as_ref();
        let extra = |key: &str| {
            context
                .and_then(|c| c.extra.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let checkpoint = graph
            .as_ref()
            .and_then(|g| checkpoints.get(g))
            .and_then(Option::as_ref);
        let pr = context
            .and_then(|c| c.extra.get("pr"))
            .and_then(Value::as_u64)
            .or_else(|| {
                checkpoint
                    .and_then(|c| c.pull_request.as_deref())
                    .and_then(|pr| pr.trim_end_matches('/').rsplit('/').next()?.parse().ok())
            });
        let issue = context
            .and_then(|c| c.work_item.as_deref())
            .and_then(|s| s.rsplit(['#', '/']).next())
            .and_then(|s| s.parse().ok());
        let (confirmed, false_positives) = pipeline_dispositions(checkpoint, &node);
        let mut row = Evidence {
            sample: json!([graph, node]).to_string(),
            graph,
            pr,
            issue,
            pattern: context.and_then(|c| c.pattern.clone()),
            lens: extra("lens").or_else(|| context.and_then(|c| c.role.clone())),
            model: pipeline_node_model(&facts),
            risk_tier: context
                .and_then(|c| c.risk_tier.clone())
                .or_else(|| extra("risk_tier")),
            task_kind: context
                .and_then(|c| c.task_kind.clone())
                .or_else(|| extra("task_kind")),
            cost_microdollars: None,
            confirmed,
            false_positives,
        };
        let report =
            provenant_telemetry::stats(facts, prices, provenant_telemetry::Window::default())
                .map_err(|e| invalid(e.to_string()))?;
        row.cost_microdollars = report.totals.cost.complete.map(|m| m.microdollars);
        evidence.push(row);
    }
    let ci: Option<Vec<provenant_telemetry::outcome::CiRun>> = options
        .ci_file
        .as_ref()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|e| invalid(e.to_string()))?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(invalid("CI file exceeds 16 MiB".into()));
            }
            serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
        })
        .transpose()?;
    let now = SystemClock::new()
        .wall_now()
        .map_err(|e| invalid(e.to_string()))?
        .as_timestamp();
    let mut outcomes = provenant_telemetry::outcome::attribute(
        edge.cwd,
        &options.main_ref,
        evidence,
        ci.as_deref(),
        options.outcome_days,
        now.as_second(),
    )
    .map_err(|e| invalid(e.to_string()))?;
    for row in &mut outcomes.rows {
        if row
            .evidence
            .graph
            .as_ref()
            .is_some_and(|graph| unreadable_graphs.contains(graph))
        {
            row.stratum = "unmeasured";
            row.signals.unmeasured = true;
        }
    }
    let ranking = provenant_telemetry::suggest::rank(
        &outcomes,
        options.min_samples as usize,
        options.expiry_days,
        now,
    )
    .map_err(invalid)?;
    Ok(json!({"outcomes": outcomes, "ranking": ranking}))
}

pub(super) fn pipeline_dispositions(
    checkpoint: Option<&provenant_fabric_runtime::graph::GraphCheckpoint>,
    node: &str,
) -> (Option<u64>, Option<u64>) {
    let mut confirmed = None;
    let mut false_positives = None;
    if let Some(checkpoint) = checkpoint {
        let mut decisions = BTreeMap::<&str, std::collections::BTreeSet<&str>>::new();
        let mut origins = BTreeMap::<&str, std::collections::BTreeSet<&str>>::new();
        for source_node in checkpoint.nodes.iter().filter(|n| n.children.is_empty()) {
            for finding in source_node.outputs.iter().flat_map(|o| &o.findings) {
                if finding.disposition.is_none() {
                    origins
                        .entry(&finding.id)
                        .or_default()
                        .insert(&source_node.id);
                }
            }
        }
        for finding in checkpoint
            .nodes
            .iter()
            .filter(|n| n.children.is_empty())
            .filter_map(|n| n.outputs.as_ref())
            .flat_map(|o| &o.findings)
        {
            if let Some(disposition) = finding.disposition.as_deref() {
                decisions
                    .entry(&finding.id)
                    .or_default()
                    .insert(disposition);
            }
        }
        if let Some(source) = checkpoint
            .nodes
            .iter()
            .find(|n| n.id == node && n.children.is_empty())
            .and_then(|n| n.outputs.as_ref())
        {
            let ids: std::collections::BTreeSet<_> = source
                .findings
                .iter()
                .filter(|f| f.disposition.is_none())
                .map(|f| f.id.as_str())
                .collect();
            let mut c = 0;
            let mut f = 0;
            let mut complete = true;
            for id in ids {
                if origins.get(id).is_none_or(|nodes| nodes.len() != 1) {
                    complete = false;
                    continue;
                }
                match decisions
                    .get(id)
                    .filter(|d| d.len() == 1)
                    .and_then(|d| d.first())
                    .copied()
                {
                    Some("accepted") => c += 1,
                    Some("rejected") => f += 1,
                    _ => complete = false,
                }
            }
            if complete {
                confirmed = Some(c);
                false_positives = Some(f);
            }
        }
    }
    (confirmed, false_positives)
}

pub(super) fn pipeline_node_model(facts: &[provenant_records::AttemptFact]) -> Option<String> {
    let models: Option<std::collections::BTreeSet<_>> = facts
        .iter()
        .map(|fact| {
            fact.applied_route
                .as_ref()
                .or(fact.resolved_route.as_ref())?
                .model
                .clone()
        })
        .collect();
    let models = models?;
    (models.len() == 1)
        .then(|| models.into_iter().next())
        .flatten()
}

/// Project only decision evidence into per-attempt telemetry.
fn route_decision(id: provenant_core::AttemptId, route: &run::RouteRecord) -> Value {
    json!({
        "attempt_id": id,
        "model": route.model,
        "effort": route.effort,
        "account_label": route.account_label,
        "source": route.source,
        "account_source": route.extra.get("account_source"),
        "pool_selection": route.pool_selection,
    })
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn detail_projects_only_route_decision_fields() -> Result<(), Box<dyn std::error::Error>> {
        let mut route = run::RouteRecord {
            model: Some("codex/example".into()),
            effort: Some("high".into()),
            source: Some("pool".into()),
            pool_selection: Some(serde_json::from_value(json!({
                "pool": "fixture", "candidates": [], "roll": 0, "total_weight": 0, "pick": "codex/example"
            }))?),
            ..Default::default()
        };
        route
            .extra
            .insert("unrelated".into(), "fixture-marker".into());
        route.account_key = Some("fixture-account-key".into());
        let value = route_decision(provenant_core::AttemptId::from_bytes([1; 16]), &route);
        assert_eq!(value["pool_selection"]["pool"], "fixture");
        let text = serde_json::to_string(&value)?;
        assert!(!text.contains("fixture-marker"));
        assert!(!text.contains("fixture-account-key"));
        Ok(())
    }
}
