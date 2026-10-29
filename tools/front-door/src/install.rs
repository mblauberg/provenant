//! Process-edge wiring for installation. State operations stay in config;
//! every child is supervised with a bounded wait and isolated environment.
use crate::release_smoke::SystemSmoke;
use crate::roots::{self, Product, Resolved, Workspace};
use provenant_cli as cli;
use provenant_config::shared_settings::SharedSettings;
use provenant_config::{
    AccountHome, Action, HarnessTarget, InstallContext, InstallError, InstallFinding,
    InstallOutcome, InstallReporter, ManualInclude, Targets,
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_runtime::peer_agent::{AgentError, launchctl};
use provenant_os::clock::SystemClock;
use provenant_records::{CommandError, CommandResult, Harness, Outcome};
use serde::Serialize;
use std::ffi::OsStr;
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
enum EdgeError {
    Root(roots::RootError),
    Install(InstallError),
    Home,
    Peer(provenant_fabric_runtime::peer_agent::AgentError),
}

fn prepare_agents(targets: &mut Targets) -> Result<(), EdgeError> {
    prepare_agents_with(targets, true)
}

/// `strict` false (uninstall) ignores a configuration defect, which only the
/// optional scheduled-gc agent reads.
fn prepare_agents_with(targets: &mut Targets, strict: bool) -> Result<(), EdgeError> {
    let state = targets.roots.state.as_path();
    if provenant_fabric_runtime::peer_agent::eligible(state, &targets.home) {
        let program = targets.bin_dir.join("provenant");
        let previous = provenant_config::installed_binary(targets)
            .map_err(EdgeError::Install)?
            .unwrap_or_else(|| program.clone());
        targets.peer_agent = Some(
            provenant_fabric_runtime::peer_agent::install_plist(
                &program,
                &targets.roots,
                peer_agent_registry_name,
            )
            .map_err(EdgeError::Peer)?,
        );
        targets.peer_agent_previous =
            provenant_fabric_runtime::peer_agent::previous_plist(&previous, state, &targets.home)
                .map_err(EdgeError::Peer)?;
        let config =
            match provenant_config::read_with_domains(&targets.roots, &cli::domains::SECTIONS) {
                Ok(config) => config,
                Err(_) if !strict => return Ok(()),
                Err(error) => return Err(EdgeError::Install(InstallError::Config(error))),
            };
        if config.gc.schedule {
            targets.gc_agent = Some(
                super::gc_agent::render(&program, state, &config.gc).map_err(EdgeError::Peer)?,
            );
        }
    }
    Ok(())
}

fn peer_agent_registry_name(path: &Path) -> std::ffi::OsString {
    provenant_confinement::worktrees::state_dir_name(path)
}

fn environment(globals: &cli::Globals) -> Result<Resolved, EdgeError> {
    roots::resolve(globals, Workspace::Project, Product::Required).map_err(|error| match error {
        roots::RootError::Home => EdgeError::Home,
        error => EdgeError::Root(error),
    })
}

fn selected_targets(
    globals: &cli::Globals,
    harnesses: &[Harness],
) -> Result<(Resolved, Targets), EdgeError> {
    let environment = environment(globals)?;
    let mut targets = cli::install::configured_targets(
        environment.roots.clone(),
        &environment.home,
        harnesses,
        environment.claude_config.as_deref(),
        environment.codex_home.as_deref(),
    )
    .map_err(EdgeError::Install)?;
    if !environment
        .assets
        .starts_with(targets.roots.state.as_path().join("releases"))
    {
        targets.protected_roots.push(environment.assets.clone());
    }
    let (accounts, shared) =
        account_config(&targets.roots, &environment.home).map_err(EdgeError::Install)?;
    targets.accounts = accounts;
    targets.shared_settings = shared;
    Ok((environment, targets))
}

/// Largest shared settings source read.
const SETTINGS_SOURCE_LIMIT: u64 = 1024 * 1024;

/// Registered account directories of every adapter and the shared settings.
/// Registration, not directory discovery, decides which accounts install keeps
/// in sync. A defect here fails install, upgrade and sync; uninstall never asks.
fn account_config(
    roots: &provenant_core::Roots,
    home: &Path,
) -> Result<(Vec<AccountHome>, Option<SharedSettings>), InstallError> {
    let config = provenant_config::read_with_domains(roots, &cli::domains::SECTIONS)?;
    let accounts = &config.domains.accounts;
    Ok((
        cli::accounts::homes(accounts),
        shared_settings(accounts, home)?,
    ))
}

/// The allow-listed values of the source file, with each account's overrides.
/// `None` when there is no source and no override, so nothing is synced.
pub(crate) fn shared_settings(
    accounts: &provenant_providers::accounts::Settings,
    home: &Path,
) -> Result<Option<SharedSettings>, InstallError> {
    if let Some(code) = accounts.settings_error() {
        return Err(InstallError::InvalidConfig(code));
    }
    let configured = accounts
        .shared_settings
        .as_ref()
        .and_then(|shared| shared.source.clone());
    let explicit = configured.is_some();
    let source_file = configured.unwrap_or_else(|| home.join(".claude/settings.json"));
    let keys = accounts.settings_keys();
    let mut source = std::collections::BTreeMap::new();
    match std::fs::File::open(&source_file) {
        Ok(file) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(SETTINGS_SOURCE_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| InstallError::InvalidConfig("shared settings source is unreadable"))?;
            if bytes.len() as u64 > SETTINGS_SOURCE_LIMIT {
                return Err(InstallError::InvalidConfig(
                    "shared settings source is larger than 1 MiB",
                ));
            }
            let serde_json::Value::Object(object) =
                serde_json::from_slice(&bytes).map_err(|_| {
                    InstallError::InvalidConfig("shared settings source is not a JSON object")
                })?
            else {
                return Err(InstallError::InvalidConfig(
                    "shared settings source is not a JSON object",
                ));
            };
            for key in &keys {
                if let Some(value) = object.get(key) {
                    provenant_config::shared_settings::check_value(key, value).map_err(|reason| {
                        InstallError::InvalidConfig(provenant_config::shared_settings::interned(
                            format!(
                                "shared settings source: {reason}; remove it or drop `{key}` from `keys`"
                            ),
                        ))
                    })?;
                    source.insert(key.clone(), value.clone());
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && !explicit => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(InstallError::InvalidConfig(
                "shared settings source file does not exist",
            ));
        }
        Err(_) => {
            return Err(InstallError::InvalidConfig(
                "shared settings source is unreadable",
            ));
        }
    }
    let overrides: std::collections::BTreeMap<_, _> = accounts
        .entries
        .values()
        .filter(|entry| entry.adapter == "claude" && !entry.settings.is_empty())
        .map(|entry| {
            (
                provenant_config::shared_settings::canonical(&entry.directory),
                entry.settings.clone().into_iter().collect(),
            )
        })
        .collect();
    if source.is_empty() && overrides.is_empty() && !explicit {
        return Ok(None);
    }
    Ok(Some(SharedSettings {
        source_file,
        source,
        keys,
        overrides,
    }))
}

/// Resolves targets as install does, so `CLAUDE_CONFIG_DIR`/`CODEX_HOME`
/// select the matching recorded target. Without them, uninstall acts on every
/// recorded target of each harness, at its recorded paths.
fn receipt_targets(
    globals: &cli::Globals,
    harnesses: &[Harness],
) -> Result<(Targets, Option<String>), EdgeError> {
    let environment = environment(globals)?;
    // A defect in the configuration (an invalid account section, say) must not
    // stop removal: fall back to the default targets and say so.
    let mut warning = None;
    let mut targets = match cli::install::configured_targets(
        environment.roots.clone(),
        &environment.home,
        harnesses,
        environment.claude_config.as_deref(),
        environment.codex_home.as_deref(),
    ) {
        Ok(targets) => targets,
        Err(InstallError::Config(error)) => {
            warning = Some(format!(
                "warning: configuration ignored for uninstall ({error}); removing recorded targets only"
            ));
            let mut targets = Targets::new(environment.roots, &environment.home, harnesses);
            targets
                .apply_environment(
                    environment.claude_config.as_deref(),
                    environment.codex_home.as_deref(),
                )
                .map_err(EdgeError::Install)?;
            targets
        }
        Err(error) => return Err(EdgeError::Install(error)),
    };
    targets
        .resolve_destinations(&provenant_os::fs::SystemPathResolver)
        .map_err(EdgeError::Install)?;
    Ok((targets, warning))
}

#[derive(Default)]
struct PreviewReporter {
    home: Option<PathBuf>,
}
impl PreviewReporter {
    fn before_remove_with(
        &self,
        action: &Action,
        stop: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        if matches!(action.selector.as_str(), "peer-agent" | "gc-agent") {
            let home = self
                .home
                .as_deref()
                .ok_or_else(|| io::Error::other("missing agent home"))?;
            let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            let expected = if action.selector == "gc-agent" {
                super::gc_agent::plist(&home)
            } else {
                provenant_fabric_runtime::peer_agent::plist(&home)
            };
            if action.before.is_some() && action.path.to_path_buf().ok().as_ref() != Some(&expected)
            {
                return Err(io::Error::other("unexpected agent plist path"));
            }
            stop(&home)?;
        }
        Ok(())
    }
}
impl InstallReporter for PreviewReporter {
    fn before_remove(&mut self, action: &Action) -> io::Result<()> {
        self.before_remove_with(action, |home| {
            if action.selector == "gc-agent" {
                super::gc_agent::stop(home).map_err(io::Error::other)
            } else {
                provenant_fabric_runtime::peer_agent::stop(home).map_err(io::Error::other)
            }
        })
    }
    fn preview(&mut self, _: &[Action]) -> io::Result<()> {
        Ok(())
    }
    fn findings(&mut self, _: &[InstallFinding]) -> io::Result<()> {
        Ok(())
    }
    fn manual_includes(&mut self, _: &[ManualInclude]) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct InstallData {
    outcome: Option<InstallOutcome>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    completed: Vec<Action>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    remaining: Vec<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gc_agent: Option<super::gc_agent::Installed>,
}
impl InstallData {
    fn empty() -> Self {
        Self {
            outcome: None,
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
            gc_agent: None,
        }
    }
    fn success(outcome: InstallOutcome) -> Self {
        Self {
            outcome: Some(outcome),
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
            gc_agent: None,
        }
    }
    fn failure(error: &InstallError) -> Self {
        match error {
            InstallError::Partial {
                completed,
                remaining,
                ..
            } => Self {
                outcome: None,
                completed: completed.clone(),
                remaining: remaining.clone(),
                peer_agent: None,
                gc_agent: None,
            },
            _ => Self::empty(),
        }
    }
}

fn error_class(error: &InstallError) -> (Outcome, &'static str) {
    match error {
        InstallError::HooksUnavailable => (Outcome::Refused, "HOOKS_UNAVAILABLE"),
        InstallError::Conflict(_)
        | InstallError::PeerAgentConflict(_)
        | InstallError::Drift(_)
        | InstallError::Tampered(_)
        | InstallError::ReleaseConflict { .. } => (Outcome::Refused, "INSTALL_REFUSED"),
        InstallError::Partial { source, .. } => error_class(source),
        InstallError::Config(provenant_config::ConfigError::Read { .. }) => {
            (Outcome::Error, "INSTALL_ERROR")
        }
        InstallError::Config(_) => (Outcome::Invalid, "INSTALL_INVALID"),
        InstallError::Path(_)
        | InstallError::Manifest(_)
        | InstallError::Platform
        | InstallError::HarnessSelection
        | InstallError::InvalidConfig(_)
        | InstallError::ConfigFile(_)
        | InstallError::Decode(_)
        | InstallError::ReceiptDecode(_)
        | InstallError::Encode(_)
        | InstallError::PathWire(_) => (Outcome::Invalid, "INSTALL_INVALID"),
        InstallError::Fs(_)
        | InstallError::Preview(_)
        | InstallError::Smoke(_)
        | InstallError::AgentRetirement(_) => (Outcome::Error, "INSTALL_ERROR"),
    }
}
pub(super) fn install_error(command: &str, error: &InstallError, globals: &cli::Globals) -> u8 {
    let result = install_error_result(command, error);
    super::emit(&result, globals, "", false)
}
fn install_error_result(command: &str, error: &InstallError) -> CommandResult<InstallData> {
    let (status, code) = error_class(error);
    let mut result = CommandResult::new(command, status, InstallData::failure(error));
    result.error = Some(CommandError {
        code: code.into(),
        message: error.to_string(),
        cause: None,
        repair: matches!(error, InstallError::Partial { .. })
            .then(|| "Resolve the failure and re-apply the same install command".into()),
    });
    result
}
fn edge_error(command: &str, error: EdgeError, globals: &cli::Globals) -> u8 {
    match error {
        EdgeError::Install(error) => install_error(command, &error, globals),
        EdgeError::Root(error) => super::fail(
            command,
            Outcome::Invalid,
            "INSTALL_ROOT",
            &error.to_string(),
            globals,
        ),
        EdgeError::Peer(error) => super::fail(
            command,
            Outcome::Error,
            "PEER_AGENT",
            &error.to_string(),
            globals,
        ),
        EdgeError::Home => super::fail(
            command,
            Outcome::Invalid,
            "HOME",
            "HOME must be an absolute directory",
            globals,
        ),
    }
}

fn context<'a>(
    clock: &'a SystemClock,
    cancel: &'a CancelToken,
) -> Result<InstallContext<'a>, String> {
    let budget = WaitBudget::cli(clock.mono_now(), 30)
        .map_err(|_| "invalid install wait budget".to_owned())?;
    Ok(InstallContext {
        clock,
        budget,
        cancel,
    })
}
fn output_install(
    command: &str,
    summaries: &[HarnessTarget],
    result: Result<InstallOutcome, InstallError>,
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    gc_agent: Option<super::gc_agent::Installed>,
    globals: &cli::Globals,
) -> u8 {
    output_install_noted(
        "", command, summaries, result, peer_agent, gc_agent, globals,
    )
}
/// `notes` lead the human output, for steps that ran before the install.
fn output_install_noted(
    notes: &str,
    command: &str,
    summaries: &[HarnessTarget],
    result: Result<InstallOutcome, InstallError>,
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    gc_agent: Option<super::gc_agent::Installed>,
    globals: &cli::Globals,
) -> u8 {
    match result {
        Ok(outcome) => {
            let (mut links, mut hooks, mut mcp) = (0, 0, 0);
            for action in &outcome.actions {
                let selector = action.selector.to_ascii_lowercase();
                if selector.contains("hook") {
                    hooks += 1;
                } else if selector.contains("mcp") {
                    mcp += 1;
                } else if matches!(
                    action.before.as_ref().or(action.after.as_ref()),
                    Some(provenant_records::OwnedValue::Link { .. })
                ) {
                    links += 1;
                }
            }
            let mut human = format!(
                "{notes}{command}: {} changes ({links} links, {hooks} hooks, {mcp} MCP); {} targets",
                outcome.actions.len(),
                summaries.len(),
            );
            for (count, label) in [
                (outcome.findings.len(), "findings"),
                (outcome.manual_includes.len(), "manual includes"),
            ] {
                if count > 0 {
                    human.push_str(&format!("; {count} {label}"));
                }
            }
            human.push('\n');
            if !outcome
                .findings
                .iter()
                .any(|finding| finding.rule == "state_root_moved")
                && outcome
                    .findings
                    .iter()
                    .any(|finding| finding.rule == "relocated_stale_entry")
            {
                human.push_str("  Home moved; old paths left untouched\n");
            }
            for finding in &outcome.findings {
                let path = finding
                    .path
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                if finding.rule == "state_root_moved" {
                    human.push_str(&format!(
                        "  State root moved from {path}; old paths left untouched\n"
                    ));
                } else if let Some(detail) = &finding.detail {
                    human.push_str(&format!("  {}: {path} ({detail})\n", finding.rule));
                } else {
                    human.push_str(&format!("  {}: {path}\n", finding.rule));
                }
            }
            if outcome
                .findings
                .iter()
                .any(|finding| finding.rule == "settings_conflict")
            {
                human.push_str(
                    "  A shared setting was edited in an account and was not overwritten.\n  Keep the account's value: `provenant accounts sync --adopt <account>`\n  Use the source's value: `provenant accounts sync --force-settings`\n",
                );
            }
            if outcome
                .findings
                .iter()
                .any(|finding| finding.rule == "orphaned_account")
            {
                human.push_str(
                    "  An install target's account is no longer registered and was not synced.\n  Remove its managed content: `provenant accounts sync --prune-orphans`\n",
                );
            }
            for manual in &outcome.manual_includes {
                let path = manual
                    .path
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                let include = manual
                    .include
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                human.push_str(&format!(
                    "manual include required: add {include} to {path}\n"
                ));
            }
            if let Some(provenant_fabric_runtime::peer_agent::Installed::Failed { message }) =
                &peer_agent
            {
                human.push_str(&format!("peer agent: failed: {message}\n"));
            }
            if let Some(super::gc_agent::Installed::Failed { message }) = &gc_agent {
                human.push_str(&format!("scheduled gc: failed: {message}\n"));
            }
            let read_only = outcome.dry_run;
            let mut data = InstallData::success(outcome);
            data.peer_agent = peer_agent;
            data.gc_agent = gc_agent;
            super::emit(
                &CommandResult::new(command, Outcome::Ok, data),
                globals,
                &human,
                read_only,
            )
        }
        Err(error) => install_error(command, &error, globals),
    }
}

fn read_install_receipt(
    targets: &Targets,
) -> Result<Option<provenant_records::InstallReceipt>, InstallError> {
    let path = targets.roots.state.as_path().join("install/receipt.json");
    match std::fs::File::open(&path) {
        Ok(file) => provenant_records::decode_v1(file, provenant_records::Limits::CONFIG)
            .map(Some)
            .map_err(InstallError::ReceiptDecode),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::Fs(provenant_os::fs::FsError::Io {
            operation: "read install receipt",
            path,
            source,
        })),
    }
}

