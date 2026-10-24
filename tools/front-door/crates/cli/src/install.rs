//! Installation grammar and thin orchestration over config's state machine.
//! The process edge supplies captured roots, environment and capabilities.
use crate::{Harness, Page, Switch};
use clap::builder::TypedValueParser;
use provenant_config::InstallFinding;
use provenant_config::{
    self as config, InstallContext, InstallError, InstallOutcome, InstallReporter, Release,
    ReleaseSmoke, Snapshots, Targets,
};
use provenant_core::{CancelToken, Roots};
use provenant_records::FileDigest;
use provenant_supervisor::fs::SafeFs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, clap::Args)]
pub struct Install {
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(absolute))]
    pub source: PathBuf,
    #[arg(long, required = true, value_parser = harness_parser())]
    pub harness: Vec<Harness>,
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(absolute))]
    pub bin_dir: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "off")]
    pub hooks: Switch,
    #[arg(long)]
    pub adopt_existing: bool,
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(absolute))]
    pub legacy_checkout: Option<PathBuf>,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, clap::Args)]
pub struct Uninstall {
    #[arg(long, required = true, value_parser = harness_parser())]
    pub harness: Vec<Harness>,
}

#[derive(Debug, clap::Args)]
pub struct InstallCheck {
    #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(absolute))]
    pub source: Option<PathBuf>,
    #[command(flatten)]
    pub page: Page,
}

fn absolute(path: PathBuf) -> Result<PathBuf, &'static str> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("expected an absolute path")
    }
}
pub(crate) fn harness_parser() -> impl clap::builder::TypedValueParser<Value = Harness> {
    clap::builder::PossibleValuesParser::new(["claude", "codex", "cursor", "agy", "opencode"])
        .try_map(|name| serde_json::from_value::<Harness>(serde_json::Value::String(name)))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallDomains {
    fabric: provenant_fabric_runtime::config::FabricConfig,
}
static SECTIONS: config::DomainSections<InstallDomains> =
    config::DomainSections::new(&[provenant_fabric_runtime::config::SECTION]);

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
    config::apply_install_config_with_domains(&mut targets, fs, &SECTIONS)?;
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
    if matches!(args.hooks, Switch::On) {
        return Err(InstallError::HooksUnavailable);
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallCheckPage {
    pub findings: Vec<InstallFinding>,
    pub count: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Error)]
pub enum InstallCheckError {
    #[error("invalid check install cursor")]
    Cursor,
    #[error("check install cursor is stale")]
    StaleCursor,
    #[error(transparent)]
    Install(#[from] InstallError),
}

pub fn execute_check_install(
    args: &InstallCheck,
    targets: &Targets,
    fs: &dyn SafeFs,
    smoke: &dyn ReleaseSmoke,
    cancel: &CancelToken,
    target: &str,
) -> Result<InstallCheckPage, InstallCheckError> {
    let source = args
        .source
        .as_deref()
        .map(|path| Release::read(path, target, fs))
        .transpose()?;
    let (findings, digest) =
        config::check_install_with_digest(targets, source.as_ref(), target, fs, smoke, cancel)?;
    page(
        findings,
        digest,
        args.page.limit,
        args.page.cursor.as_deref(),
    )
}

fn page(
    findings: Vec<InstallFinding>,
    digest: FileDigest,
    limit: u64,
    cursor: Option<&str>,
) -> Result<InstallCheckPage, InstallCheckError> {
    let digest_text = digest.to_string();
    let start = match cursor {
        None => 0,
        Some(value) => {
            let mut parts = value.split('.');
            let (Some(version), Some(cursor_digest), Some(offset), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(InstallCheckError::Cursor);
            };
            if version != "v1" {
                return Err(InstallCheckError::Cursor);
            }
            let parsed_digest =
                FileDigest::parse(cursor_digest).map_err(|_| InstallCheckError::Cursor)?;
            if parsed_digest != digest {
                return Err(InstallCheckError::StaleCursor);
            }
            let parsed = offset
                .parse::<usize>()
                .map_err(|_| InstallCheckError::Cursor)?;
            if parsed.to_string() != offset {
                return Err(InstallCheckError::Cursor);
            }
            parsed
        }
    };
    let count = findings.len();
    if start > count {
        return Err(InstallCheckError::Cursor);
    }
    let limit = usize::try_from(limit).map_err(|_| InstallCheckError::Cursor)?;
    let end = start
        .checked_add(limit)
        .ok_or(InstallCheckError::Cursor)?
        .min(count);
    let selected = findings
        .get(start..end)
        .ok_or(InstallCheckError::Cursor)?
        .to_vec();
    let truncated = end < count;
    let next_cursor = truncated.then(|| format!("v1.{digest_text}.{end}"));
    Ok(InstallCheckPage {
        findings: selected,
        count,
        truncated,
        next_cursor,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn install_cursor_is_digest_bound_and_canonical() {
        let digest = FileDigest::from_bytes([0xab; 32]);
        let empty = Vec::new();
        assert!(matches!(
            page(
                empty.clone(),
                digest,
                10,
                Some(
                    "v1.sha256:0000000000000000000000000000000000000000000000000000000000000000.0"
                )
            ),
            Err(InstallCheckError::StaleCursor)
        ));
        assert!(matches!(
            page(empty.clone(), digest, 10, Some(&format!("v1.{digest}.00"))),
            Err(InstallCheckError::Cursor)
        ));
        assert!(matches!(
            page(empty.clone(), digest, 10, Some(&format!("v1.{digest}.1"))),
            Err(InstallCheckError::Cursor)
        ));
        let page =
            page(empty, digest, 10, Some(&format!("v1.{}.0", digest))).expect("canonical cursor");
        assert_eq!(page.count, 0);
        assert!(!page.truncated);
    }
}
