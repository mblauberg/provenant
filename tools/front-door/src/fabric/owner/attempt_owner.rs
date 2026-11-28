use super::*;

impl provenant_fabric_runtime::graph::AttemptOwner for BoundGraphOwner<'_> {
    fn continuation(
        &mut self,
        reference: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::AttemptContinuation>, Self::Error> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        let mut target = AttemptTarget {
            extra: Default::default(),
            host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
        };
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let previous = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        let Some(next) = previous.superseded_by else {
            return Ok(None);
        };
        target.attempt = next;
        let successor = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        if previous.key.next_attempt().as_ref() != Some(&successor.key) {
            return Err(FacadeError::StateCorrupt);
        }
        Ok(Some(provenant_fabric_runtime::graph::AttemptContinuation {
            key: successor.key,
            attempt_ref: format!(
                "{}/{}",
                target.host.unwrap_or(self.factory.context.identity.host),
                next
            ),
        }))
    }

    fn continuation_refused(&mut self, reference: &str) -> Result<bool, Self::Error> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
        };
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let record = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        let refused = |steering: &provenant_records::run::SteeringStatus| {
            steering.state == provenant_records::run::SteeringState::NotContinued
                && steering.cause
                    == Some(provenant_records::run::SteeringCause::ContinuationRefused)
        };
        if record.steering.as_ref().is_some_and(refused) {
            return Ok(true);
        }
        // The graph may already have adopted the successor before the launch
        // receipt was recorded. Its parent owns the steering disposition.
        if record.provider_launched == Some(false)
            && let Some(parent) = record.retry_of
        {
            let parent = owner.snapshot(
                AttemptTarget {
                    attempt: parent,
                    ..target
                },
                self.budget,
                self.cancel,
            )?;
            return Ok(parent.steering.as_ref().is_some_and(|steering| {
                steering.successor == Some(record.attempt_id) && refused(steering)
            }));
        }
        Ok(false)
    }

    fn detect_repair_growth(&mut self, before: &str, after: &str) -> Result<bool, String> {
        let before =
            provenant_core::GitOid::parse(before).map_err(|_| "repair source commit is invalid")?;
        let after =
            provenant_core::GitOid::parse(after).map_err(|_| "repaired commit is invalid")?;
        let base = self
            .integration_base
            .and_then(|bases| bases.review.or(bases.integration))
            .ok_or("cannot assess repair growth without a pinned review base")?;
        let paths = |subject: &provenant_core::GitOid| -> Result<Vec<String>, String> {
            let range = format!("{base}...{subject}");
            let names = self.repair_git(
                &self.factory.cwd,
                &["diff", "--no-renames", "--name-only", "-z", &range, "--"],
            )?;
            Ok(names
                .split('\0')
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect())
        };
        let old = paths(&before)?;
        let new = paths(&after)?;
        let routing: provenant_routing::RoutingConfig =
            provenant_config::read_domain(self.factory.context.roots.state.as_path(), "routing")
                .map_err(
                    |_| "repair review configuration is unavailable; correct routing settings",
                )?;
        let delta = self.repair_git(
            &self.factory.cwd,
            &[
                "diff",
                "--no-renames",
                "--name-only",
                "-z",
                &before.to_string(),
                &after.to_string(),
                "--",
            ],
        )?;
        let touched: Vec<String> = delta
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        Ok(repair_scope_grew(&routing.review, &old, &new)
            || repair_touches_risk(&routing.review, &touched))
    }

    fn resolve_repair_panel(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        subject: &str,
    ) -> Result<provenant_fabric_runtime::graph::RepairPanelResolution, String> {
        let subject = provenant_core::GitOid::parse(subject)
            .map_err(|_| "repair review subject is invalid")?;
        let base = self
            .integration_base
            .and_then(|bases| bases.review.or(bases.integration))
            .ok_or("cannot replan review without a pinned base")?;
        let range = format!("{base}...{subject}");
        let names = self.repair_git(
            &self.factory.cwd,
            &["diff", "--no-renames", "--name-only", "-z", &range, "--"],
        )?;
        let paths: Vec<String> = names
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        let routing: provenant_routing::RoutingConfig =
            provenant_config::read_domain(self.factory.context.roots.state.as_path(), "routing")
                .map_err(
                    |_| "repair review configuration is unavailable; correct routing settings",
                )?;
        let raw = serde_json::to_value(document)
            .map_err(|_| "repair review document could not be encoded")?;
        let mut request = document.review.clone().unwrap_or_default();
        if request.kind.is_none() {
            request.kind = raw["context"]["task_kind"].as_str().map(str::to_owned);
        }
        // Replanning may raise coverage, but must retain the original hard
        // family floors and selected named-lens requirements.
        let declared_panel = routing
            .review
            .resolve_panel(&request, None, &paths, false)?;
        // A growth signal does not carry a new tier. Resolve at the highest
        // tier until the graph carries a measured replacement risk record.
        request.risk_tier = Some("terminal".into());
        request.shape = None;
        request.lenses.clear();
        let mut panel = routing
            .review
            .resolve_panel(&request, None, &paths, false)
            .map_err(
                |_| "repair review panel could not resolve; correct review routing settings",
            )?;
        for (family, count) in declared_panel.minimum_family_counts {
            let minimum = panel.minimum_family_counts.entry(family).or_default();
            *minimum = (*minimum).max(count);
        }
        for mut declared in declared_panel
            .slots
            .into_iter()
            .filter(|slot| slot.required_family.is_some())
        {
            if let Some(slot) = panel
                .slots
                .iter_mut()
                .find(|slot| slot.lens == declared.lens)
            {
                slot.required_family = declared.required_family;
            } else {
                // The raised shape can use a different lens order. Keep an
                // explicitly constrained lens rather than dropping its read.
                declared.distinct_families = false;
                panel
                    .omitted
                    .retain(|omitted| omitted.lens != declared.lens);
                panel.slots.push(declared);
            }
        }
        panel.resolve_families(|slot, family| {
            provenant_cli::route::resolve_review_family(
                &self.factory.context.roots,
                &slot.tier,
                family,
                &document.exclude,
            )
        })?;
        let single = panel.slots.len() == 1;
        let frame = !single && panel.frame;
        let verifiers = if single { 0 } else { panel.verifiers };
        let synthesis = !single && panel.synthesis == provenant_routing::ReviewSynthesis::Model;
        let advice = panel
            .slots
            .iter()
            .map(|slot| (slot.lens.clone(), panel.lens_advice(&slot.lens, true)))
            .collect();
        // Lower repair-panel tiers with the host's effective catalogue and
        // surface every warning; no bundled fallback in product code.
        let catalogue = provenant_cli::route::effective_catalogue(&self.factory.context.roots).ok();
        let slots = panel
            .slots
            .into_iter()
            .map(|slot| {
                let (key, mut route, warnings) = crate::graph_support::tier_route(
                    &slot.lens,
                    &slot.tier,
                    None,
                    true,
                    catalogue.as_ref(),
                )
                .map_err(|_| "repair review route is invalid; correct reviewer tier settings")?;
                for warning in warnings {
                    provenant_records::warning::warn(warning);
                }
                if let Some(family) = slot
                    .satisfied_family
                    .as_ref()
                    .or(slot.required_family.as_ref())
                {
                    if let Some(routes) = route.as_array_mut() {
                        for route in routes {
                            route["required_family"] = serde_json::json!(family);
                        }
                    } else {
                        route["required_family"] = serde_json::json!(family);
                    }
                }
                let mut fields = serde_json::Map::new();
                fields.insert(key.into(), route);
                Ok(provenant_fabric_runtime::graph::RepairPanelSlot {
                    lens: slot.lens,
                    route: fields.into(),
                    family_group: slot
                        .distinct_families
                        .then(|| crate::graph_support::format_family_group(slot.group_index)),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(provenant_fabric_runtime::graph::RepairPanelResolution {
            slots,
            frame,
            verifiers,
            synthesis,
            advice,
        })
    }

    fn input_cap_source(&self) -> provenant_records::Layer {
        self.factory.context.graph_prompt_limit_source
    }

    fn catalogue(&self) -> Option<provenant_routing::Catalogue> {
        provenant_cli::route::effective_catalogue(&self.factory.context.roots).ok()
    }

    fn graph_admission_policy(&self) -> provenant_records::policy::Policy {
        let project = self.factory.context.identity.project.to_string();
        match provenant_cli::config_reads::read_for(&self.factory.context.roots, "run admission") {
            Ok(config) => config.domains.policy.resolve_with_knobs(
                &project,
                &config.knobs,
                &Default::default(),
            ),
            Err(error) => {
                let error = provenant_records::diagnostic::safe_text(&error.to_string());
                warn(
                    Warning::new(
                        provenant_records::warning::CONFIG_UNAVAILABLE,
                        format!(
                            "full config unavailable ({error}); using the owner's resolved policy"
                        ),
                        provenant_records::warning::detected_source(
                            provenant_records::warning::CONFIG_UNAVAILABLE,
                        ),
                    )
                    .subject(project.clone()),
                );
                provenant_config::policy::read(
                    self.factory.context.roots.state.as_path(),
                    &project,
                    &Default::default(),
                )
            }
        }
    }

    fn graph_on_stop_defaults(&self) -> provenant_records::OnStop {
        resolve_on_stop_defaults(
            &self.factory.context.roots,
            &self.factory.context.identity.project.to_string(),
        )
    }

    fn review_policy(&self) -> provenant_fabric_runtime::graph::ReviewPolicy {
        self.factory.context.review_policy.clone()
    }

    fn launch_retry_policy(&self) -> provenant_fabric_runtime::graph::LaunchRetryPolicy {
        let mut policy = provenant_fabric_runtime::graph::LaunchRetryPolicy::default();
        let config = match provenant_cli::config_reads::read_for(&self.factory.context.roots, "run")
        {
            Ok(config) => config,
            Err(error) => {
                warn(Warning::new(
                    provenant_records::warning::CONFIG_UNAVAILABLE,
                    format!("graph launch retry settings unavailable: {error}; using defaults"),
                    provenant_records::warning::detected_source(
                        provenant_records::warning::CONFIG_UNAVAILABLE,
                    ),
                ));
                return policy;
            }
        };
        if let Some(knob) = config.knobs.get("lanes.launch_retry_limit") {
            if let Some(value) = knob.value.as_u64().and_then(|n| u8::try_from(n).ok()) {
                policy.limit = value;
            }
            policy.limit_source = knob.source.layer.as_str().into();
        }
        if let Some(value) = config
            .knobs
            .get("lanes.launch_retry_first_seconds")
            .and_then(|knob| knob.value.as_u64())
        {
            policy.first_seconds = value;
        }
        if let Some(value) = config
            .knobs
            .get("lanes.launch_retry_max_seconds")
            .and_then(|knob| knob.value.as_u64())
        {
            policy.max_seconds = value;
        }
        policy
    }

    fn begin_launch(
        &mut self,
        access: provenant_fabric_runtime::graph::AccessMode,
        deadline: Option<provenant_core::WallTime>,
    ) -> Result<Option<provenant_core::WallTime>, Self::Error> {
        if access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
            self.writer_launch_budget = None;
            return Ok(None);
        }
        let now = self.factory.clock.mono_now();
        let wall = self
            .factory
            .clock
            .wall_now()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let budget = match deadline {
            Some(end) => WaitBudget::up_to(
                now,
                provenant_core::DurationMicros::from_micros(
                    end.as_micros().saturating_sub(wall.as_micros()).max(0) as u64,
                ),
            ),
            None => provenant_cli::fabric::graph_relay::writer_launch_budget(
                &self.factory.context,
                &self.factory.clock,
                self.budget,
            ),
        };
        self.writer_launch_budget = Some(budget);
        Ok(Some(
            deadline.unwrap_or(
                wall.checked_add(budget.remaining(now))
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
        ))
    }

    fn decide_timeout(
        &self,
    ) -> Result<provenant_fabric_runtime::graph::DecideTimeout, Self::Error> {
        Ok(provenant_fabric_runtime::graph::DecideTimeout {
            seconds: self.factory.context.config.graph.decide_timeout_seconds,
            source: self.factory.context.graph_decide_timeout_source,
        })
    }

    fn admission_timeout_seconds(&self) -> Option<u64> {
        if let Some(budget) = self.writer_launch_budget {
            return Some(
                budget
                    .remaining(self.factory.clock.mono_now())
                    .as_micros()
                    .div_ceil(1_000_000),
            );
        }
        let configured = provenant_confinement::run_config::RunConfig::read(
            self.factory.context.roots.state.as_path(),
        )
        .ok()
        .and_then(|config| {
            config.admission_seconds_for(&self.factory.context.identity.project.to_string())
        });
        let effective = provenant_run::AdmissionPolicy::resolve_timeout(None, configured);
        Some(self.admission_timeout.map_or(effective.seconds, |limit| {
            let micros = limit.remaining(self.factory.clock.mono_now()).as_micros();
            effective.seconds.min(micros / 1_000_000)
        }))
    }
    type Error = FacadeError;
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        self.factory.context.port_table.worktree_layout.clone()
    }
    fn resolve_review_base(
        &mut self,
        reference: &str,
        worktree: Option<&str>,
    ) -> Result<Option<String>, Self::Error> {
        let directory = worktree.map_or_else(
            || self.factory.cwd.clone(),
            |path| self.factory.cwd.join(path),
        );
        self.factory
            .resolve_base(&directory, reference, &self.repo_facts, self.cancel)
            .map(|base| Some(base.to_string()))
    }

    fn take_launch_warnings(&mut self) -> Vec<Warning> {
        std::mem::take(&mut self.launch_warnings)
    }

    fn resolve_creation_base(
        &mut self,
        branch: &str,
        reference: &str,
        default_base: bool,
    ) -> Result<Option<String>, Self::Error> {
        // The one forge context for this command, resolved here and passed to
        // both creation paths; neither rediscovers the remote.
        // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
        let state = self.factory.context.roots.state.as_path();
        // Forge metadata lives at the checkout root. The invoking cwd may be a
        // plain subdirectory of that checkout, which has no `.git` of its own;
        // hand the resolved toplevel to every Git-metadata reader. Keep the
        // Git worktree contexts below on the cwd, which resolve subdirectories.
        // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
        let project = self
            .factory
            .graph_checkout()
            .map_err(|_| FacadeError::io("forge context Git metadata"))?;
        let ports = &self.factory.context.port_table;
        let policy = provenant_config::operation_policy(state, &project, ports);
        let config = provenant_config::ForgeConfig::new(ports, (*policy).clone());
        let git = provenant_config::ForgeGit::new(&project)
            .map_err(|_| FacadeError::io("forge context Git metadata"))?;
        let forge_remote = provenant_config::ForgeContext::remote_if_any(&config, &git)
            .map_err(|_| FacadeError::io("forge context resolution"))?;
        // Only a new top-level writer may prefer a remote-only branch. Child
        // provisioning and recovery always use their checkpointed source.
        let remote_tip = provenant_confinement::worktrees::with_context_using_facts(
            &self.factory.context.roots,
            &self.factory.home,
            &self.factory.cwd,
            &self.factory.clock,
            self.cancel,
            &self.repo_facts,
            |context| {
                provenant_confinement::worktrees::fetch_remote_branch_tip(
                    context,
                    branch,
                    self.cancel,
                    forge_remote.as_ref(),
                )
            },
        )
        .map_err(git_facts::repository_fact_error)?;
        if let Some(tip) = remote_tip {
            if !default_base && reference != tip.to_string() {
                let warning = format!(
                    "ignoring --base {reference} for --branch {branch}; using remote tip {tip}"
                );
                let warning = Warning::new(
                    BRANCH_BEHIND_BASE,
                    warning,
                    provenant_records::warning::detected_source(BRANCH_BEHIND_BASE),
                )
                .subject(branch.to_string())
                .source(provenant_records::Source {
                    layer: provenant_records::Layer::Detected,
                    origin: Some(format!("git remote tip {tip}")),
                })
                .repair(format!("use --base {tip}"));
                warn(warning.clone());
                self.launch_warnings.push(warning);
            }
            return Ok(Some(tip.to_string()));
        }
        // Admission resolved `lanes.writer_base` into this intent; only an
        // unresolved default (a peer, or no Git evidence) is re-derived here.
        if default_base && let Ok(admitted) = provenant_core::GitOid::parse(reference) {
            return Ok(Some(admitted.to_string()));
        }
        if default_base {
            let (base, warnings) = provenant_confinement::worktrees::with_context_using_facts(
                &self.factory.context.roots,
                &self.factory.home,
                &self.factory.cwd,
                &self.factory.clock,
                self.cancel,
                &self.repo_facts,
                |context| {
                    provenant_confinement::worktrees::default_creation_base(
                        context,
                        &self.repo_facts,
                        self.cancel,
                        forge_remote.as_ref(),
                    )
                },
            )
            .map_err(git_facts::repository_fact_error)?;
            for warning in warnings {
                let warning = Warning::new(
                    provenant_records::warning::BASE_FALLBACK_ADVISORY,
                    warning,
                    provenant_records::warning::detected_source(
                        provenant_records::warning::BASE_FALLBACK_ADVISORY,
                    ),
                );
                warn(warning.clone());
                self.launch_warnings.push(warning);
            }
            return Ok(Some(base.to_string()));
        }
        self.resolve_review_base(reference, None)
    }

    fn retry_allowance(
        &self,
        node: &provenant_records::ExtraFields,
        graph: &provenant_records::ExtraFields,
    ) -> Result<Option<(u8, String)>, Self::Error> {
        Ok(Some(graph_retry_allowance(
            node,
            graph,
            &self.factory.context.roots,
        )))
    }

    fn preparation_fallback_evidence(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::PreparationFallbackEvidence>, Self::Error>
    {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        // Only this host can verify its writer fences and custody lock.
        if host != self.factory.context.identity.host.to_string() {
            return Ok(None);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        if store
            .attempt_operation(record.attempt_id, &call)
            .map_err(run_error)?
            .operation_id
            != graph_operation("graph-attempt-operation", key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            })
        {
            return Ok(None);
        }
        let custody_released = store.custody_released(&record, &call).map_err(run_error)?;
        let failure = record.preparation_failure.or_else(|| {
            (record.provider_launched == Some(false)
                && matches!(
                    &record.state,
                    provenant_records::run::AttemptState::Terminal {
                        cause: Some(provenant_records::run::CauseKind::SpawnFailure),
                        ..
                    }
                ))
            .then_some(provenant_records::run::PreparationFailure::AdapterUnavailable)
        });
        Ok(failure.map(
            |failure| provenant_fabric_runtime::graph::PreparationFallbackEvidence {
                failure,
                handoff: if record.provider_launched == Some(false) {
                    provenant_routing::FallbackHandoff::Local
                } else {
                    provenant_routing::FallbackHandoff::UncertainOrLaunched
                },
                custody_released,
            },
        ))
    }

    fn quota_alias_redraw(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<bool, Self::Error> {
        use provenant_records::run::{AttemptState, Outcome, RequestedRoute};
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        // Only the owning host can prove terminal custody and the sealed output.
        if host != self.factory.context.identity.host.to_string() {
            return Ok(false);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        if store
            .attempt_operation(record.attempt_id, &call)
            .map_err(run_error)?
            .operation_id
            != graph_operation("graph-attempt-operation", key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            })
            || !matches!(
                record.state,
                AttemptState::Terminal {
                    outcome: Outcome::QuotaLimited,
                    ..
                }
            )
            || !matches!(record.requested_route, RequestedRoute::Named { .. })
            || record
                .applied_route
                .as_ref()
                .and_then(|route| route.model.as_ref())
                .is_none()
            || !store.custody_released(&record, &call).map_err(run_error)?
        {
            return Ok(false);
        }
        // Main 4c52d7f28 deliberately permits partial output and writer commits.
        // The checks above prove identity, typed quota, named alias, an applied
        // member, and released custody; the kernel separately bounds redraws.
        Ok(matches!(
            record.requested_route,
            RequestedRoute::Named { .. }
        ))
    }

    fn depletion_facts(
        &mut self,
        request: provenant_fabric_runtime::graph::DepletionRequest<'_>,
    ) -> Result<Option<provenant_fabric_runtime::graph::DepletionFacts>, Self::Error> {
        let host = self.factory.context.identity.host.to_string();
        let (stopped_host, attempt) = request
            .reference
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        // Only the owning host holds the stopped attempt's probed availability.
        if stopped_host != host {
            return Ok(None);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let id = attempt.parse().map_err(|_| FacadeError::StateCorrupt)?;
        let record = store.record(id, &call).map_err(run_error)?;
        if store
            .attempt_operation(record.attempt_id, &call)
            .map_err(run_error)?
            .operation_id
            != graph_operation("graph-attempt-operation", request.key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(request.key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(request.key.node_id.as_str())
            })
        {
            return Err(FacadeError::StateCorrupt);
        }
        let availability: Vec<provenant_providers::AvailabilityRecord> = store
            .published_availability(id, &call)
            .map_err(run_error)?
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()
            .map_err(|_| FacadeError::StateCorrupt)?
            .unwrap_or_default();
        let mut tried = Vec::new();
        for reference in request.tried {
            if let Some((tried_host, attempt)) = reference.split_once('/')
                && tried_host == host
                && reference != request.reference
            {
                tried.push(
                    store
                        .record(
                            attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                            &call,
                        )
                        .map_err(run_error)?,
                );
            }
        }
        provenant_cli::route::depletion_facts(
            &self.factory.context.roots,
            &record,
            &tried,
            &availability,
        )
        .map(Some)
        .map_err(|reason| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason,
        })
    }

    fn preparation_refusal_cause(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Option<provenant_records::RouteExclusionReason>, Self::Error> {
        Ok(self
            .preparation_skipped_routes(key, reference)?
            .and_then(|skipped| unanimous_preparation_cause(&skipped)))
    }

    fn preparation_skipped_accounts(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Vec<provenant_records::SkippedAccount>, Self::Error> {
        Ok(self
            .preparation_skipped_routes(key, reference)?
            .unwrap_or_default()
            .into_iter()
            .flat_map(|route| route.skipped_accounts)
            .collect())
    }

    fn attempt_models(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        self.prior_models(attempts)
    }

    fn bind_writer_worktree(
        &mut self,
        path: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::WriterWorktreeBinding>, Self::Error> {
        let (directory, _) = selected_writer_project(self.factory, path)?;
        let request = provenant_run::resolve_writer(
            self.factory.context.roots.state.as_path(),
            &directory,
            &directory,
        )
        .map_err(run_error)?;
        let writer = provenant_run::prepare_writer_with_facts(
            &request,
            self.factory.context.roots.state.as_path(),
            self.factory.context.identity.project, &self.repo_facts)
        .map_err(|error| match error {
            provenant_run::RunError::Validation("writer needs a branch") => FacadeError::DispatchRefused {
                fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                disk: None,
                reason: "writer needs a branch".into(),
            },
            other => run_error(other),
        })?;
        let worktree = writer
            .worktree
            .path()
            .to_str()
            .ok_or_else(|| FacadeError::field("worktree"))?
            .to_owned();
        let identity = writer.worktree.identity();
        Ok(Some(
            provenant_fabric_runtime::graph::WriterWorktreeBinding {
                extra: Default::default(),
                shared_worktree: None,
                source_directory_identity: Some(provenant_records::run::SourceIdentity {
                    device: identity.device,
                    inode: identity.inode,
                }),
                worktree,
                head: writer.source_commit().to_string(),
            },
        ))
    }

    fn provision_writer_worktree(
        &mut self,
        path: &str,
        branch: &str,
        base: &str,
        owner: Option<provenant_core::Digest>,
        carry_request: Option<&provenant_fabric_runtime::graph::WriterCarryRequest<'_>>,
    ) -> Result<Option<provenant_fabric_runtime::graph::WriterWorktreeBinding>, Self::Error> {
        let target = self.factory.context.project_root.join(path);
        let recovered = match carry_request {
            Some(request) => super::carry::recover(self, &target, request, base)?,
            None => None,
        };
        let snapshot = match (carry_request, &recovered) {
            (Some(request), None) => super::carry::capture(self, request, base)?,
            _ => None,
        };
        let carry_examined = recovered.is_some() || snapshot.is_some();
        let provision_base = recovered.as_ref().map_or(base, |(head, _)| head.as_str());
        let provisioned = provenant_cli::fabric::graph_relay::provision_writer_checked(
            &self.factory.context,
            path,
            branch,
            provision_base,
            owner.as_ref(),
            self.writer_launch_budget.unwrap_or(self.budget),
            self.cancel,
        )
        .map_err(|error| run_error(error.into()))?;
        let path = provisioned
            .to_str()
            .ok_or_else(|| FacadeError::field("worktree"))?;
        let paths = match (carry_request, snapshot) {
            (Some(request), Some(snapshot)) => {
                super::carry::apply(self, &provisioned, request, base, &snapshot)?
            }
            _ => recovered.map_or_else(Vec::new, |(_, paths)| paths),
        };
        let mut binding = self.bind_writer_worktree(path)?;
        if let Some(binding) = binding.as_mut() {
            if carry_examined {
                binding
                    .extra
                    .insert("carry_examined".into(), serde_json::json!(true));
            }
            if !paths.is_empty() {
                binding
                    .extra
                    .insert("carried_paths".into(), serde_json::json!(paths));
            }
        }
        Ok(binding)
    }

    fn provision_child_worktree(
        &mut self,
        path: &str,
        source: &str,
        owner: Option<provenant_core::Digest>,
        carry_request: Option<&provenant_fabric_runtime::graph::WriterCarryRequest<'_>>,
    ) -> Result<Option<provenant_fabric_runtime::graph::ChildWorktreeProvision>, Self::Error> {
        let target = self.factory.context.project_root.join(path);
        let recovered = match carry_request {
            Some(request) => super::carry::recover(self, &target, request, source)?,
            None => None,
        };
        let snapshot = match (carry_request, &recovered) {
            (Some(request), None) => super::carry::capture(self, request, source)?,
            _ => None,
        };
        let carry_examined = recovered.is_some() || snapshot.is_some();
        let provision_source = recovered.as_ref().map_or(source, |(head, _)| head.as_str());
        // The graph has already selected a managed worktree path. Admit the
        // child through the shared request so destination and claims are one
        // decision; a host `refuse` here is a configured stop, not a busy lock.
        let (choice, sharing) = self.child_launch_settings();
        let parent_attempt = std::env::var(provenant_core::consumer_env::PARENT_ATTEMPT_ID)
            .ok()
            .and_then(|value| provenant_core::AttemptId::parse(&value).ok())
            .unwrap_or_else(|| provenant_core::AttemptId::from_bytes([0; 16]));
        let request = ChildLaunchRequest {
            parent_attempt,
            authority: NarrowedAuthority {
                routes: Default::default(),
                budgets: Default::default(),
                paths: Vec::new(),
                extra: Default::default(),
            },
            destination: Destination::ParentOwnedPath {
                path: path.to_owned(),
            },
            path_claim: Sourced {
                value: None,
                source: Source::default(),
            },
            commit: Sourced {
                value: None,
                source: Source::default(),
            },
            extra: Default::default(),
        };
        let tree = CustodySubject::new(SubjectKind::Worktree, path.to_owned());
        resolve_child_launch(&tree, &request, choice, &sharing, None, &[]).map_err(|error| {
            let diagnostic = provenant_run::RunError::Writer(
                provenant_confinement::worktrees::WorktreeError::CustodyCommand(error.clone()),
            )
            .diagnostic(provenant_records::diagnostic::Destination::Local);
            FacadeError::RecordedRefusal {
                refusal_code: error.code.clone(),
                diagnostic: Box::new(diagnostic),
            }
        })?;
        provenant_cli::fabric::graph_relay::provision_child_with_budget(
            &self.factory.context,
            path,
            provision_source,
            owner.as_ref(),
            self.writer_launch_budget.unwrap_or(self.budget),
            self.cancel,
        )
        .map_err(super::super::interface::relay_error)?;
        let carried_paths = match (carry_request, snapshot) {
            (Some(request), Some(snapshot)) => {
                super::carry::apply(self, &target, request, source, &snapshot)?
            }
            _ => recovered.map_or_else(Vec::new, |(_, paths)| paths),
        };
        Ok(Some(
            provenant_fabric_runtime::graph::ChildWorktreeProvision {
                carried_paths,
                carry_examined,
            },
        ))
    }

    fn bound_graph_bases(
        &self,
    ) -> (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ) {
        self.integration_base
            .map_or((None, None), |bases| (bases.review, bases.integration))
    }

    fn applied_route(
        &mut self,
        reference: &str,
    ) -> Result<Option<provenant_fabric_runtime::graph::GraphRoute>, Self::Error> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
        };
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let record = owner.snapshot(target, self.budget, self.cancel)?;
        let (name, alias) = match record.requested_route {
            provenant_records::run::RequestedRoute::Adapter { name, .. } => (Some(name), None),
            provenant_records::run::RequestedRoute::Named { name, .. }
            | provenant_records::run::RequestedRoute::Review { name, .. } => (None, Some(name)),
            provenant_records::run::RequestedRoute::Model { .. } => (None, None),
        };
        Ok(record
            .applied_route
            .map(|route| provenant_fabric_runtime::graph::GraphRoute {
                account_key: route.account_key,
                source: route.source,
                pool_selection: route.pool_selection,
                extra: route.extra,
                adapter: route.provider,
                model: route.model,
                effort: route.effort,
                account: route.account_label,
                name: name.or(route.host_native),
                alias,
                ..Default::default()
            }))
    }

    fn review_range(
        &mut self,
        reference: &str,
        explicit_base: bool,
    ) -> Result<provenant_fabric_runtime::graph::ReviewRange, Self::Error> {
        use provenant_fabric_runtime::graph::ReviewRange;
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        if host != self.factory.context.identity.host.to_string() {
            return Ok(ReviewRange::Unknown);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        let request = store.request(record.attempt_id, &call).map_err(run_error)?;
        let task = &request.task;
        if !explicit_base
            && self
                .integration_base
                .is_none_or(|bases| bases.review.is_none() && bases.integration.is_none())
        {
            return Ok(ReviewRange::NoBase);
        }
        Ok(match task.base.as_ref().zip(task.head.as_ref()) {
            Some((base, head)) => ReviewRange::Resolved {
                base: base.clone(),
                head: head.clone(),
            },
            None => ReviewRange::Unknown,
        })
    }

    fn review_delta_attributable(&mut self, base: &str, head: &str) -> bool {
        let (Ok(base), Ok(head)) = (
            provenant_core::GitOid::parse(base),
            provenant_core::GitOid::parse(head),
        ) else {
            return false;
        };
        provenant_confinement::worktrees::with_context_using_facts(
            &self.factory.context.roots,
            &self.factory.home,
            &self.factory.cwd,
            &self.factory.clock,
            self.cancel,
            &self.repo_facts,
            |context| {
                provenant_confinement::worktrees::is_ancestor(context, base, head, self.cancel)
            },
        )
        .is_ok_and(|attributable| attributable)
    }

    fn respect_ci_wake(&self) -> bool {
        self.tick
    }
    fn ci_poll_backoff(&self, node_id: &str) -> provenant_fabric_runtime::graph::CiPollBackoff {
        let config = &self.factory.context.config.graph;
        config
            .ci_poll_overrides
            .get(node_id)
            .copied()
            .unwrap_or(config.ci_poll_backoff)
    }
    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        let branch =
            self.integration_branch
                .as_deref()
                .ok_or_else(|| FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: "an integration source requires graph.integration_branch".into(),
                })?;
        let state = self.factory.context.roots.state.as_path();
        let ports = &self.factory.context.port_table;
        let policy = provenant_config::operation_policy(state, self.factory.cwd.as_path(), ports);
        let config = provenant_config::ForgeConfig::new(ports, (*policy).clone());
        let git = provenant_config::ForgeGit::new(self.factory.cwd.as_path())
            .map_err(|_| FacadeError::io("forge context Git metadata"))?;
        // A local integration branch needs no remote: resolving one here would
        // refuse a read-only base lookup in a checkout that declares none. Only
        // a remote-qualified branch needs the resolved remote name.
        // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
        let forge_remote = provenant_config::ForgeContext::remote_if_any(&config, &git)
            .map_err(|_| FacadeError::io("forge context resolution"))?;
        let reference = if let Some(remote) = forge_remote.as_ref().map(|name| name.as_str())
            && let Some(branch) = branch.strip_prefix(&format!("{remote}/"))
        {
            format!("refs/remotes/{remote}/{branch}")
        } else {
            format!("refs/heads/{branch}")
        };
        self.factory
            .resolve_base(&self.factory.cwd, &reference, &self.repo_facts, self.cancel)
            .map(|base| Some(base.to_string()))
    }
    fn ci_status(&mut self) -> Option<&mut dyn provenant_fabric_runtime::graph::CiStatus> {
        Some(self)
    }
    fn actor(&self) -> String {
        self.factory.context.identity.inbox.actor.to_string()
    }
    fn run_hygiene(
        &mut self,
        request: provenant_fabric_runtime::graph::HygieneRequest<'_>,
    ) -> Result<provenant_fabric_runtime::graph::HygieneExecution, Self::Error> {
        use provenant_fabric_runtime::graph::HygieneExecution;
        match self.execute_delivery_hygiene_inner(&request) {
            Ok(execution) => Ok(execution),
            Err(error) => Ok(HygieneExecution {
                outcome: provenant_records::graph::HygieneOutcome {
                    kind: provenant_records::graph::HygieneOutcomeKind::Error,
                    before_head: None,
                    after_head: None,
                    handoff: request.handoff.map(str::to_owned),
                },
                branch: None,
                formatter_source: None,
                diagnostic: Some(delivery_hygiene_text(&error.to_string(), 1024)),
            }),
        }
    }
    fn publish_delivery(
        &mut self,
        request: provenant_fabric_runtime::graph::PublicationRequest<'_>,
    ) -> Result<Option<provenant_fabric_runtime::graph::DeliveryPublication>, Self::Error> {
        self.execute_delivery_publication_inner(&request).map(Some)
    }
    fn seed_delivery_adoption(
        &mut self,
        request: provenant_fabric_runtime::graph::DeliveryAdoptionRequest<'_>,
    ) -> Result<Option<provenant_fabric_runtime::graph::DeliveryPublication>, Self::Error> {
        super::super::queue_adoption::revalidate(self.factory, &request, self.budget, self.cancel)
            .map(Some)
            .map_err(|reason| FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: format!("cannot adopt pull request into queue item: {reason}"),
            })
    }
    fn admit_delivery_ci(
        &mut self,
        request: provenant_fabric_runtime::graph::DeliveryCiRequest<'_>,
    ) -> Result<Option<provenant_fabric_runtime::graph::DeliveryCiAdmission>, Self::Error> {
        let provenant_fabric_runtime::graph::DeliveryCiRequest {
            graph_id,
            node_id,
            head_sha,
            ci_slots,
            wait_policy,
            allow_overflow,
            waited_millis,
        } = request;
        let owner = provenant_run::build::queue::CiLeaseOwner {
            project: self.factory.context.identity.project.to_string(),
            graph_id: graph_id.to_owned(),
            node_id: node_id.to_owned(),
            head_sha: head_sha.to_owned(),
        };
        Ok(crate::admit::acquire_delivery_ci_lease(
            self.factory.context.roots.state.as_path(),
            owner,
            ci_slots,
            wait_policy,
            (allow_overflow, waited_millis),
            self.factory.context.config.graph.launch_wait_seconds,
            self.cancel,
        ))
    }
    fn release_delivery_ci(
        &mut self,
        lease: &provenant_fabric_runtime::graph::DeliveryCiLease,
    ) -> Result<(), Self::Error> {
        crate::admit::release_delivery_ci_lease(
            self.factory.context.roots.state.as_path(),
            lease,
            self.factory.context.config.graph.launch_wait_seconds,
            self.cancel,
        )
        .map_err(|error| FacadeError::io(format!("release delivery CI lease: {error}")))
    }
    fn park_delivery(
        &mut self,
        request: provenant_fabric_runtime::graph::DeliveryParkRequest<'_>,
    ) -> Result<Option<provenant_core::HumanRequestId>, Self::Error> {
        use provenant_fabric_runtime::inbox::{InboxResult, delivery_park};
        use provenant_records::inbox::RequestWork;

        let attempt_id = request
            .attempt_ref
            .map(|reference| {
                reference
                    .split_once('/')
                    .ok_or(FacadeError::StateCorrupt)?
                    .1
                    .parse::<provenant_core::AttemptId>()
                    .map_err(|_| FacadeError::StateCorrupt)
            })
            .transpose()?;
        let repository = if let Some(repository) = request.repository {
            PathBuf::from(repository)
        } else {
            provenant_os::git::repository_path(
                &self.factory.context.project_root,
                provenant_os::git::RepositoryPath::CommonDir,
            )
            .map_err(|_| FacadeError::io("locate delivery park repository"))?
        };
        let repository = provenant_records::AbsolutePath::new(&repository)
            .map_err(|_| FacadeError::StateCorrupt)?;
        let session = request
            .session
            .map(str::to_owned)
            .or_else(|| {
                provenant_workflow::session::chair_session_for(
                    &self.factory.context.roots.state,
                    self.factory.context.identity.project,
                    self.factory.context.identity.inbox.actor,
                )
                .map(|session| session.as_str().to_owned())
            })
            .ok_or(FacadeError::StateCorrupt)?;
        let work = RequestWork {
            extra: Default::default(),
            graph: request.graph_id.to_owned(),
            node: Some(request.node_id.to_owned()),
            attempt_id,
            repository: Some(repository),
            head: request
                .head
                .map(str::parse::<provenant_core::GitOid>)
                .transpose()
                .map_err(|_| FacadeError::StateCorrupt)?,
        };
        let input = delivery_park(
            session,
            request
                .operation_id
                .parse()
                .map_err(|_| FacadeError::StateCorrupt)?,
            work,
            request.item,
            request.attempt_key,
            request.reason,
            request.handoff,
            request.evidence,
        )
        .map_err(|error| graph_ask_refused(request.transition_id, &error))?;
        let cancel = self.cancel;
        graph_ask_with(
            self.factory,
            cancel,
            &self.actor(),
            request.transition_id,
            |service| {
                let InboxResult::Saved { receipt } = service
                    .execute(input, cancel)
                    .map_err(|error| graph_ask_refused(request.transition_id, &error))?
                else {
                    return Err(FacadeError::StateCorrupt);
                };
                Ok(Some(receipt.request_id))
            },
        )
    }
    fn validate_expansion(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        _checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<(), Self::Error> {
        let ports = &self.factory.context.port_table;
        // One context for this expansion, resolved only when a delivery target
        // check first needs it. A read-only expansion delivers nothing, so it
        // must not be refused in a checkout without a resolvable remote.
        // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
        let forge = provenant_run::ForgeOnce::new(
            self.factory.context.roots.state.as_path(),
            self.factory.cwd.as_path(),
            ports,
        );
        for node in &document.nodes {
            if let Some(provenant_records::run::DeliveryMode::Integrate(target)) = &node.delivery {
                provenant_run::HostDeliveryContext::validate_integration_target(
                    &self.factory.cwd,
                    target,
                    forge.get().map_err(run_error)?,
                )
                .map_err(run_error)?;
            }
        }
        Ok(())
    }
    fn file_ask(
        &mut self,
        filing: provenant_fabric_runtime::graph::AskFiling<'_>,
    ) -> Result<provenant_fabric_runtime::graph::AskFiled, Self::Error> {
        let cancel = self.cancel;
        graph_ask_with(
            self.factory,
            cancel,
            filing.actor,
            &filing.ask.ask_id,
            |service| graph_ask_file(service, &filing, cancel),
        )
    }
    fn node_ask(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, Self::Error> {
        let cancel = self.cancel;
        graph_ask_with(
            self.factory,
            cancel,
            scope.actor,
            &scope.ask.ask_id,
            |service| graph_ask_poll(service, &scope, cancel),
        )
    }
    fn notify_owner(
        &mut self,
        filing: provenant_fabric_runtime::graph::AskFiling<'_>,
    ) -> Result<bool, Self::Error> {
        use provenant_fabric_runtime::inbox::InboxResult;
        let cancel = self.cancel;
        let input = graph_notification_input(&filing)?;
        graph_ask_with(
            self.factory,
            cancel,
            filing.actor,
            &filing.ask.ask_id,
            |service| {
                let result = service
                    .execute(input, cancel)
                    .map_err(|error| graph_ask_refused(&filing.ask.ask_id, &error))?;
                Ok(matches!(result, InboxResult::Saved { .. }))
            },
        )
    }
    fn close_node_ask(
        &mut self,
        scope: provenant_fabric_runtime::graph::AskScope<'_>,
    ) -> Result<(), Self::Error> {
        let cancel = self.cancel;
        graph_ask_with(
            self.factory,
            cancel,
            scope.actor,
            &scope.ask.ask_id,
            |service| graph_ask_close(service, &scope, cancel),
        )
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        self.factory.clock.wall_now().ok()
    }
    fn land_policy(
        &self,
        tier: provenant_records::workflow::RiskTier,
    ) -> provenant_fabric_runtime::config::LandChoice {
        let project = self.factory.context.identity.project.to_string();
        self.factory
            .context
            .config
            .graph
            .land
            .choose(Some(&project), tier)
    }
    fn review_coverage(
        &mut self,
        branch: &str,
        reviewed: provenant_core::review_cover::Reviewed<'_>,
        head: &str,
    ) -> Result<Option<provenant_core::review_cover::Coverage>, Self::Error> {
        // A proof that cannot be read, including before the branch has a
        // pull request, refuses the land as an unproved one does.
        Ok(crate::pr::landing::approval::graph_coverage(
            &self.factory.cwd,
            self.factory.context.roots.state.as_path(),
            branch,
            reviewed,
            head,
        )
        .ok())
    }
    fn land(
        &mut self,
        request: provenant_fabric_runtime::graph::LandRequest<'_>,
    ) -> Result<provenant_fabric_runtime::graph::LandObservation, Self::Error> {
        let execution = match provenant_fabric_runtime::graph::FileJournal::execution(
            &self.factory.context.roots.state,
            self.factory.context.identity.project,
            request.graph_id,
        ) {
            Ok(Some(execution)) => execution,
            Ok(None) => {
                return Ok(provenant_fabric_runtime::graph::LandObservation::Pending {
                    reason: format!(
                        "cannot verify delivery queue policy for graph {}: saved execution is missing",
                        request.graph_id
                    ),
                });
            }
            Err(error) => {
                return Ok(provenant_fabric_runtime::graph::LandObservation::Pending {
                    reason: format!(
                        "cannot verify delivery queue policy for graph {}: {error}",
                        request.graph_id
                    ),
                });
            }
        };
        // Graph landing submits through the one landing owner.
        Ok(crate::pr::land::graph_land(
            &self.factory.cwd,
            self.factory.context.roots.state.as_path(),
            &request,
            execution.document.queue.is_some(),
            self.factory.workers,
        ))
    }
    fn cost_unknown_reason(&mut self) -> Option<String> {
        let graph_budget = &self.factory.context.config.graph.budget;
        if graph_budget.per_graph.cost_usd.is_none() && graph_budget.per_node.cost_usd.is_none() {
            return None;
        }
        self.prices
            .get_or_insert_with(|| {
                load_graph_prices_layered(&self.factory.assets, &self.factory.context.roots)
            })
            .as_ref()
            .err()
            .cloned()
    }
    fn deliver_graph(
        &mut self,
        document: &provenant_fabric_runtime::graph::GraphDocument,
        checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    ) -> Result<Option<String>, Self::Error> {
        let branch = document
            .integration_branch
            .as_deref()
            .ok_or_else(|| FacadeError::field("integration_branch"))?;
        let session = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        }
        .session_with_facts(self.cancel, &self.repo_facts)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            cancel: self.cancel,
        };
        let mut body = format!("Graph {}\n\n", document.graph_id);
        let mut reviews = Vec::new();
        for node in &checkpoint.nodes {
            if let Some(summary) = node.deterministic_summary() {
                body.push_str(&summary);
                continue;
            }
            let reference = node
                .attempts
                .last()
                .and_then(|attempt| attempt.attempt_ref.as_deref())
                .ok_or(FacadeError::StateCorrupt)?;
            let (_, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let id = id.parse().map_err(|_| FacadeError::StateCorrupt)?;
            let attempt = session.service().status(id, &call).map_err(run_error)?;
            body.push_str(&format!(
                "Node {}: attempt {id} ({})\n\n",
                node.id, attempt.key
            ));
            if graph_review_node(document, &node.id) {
                // Retain each launched review route, including failed attempts
                // before a successful retry. These are evidence, not a verdict.
                for review in &node.attempts {
                    let Some(reference) = review.attempt_ref.as_deref() else {
                        continue;
                    };
                    let (_, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
                    let id = id.parse().map_err(|_| FacadeError::StateCorrupt)?;
                    let record = session.service().status(id, &call).map_err(run_error)?;
                    let mut facts = provenant_cli::run::recorded_attempt_facts(&record);
                    facts.role = document
                        .nodes
                        .iter()
                        .find(|spec| spec.id == node.id)
                        .and_then(|spec| spec.role.clone())
                        .or(facts.role);
                    // Each row describes its own result, including failed
                    // routes before a retry. Never borrow a later verdict.
                    facts.verdict = record.result.as_ref().and_then(|_| {
                        let captured = session.store.stream_result(id, &call, None).ok()?;
                        (!captured.verdict_invalid)
                            .then_some(captured.verdict)?
                            .map(String::from)
                    });
                    facts.head = node
                        .source_commit
                        .as_deref()
                        .and_then(|head| provenant_core::GitOid::parse(head).ok())
                        .or(facts.head);
                    reviews.push(facts);
                }
            }
        }
        body.push_str(&provenant_cli::pr_body::independent_review_rows(&reviews));
        let forge = provenant_run::resolve_forge(
            self.factory.context.roots.state.as_path(),
            self.factory.cwd.as_path(),
            &self.factory.context.port_table,
        )
        .map_err(run_error)?;
        let url = provenant_run::HostDeliveryContext::capture()
            .with_repo_facts(&self.repo_facts)
            .deliver_graph_pr(
                &self.factory.cwd,
                branch,
                self.integration_base
                    .and_then(|bases| bases.integration)
                    .ok_or(FacadeError::Conflict)?,
                &body,
                &forge,
            )
            .map_err(run_error)?;
        Ok(Some(url))
    }
    fn usage(
        &mut self,
        attempt_ref: &str,
    ) -> Result<provenant_fabric_runtime::graph::AttemptUsage, Self::Error> {
        let (host, attempt) = attempt_ref
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
            attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
        };
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let record = owner.snapshot(target, self.budget, self.cancel)?;
        let cost_unknown_reason = self.cost_unknown_reason();
        let prices = self.prices.get_or_insert_with(|| {
            load_graph_prices_layered(&self.factory.assets, &self.factory.context.roots)
        });
        Ok(graph_attempt_usage(
            record,
            prices.as_ref().ok(),
            cost_unknown_reason,
        ))
    }

    fn attempt_families(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        self.writer_families(attempts)
    }

    fn materialise_inputs(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        requests: &[provenant_fabric_runtime::graph::InputRequest],
        cap: usize,
    ) -> Option<provenant_fabric_runtime::graph::MaterialisedInputs> {
        Some(graph_inputs::materialise(
            self.factory,
            self.budget,
            self.cancel,
            key,
            requests,
            cap,
        ))
    }

    fn placement_host_id(&self, name: &str) -> Result<Option<provenant_core::HostId>, Self::Error> {
        let local = self.factory.context.identity.host;
        let host = if matches!(name, "local" | "localhost")
            || name
                .parse::<provenant_core::HostId>()
                .is_ok_and(|host| host == local)
        {
            local
        } else {
            self.factory
                .context
                .peer_host(name)
                .ok_or(FacadeError::UnknownHost)?
        };
        Ok(Some(host))
    }

    fn stop_cancelled_attempt(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<(), Self::Error> {
        let mut owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        self.factory
            .cancel_graph_attempt(&mut owner, attempt, self.budget, self.cancel)
    }

    fn cancelled_attempt_settled(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        if let Some(settled) =
            self.factory
                .cancel_peer_submission(attempt, false, self.budget, self.cancel)?
        {
            return Ok(settled);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        if let Some(reference) = attempt.attempt_ref.as_deref() {
            let (host, id) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let host = host.parse().map_err(|_| FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host),
                attempt: id.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            let record = owner.snapshot(target, self.budget, self.cancel)?;
            if record.peer.peer_subtree.as_ref().is_some_and(|subtree| {
                matches!(
                    subtree.status,
                    provenant_records::graph::GraphStatus::Pending
                        | provenant_records::graph::GraphStatus::Paused
                )
            }) {
                return Ok(false);
            }
            return Ok(matches!(
                record.state,
                provenant_records::run::AttemptState::Terminal { .. }
            ) && record.custody_pending == Some(false));
        }
        let operation = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        match store.operation(&operation, &call).map_err(run_error)? {
            // Absence in this host's store cannot settle an uncertain peer
            // launch whose accepted target has not reached the graph yet.
            None => Ok(!self.uncertain_peer_placement(attempt)),
            Some(operation) if operation.state == provenant_run::OperationState::Refused => {
                Ok(true)
            }
            Some(operation) => {
                let admitted = match store.operation_attempt(&operation.operation_id, &call) {
                    Ok(admitted) => Some(admitted),
                    Err(provenant_run::RunError::NotFound) => None,
                    Err(error) => return Err(run_error(error)),
                };
                let found = admitted.is_some();
                if let Some(admitted) = admitted {
                    let record = store.record(admitted, &call).map_err(run_error)?;
                    if !store.custody_released(&record, &call).map_err(run_error)? {
                        return Ok(false);
                    }
                }
                // A prepared operation with no attempt never admitted work;
                // an unknown launch without evidence must remain unresolved.
                Ok(found || operation.state == provenant_run::OperationState::Prepared)
            }
        }
    }

    fn launch_accepted(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        if attempt.attempt_ref.is_some() {
            return Ok(true);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        let operation_id = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        Ok(store
            .operation(&operation_id, &call)
            .map_err(run_error)?
            .is_some_and(|operation| {
                !matches!(
                    operation.state,
                    provenant_run::OperationState::Prepared
                        | provenant_run::OperationState::Refused
                )
            }))
    }

    fn launch_refused_before_acceptance(
        &mut self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> Result<bool, Self::Error> {
        if attempt.attempt_ref.is_some() {
            return Ok(false);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: Some(
                attempt
                    .launch_actor
                    .parse()
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            chair: None,
        };
        let operation = graph_operation("graph-attempt-operation", &attempt.key)?;
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        // An absent local operation proves nothing about a peer submission;
        // local prelaunch refusals keep their existing cancellation semantics.
        Ok(
            match store.operation(&operation, &call).map_err(run_error)? {
                Some(operation) => matches!(
                    operation.state,
                    provenant_run::OperationState::Prepared
                        | provenant_run::OperationState::Refused
                ),
                None => !self.uncertain_peer_placement(attempt),
            },
        )
    }

    fn observe_or_start(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
        if replay.stored_ref.is_some() {
            return self.observe_or_start_inner(key, node, route, access, worktree, replay, None);
        }
        let refusal = GraphRefusalScope::new(self, key, &node, route, access, worktree, replay)?;
        // Refusals have no admitted run. Replay them before reading a request,
        // consulting current writer state, or repeating any preparation effect.
        if replay.stored_ref.is_none() {
            let store = self.refusal_store(&refusal)?;
            let call = provenant_run::Request {
                clock: &self.factory.clock,
                cancel: self.cancel,
            };
            if let Some(record) = store
                .operation(&refusal.operation, &call)
                .map_err(run_error)?
                && record.state == provenant_run::OperationState::Refused
            {
                let intent = record
                    .origin_intent_digest
                    .ok_or(FacadeError::StateCorrupt)?;
                if intent != refusal.digest {
                    return Err(run_error(provenant_run::RunError::OperationConflict));
                }
                self.post_graph_refusal(&store, &refusal.operation, &call);
                return Err(stored_graph_refusal(&record)?);
            }
        }
        let result = self
            .observe_or_start_inner(key, node, route, access, worktree, replay, Some(&refusal))
            .map_err(|error| error.with_node(key.node_id.as_str()));
        if replay.stored_ref.is_none()
            && let Err(error) = &result
            && matches!(
                error,
                FacadeError::DispatchRefused { .. }
                    | FacadeError::InvalidInput { .. }
                    | FacadeError::GraphRelayRefused { .. }
                    | FacadeError::ConfinementRefused { .. }
                    | FacadeError::CredentialRoot { .. }
                    | FacadeError::ContinuationUnsupported
                    | FacadeError::ProviderStateExpired
                    | FacadeError::LaneDepth
                    | FacadeError::LaneCoordinationRefused
                    | FacadeError::WorktreeMissing { .. }
                    | FacadeError::WorktreeUnregistered { .. }
                    | FacadeError::WorktreeNotApplicable
                    | FacadeError::BaseUnreachable
                    | FacadeError::BranchExists
            )
        {
            // Busy/unknown handoffs and storage errors are not terminal
            // refusals. An admitted operation retains its own custody owner.
            let store = self.refusal_store(&refusal)?;
            let call = provenant_run::Request {
                clock: &self.factory.clock,
                cancel: self.cancel,
            };
            if store
                .operation(&refusal.operation, &call)
                .map_err(run_error)?
                .is_none()
            {
                self.retain_graph_refusal(&refusal, error, error.diagnostic())?;
            }
        }
        result
    }
}