/// Recorded account directories that are no longer registered. Only judged when
/// the user registers accounts of that harness at all: with none registered,
/// recorded directories are the detected ones and stay in sync as before.
fn orphaned_targets(
    environment: &Resolved,
    base: &Targets,
    receipt: &provenant_records::InstallReceipt,
) -> Vec<provenant_records::ResolvedHarnessTarget> {
    let canonical = |path: &Path| provenant_config::shared_settings::canonical(path);
    let home = canonical(&environment.home);
    receipt
        .harness_targets
        .iter()
        .filter(|record| {
            let base_dir = match record.harness {
                Harness::Claude => ".claude",
                Harness::Codex => ".codex",
                _ => return false,
            };
            let Ok(skills) = record.skills_dir.to_path_buf() else {
                return false;
            };
            let Some(directory) = skills.parent().map(canonical) else {
                return false;
            };
            let env_dir = match record.harness {
                Harness::Claude => environment.claude_config.as_deref(),
                _ => environment.codex_home.as_deref(),
            };
            base.accounts.iter().any(|a| a.harness == record.harness)
                && directory != home.join(base_dir)
                && env_dir.is_none_or(|dir| canonical(dir) != directory)
                && !base
                    .accounts
                    .iter()
                    .any(|a| a.harness == record.harness && canonical(&a.home) == directory)
        })
        .cloned()
        .collect()
}

