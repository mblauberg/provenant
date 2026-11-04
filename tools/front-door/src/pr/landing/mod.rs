//! Batch landing contract. Lane-owned functions refuse until their implementations land.
#![allow(
    dead_code,
    reason = "integration contracts are populated by independent landing lanes"
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
    #[error("landing target moved too many times")]
    Contention,
    #[error("publication outcome is unknown")]
    PublicationUnknown,
}
pub mod adapters;
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
    pub head: Option<provenant_core::GitOid>,
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
        Err(error) => crate::fail(
            "land",
            provenant_records::Outcome::Refused,
            "LAND_POLICY",
            &error.to_string(),
            globals,
        ),
    }
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

fn run_inner(
    options: Options,
    globals: &provenant_cli::Globals,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<u8, Box<dyn std::error::Error>> {
    use provenant_cli::domains::{Integration, LandOverride};
    use provenant_records::{CommandResult, Outcome};
    if !options.explain
        && !options.batch
        && !options.shadow
        && options.tier.is_none()
        && options.numbers.len() == 1
    {
        return Ok(super::land::run(
            options.numbers[0],
            options.head,
            options.wait,
            options.poll,
            false,
            globals,
            workers,
        ));
    }
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
        let text = format!("{}\n", serde_json::to_string_pretty(&policy)?);
        return Ok(crate::emit(
            &CommandResult::new("land", Outcome::Ok, policy),
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
    let use_batch = options.batch || options.shadow || options.numbers.len() > 1;
    if integration == Integration::Single && use_batch {
        return Err("single integration accepts one pull request without batch or shadow".into());
    }
    if !use_batch {
        return Ok(super::land::run(
            options.numbers[0],
            options.head,
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
    if integration == Integration::ForgeQueue {
        return Err(
            LandingError::Failed("land.integration=forge_queue is not supported".into()).into(),
        );
    }
    use sha2::{Digest, Sha256};
    let definition = model::GateDefinition {
        kind: effective.gate.kind,
        digest: Sha256::digest(serde_json::to_vec(&effective.gate)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        checks: effective.gate.checks.clone(),
        commands: effective.gate.commands.clone(),
        issuer: effective.gate.issuer.clone(),
        executor: effective.gate.executor.clone(),
        coverage: effective.gate.coverage.clone(),
        timeout_seconds: effective.gate.timeout_seconds,
    };
    let (mut forge, mut repo, mut runner, mut alarms) = adapters::connect(&resolved.project)?;
    let target = forge.target_branch()?;
    let report = driver::run(
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
        },
    )?;
    let value = serde_json::json!({"receipts": report.receipts, "outcomes": report.outcomes, "warnings": report.warnings});
    let text = format!("{}\n", serde_json::to_string_pretty(&value)?);
    Ok(crate::emit(
        &CommandResult::new("land", Outcome::Ok, value),
        globals,
        &text,
        false,
    ))
}
