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
pub mod policy;
pub mod ports;

#[derive(Debug, thiserror::Error)]
pub enum LandingError {
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

/// `land N M ...` selects a batch; `--batch` selects a singleton batch.
/// `--shadow` uses the batch path without publishing; `--explain --json`
/// prints the policy and sources without requiring pull request numbers.
pub struct Options {
    pub numbers: Vec<u64>,
    /// Any object name that resolves to one commit; resolved before landing.
    pub head: Option<String>,
    pub wait: Option<u64>,
    pub poll: Option<u64>,
    pub explain: bool,
    pub batch: bool,
    pub shadow: bool,
    pub tier: Option<String>,
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
        Some(LandingError::Unimplemented(_)) => (Outcome::Unavailable, "LAND_UNSUPPORTED"),
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

fn run_inner(
    options: Options,
    globals: &provenant_cli::Globals,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<u8, Box<dyn std::error::Error>> {
    use provenant_cli::domains::{Integration, LandOverride, LandSettings};
    use provenant_records::{CommandResult, Outcome};
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Checkout,
        crate::roots::Product::Optional,
    )?;
    let config =
        provenant_config::read_with_domains(&resolved.roots, &provenant_cli::domains::SECTIONS)?;
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
    let invocation = LandOverride {
        poll_seconds: options.poll,
        ..LandOverride::default()
    };
    let policy = policy::LandPolicy::resolve(
        &config.domains.pr.land,
        &config.origins,
        project.as_deref(),
        options.tier.as_deref(),
        &invocation,
    )?;
    let effective = policy.settings()?;
    let tier_warning = options
        .tier
        .as_ref()
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
    if options.explain {
        let mut warnings = tier_warning.clone().into_iter().collect::<Vec<_>>();
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
        let value = serde_json::json!({"fields": policy.fields, "budget_rate_micros_per_minute": policy.budget_rate_micros_per_minute, "retry_budget_seconds": options.wait.unwrap_or(60), "retry_budget_source": if options.wait.is_some() { "--wait" } else { "compiled default" }, "warnings": warnings});
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
    let integration: Integration =
        serde_json::from_value(policy.fields["integration"].value.clone())?;
    let explicit_batch = options.batch || options.shadow || options.numbers.len() > 1;
    let use_batch = explicit_batch
        || (integration == Integration::Batch && options.head.is_none() && options.wait.is_none());
    if integration == Integration::Single && explicit_batch {
        return Err("single integration accepts one pull request without batch or shadow".into());
    }
    if integration == Integration::ForgeQueue {
        return Err(
            LandingError::Failed("land.integration=forge_queue is not supported".into()).into(),
        );
    }
    if !use_batch {
        if let Some(warning) = &tier_warning {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
        }
        let head = options
            .head
            .as_deref()
            .map(|name| super::git::Git::new()?.resolve_commit(&resolved.project, name))
            .transpose()?;
        return Ok(super::land::run(
            options.numbers[0],
            head,
            options.wait,
            options.poll,
            false,
            globals,
            workers,
        ));
    }
    if options.head.is_some() || options.wait.is_some() {
        return Err("--head and --wait apply only to single-pull-request landing".into());
    }
    let mut tracking = std::collections::BTreeMap::new();
    if !options.shadow {
        for number in &options.numbers {
            tracking.insert(
                *number,
                super::land::batch_track(&resolved.project, *number, globals)?,
            );
        }
    }
    let tracked_repository = tracking
        .values()
        .find_map(|entry| entry.repository().map(str::to_owned));
    let definition = gate_definition(&effective.gate)?;
    let (mut forge, mut repo, mut runner, mut alarms) =
        adapters::connect(&resolved.project, globals, workers, tracking)?;
    let target = forge.target_branch()?;
    let project_id = project
        .as_deref()
        .map(provenant_core::ProjectId::parse)
        .transpose()?;
    let scope = match (effective.approval, project_id) {
        (provenant_cli::domains::ApprovalSource::Receipt, Some(project)) => {
            Ok((project, adapters::repository_id(&mut *forge)?))
        }
        (provenant_cli::domains::ApprovalSource::Receipt, None) => {
            Err("needs a registered project".to_owned())
        }
        (provenant_cli::domains::ApprovalSource::Forge, _) => {
            Err("approval source is the forge".to_owned())
        }
    };
    let reviews = adapters::DeliveryReviews::new(resolved.roots.state.clone(), scope);
    let result = driver::run(
        &mut *forge,
        &mut *repo,
        &mut *runner,
        &mut *alarms,
        driver::Request {
            numbers: &options.numbers,
            target: &target,
            settings: &effective,
            policy: &policy,
            gate: &definition,
            shadow: options.shadow,
            state: resolved.roots.state.as_path(),
            approvals: approval::Approvals::new(effective.approval, &reviews),
        },
    );
    let (mut report, failure) = match result {
        Ok(report) => (report, None),
        Err(LandingError::RunFailed { cause, report }) => (*report, Some(*cause)),
        Err(error) => (driver::Report::default(), Some(error)),
    };
    if let Some(warning) = tier_warning {
        report.warnings.push(warning);
    }
    let mut response = batch_response(report, failure);
    response.data["lanes"] =
        serde_json::json!(tracked_repository.as_deref().and_then(|repository| {
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