struct Batches {
    targets: Vec<Targets>,
    /// Registered accounts install had to skip, reported as findings.
    findings: Vec<InstallFinding>,
}

/// The selected, recorded or detected targets plus one batch for every
/// registered account directory of a selected harness, so a new or upgraded
/// install leaves every account with the same managed content.
fn install_batches(globals: &cli::Globals, args: &cli::Install) -> Result<Batches, EdgeError> {
    let mut targets = detected_batches(globals, args)?;
    let (environment, base) = selected_targets(globals, &[])?;
    let mut findings = Vec::new();
    if args.harness.is_empty()
        && let Some(receipt) = read_install_receipt(&base).map_err(EdgeError::Install)?
    {
        for record in orphaned_targets(&environment, &base, &receipt) {
            findings.push(InstallFinding {
                rule: "orphaned_account".into(),
                detail: None,
                path: record.skills_dir.clone(),
            });
        }
    }
    let bin_dir = targets
        .first()
        .map_or_else(|| base.bin_dir.clone(), |batch| batch.bin_dir.clone());
    for account in &base.accounts {
        if !args.harness.is_empty() && !args.harness.contains(&account.harness) {
            continue;
        }
        let target = account.target(&base.home);
        if targets
            .iter()
            .flat_map(|batch| &batch.harnesses)
            .any(|known| known.harness == target.harness && known.skills_dir == target.skills_dir)
        {
            continue;
        }
        if !std::fs::metadata(&account.home).is_ok_and(|metadata| metadata.is_dir()) {
            findings.push(InstallFinding {
                rule: "missing_account_directory".into(),
                detail: None,
                path: provenant_records::AbsolutePath::new(&account.home)
                    .map_err(|error| EdgeError::Install(error.into()))?,
            });
            continue;
        }
        let mut batch = base.clone();
        batch.bin_dir = bin_dir.clone();
        batch.harnesses = vec![target];
        targets.push(batch);
    }
    Ok(Batches { targets, findings })
}

fn detected_batches(
    globals: &cli::Globals,
    args: &cli::Install,
) -> Result<Vec<Targets>, EdgeError> {
    if !args.harness.is_empty() {
        return selected_targets(globals, &args.harness).map(|(_, targets)| vec![targets]);
    }
    // Resolve config and roots without validating an empty selection.
    let (environment, base) = selected_targets(globals, &[])?;
    if let Some(receipt) = read_install_receipt(&base).map_err(EdgeError::Install)? {
        let bin = receipt
            .bin_dir
            .to_path_buf()
            .map_err(InstallError::from)
            .map_err(EdgeError::Install)?;
        let orphans = orphaned_targets(&environment, &base, &receipt);
        return receipt
            .harness_targets
            .into_iter()
            .filter(|record| !orphans.contains(record))
            .map(|record| {
                let mut batch = base.clone();
                batch.bin_dir = bin.clone();
                let convert = || -> Result<HarnessTarget, InstallError> {
                    Ok(HarnessTarget {
                        harness: record.harness,
                        skills_dir: record.skills_dir.to_path_buf()?,
                        mcp_file: record.mcp_file.to_path_buf()?,
                        hook_file: record
                            .hook_file
                            .map(|path| path.to_path_buf())
                            .transpose()?,
                        instruction: record
                            .instruction
                            .map(|path| path.to_path_buf())
                            .transpose()?,
                    })
                };
                batch.harnesses = vec![convert().map_err(EdgeError::Install)?];
                Ok(batch)
            })
            .collect();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut batches = Vec::new();
    for (harness, executable) in [
        (Harness::Claude, "claude"),
        (Harness::Codex, "codex"),
        (Harness::Cursor, "cursor-agent"),
        (Harness::Agy, "agy"),
        (Harness::Opencode, "opencode"),
    ] {
        if std::env::split_paths(&path)
            .any(|dir| provenant_os::sys::is_executable(&dir.join(executable)))
        {
            batches.push(selected_targets(globals, &[harness])?.1);
        }
    }
    let mut directories: Vec<_> = std::fs::read_dir(&environment.home)
        .map_err(|source| EdgeError::Install(InstallError::Smoke(source)))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".claude-")
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .map(|entry| entry.path())
        .collect();
    directories.sort();
    for directory in directories {
        let mut batch = base.clone();
        let mut defaults = Targets::new(
            environment.roots.clone(),
            &environment.home,
            &[Harness::Claude],
        );
        defaults
            .apply_environment(Some(&directory), None)
            .map_err(EdgeError::Install)?;
        batch.harnesses = defaults.harnesses;
        if !batches
            .iter()
            .any(|existing| existing.harnesses[0].skills_dir == batch.harnesses[0].skills_dir)
        {
            batches.push(batch);
        }
    }
    Ok(batches)
}

