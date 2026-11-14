//! Candidate-based batch landing orchestration.
#![cfg_attr(
    test,
    allow(
        dead_code,
        reason = "the custom worktree harness includes tests without running them"
    )
)]
#[cfg(test)]
pub mod fakes;
pub mod model;
pub mod owner;
pub mod policy;
pub mod ports;

#[derive(Debug, thiserror::Error)]
pub enum LandingError {
    #[error("landing {id} unavailable: {reason}")]
    Unavailable {
        id: provenant_records::landing::LandingId,
        reason: Box<provenant_records::landing::Unavailable>,
    },
    #[error("landing operation is not implemented: {0}")]
    Unimplemented(&'static str),
    #[error("landing operation failed: {0}")]
    Failed(String),
    #[error("candidate conflicts at pull request {0}")]
    Conflict(u64),
    #[error("required gate did not pass: {0}")]
    UnmetGate(String),
    #[error("estimated cost {micros} micros exceeds the spending cap of {cap} micros")]
    SpendingCap { micros: u64, cap: u64 },
    #[error("local follow-up incomplete: {0}")]
    LocalFollowUp(String),
    #[error("GitHub rate limit: {0}")]
    RateLimited(String),
    #[error("CI evidence unavailable: {0}")]
    CiUnavailable(String),
    #[error("landing target moved too many times")]
    Contention,
    /// Another lander holds the publication lock; a resubmission resumes.
    #[error("another landing run holds the publication lock; resubmit to resume")]
    Busy,
    /// CI is still running past this call's patience; a resubmission resumes.
    #[error("{0}")]
    Pending(String),
    #[error("landing target moved during publication")]
    TargetMoved,
    #[error("pull request {0} changed before publication")]
    PullRequestChanged(u64),
    #[error("publication refused: {0}")]
    Refused(String),
    #[error("publication outcome is unknown")]
    PublicationUnknown,
    #[error(
        "publication outcome is unknown; inspect pending marker {marker} and rerun land to reconcile it"
    )]
    PublicationUnknownAt { marker: std::path::PathBuf },
    #[error(
        "warning: publication remains unknown; inspect {marker} and run `provenant land {number}` to recover"
    )]
    PublicationStale {
        marker: std::path::PathBuf,
        number: u64,
    },
    #[error(
        "pending publication requires recovery; inspect {marker} and rerun land without --shadow"
    )]
    RecoveryRequired { marker: std::path::PathBuf },
    #[error("{cause}")]
    RunFailed {
        cause: Box<LandingError>,
        report: Box<driver::Report>,
    },
}
use provenant_confinement::review_exempt::ReviewExempt;
pub mod adapters;
pub mod approval;
pub mod budget;
pub mod candidate;
pub mod driver;
pub mod gate;
pub mod postmerge;
pub mod publish;
pub mod receipt;
pub mod reconcile;
pub mod split;

/// `land N M ...` submits one landing request to the landing owner; `--head`
/// pins the single member's head and `--wait` bounds this call's CI wait.
/// `--shadow` runs it without publishing; `--explain --json` prints the
/// policy and sources without requiring pull request numbers.
pub struct Options {
    pub numbers: Vec<u64>,
    pub strategy: Option<provenant_records::landing::Strategy>,
    pub target: Option<String>,
    /// Any object name that resolves to one commit; resolved before landing.
    pub head: Option<String>,
    pub wait: Option<u64>,
    pub poll: Option<u64>,
    pub explain: bool,
    pub shadow: bool,
    pub tier: Option<String>,
    /// Land a delta no review covers; the reason is recorded in the receipt.
    pub accept_delta: Option<String>,
}

