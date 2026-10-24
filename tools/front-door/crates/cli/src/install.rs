//! Installation grammar and thin orchestration over config's state machine.
//! The process edge supplies captured roots, environment and capabilities.
use crate::{Harness, Switch};
use clap::builder::TypedValueParser;
use provenant_config::{
    self as config, InstallContext, InstallError, InstallOutcome, InstallReporter, Release,
    ReleaseSmoke, Snapshots, Targets,
};
use provenant_core::Roots;
use provenant_supervisor::fs::SafeFs;
use std::path::{Path, PathBuf};

#[derive(Debug, clap::Args)]
pub struct Install {
    /// Release directory to install from
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(crate::grammar::absolute))]
    pub source: PathBuf,
    /// Harness to install for (repeatable)
    #[arg(long, required = true, value_parser = harness_parser())]
    pub harness: Vec<Harness>,
    /// Directory for the installed binary
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(crate::grammar::absolute))]
    pub bin_dir: Option<PathBuf>,
    /// Install harness hooks
    #[arg(long, value_enum, default_value = "off")]
    pub hooks: Switch,
    /// Adopt matching existing files instead of refusing
    #[arg(long)]
    pub adopt_existing: bool,
    /// Legacy checkout to migrate from
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(crate::grammar::absolute))]
    pub legacy_checkout: Option<PathBuf>,
    /// Plan the install without changing anything
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, clap::Args)]
pub struct Uninstall {
    /// Harness to remove (repeatable)
    #[arg(long, required = true, value_parser = harness_parser())]
    pub harness: Vec<Harness>,
}

pub(crate) fn harness_parser() -> impl clap::builder::TypedValueParser<Value = Harness> {
    clap::builder::PossibleValuesParser::new(["claude", "codex", "cursor", "agy", "opencode"])
        .try_map(|name| serde_json::from_value::<Harness>(serde_json::Value::String(name)))
}

/// Config values are applied before explicit harness environment overrides;
/// leaf flags are applied by `execute_install` after this function returns.
pub fn configured_targets(
    roots: Roots,
    home: &Path,
    harnesses: &[Harness],
    fs: &dyn SafeFs,
    claude_config_dir: Option<&Path>,
    codex_home: Option<&Path>,
    git_hooks_dir: Option<PathBuf>,
) -> Result<Targets, InstallError> {
    let mut targets = Targets::new(roots, home, harnesses);
    // Validate against every registered domain, so sections other commands
    // write (accounts, routing, quota) never block installation.
    config::apply_install_config_with_domains(&mut targets, fs, &crate::domains::SECTIONS)?;
    targets.apply_environment(claude_config_dir, codex_home)?;
    targets.git_hooks_dir = git_hooks_dir;
    Ok(targets)
}

pub fn execute_install(
    args: &Install,
    mut targets: Targets,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
    smoke: &dyn ReleaseSmoke,
    target: &str,
) -> Result<InstallOutcome, InstallError> {
    targets.hook_specs = crate::hooks::HOOK_SPECS
        .iter()
        .map(|spec| spec.registration)
        .collect();
    targets.bin_dir = args.bin_dir.clone().unwrap_or(targets.bin_dir);
    targets.adopt_existing = args.adopt_existing;
    targets.hooks = matches!(args.hooks, Switch::On);
    targets.legacy_checkout = args.legacy_checkout.clone();
    targets.validate()?;
    if targets
        .legacy_checkout
        .as_ref()
        .is_some_and(|root| args.source.starts_with(root))
    {
        return Err(InstallError::Conflict(args.source.clone()));
    }
    let release = Release::read(&args.source, target, context.fs)?;
    let snapshots = Snapshots::capture(&release, &targets, context.fs)?;
    let plan = config::plan_install(release, targets, snapshots)?;
    config::install(&plan, context, reporter, smoke, args.dry_run)
}

pub fn execute_uninstall(
    _args: &Uninstall,
    targets: &Targets,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
) -> Result<InstallOutcome, InstallError> {
    config::uninstall(targets, context, reporter)
}