/// A refusal before the install ran, rendered as the command's own error.
enum Refused {
    Edge(EdgeError),
    Install(InstallError),
    Fail(Outcome, &'static str, String),
}
impl From<EdgeError> for Refused {
    fn from(error: EdgeError) -> Self {
        Self::Edge(error)
    }
}
impl From<InstallError> for Refused {
    fn from(error: InstallError) -> Self {
        Self::Install(error)
    }
}
impl Refused {
    fn emit(self, command: &str, globals: &cli::Globals) -> u8 {
        match self {
            Self::Edge(error) => edge_error(command, error, globals),
            Self::Install(error) => install_error(command, &error, globals),
            Self::Fail(status, code, message) => {
                super::fail(command, status, code, &message, globals)
            }
        }
    }
    fn message(&self) -> String {
        match self {
            Self::Edge(EdgeError::Root(error)) => error.to_string(),
            Self::Edge(EdgeError::Install(error)) | Self::Install(error) => error.to_string(),
            Self::Edge(EdgeError::Peer(error)) => error.to_string(),
            Self::Edge(EdgeError::Home) => "HOME must be an absolute directory".into(),
            Self::Fail(_, _, message) => message.clone(),
        }
    }
}

/// What one pass over every batch produced, before any output is rendered.
struct Applied {
    result: Result<InstallOutcome, InstallError>,
    summaries: Vec<HarnessTarget>,
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    gc_agent: Option<super::gc_agent::Installed>,
}

/// What a sync does beyond reapplying the release.
#[derive(Clone, Copy, Default)]
struct SyncOptions {
    /// Overwrite shared settings edited in an account since the last sync.
    force_settings: bool,
}

pub(super) fn run_install(args: cli::Install, globals: &cli::Globals) -> u8 {
    match apply_install(args, globals, true, SyncOptions::default()) {
        Ok(applied) => output_install(
            "install",
            &applied.summaries,
            applied.result,
            applied.peer_agent,
            applied.gc_agent,
            globals,
        ),
        Err(refused) => refused.emit("install", globals),
    }
}

/// Installs every batch. `agents` also settles the per-user LaunchAgents,
/// which only a new release needs; a sync leaves them running as they are.
fn apply_install(
    mut args: cli::Install,
    globals: &cli::Globals,
    agents: bool,
    options: SyncOptions,
) -> Result<Applied, Refused> {
    args.source = provenant_core::PathResolver::resolve_existing_ancestor(
        &provenant_os::fs::SystemPathResolver,
        &args.source,
    )
    .map_err(|source| {
        InstallError::Fs(provenant_os::fs::FsError::Io {
            operation: "resolve install source",
            path: args.source.clone(),
            source,
        })
    })?;
    let Batches {
        targets: mut batches,
        findings: skipped,
    } = install_batches(globals, &args)?;
    for targets in &mut batches {
        targets.bin_dir = args
            .bin_dir
            .clone()
            .unwrap_or_else(|| targets.bin_dir.clone());
        targets.resolve_destinations(&provenant_os::fs::SystemPathResolver)?;
    }
    // A registered account may also be a default or recorded target.
    let mut seen = std::collections::BTreeSet::new();
    for batch in &mut batches {
        batch
            .harnesses
            .retain(|target| seen.insert((target.harness, target.skills_dir.clone())));
    }
    batches.retain(|batch| !batch.harnesses.is_empty());
    for batch in &mut batches {
        batch.force_settings = options.force_settings;
    }
    let summaries: Vec<_> = batches
        .iter()
        .flat_map(|batch| batch.harnesses.clone())
        .collect();
    let Some(mut targets) = batches.last().cloned() else {
        return Err(Refused::Fail(
            Outcome::Invalid,
            "NO_HARNESSES",
            "No harness CLIs, Claude config directories or registered accounts found; select --harness".into(),
        ));
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = context(&clock, &cancel)
        .map_err(|error| Refused::Fail(Outcome::Error, "WAIT_BUDGET", error))?;
    let smoke = SystemSmoke::new(None)
        .map_err(|error| Refused::Fail(Outcome::Error, "SMOKE_EXECUTOR", error.to_string()))?;
    let state = targets.roots.state.as_path().to_path_buf();
    let home = targets.home.clone();
    prepare_agents(&mut targets)?;
    let gc_enabled = targets.gc_agent.is_some();
    let mut reporter = PreviewReporter {
        home: Some(home.clone()),
    };
    let result = (|| {
        let mut combined = InstallOutcome {
            actions: Vec::new(),
            findings: skipped,
            manual_includes: Vec::new(),
            dry_run: args.dry_run,
        };
        // Refuse predictable conflicts before publishing the first target's receipt.
        for batch in &mut batches {
            prepare_agents(batch).map_err(|error| match error {
                EdgeError::Install(error) => error,
                _ => InstallError::InvalidConfig("cannot prepare install agents"),
            })?;
        }
        let supports_hooks = |batch: &Targets| {
            batch.harnesses.iter().any(|target| {
                cli::hooks::HOOK_SPECS
                    .iter()
                    .any(|spec| spec.registration.harness == Some(target.harness))
            })
        };
        if matches!(args.hooks, Some(cli::Switch::On)) && !batches.iter().any(supports_hooks) {
            return Err(InstallError::HooksUnavailable);
        }
        let batch_args = |batch: &Targets, dry_run| cli::Install {
            bin_dir: Some(batch.bin_dir.clone()),
            hooks: if matches!(args.hooks, Some(cli::Switch::On)) && !supports_hooks(batch) {
                None
            } else {
                args.hooks
            },
            dry_run,
            ..args.clone()
        };
        for batch in &batches {
            cli::install::execute_install(
                &batch_args(batch, true),
                batch.clone(),
                &context,
                &mut reporter,
                &smoke,
                env!("PROVENANT_TARGET_TRIPLE"),
            )?;
        }
        if agents && !args.dry_run && provenant_fabric_runtime::peer_agent::eligible(&state, &home)
        {
            retire_disk_guard(&home)
                .map_err(|error| InstallError::AgentRetirement(error.to_string()))?;
        }
        for batch in batches {
            // Publish one receipt target at a time, using config's existing lock and ownership rules.
            let settings = batch_args(&batch, args.dry_run);
            let outcome = cli::install::execute_install(
                &settings,
                batch,
                &context,
                &mut reporter,
                &smoke,
                env!("PROVENANT_TARGET_TRIPLE"),
            )
            .map_err(|error| {
                let (mut completed, remaining, source) = match error {
                    InstallError::Partial {
                        completed,
                        remaining,
                        source,
                    } => (completed, remaining, source),
                    error => (Vec::new(), Vec::new(), Box::new(error)),
                };
                completed.splice(0..0, combined.actions.clone());
                InstallError::Partial {
                    completed,
                    remaining,
                    source,
                }
            })?;
            combined.actions.extend(outcome.actions);
            combined.findings.extend(outcome.findings);
            combined.manual_includes.extend(outcome.manual_includes);
        }
        if !args.dry_run {
            let inspection = provenant_config::inspect_install(
                &targets,
                None,
                env!("PROVENANT_TARGET_TRIPLE"),
                &smoke,
                &cancel,
            )?;
            if let Some(error) = inspection.error {
                return Err(error);
            }
            combined.findings.extend(inspection.findings);
        }
        // The plan and the final inspection can name the same finding.
        let mut seen = std::collections::BTreeSet::new();
        combined.findings.retain(|finding| {
            seen.insert((
                finding.rule.clone(),
                finding.path.to_path_buf().ok(),
                finding.detail.clone(),
            ))
        });
        Ok(combined)
    })();
    // The release is installed first; the agent then serves the new binary.
    let peer_agent = match &result {
        Ok(outcome) if agents && !outcome.dry_run => {
            Some(provenant_fabric_runtime::peer_agent::install(
                &state,
                &home,
                outcome
                    .actions
                    .iter()
                    .any(|action| action.selector == "peer-agent"),
            ))
        }
        _ => None,
    };
    let gc_agent = match &result {
        Ok(outcome) if agents && !outcome.dry_run => Some(super::gc_agent::install(
            &state,
            &home,
            outcome
                .actions
                .iter()
                .any(|action| action.selector == "gc-agent"),
            gc_enabled,
        )),
        _ => None,
    };
    Ok(Applied {
        result,
        summaries,
        peer_agent,
        gc_agent,
    })
}

/// Reapplies the installed release to every registered account directory and
/// recorded target. `None` when nothing is installed, so nothing can drift.
fn sync_accounts(
    globals: &cli::Globals,
    dry_run: bool,
    options: SyncOptions,
) -> Result<Option<Applied>, Refused> {
    let (_, targets) = selected_targets(globals, &[])?;
    let Some(receipt) = read_install_receipt(&targets)? else {
        return Ok(None);
    };
    let source =
        provenant_config::release_root(targets.roots.state.as_path(), receipt.source_digest);
    let args = cli::Install {
        source,
        harness: Vec::new(),
        bin_dir: None,
        hooks: None,
        adopt_existing: false,
        dry_run,
    };
    apply_install(args, globals, false, options).map(Some)
}

/// Copies the locally edited shared settings of `account` into the source file
/// and returns the keys moved. The source account itself cannot be adopted.
fn adopt_settings(globals: &cli::Globals, account: &str) -> Result<Vec<String>, String> {
    let (_, targets) = selected_targets(globals, &[]).map_err(|e| Refused::from(e).message())?;
    let shared = targets
        .shared_settings
        .as_ref()
        .ok_or("no shared settings are configured")?;
    let config = provenant_config::read_with_domains(&targets.roots, &cli::domains::SECTIONS)
        .map_err(|error| error.to_string())?;
    let accounts = &config.domains.accounts;
    let id = cli::accounts::resolve(accounts, account)?;
    let entry = accounts.entries.get(&id).ok_or("unknown account")?;
    if entry.adapter != "claude" {
        return Err("only Claude accounts have shared settings".into());
    }
    let canonical = |path: &Path| provenant_config::shared_settings::canonical(path);
    let file = canonical(&entry.directory).join("settings.json");
    if file == canonical(&shared.source_file) {
        return Err("that account holds the source settings; there is nothing to adopt".into());
    }
    let read = |path: &Path| -> Result<Vec<u8>, String> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(SETTINGS_SOURCE_LIMIT + 1).read_to_end(&mut bytes))
            .map_err(|error| format!("cannot read {}: {}", path.display(), error.kind()))?;
        if bytes.len() as u64 > SETTINGS_SOURCE_LIMIT {
            return Err(format!("{} is larger than 1 MiB", path.display()));
        }
        Ok(bytes)
    };
    let account_bytes = read(&file)?;
    let source_bytes = read(&shared.source_file)?;
    let synced: std::collections::BTreeMap<String, serde_json::Value> =
        read_install_receipt(&targets)
            .map_err(|error| error.to_string())?
            .into_iter()
            .flat_map(|receipt| receipt.entries)
            .filter(|e| e.path.to_path_buf().is_ok_and(|p| canonical(&p) == file))
            .filter_map(|e| match e.expected {
                provenant_records::OwnedValue::Selector { value } => e
                    .selector
                    .strip_prefix("setting:")
                    .map(|key| (key.to_owned(), value)),
                _ => None,
            })
            .collect();
    let overridden = entry.settings.keys().cloned().collect();
    let (bytes, adopted) = provenant_config::shared_settings::adopt(
        &source_bytes,
        &account_bytes,
        &shared.keys,
        &synced,
        &overridden,
    )
    .map_err(|error| error.to_string())?;
    if !adopted.is_empty() {
        provenant_config::shared_settings::publish_source(
            &shared.source_file,
            &source_bytes,
            &bytes,
            || {},
        )
        .map_err(|error| match error {
            InstallError::Drift(path) => format!(
                "{} changed while adopting; nothing was written. Rerun `accounts sync --adopt`",
                path.display()
            ),
            other => format!("cannot write the source settings: {other}"),
        })?;
    }
    Ok(adopted)
}

