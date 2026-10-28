//! Installation grammar and thin orchestration over config's state machine.
//! The process edge supplies captured roots, environment and capabilities.
use crate::{Harness, Switch};
use clap::builder::TypedValueParser;
use provenant_config::{
    self as config, InstallContext, InstallError, InstallOutcome, InstallReporter, Release,
    ReleaseSmoke, Targets,
};
use provenant_core::Roots;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, clap::Args)]
pub struct Install {
    /// Release directory to install from
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(crate::grammar::absolute))]
    pub source: PathBuf,
    /// Harness to install for (repeatable; defaults to the receipt or detected CLIs)
    #[arg(long, value_parser = harness_parser())]
    pub harness: Vec<Harness>,
    /// Directory for the installed binary
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(crate::grammar::absolute))]
    pub bin_dir: Option<PathBuf>,
    /// Enable hooks, or remove unchanged owned hooks (omission keeps ownership)
    #[arg(long, value_enum)]
    pub hooks: Option<Switch>,
    /// Adopt matching existing files instead of refusing
    #[arg(long)]
    pub adopt_existing: bool,
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

#[derive(Debug, clap::Args)]
pub struct Upgrade {
    /// Build this checkout instead of downloading the latest release
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(crate::grammar::absolute))]
    pub from_source: Option<PathBuf>,
    /// Upgrade this host and every registered Fabric peer
    #[arg(long, value_parser = ["all"], default_value = "all")]
    pub hosts: String,
}

pub(crate) fn harness_parser() -> impl clap::builder::TypedValueParser<Value = Harness> {
    clap::builder::PossibleValuesParser::new(
        provenant_records::ADAPTER_SPECS
            .iter()
            .map(|spec| spec.name),
    )
    .try_map(|name| serde_json::from_value::<Harness>(serde_json::Value::String(name)))
}

/// Config values are applied before explicit harness environment overrides;
/// leaf flags are applied by `execute_install` after this function returns.
pub fn configured_targets(
    roots: Roots,
    home: &Path,
    harnesses: &[Harness],
    claude_config_dir: Option<&Path>,
    codex_home: Option<&Path>,
) -> Result<Targets, InstallError> {
    let mut targets = Targets::new(roots, home, harnesses);
    // Validate against every registered domain, so sections other commands
    // write (accounts, routing, quota) never block installation.
    config::apply_install_config_with_domains(&mut targets, &crate::domains::SECTIONS)?;
    targets.apply_environment(claude_config_dir, codex_home)?;
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
    targets.hooks = matches!(args.hooks, Some(Switch::On));
    targets.remove_hooks = matches!(args.hooks, Some(Switch::Off));
    targets.validate()?;
    let release = Release::read(&args.source, target)?;
    config::plan_and_install(&release, &targets, context, reporter, smoke, args.dry_run)
}

pub fn execute_uninstall(
    _args: &Uninstall,
    targets: &Targets,
    context: &InstallContext<'_>,
    reporter: &mut dyn InstallReporter,
) -> Result<InstallOutcome, InstallError> {
    config::uninstall(targets, context, reporter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{ADAPTER_SPECS, AdapterSpec};
    use std::collections::BTreeSet;

    #[test]
    fn adapter_specs_cover_config_routing_and_cli_names() -> Result<(), Box<dyn std::error::Error>>
    {
        let specs: BTreeSet<_> = ADAPTER_SPECS.iter().map(|spec| spec.name).collect();
        assert_eq!(specs.len(), ADAPTER_SPECS.len());
        let parser = harness_parser();
        let cli: BTreeSet<_> = parser
            .possible_values()
            .ok_or("CLI values missing")?
            .map(|value| value.get_name().to_owned())
            .collect();
        assert_eq!(cli, specs.iter().map(|name| (*name).to_owned()).collect());
        let harnesses = [
            Harness::Claude,
            Harness::Codex,
            Harness::Cursor,
            Harness::Agy,
            Harness::Opencode,
        ];
        for harness in harnesses {
            let name = serde_json::to_value(harness)?;
            let name = name.as_str().ok_or("harness name missing")?;
            assert_eq!(
                AdapterSpec::get(name).ok_or("harness spec missing")?.id,
                harness
            );
            assert_eq!(
                serde_json::from_value::<Harness>(serde_json::json!(name))?,
                harness
            );
        }
        for model in provenant_routing::Catalogue::bundled()?.models() {
            assert_eq!(
                ADAPTER_SPECS
                    .iter()
                    .filter(|spec| spec.name == model.model.adapter())
                    .count(),
                1
            );
        }
        let providers: BTreeSet<_> = provenant_providers::REGISTRY
            .iter()
            .map(|entry| entry.adapter.id())
            .collect();
        assert_eq!(providers, specs);
        assert_eq!(providers.len(), provenant_providers::REGISTRY.len());
        Ok(())
    }
}