pub(crate) fn run(
    options: Options,
    globals: &provenant_cli::Globals,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> u8 {
    match run_inner(options, globals, workers) {
        Ok(code) => code,
        Err(error) => {
            let code = error
                .downcast_ref::<super::wait::Failure>()
                .map_or("LAND_POLICY", |failure| failure.code);
            crate::fail(
                "land",
                provenant_records::Outcome::Refused,
                code,
                &error.to_string(),
                globals,
            )
        }
    }
}

fn batch_response(
    report: driver::Report,
    failure: Option<LandingError>,
) -> provenant_records::CommandResult<serde_json::Value> {
    use provenant_records::{CommandError, CommandResult, Outcome};
    let cleanup_error = match failure.as_ref() {
        Some(LandingError::LocalFollowUp(message))
            if report
                .outcomes
                .values()
                .any(|outcome| *outcome == model::Outcome::Landed) =>
        {
            Some(message.clone())
        }
        _ => None,
    };
    let failure = if cleanup_error.is_some() {
        None
    } else {
        failure
    };
    let failure = failure.or_else(|| {
        report
            .outcomes
            .values()
            .any(|outcome| *outcome == model::Outcome::Red)
            .then(|| {
                LandingError::UnmetGate(
                    report
                        .warnings
                        .iter()
                        .filter(|warning| warning.contains("failed gate checks:"))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })
    });
    let failure = failure.or_else(|| {
        report.outcomes.iter().find_map(|(number, outcome)| {
            (*outcome == model::Outcome::Conflicted).then_some(LandingError::Conflict(*number))
        })
    });
    let failure = failure.or_else(|| {
        report.outcomes.iter().find_map(|(number, outcome)| {
            (*outcome == model::Outcome::Excluded)
                .then(|| LandingError::Refused(format!("pull request #{number} was excluded: {}; inspect the pull request, correct the issue and rerun `provenant land {number}`", report.exclusion_reasons.get(number).map(String::as_str).unwrap_or("eligibility could not be established"))))
        })
    });
    let (status, code) = match failure.as_ref() {
        Some(
            LandingError::PublicationUnknown
            | LandingError::PublicationUnknownAt { .. }
            | LandingError::PublicationStale { .. }
            | LandingError::RecoveryRequired { .. },
        ) => (Outcome::Pending, "LAND_PUBLICATION_UNKNOWN"),
        Some(LandingError::Pending(_)) => (Outcome::Pending, "LAND_PENDING"),
        Some(LandingError::Busy) => (Outcome::Pending, "LAND_BUSY"),
        Some(LandingError::Contention | LandingError::TargetMoved) => {
            (Outcome::Refused, "LAND_CONTENTION")
        }
        Some(LandingError::Refused(_) | LandingError::PullRequestChanged(_)) => {
            (Outcome::Refused, "LAND_REFUSED")
        }
        Some(LandingError::UnmetGate(_)) => (Outcome::Failed, "LAND_GATE_UNMET"),
        Some(LandingError::CiUnavailable(_)) => (Outcome::Unavailable, "CI_UNAVAILABLE"),
        Some(LandingError::RateLimited(_)) => (Outcome::Unavailable, "RATE_LIMITED"),
        Some(LandingError::SpendingCap { .. }) => (Outcome::Refused, "LAND_SPENDING_CAP"),
        Some(LandingError::Conflict(_)) => (Outcome::Failed, "LAND_CONFLICT"),
        Some(LandingError::Unimplemented(_) | LandingError::Unavailable { .. }) => {
            (Outcome::Unavailable, "LAND_UNSUPPORTED")
        }
        Some(_) => (Outcome::Failed, "LAND_FAILED"),
        None if report
            .outcomes
            .values()
            .all(|outcome| *outcome == model::Outcome::Landed) =>
        {
            (Outcome::Ok, "")
        }
        None => (Outcome::Failed, ""),
    };
    let mut value = serde_json::json!({"receipts": report.receipts, "outcomes": report.outcomes, "warnings": report.warnings, "local_follow_up": report.local_steps});
    if let Some(LandingError::Unavailable { id, reason }) = &failure {
        value["landing_id"] = serde_json::json!(id);
        value["unavailable"] = serde_json::json!(reason);
    }
    if report.outcomes.len() == 1
        && let Some((&number, outcome)) = report.outcomes.iter().next()
    {
        let landed = *outcome == model::Outcome::Landed;
        value["number"] = number.into();
        value["merged"] = landed.into();
        if let Some(error) = &cleanup_error {
            value["cleanup_pending"] = true.into();
            value["reason"] = "cleanup_pending".into();
            value["cleanup_error"] = error.clone().into();
        }
        if let Some(local) = report.local_steps.get(&number) {
            if let Some(object) = value.as_object_mut() {
                object.remove("local_follow_up");
            }
            value["head"] = local["head"].clone();
            value["steps"] = local["steps"].clone();
            value["cleanup_pending"] = (cleanup_error.is_some()
                || local["steps"]
                    .as_array()
                    .is_some_and(|steps| steps.iter().any(|step| step["status"] == "refused")))
            .into();
            if value["cleanup_pending"] == true {
                value["reason"] = "cleanup_pending".into();
            }
        }
    }
    let mut response = CommandResult::new("land", status, value);
    if let Some(error) = failure {
        response.error = Some(CommandError {
            code: code.into(),
            message: error.to_string(),
            cause: None,
            repair: None,
            diagnostic: None,
        });
    }
    response
}

/// Resolve a checkout's optional project binding for landing policy consumers.
pub(crate) fn project_id(
    roots: &provenant_core::Roots,
    runtime: &provenant_confinement::UtilityRuntime,
    executor: &provenant_os::exec::SystemExecutor,
    project: &std::path::Path,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let location = runtime
        .project_location(executor, project, budget, cancel)?
        .location;
    Ok(provenant_config::lookup_project(roots, &location)?
        .map(|binding| binding.project_id().to_string()))
}

fn gate_definition(
    gate: &provenant_cli::domains::GateSettings,
) -> Result<model::GateDefinition, serde_json::Error> {
    use sha2::{Digest, Sha256};
    Ok(model::GateDefinition {
        kind: gate.kind,
        digest: Sha256::digest(serde_json::to_vec(&gate)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        checks: gate.checks.clone(),
        commands: gate.commands.clone(),
        issuer: gate.issuer.clone(),
        executor: gate.executor.clone(),
        coverage: gate.coverage.clone(),
        timeout_seconds: gate.timeout_seconds,
        pool: gate.pool.clone(),
        env_passthrough: gate.env_passthrough.clone(),
    })
}

/// Freeze the project and exemption patterns used by every resumed approval read.
fn review_scope(
    state: &std::path::Path,
    project: Option<&str>,
) -> Result<provenant_records::landing::ReviewScope, Box<dyn std::error::Error>> {
    let exempt = project.map_or_else(ReviewExempt::none, |project| {
        provenant_confinement::run_config::RunConfig::review_exempt_at(state, project)
    });
    Ok(provenant_records::landing::ReviewScope {
        project: project.map(provenant_core::ProjectId::parse).transpose()?,
        exempt_patterns: exempt.patterns().into_iter().map(str::to_owned).collect(),
    })
}

/// The landing policy one adapter call resolved for a checkout.
pub(crate) struct Prepared {
    resolved: crate::roots::Resolved,
    project: Option<String>,
    policy: policy::LandPolicy,
    effective: provenant_cli::domains::LandSettings,
    tier_warning: Option<String>,
    target: String,
}

/// Resolve the checkout, its project and the effective landing policy.
pub(crate) fn prepare(
    globals: &provenant_cli::Globals,
    tier: Option<&str>,
    poll: Option<u64>,
    strategy: Option<provenant_records::landing::Strategy>,
    target: Option<&str>,
) -> Result<Prepared, Box<dyn std::error::Error>> {
    use provenant_cli::domains::LandOverride;
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Checkout,
        crate::roots::Product::Optional,
    )?;
    let config = provenant_cli::config_reads::read_for(&resolved.roots, "land policy")?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
    let project = project_id(
        &resolved.roots,
        &runtime,
        &provenant_os::exec::SystemExecutor::new(1)?,
        &resolved.project,
        provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))?,
        &provenant_core::CancelToken::new(),
    )?;
    use ports::Forge as _;
    let target = match target {
        Some(target) => target.to_owned(),
        None => adapters::GhForge::new(&resolved.project, resolved.roots.state.as_path())
            .target_branch()?,
    };
    if !super::git::plain_ref(&target) {
        return Err("landing target must be a plain branch name".into());
    }
    let repository_path = resolved.project.join(provenant_config::REPOSITORY_CONFIG);
    let repository = provenant_config::read_land::<LandOverride>(&resolved.project)?;
    let invocation = LandOverride {
        strategy,
        poll_seconds: poll,
        ..LandOverride::default()
    };
    let policy = policy::LandPolicy::resolve_for(
        &config.domains.pr.land,
        &config.origins,
        project.as_deref(),
        tier,
        &invocation,
        repository
            .as_ref()
            .map(|declaration| (declaration, repository_path.as_path())),
        Some(&target),
    )?;
    let effective = policy.settings()?;
    let tier_warning = tier
        .filter(|tier| !config.domains.pr.land.tiers.contains_key(*tier))
        .map(|tier| {
            format!("no landing override is configured for tier {tier}; inherited settings apply")
        });
    let errors = provenant_config::DomainConfig::validate(&provenant_cli::domains::PrSettings {
        land: effective.clone(),
    });
    if !errors.is_empty() {
        return Err(format!(
            "invalid effective landing policy: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }
    Ok(Prepared {
        resolved,
        project,
        policy,
        effective,
        tier_warning,
        target,
    })
}

/// One submission through the landing owner, with what an adapter renders.
pub(crate) struct Submission {
    pub(crate) id: Option<provenant_records::landing::LandingId>,
    pub(crate) record: Option<provenant_records::landing::LandingRecord>,
    pub(crate) report: driver::Report,
    pub(crate) failure: Option<LandingError>,
    pub(crate) repository: Option<String>,
}

/// Submit `request` to the one landing owner over the live forge and Git.
pub(crate) fn submit(
    prepared: &Prepared,
    mut request: provenant_records::landing::LandRequest,
    shadow: bool,
    globals: &provenant_cli::Globals,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<Submission, Box<dyn std::error::Error>> {
    use owner::LandingOwner as _;

    let Prepared {
        resolved,
        project,
        policy,
        effective,
        ..
    } = prepared;
    request
        .target
        .get_or_insert_with(|| prepared.target.clone());
    let settings = effective.clone();
    let mut tracking = std::collections::BTreeMap::new();
    if !shadow && request.authority != provenant_records::landing::Authority::Observe {
        for member in &request.members {
            tracking.insert(
                member.pr,
                super::land::batch_track(&resolved.project, member.pr, globals)?,
            );
        }
    }
    let repository = tracking
        .values()
        .find_map(|entry| entry.repository().map(str::to_owned));
    let mut definition = gate_definition(&settings.gate)?;
    if let Some(patience) = request.policy.patience_seconds {
        definition.timeout_seconds = patience;
    }
    let (mut forge, mut repo, mut runner, mut alarms) = adapters::connect(
        &resolved.project,
        resolved.roots.state.as_path(),
        globals,
        workers,
        tracking,
    )?;
    let reviews = adapters::ProvenantReviews::new(resolved.roots.state.clone());
    let review_scope = review_scope(resolved.roots.state.as_path(), project.as_deref())?;
    let mut owner = owner::Owner {
        forge: &mut *forge,
        repo: &mut *repo,
        runner: &mut *runner,
        alarms: &mut *alarms,
        state: resolved.roots.state.as_path(),
        policy,
        gate: &definition,
        reviews: &reviews,
        review_scope,
        shadow,
        last: None,
    };
    let numbers: Vec<u64> = request.members.iter().map(|member| member.pr).collect();
    let submitted = owner.submit(request);
    let (id, record) = match &submitted {
        Ok(id) => (Some(id.clone()), owner.record(id)?),
        Err(LandingError::Unavailable { id, .. }) => (Some(id.clone()), owner.record(id)?),
        Err(_) => (None, None),
    };
    let (mut report, failure) = owner.last.take().unwrap_or_default();
    let failure = failure.or(submitted.err());
    if report.outcomes.is_empty()
        && let Some(record) = &record
    {
        // A replayed landing reports its recorded outcomes.
        for (number, outcome) in &record.outcomes {
            report.outcomes.insert(
                *number,
                match outcome {
                    provenant_records::landing::MemberOutcome::Landed { .. } => {
                        model::Outcome::Landed
                    }
                    provenant_records::landing::MemberOutcome::Pending { .. } => {
                        model::Outcome::Pending
                    }
                    provenant_records::landing::MemberOutcome::Unknown { .. } => {
                        model::Outcome::PendingUnknown
                    }
                    provenant_records::landing::MemberOutcome::Refused { reason } => {
                        report.exclusion_reasons.insert(*number, reason.clone());
                        model::Outcome::Excluded
                    }
                },
            );
        }
    }
    for number in numbers {
        // A submission that failed before the driver ran reports each member.
        report
            .outcomes
            .entry(number)
            .or_insert(model::Outcome::Excluded);
    }
    Ok(Submission {
        id,
        record,
        report,
        failure,
        repository,
    })
}

fn run_inner(
    options: Options,
    globals: &provenant_cli::Globals,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<u8, Box<dyn std::error::Error>> {
    use provenant_cli::domains::LandSettings;
    use provenant_records::{CommandResult, Outcome};
    let prepared = prepare(
        globals,
        options.tier.as_deref(),
        options.poll,
        options.strategy,
        options.target.as_deref(),
    )?;
    let (policy, effective) = (&prepared.policy, &prepared.effective);
    if options.explain {
        let mut warnings = prepared
            .tier_warning
            .clone()
            .into_iter()
            .collect::<Vec<_>>();
        if effective.post_merge.revert_forward && !effective.post_merge.bisect {
            warnings.push(
                "post_merge.revert_forward requires post_merge.bisect to identify a culprit".into(),
            );
        }
        if effective.candidate_ref_prefix != LandSettings::default().candidate_ref_prefix {
            warnings.push("candidate_ref_prefix differs from the bundled default; update the CI trigger to match".to_owned());
        }
        if effective.publish_settle_seconds == 0 {
            warnings.push(
                "publish_settle_seconds = 0 disables the in-flight publication settle window"
                    .into(),
            );
        }
        let patience = options.wait.unwrap_or(effective.gate.timeout_seconds);
        use ports::Forge as _;
        let mut forge = adapters::GhForge::new(
            &prepared.resolved.project,
            prepared.resolved.roots.state.as_path(),
        );
        let repository = forge.repository_identity()?;
        let capabilities = match forge.capabilities(&prepared.target) {
            Ok(capabilities) => capabilities,
            Err(error) => {
                warnings.push(format!("optional forge capabilities unknown: {error}"));
                provenant_records::landing::Capabilities {
                    repository: Some(repository.clone()),
                    ..Default::default()
                }
            }
        };
        let plan = policy.plan(
            capabilities,
            &review_scope(
                prepared.resolved.roots.state.as_path(),
                prepared.project.as_deref(),
            )?,
        )?;
        let unavailable = plan.strategy.ensure_executable(&plan.plan).err();
        let value = serde_json::json!({"repository": repository, "target": prepared.target, "resolved_plan": plan, "unavailable": unavailable, "fields": policy.fields, "budget_rate_micros_per_minute": policy.budget_rate_micros_per_minute, "ci_patience_seconds": patience, "ci_patience_source": if options.wait.is_some() { "--wait" } else { "gate.timeout_seconds" }, "warnings": warnings});
        let text = format!("{}\n", serde_json::to_string_pretty(&value)?);
        return Ok(crate::emit(
            &CommandResult::new("land", Outcome::Ok, value),
            globals,
            &text,
            true,
        ));
    }
    if options.numbers.is_empty() {
        return Err("supply at least one pull request number".into());
    }
    if options.head.is_some() && options.numbers.len() > 1 {
        return Err("--head pins one pull request; land a batch without it".into());
    }
    let head = options
        .head
        .as_deref()
        .map(|name| super::git::Git::new()?.resolve_commit(&prepared.resolved.project, name))
        .transpose()?;
    let review_proof = options
        .accept_delta
        .clone()
        .map_or(provenant_records::landing::ReviewProof::Policy, |reason| {
            provenant_records::landing::ReviewProof::Accepted { reason }
        });
    let request = provenant_records::landing::LandRequest {
        members: options
            .numbers
            .iter()
            .map(|number| provenant_records::landing::LandMember {
                pr: *number,
                head: head.map(|head| head.to_string()),
                base: None,
                review_proof: review_proof.clone(),
            })
            .collect(),
        target: options.target.clone(),
        strategy: options.strategy,
        authority: provenant_records::landing::Authority::Chair,
        policy: provenant_records::landing::PolicyChoice {
            tier: options.tier.clone(),
            poll_seconds: options.poll,
            patience_seconds: options.wait,
        },
        ..Default::default()
    };
    let submission = submit(&prepared, request, options.shadow, globals, workers)?;
    let mut report = submission.report;
    if let Some(warning) = prepared.tier_warning.clone() {
        report.warnings.push(warning);
    }
    let mut response = batch_response(report, submission.failure);
    // A durable replay has no new local steps, but still names its pinned head.
    if response.data["head"].is_null()
        && let Some(number) = response.data["number"].as_u64()
        && let Some(member) = submission.record.as_ref().and_then(|record| {
            record
                .request
                .members
                .iter()
                .find(|member| member.pr == number)
        })
    {
        response.data["head"] = serde_json::json!(member.head);
    }
    response.data["landing_id"] = serde_json::json!(submission.id);
    response.data["members"] =
        serde_json::json!(submission.record.as_ref().map(|record| &record.outcomes));
    response.data["lanes"] =
        serde_json::json!(submission.repository.as_deref().and_then(|repository| {
            super::contributions::projected_lanes(globals, &options.numbers, repository)
        }));
    let text = match &response.error {
        Some(error) => format!(
            "{}: {}\n{}\n",
            error.code,
            error.message,
            serde_json::to_string_pretty(&response.data)?
        ),
        None if response.data["merged"] == true => {
            let mut text = format!(
                "merged #{} {}\n",
                response.data["number"],
                response.data["head"].as_str().unwrap_or("unknown")
            );
            if let Some(steps) = response.data["steps"].as_array() {
                for step in steps {
                    text.push_str(&format!(
                        "  {} {}: {}\n",
                        step["step"].as_str().unwrap_or(""),
                        step["status"].as_str().unwrap_or(""),
                        step["detail"].as_str().unwrap_or("")
                    ));
                }
            }
            text
        }
        None => format!("{}\n", serde_json::to_string_pretty(&response.data)?),
    };
    Ok(crate::emit(&response, globals, &text, false))
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod response_tests {
    use super::*;
    #[test]
    fn gate_definition_comes_from_resolved_policy() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_cli::domains::{GateOverride, LandOverride, LandSettings};
        let mut settings = LandSettings::default();
        settings.tiers.insert(
            "crucial".into(),
            LandOverride {
                gate: Some(GateOverride {
                    checks: Some(vec!["tier-check".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let policy = policy::LandPolicy::resolve(
            &settings,
            &Default::default(),
            None,
            Some("crucial"),
            &Default::default(),
        )?;
        let effective = policy.settings()?;
        let definition = gate_definition(&effective.gate)?;
        assert_eq!(definition.checks, vec!["tier-check"]);
        assert_eq!(definition.kind, effective.gate.kind);
        assert_eq!(definition.timeout_seconds, effective.gate.timeout_seconds);
        assert_ne!(
            definition.digest,
            gate_definition(&LandSettings::default().gate)?.digest
        );
        Ok(())
    }

    #[test]
    fn excluded_pr_has_error_envelope() {
        let report = driver::Report {
            outcomes: std::collections::BTreeMap::from([(7, model::Outcome::Excluded)]),
            exclusion_reasons: std::collections::BTreeMap::from([(
                7,
                "no independent approval for the current head".into(),
            )]),
            ..Default::default()
        };
        let response = batch_response(report, None);
        let error = response.error.unwrap();
        assert_eq!(error.code, "LAND_REFUSED");
        assert!(error.message.contains("no independent approval"));
        assert!(error.message.contains("provenant land 7"));
    }

    #[test]
    fn unknown_publication_keeps_pending_outcomes_and_recovery_path() {
        let report = driver::Report {
            outcomes: std::collections::BTreeMap::from([(7, model::Outcome::PendingUnknown)]),
            ..Default::default()
        };
        let response = batch_response(
            report,
            Some(LandingError::PublicationUnknownAt {
                marker: "/state/landing/pending/marker.json".into(),
            }),
        );
        assert_eq!(response.status, provenant_records::Outcome::Pending);
        let error = response.error.expect("unknown publication has an error");
        assert_eq!(error.code, "LAND_PUBLICATION_UNKNOWN");
        assert!(error.message.contains("/state/landing/pending/marker.json"));
        assert!(error.message.contains("rerun land"));
        assert_eq!(response.data["outcomes"]["7"], "pending_unknown");
    }

    #[test]
    fn gate_conflict_and_cap_errors_preserve_prior_outcomes() {
        for (failure, code, detail) in [
            (
                LandingError::UnmetGate("build: missing".into()),
                "LAND_GATE_UNMET",
                "build",
            ),
            (LandingError::Conflict(9), "LAND_CONFLICT", "9"),
            (
                LandingError::SpendingCap {
                    micros: 11,
                    cap: 10,
                },
                "LAND_SPENDING_CAP",
                "10",
            ),
        ] {
            let report = driver::Report {
                outcomes: std::collections::BTreeMap::from([(7, model::Outcome::Landed)]),
                ..Default::default()
            };
            let response = batch_response(report, Some(failure));
            let error = response.error.unwrap();
            assert_eq!(error.code, code);
            assert!(error.message.contains(detail));
            assert_eq!(response.data["outcomes"]["7"], "landed");
        }
    }

    #[test]
    fn ci_read_and_rate_limit_keep_distinct_codes() {
        let unavailable = batch_response(
            driver::Report::default(),
            Some(LandingError::CiUnavailable("unreadable checks".into())),
        );
        assert_eq!(unavailable.error.unwrap().code, "CI_UNAVAILABLE");
        let limited = batch_response(
            driver::Report::default(),
            Some(LandingError::RateLimited("retry later".into())),
        );
        assert_eq!(limited.error.unwrap().code, "RATE_LIMITED");
    }

    #[test]
    fn contention_and_definite_refusal_have_distinct_codes() {
        let contention = batch_response(driver::Report::default(), Some(LandingError::Contention));
        assert_eq!(contention.status, provenant_records::Outcome::Refused);
        assert_eq!(contention.error.unwrap().code, "LAND_CONTENTION");
        let refused = batch_response(
            driver::Report::default(),
            Some(LandingError::Refused("no".into())),
        );
        assert_eq!(refused.status, provenant_records::Outcome::Refused);
        assert_eq!(refused.error.unwrap().code, "LAND_REFUSED");
    }
}