/// Removes managed content from recorded targets that are no longer registered.
fn prune_orphans(globals: &cli::Globals) -> Result<Vec<String>, String> {
    let (environment, base) =
        selected_targets(globals, &[]).map_err(|e| Refused::from(e).message())?;
    let Some(receipt) = read_install_receipt(&base).map_err(|e| e.to_string())? else {
        return Ok(Vec::new());
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = context(&clock, &cancel)?;
    let mut pruned = Vec::new();
    for record in orphaned_targets(&environment, &base, &receipt) {
        let directory = record
            .skills_dir
            .to_path_buf()
            .map_err(|e| e.to_string())?
            .parent()
            .map(Path::to_path_buf)
            .ok_or("invalid recorded target")?;
        let mut targets = receipt_targets(globals, &[record.harness])
            .map_err(|e| Refused::from(e).message())?
            .0;
        match record.harness {
            Harness::Claude => targets.apply_environment(Some(&directory), None),
            _ => targets.apply_environment(None, Some(&directory)),
        }
        .map_err(|e| e.to_string())?;
        let mut reporter = PreviewReporter {
            home: Some(targets.home.clone()),
        };
        let outcome = cli::install::execute_uninstall(
            &cli::Uninstall {
                harness: vec![record.harness],
            },
            &targets,
            &context,
            &mut reporter,
        )
        .map_err(|e| e.to_string())?;
        pruned.push(format!(
            "{} ({} changes, {} findings)",
            directory.display(),
            outcome.actions.len(),
            outcome.findings.len()
        ));
    }
    Ok(pruned)
}

/// `accounts sync`: the repair for what `check install` and `accounts list` report.
pub(super) fn run_sync(
    dry_run: bool,
    adopt: Option<&str>,
    force_settings: bool,
    prune: bool,
    globals: &cli::Globals,
) -> u8 {
    let mut notes = String::new();
    if let Some(account) = adopt {
        if dry_run {
            notes.push_str("dry run: --adopt not applied\n");
        } else {
            match adopt_settings(globals, account) {
                Ok(keys) if keys.is_empty() => {
                    notes.push_str("adopted no settings: nothing differs in that account\n");
                }
                Ok(keys) => {
                    notes.push_str(&format!(
                        "adopted settings into the source: {}\n",
                        keys.join(", ")
                    ));
                }
                Err(message) => {
                    return super::fail(
                        "accounts sync",
                        Outcome::Invalid,
                        "SYNC_ADOPT",
                        &message,
                        globals,
                    );
                }
            }
        }
    }
    if prune && !dry_run {
        match prune_orphans(globals) {
            Ok(pruned) => {
                for directory in pruned {
                    notes.push_str(&format!("removed managed content from {directory}\n"));
                }
            }
            Err(message) => {
                return super::fail(
                    "accounts sync",
                    Outcome::Error,
                    "SYNC_PRUNE",
                    &message,
                    globals,
                );
            }
        }
    }
    match sync_accounts(globals, dry_run, SyncOptions { force_settings }) {
        Ok(Some(applied)) => output_install_noted(
            &notes,
            "accounts sync",
            &applied.summaries,
            applied.result,
            None,
            None,
            globals,
        ),
        Ok(None) => super::fail(
            "accounts sync",
            Outcome::Invalid,
            "SYNC_RECEIPT",
            "No install receipt; run install first",
            globals,
        ),
        Err(refused) => refused.emit("accounts sync", globals),
    }
}

/// Per-account sync state for `accounts list`; reads only the receipt and the
/// files it owns.
pub(super) fn account_status(
    globals: &cli::Globals,
) -> Result<Vec<provenant_config::AccountStatus>, String> {
    let (_, targets) =
        selected_targets(globals, &[]).map_err(|error| Refused::from(error).message())?;
    provenant_config::inspect_accounts(&targets).map_err(|error| error.to_string())
}

/// The sync that follows a registration. `Ok(None)` when nothing is installed.
pub(super) fn sync_after_registration(
    globals: &cli::Globals,
) -> Result<Option<InstallOutcome>, String> {
    match sync_accounts(globals, false, SyncOptions::default()) {
        Ok(Some(applied)) => applied.result.map(Some).map_err(|error| error.to_string()),
        Ok(None) => Ok(None),
        Err(refused) => Err(refused.message()),
    }
}

struct UpgradeRunner {
    executor: provenant_os::exec::SystemExecutor,
    environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    cwd: PathBuf,
}
impl UpgradeRunner {
    fn execute(
        &self,
        argv: &[std::ffi::OsString],
        input: Option<&[u8]>,
        limit: usize,
        seconds: u64,
    ) -> Result<Vec<u8>, String> {
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
        let mut options =
            ExecOptions::new(argv, &self.cwd, std::time::Duration::from_secs(seconds));
        options.env = Some(&self.environment);
        options.stdin = input;
        options.stdout = StreamPolicy::Capture { limit };
        options.stderr = StreamPolicy::Capture { limit: 1_048_576 };
        options.stop_on_output_limit = true;
        let result = self
            .executor
            .execute(options)
            .map_err(|error| error.to_string())?;
        if result.termination != Termination::Exit(0)
            || result.timed_out
            || result.cancelled
            || result.drain_truncated
            || result.stdout.truncated
            || result.stderr.truncated
            || result.stdout.read_failed
            || result.stderr.read_failed
        {
            return Err(format!(
                "Command failed ({:?}): {}{}",
                result.termination,
                String::from_utf8_lossy(&result.stdout.bytes),
                String::from_utf8_lossy(&result.stderr.bytes)
            ));
        }
        Ok(result.stdout.bytes)
    }
}

#[derive(Serialize)]
struct HostUpgrade {
    host: String,
    success: bool,
    output: String,
}

pub(super) fn run_upgrade(args: cli::Upgrade, globals: &cli::Globals) -> u8 {
    let (_, targets) = match selected_targets(globals, &[]) {
        Ok(value) => value,
        Err(error) => return edge_error("upgrade", error, globals),
    };
    match read_install_receipt(&targets) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return super::fail(
                "upgrade",
                Outcome::Invalid,
                "UPGRADE_RECEIPT",
                "No install receipt; run install first",
                globals,
            );
        }
        Err(error) => return install_error("upgrade", &error, globals),
    }
    let upgraded = (|| -> Result<Vec<HostUpgrade>, String> {
        let config = provenant_config::read_with_domains(&targets.roots, &cli::domains::SECTIONS)
            .map_err(|error| error.to_string())?;
        let fabric = config.domains.fabric;
        let runner = UpgradeRunner {
            executor: provenant_os::exec::SystemExecutor::new(1)
                .map_err(|error| error.to_string())?,
            environment: std::env::vars_os().collect(),
            cwd: targets.home.clone(),
        };
        // Resolve latest once so every host downloads the same tag, even during a release.
        let mut install_args: Vec<std::ffi::OsString> = vec!["--yes".into(), "--upgrade".into()];
        let mut source_archive = None;
        let script;
        if let Some(source) = &args.from_source {
            script = std::fs::read(source.join("install.sh")).map_err(|error| error.to_string())?;
            install_args.extend(["--from-source".into(), source.as_os_str().to_owned()]);
            if !fabric.peers.is_empty() {
                let files = runner.execute(
                    &[
                        provenant_os::git::git_executable()
                            .ok_or("Git unavailable")?
                            .into_os_string(),
                        "-C".into(),
                        source.as_os_str().to_owned(),
                        "ls-files".into(),
                        "-z".into(),
                    ],
                    None,
                    4_194_304,
                    30,
                )?;
                source_archive = Some(runner.execute(
                    &[
                        "/usr/bin/tar".into(),
                        "-czf".into(),
                        "-".into(),
                        "-C".into(),
                        source.as_os_str().to_owned(),
                        "--null".into(),
                        "-T".into(),
                        "-".into(),
                    ],
                    Some(&files),
                    67_108_864,
                    120,
                )?);
            }
        } else {
            let latest = runner.execute(
                &[
                    "curl".into(),
                    "-fLsS".into(),
                    "--proto".into(),
                    "=https".into(),
                    "https://api.github.com/repos/mblauberg/provenant/releases/latest".into(),
                ],
                None,
                1_048_576,
                30,
            )?;
            let release: serde_json::Value =
                serde_json::from_slice(&latest).map_err(|error| error.to_string())?;
            let tag = release["tag_name"]
                .as_str()
                .filter(|tag| {
                    !tag.is_empty()
                        && tag
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                })
                .ok_or("Latest release has no valid tag")?;
            install_args.extend(["--tag".into(), tag.into()]);
            script = format!(
                "release_tag='{tag}'\n{}",
                include_str!("upgrade-release.sh")
            )
            .into_bytes();
        }
        let ssh = if fabric.peers.is_empty() {
            None
        } else {
            Some(
                provenant_fabric_runtime::hosts::discover_ssh_executable(
                    fabric.ssh_executable.as_deref(),
                )
                .map_err(|error| error.to_string())?,
            )
        };
        // Pass complete shell code as an argument, leaving stdin closed for children.
        let script = String::from_utf8(script).map_err(|error| error.to_string())?;
        let mut local: Vec<std::ffi::OsString> = vec![
            "/bin/sh".into(),
            "-c".into(),
            script.clone().into(),
            "install.sh".into(),
        ];
        local.extend(install_args.clone());
        local.extend([
            "--state-dir".into(),
            targets.roots.state.as_path().as_os_str().to_owned(),
        ]);
        let report = |host: String, result: Result<Vec<u8>, String>| match result {
            Ok(bytes) => HostUpgrade {
                host,
                success: true,
                output: String::from_utf8_lossy(&bytes).into_owned(),
            },
            Err(output) => HostUpgrade {
                host,
                success: false,
                output,
            },
        };
        let mut results = vec![report(
            "local".into(),
            runner.execute(&local, None, 1_048_576, 3600),
        )];
        // SSH uses Fabric's validated destination, batch mode and connection deadline.
        for (name, peer) in fabric.peers {
            let (command, input) = if let Some(archive) = &source_archive {
                ("set -eu; scratch=$(mktemp -d); trap 'rm -rf \"$scratch\"' 0; tar -xzf - -C \"$scratch\"; git -C \"$scratch\" init -q </dev/null; git -C \"$scratch\" add -f . </dev/null; sh \"$scratch/install.sh\" --yes --upgrade --from-source \"$scratch\" </dev/null".to_owned(), archive.as_slice())
            } else {
                let command = format!(
                    "sh -c '{}' install.sh {} </dev/null",
                    script.replace('\'', "'\\''"),
                    install_args
                        .iter()
                        .map(|arg| {
                            format!("'{}'", arg.to_string_lossy().replace('\'', "'\\''"))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                (command, &[][..])
            };
            let argv = vec![
                ssh.as_ref()
                    .ok_or("SSH unavailable")?
                    .as_os_str()
                    .to_owned(),
                "-T".into(),
                "-oBatchMode=yes".into(),
                format!("-oConnectTimeout={}", peer.connect_timeout_seconds).into(),
                "--".into(),
                peer.destination.into(),
                command.into(),
            ];
            results.push(report(
                name,
                runner.execute(&argv, Some(input), 1_048_576, 3600),
            ));
        }
        Ok(results)
    })();
    match upgraded {
        Err(message) => super::fail(
            "upgrade",
            Outcome::Error,
            "UPGRADE_FAILED",
            &message,
            globals,
        ),
        Ok(hosts) => {
            let success = hosts.iter().all(|host| host.success);
            let human = hosts
                .iter()
                .map(|host| {
                    format!(
                        "{}: {}\n{}\n",
                        host.host,
                        if host.success { "upgraded" } else { "failed" },
                        host.output
                    )
                })
                .collect::<String>();
            let mut result = CommandResult::new(
                "upgrade",
                if success { Outcome::Ok } else { Outcome::Error },
                hosts,
            );
            if !success {
                result.error = Some(CommandError {
                    code: "UPGRADE_FAILED".into(),
                    message: "Some hosts failed to upgrade; inspect host results and rerun upgrade"
                        .into(),
                    cause: None,
                    repair: None,
                });
            }
            super::emit(&result, globals, &human, false)
        }
    }
}

pub(super) fn run_uninstall(args: cli::Uninstall, globals: &cli::Globals) -> u8 {
    let (mut targets, warning) = match receipt_targets(globals, &args.harness) {
        Ok(value) => value,
        Err(error) => return edge_error("uninstall", error, globals),
    };
    if let Err(error) = prepare_agents_with(&mut targets, false) {
        return edge_error("uninstall", error, globals);
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = match context(&clock, &cancel) {
        Ok(context) => context,
        Err(error) => {
            return super::fail("uninstall", Outcome::Error, "WAIT_BUDGET", &error, globals);
        }
    };
    let mut reporter = PreviewReporter {
        home: Some(targets.home.clone()),
    };
    if provenant_fabric_runtime::peer_agent::eligible(targets.roots.state.as_path(), &targets.home)
        && let Err(error) = retire_disk_guard(&targets.home)
    {
        return edge_error("uninstall", EdgeError::Peer(error), globals);
    }
    let notes = warning
        .map(|warning| format!("{warning}\n"))
        .unwrap_or_default();
    output_install_noted(
        &notes,
        "uninstall",
        &[],
        cli::install::execute_uninstall(&args, &targets, &context, &mut reporter),
        None,
        None,
        globals,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn peer_agent_registry_name_uses_the_worktree_registry_key() {
        let path = std::path::Path::new("/worktrees/project-a");
        assert_eq!(
            super::peer_agent_registry_name(path),
            provenant_confinement::worktrees::state_dir_name(path)
        );
    }

    #[test]
    fn peer_agent_removal_accepts_canonical_home_and_refuses_other_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use provenant_config::Disposition;
        use provenant_records::AbsolutePath;
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().canonicalize()?.join("home");
        std::fs::create_dir(&home)?;
        let alias = tmp.path().join("home-alias");
        std::os::unix::fs::symlink(&home, &alias)?;
        let reporter = PreviewReporter { home: Some(alias) };
        let mut action = Action {
            path: AbsolutePath::new(&provenant_fabric_runtime::peer_agent::plist(&home))?,
            disposition: Disposition::Remove,
            selector: "peer-agent".into(),
            before: None,
            after: None,
            identity: None,
            edits: Vec::new(),
        };
        let mut stops = 0;
        reporter.before_remove_with(&action, |cwd| {
            assert_eq!(cwd, home);
            stops += 1;
            Ok(())
        })?;
        assert_eq!(stops, 1);
        action.path = AbsolutePath::new(&home.join("foreign.plist"))?;
        reporter.before_remove_with(&action, |cwd| {
            assert_eq!(cwd, home);
            stops += 1;
            Ok(())
        })?;
        assert_eq!(stops, 2);
        action.before = Some(provenant_records::OwnedValue::File {
            extra: Default::default(),
            digest: provenant_records::CanonicalV1::file_digest(b"rendered plist\n"),
            mode: 0o600,
        });
        assert!(
            reporter
                .before_remove_with(&action, |_| {
                    stops += 1;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(stops, 2);
        Ok(())
    }
}

fn retire_disk_guard(home: &Path) -> Result<(), AgentError> {
    retire_disk_guard_with(home, launchctl)
}

fn retire_disk_guard_with(
    home: &Path,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    let plist = home.join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
    match std::fs::symlink_metadata(&plist) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let service = format!(
        "gui/{}/dev.provenant.disk-guard",
        provenant_os::fs::effective_uid()
    );
    if run(&["print".as_ref(), service.as_ref()], home)?.0 {
        let (ok, error) = run(&["bootout".as_ref(), service.as_ref()], home)?;
        if !ok {
            return Err(AgentError::Launchctl("bootout", error));
        }
    }
    // A live checker holds guard.lock while builds remain paused, so stop it
    // before taking recovery custody. Once bootout prevents another pause,
    // resume the durable identities ourselves and only then remove the plist.
    let _recovery = legacy_guard_ready(home)?;
    std::fs::remove_file(plist)?;
    Ok(())
}

fn legacy_guard_ready(home: &Path) -> io::Result<provenant_os::fs::LockGuard> {
    use provenant_os::fs::LockMode;
    let path = home.join(".local/share/provenant/v1/disk-guard");
    let guard = provenant_config::private_directory(&path, true).map_err(io::Error::other)?;
    let clock = SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), 3).map_err(io::Error::other)?;
    let lock = guard
        .lock("guard.lock".as_ref(), LockMode::Exclusive, budget, &CancelToken::new(), &clock)
        .map_err(|error| io::Error::other(format!("disk guard recovery lock unavailable after bootout: {error}; retry after the checker exits")))?;
    for name in ["paused.json", "state.json"] {
        let bytes = match guard.read_stable(name.as_ref(), 1024 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(io::Error::other(error)),
        };
        #[derive(serde::Deserialize)]
        struct State {
            paused: Vec<LegacyPausedBuild>,
        }
        let state: State = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                use std::io::Write;
                let _ = writeln!(io::stderr(), "invalid disk guard {name}: {error}");
                continue;
            }
        };
        for process in state.paused {
            resume_legacy_build(&process)?;
        }
        break;
    }
    // paused.json superseded state.json. Remove both so a retry cannot
    // resurrect an older snapshot, including when both records were corrupt.
    for name in ["paused.json", "state.json"] {
        match guard.unlink(name.as_ref()) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    guard.sync().map_err(io::Error::other)?;
    Ok(lock)
}

#[derive(serde::Deserialize)]
struct LegacyPausedBuild {
    pid: u32,
    uid: u32,
    name: String,
    started: String,
    #[serde(default)]
    started_unix: Option<i64>,
}

fn resume_legacy_build(saved: &LegacyPausedBuild) -> io::Result<()> {
    use provenant_supervisor::census::{ProcessObservation, SystemProcessObserver};
    if saved.pid <= 1 || saved.uid != provenant_os::fs::effective_uid() {
        return Ok(());
    }
    let boot = provenant_os::clock::boot_unix_seconds()?;
    let observer = SystemProcessObserver;
    let identity = match observer.observe(i64::from(saved.pid)) {
        ProcessObservation::Present(fact) if !fact.zombie => {
            // LaunchAgent records use ps start seconds. Darwin's native stamp
            // carries the same UNIX seconds plus microseconds. A different
            // start time or command proves this PID no longer owns the pause.
            let started = legacy_start_seconds(&fact.identity)?;
            let same_start = match saved.started_unix {
                Some(saved) => saved == started,
                // The first checker stored local ps text without an offset.
                // Compare the live timestamp in that same local time zone.
                None => jiff::Timestamp::from_second(started)
                    .map_err(io::Error::other)?
                    .to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%a %b %e %T %Y")
                    .to_string()
                    .split_whitespace()
                    .eq(saved.started.split_whitespace()),
            };
            if started < boot || !same_start || fact.command.as_deref() != Some(saved.name.as_str())
            {
                return Ok(());
            }
            fact.identity
        }
        ProcessObservation::Absent | ProcessObservation::Present(_) => return Ok(()),
        ProcessObservation::Unknown(_) => {
            return Err(io::Error::other("cannot inspect saved build identity"));
        }
    };
    match observer.observe(identity.pid) {
        ProcessObservation::Present(fact) if fact.identity == identity && !fact.zombie => {
            provenant_os::pid::signal_process(
                identity.pid,
                provenant_os::pid::ProcessSignal::Continue,
            )
            .map_err(io::Error::other)?;
        }
        ProcessObservation::Unknown(_) => {
            return Err(io::Error::other("cannot recheck saved build identity"));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn legacy_start_seconds(
    identity: &provenant_supervisor::census::ProcessIdentity,
) -> io::Result<i64> {
    i64::try_from(identity.native_start / 1_000_000).map_err(io::Error::other)
}

#[cfg(not(target_os = "macos"))]
fn legacy_start_seconds(_: &provenant_supervisor::census::ProcessIdentity) -> io::Result<i64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "legacy LaunchAgent recovery requires macOS",
    ))
}

#[cfg(test)]
mod retired_disk_guard_tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::*;

    #[test]
    fn retirement_removes_corrupt_pause_state_after_bootout()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().ok_or("plist parent")?)?;
        std::fs::write(&plist, b"old plist")?;
        let path = home.path().join(".local/share/provenant/v1/disk-guard");
        let _directory = provenant_config::private_directory(&path, true)?;
        for name in ["paused.json", "state.json"] {
            std::fs::write(path.join(name), b"{")?;
        }
        let mut calls = Vec::new();
        for _ in 0..2 {
            retire_disk_guard_with(home.path(), |args, _| {
                calls.push(args[0].to_owned());
                Ok((true, String::new()))
            })?;
        }
        assert_eq!(calls, [OsStr::new("print"), OsStr::new("bootout")]);
        assert!(!plist.exists());
        assert!(!path.join("paused.json").exists());
        assert!(!path.join("state.json").exists());
        Ok(())
    }

    #[test]
    fn retirement_stops_and_deletes_the_old_agent_once() {
        let home = tempfile::tempdir().unwrap();
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"old plist").unwrap();
        let mut calls = Vec::new();
        for _ in 0..2 {
            retire_disk_guard_with(home.path(), |args, _| {
                calls.push(args[0].to_owned());
                Ok((true, String::new()))
            })
            .unwrap();
        }
        assert_eq!(calls, [OsStr::new("print"), OsStr::new("bootout")]);
        assert!(!plist.exists());
    }

    #[test]
    fn retirement_boots_out_a_live_checker_before_taking_recovery_custody() {
        let home = tempfile::tempdir().unwrap();
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"old plist").unwrap();
        let path = home.path().join(".local/share/provenant/v1/disk-guard");
        let directory = provenant_config::private_directory(&path, true).unwrap();
        let mut checker_lock = Some(
            directory
                .try_lock("guard.lock".as_ref(), provenant_os::fs::LockMode::Exclusive)
                .unwrap(),
        );
        std::fs::write(path.join("paused.json"), br#"{"paused":[]}"#).unwrap();
        retire_disk_guard_with(home.path(), |args, _| {
            if args[0] == "bootout" {
                // A low-disk checker releases this lease only when stopped.
                checker_lock.take();
            }
            Ok((true, String::new()))
        })
        .unwrap();
        assert!(checker_lock.is_none());
        assert!(!path.join("paused.json").exists());
        assert!(!plist.exists());
    }

    #[test]
    fn failed_bootout_preserves_the_plist_for_retry() {
        let home = tempfile::tempdir().unwrap();
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"old plist").unwrap();
        let result = retire_disk_guard_with(home.path(), |args, _| {
            Ok((args[0] == "print", "busy".into()))
        });
        assert!(matches!(result, Err(AgentError::Launchctl("bootout", _))));
        assert!(plist.exists());
    }
}

#[cfg(all(test, target_os = "macos"))]
#[allow(dead_code)] // Also included by the harness=false worktree driver.
#[allow(clippy::disallowed_methods)] // Real stopped-child and bounded wall-clock integration tests.
mod legacy_pause_retirement_tests {
    #[allow(unused_imports)]
    use super::*;
    use std::io::Write;
    struct Build(std::process::Child);
    impl Drop for Build {
        fn drop(&mut self) {
            let _ = provenant_os::pid::signal_process(
                i64::from(self.0.id()),
                provenant_os::pid::ProcessSignal::Continue,
            );
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn wait_for(path: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "build did not reach {}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    fn retirement_case(reused: bool, local: bool) -> Result<(), Box<dyn std::error::Error>> {
        use provenant_supervisor::census::{ProcessObservation, SystemProcessObserver};
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir()?;
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().ok_or("plist parent")?)?;
        std::fs::write(&plist, "old plist")?;
        let guard = home.path().join(".local/share/provenant/v1/disk-guard");
        std::fs::create_dir_all(&guard)?;
        std::fs::set_permissions(&guard, std::fs::Permissions::from_mode(0o700))?;
        let ready = home.path().join("ready");
        let resumed = home.path().join("resumed");
        let mut build = Build(
            std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf ready > \"$1\"; read value; printf resumed > \"$2\"",
                    "legacy-build",
                ])
                .arg(&ready)
                .arg(&resumed)
                .stdin(std::process::Stdio::piped())
                .spawn()?,
        );
        wait_for(&ready);
        let pid = build.0.id();
        let ProcessObservation::Present(fact) = SystemProcessObserver.observe(i64::from(pid))
        else {
            return Err("build identity unavailable".into());
        };
        let started_unix = i64::try_from(fact.identity.native_start / 1_000_000)?;
        let saved = serde_json::json!({"paused": [{
            "pid": pid, "uid": provenant_os::fs::effective_uid(), "name": fact.command,
            "started": jiff::Timestamp::from_second(started_unix)?.to_zoned(jiff::tz::TimeZone::system()).strftime("%a %b %e %T %Y").to_string(),
            "started_unix": if local { None } else { Some(if reused { started_unix - 1 } else { started_unix }) }
        }]});
        std::fs::write(guard.join("paused.json"), serde_json::to_vec(&saved)?)?;
        provenant_os::pid::signal_process(i64::from(pid), provenant_os::pid::ProcessSignal::Stop)?;
        writeln!(build.0.stdin.as_mut().ok_or("build stdin")?, "continue")?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!resumed.exists(), "build must be stopped before retirement");
        retire_disk_guard_with(home.path(), |_, _| Ok((true, String::new())))?;
        assert!(!guard.join("paused.json").exists());
        assert!(!plist.exists());
        if reused {
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(!resumed.exists(), "reused PID must not receive CONT");
        } else {
            wait_for(&resumed);
        }
        Ok(())
    }
    #[test]
    fn retirement_resumes_saved_paused_builds() -> Result<(), Box<dyn std::error::Error>> {
        retirement_case(false, false)
    }
    #[test]
    fn retirement_resumes_a_legacy_local_start_record() -> Result<(), Box<dyn std::error::Error>> {
        retirement_case(false, true)
    }
    #[test]
    fn retirement_does_not_resume_a_reused_pid() -> Result<(), Box<dyn std::error::Error>> {
        retirement_case(true, false)
    }
}
