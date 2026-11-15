//! Process-edge wiring for installation. State operations stay in config;
//! every child is supervised with a bounded wait and isolated environment.
use crate::release_smoke::SystemSmoke;
use crate::roots::{self, Product, Resolved, Workspace};
use provenant_cli as cli;
use provenant_config::shared_settings::SharedSettings;
use provenant_config::{
    AccountHome, Action, HarnessTarget, InstallContext, InstallError, InstallFinding,
    InstallOutcome, InstallReporter, ManualInclude, ReleaseSmoke, Targets,
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_runtime::peer_agent::{AgentError, launchctl};
use provenant_os::clock::SystemClock;
use provenant_records::{CommandError, CommandResult, Harness, Outcome};
use serde::Serialize;
use std::ffi::OsStr;
use std::{
    io::{self, Write},
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
    let roots = targets.roots.clone();
    prepare_agents_at(targets, &roots)
}

/// Read staged owner configuration, but render agents for the published root.
fn prepare_agents_at(
    targets: &mut Targets,
    destination: &provenant_core::Roots,
) -> Result<(), EdgeError> {
    let state = destination.state.as_path();
    if provenant_fabric_runtime::peer_agent::eligible(state, &targets.home) {
        let program = targets.bin_dir.join(provenant_config::STABLE_BINARY_NAME);
        targets.peer_agent = Some(
            provenant_fabric_runtime::peer_agent::install_plist(
                &program,
                &targets.roots,
                state,
                provenant_confinement::worktrees::state_dir_name,
            )
            .map_err(EdgeError::Peer)?,
        );
    }
    if super::tick_agent::eligible(state, &targets.home) {
        let program = targets.bin_dir.join(provenant_config::STABLE_BINARY_NAME);
        let search = provenant_fabric_runtime::peer_agent::scheduler_path(
            &program,
            &targets.roots,
            provenant_confinement::worktrees::state_dir_name,
        )
        .map_err(EdgeError::Peer)?;
        if cfg!(target_os = "linux") {
            targets.tick_agent = Some(super::tick_agent::render_timer());
            targets.tick_service = Some(
                super::tick_agent::render_service(&program, state, &search)
                    .map_err(EdgeError::Peer)?,
            );
        } else {
            targets.tick_agent =
                Some(super::tick_agent::render(&program, state, &search).map_err(EdgeError::Peer)?);
        }
    }
    Ok(())
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
    let targets = selected_targets_in(&environment, harnesses)?;
    Ok((environment, targets))
}

fn selected_targets_in(
    environment: &Resolved,
    harnesses: &[Harness],
) -> Result<Targets, EdgeError> {
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
    Ok(targets)
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
    let config = provenant_cli::config_reads::read_for(roots, "install")?;
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
    if let Some((message, code)) = accounts.settings_error() {
        return Err(if code.is_some() {
            InstallError::SharedSettingsNeverSynced(message)
        } else {
            InstallError::InvalidConfig(message)
        });
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
                        let message = provenant_config::shared_settings::interned(
                            format!(
                                "shared settings source: {reason}; remove it or drop `{key}` from `keys`"
                            ),
                        );
                        if reason.code().is_some() {
                            InstallError::SharedSettingsNeverSynced(message)
                        } else {
                            InstallError::InvalidConfig(message)
                        }
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
        .filter(|entry| {
            !entry.settings.is_empty()
                && provenant_records::AdapterSpec::get(&entry.adapter)
                    .is_some_and(|spec| spec.shared_account_settings)
        })
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Default)]
struct PreviewReporter {
    home: Option<PathBuf>,
    stopped_agents: std::collections::BTreeSet<&'static str>,
    uncertain_agents: std::collections::BTreeSet<&'static str>,
}
impl PreviewReporter {
    fn stopped(&mut self, agent: &'static str) {
        self.stopped_agents.insert(agent);
    }

    fn record_stop(&mut self, agent: &'static str, result: io::Result<bool>) -> io::Result<()> {
        match result {
            Ok(true) => {
                self.stopped(agent);
                self.uncertain_agents.remove(agent);
            }
            Ok(false) => {
                self.uncertain_agents.remove(agent);
            }
            Err(error) => {
                self.stopped_agents.remove(agent);
                self.uncertain_agents.insert(agent);
                return Err(error);
            }
        }
        Ok(())
    }

    fn stopped_report(&self, state: &Path) -> Option<String> {
        if self.stopped_agents.is_empty() && self.uncertain_agents.is_empty() {
            return None;
        }
        let state = shell_quote(&state.display().to_string());
        let mut report = String::new();
        if !self.stopped_agents.is_empty() {
            report.push_str(&format!(
                "Agents stopped: {}. ",
                self.stopped_agents
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.uncertain_agents.is_empty() {
            report.push_str(&format!(
                "Agents may still be running: {}. ",
                self.uncertain_agents
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        report.push_str(&format!(
            "State directory: {state}. To finish installation and restart agents, rerun your original install command; preserve its source and destination options. See docs/runbooks/install.md, Upgrade section.",
        ));
        Some(report)
    }

    /// Stops every agent a 1.x install may run, by label. A stopped peer or
    /// tick agent is restarted from the new root once the install is done.
    fn stop_legacy_agents(&mut self, state: &Path, home: &Path) -> io::Result<()> {
        if provenant_fabric_runtime::peer_agent::eligible(state, home) {
            let labels: Vec<_> = provenant_config::LEGACY_1X_ARTIFACTS
                .iter()
                .filter_map(|artifact| match artifact {
                    provenant_config::LegacyArtifact::LaunchAgent { label, .. } => Some(*label),
                    _ => None,
                })
                .collect();
            crate::state_handoff::quiesce_launch_agents(
                home,
                &labels,
                launchctl,
                |label, result| {
                    // Retired labels must stay absent. Only current agents
                    // need restart/recovery guidance after a successful stop.
                    self.record_stop(
                        label,
                        result.map(|stopped| {
                            stopped && provenant_config::LEGACY_1X_ARTIFACTS.iter().any(|artifact| {
                                matches!(artifact, provenant_config::LegacyArtifact::LaunchAgent {
                                    label: known,
                                    disposition: provenant_config::ArtifactDisposition::Replace,
                                    ..
                                } if *known == label)
                            })
                        }),
                    )
                },
            )?;
        } else if super::tick_agent::eligible(state, home) {
            self.record_stop(
                "dev.provenant.tick",
                super::tick_agent::quiesce(home).map_err(io::Error::other),
            )?;
        }
        Ok(())
    }

    fn before_remove_with(
        &mut self,
        action: &Action,
        stop: impl FnOnce(&Path) -> io::Result<bool>,
    ) -> io::Result<()> {
        if matches!(
            action.selector.as_str(),
            "peer-agent" | "tick-agent" | "tick-service"
        ) {
            let home = self
                .home
                .as_deref()
                .ok_or_else(|| io::Error::other("missing agent home"))?;
            let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            let expected = match action.selector.as_str() {
                "tick-agent" if cfg!(target_os = "linux") => super::tick_agent::timer(&home),
                "tick-agent" => super::tick_agent::plist(&home),
                "tick-service" => super::tick_agent::service(&home),
                _ => provenant_fabric_runtime::peer_agent::plist(&home),
            };
            if action.before.is_some() && action.path.to_path_buf().ok().as_ref() != Some(&expected)
            {
                return Err(io::Error::other("unexpected agent plist path"));
            }
            self.record_stop(
                if action.selector == "peer-agent" {
                    "dev.provenant.peer"
                } else {
                    "dev.provenant.tick"
                },
                stop(&home),
            )?;
        }
        Ok(())
    }
}
impl InstallReporter for PreviewReporter {
    fn before_remove(&mut self, action: &Action) -> io::Result<()> {
        self.before_remove_with(action, |home| match action.selector.as_str() {
            "tick-agent" | "tick-service" => {
                super::tick_agent::stop(home).map_err(io::Error::other)
            }
            _ => provenant_fabric_runtime::peer_agent::stop(home).map_err(io::Error::other),
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
    tick_agent: Option<super::tick_agent::Installed>,
    /// The 1.x configuration this install copied, or would copy in a dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    state_copy: Option<crate::state_handoff::Handoff>,
}
impl InstallData {
    fn empty() -> Self {
        Self {
            outcome: None,
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
            tick_agent: None,
            state_copy: None,
        }
    }
    fn success(outcome: InstallOutcome) -> Self {
        Self {
            outcome: Some(outcome),
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
            tick_agent: None,
            state_copy: None,
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
                tick_agent: None,
                state_copy: None,
            },
            _ => Self::empty(),
        }
    }
}

fn error_class(error: &InstallError) -> (Outcome, &'static str) {
    match error {
        InstallError::HooksUnavailable => (Outcome::Refused, "HOOKS_UNAVAILABLE"),
        InstallError::SharedSettingsNeverSynced(_) => {
            (Outcome::Invalid, "SHARED_SETTINGS_NEVER_SYNCED")
        }
        InstallError::Conflict(_)
        | InstallError::PeerAgentConflict(_)
        | InstallError::Drift(_)
        | InstallError::Tampered(_)
        | InstallError::ReleaseConflict { .. } => (Outcome::Refused, "INSTALL_REFUSED"),
        InstallError::Partial { source, .. } => error_class(source),
        InstallError::Config(provenant_config::ConfigError::Read { .. }) => {
            (Outcome::Error, "INSTALL_ERROR")
        }
        InstallError::Config(error) if error.refusal_code().is_some() => (
            Outcome::Invalid,
            error
                .refusal_code()
                .unwrap_or(provenant_config::shared_settings::NEVER_SYNCED_CODE),
        ),
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
    let projection = (code == provenant_config::shared_settings::NEVER_SYNCED_CODE)
        .then(provenant_config::shared_settings::never_synced_diagnostic);
    let mut result = CommandResult::new(command, status, InstallData::failure(error));
    result.error = Some(CommandError {
        diagnostic: projection.as_ref().map(Into::into),
        code: code.into(),
        message: error.to_string(),
        cause: None,
        repair: projection
            .and_then(|projection| projection.next)
            .or_else(|| {
                matches!(error, InstallError::Partial { .. })
                    .then(|| "Resolve the failure and re-apply the same install command".into())
            }),
    });
    result
}
const HOME_REPAIR: &str =
    "Set HOME to your absolute home directory, then rerun `provenant install`.";

fn home_error_result(command: &str, message: &str) -> CommandResult<()> {
    let mut result = CommandResult::new(command, Outcome::Invalid, ());
    result.error = Some(CommandError {
        diagnostic: None,
        code: "HOME".into(),
        message: message.into(),
        cause: None,
        repair: Some(HOME_REPAIR.into()),
    });
    result
}

pub(super) fn home_error(command: &str, message: &str, globals: &cli::Globals) -> u8 {
    super::emit(&home_error_result(command, message), globals, "", false)
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
        EdgeError::Home => home_error(command, "HOME must be an absolute directory", globals),
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
fn output_install(command: &str, applied: Applied, globals: &cli::Globals) -> u8 {
    let notes = applied
        .state_copy
        .as_ref()
        .map(crate::state_handoff::Handoff::note)
        .unwrap_or_default();
    let state_copy = applied.state_copy;
    output_install_with(
        &notes,
        command,
        &applied.summaries,
        applied.result,
        applied.peer_agent,
        applied.tick_agent,
        |data| data.state_copy = state_copy,
        globals,
    )
}
/// `notes` lead the human output, for steps that ran before the install.
fn output_install_noted(
    notes: &str,
    command: &str,
    summaries: &[HarnessTarget],
    result: Result<InstallOutcome, InstallError>,
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    tick_agent: Option<super::tick_agent::Installed>,
    globals: &cli::Globals,
) -> u8 {
    output_install_with(
        notes,
        command,
        summaries,
        result,
        peer_agent,
        tick_agent,
        |_| {},
        globals,
    )
}
#[allow(
    clippy::too_many_arguments,
    reason = "one renderer behind both entry points"
)]
fn output_install_with(
    notes: &str,
    command: &str,
    summaries: &[HarnessTarget],
    result: Result<InstallOutcome, InstallError>,
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    tick_agent: Option<super::tick_agent::Installed>,
    extend: impl FnOnce(&mut InstallData),
    globals: &cli::Globals,
) -> u8 {
    match result {
        Ok(outcome) => {
            let (mut links, mut hooks) = (0, 0);
            for action in &outcome.actions {
                let selector = action.selector.to_ascii_lowercase();
                if selector.contains("hook") {
                    hooks += 1;
                } else if matches!(
                    action.before.as_ref().or(action.after.as_ref()),
                    Some(provenant_records::OwnedValue::Link { .. })
                ) {
                    links += 1;
                }
            }
            let mut human = format!(
                "{notes}{command}: {} changes ({links} links, {hooks} hooks); {} targets",
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
            if let Some(super::tick_agent::Installed::Failed { message }) = &tick_agent {
                human.push_str(&format!("host tick: failed: {message}\n"));
            }
            let read_only = outcome.dry_run;
            let mut data = InstallData::success(outcome);
            data.peer_agent = peer_agent;
            data.tick_agent = tick_agent;
            extend(&mut data);
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
    let path = targets.roots.state.as_path().join("install");
    let directory = match provenant_os::fs::Directory::open_anchor(&path) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(InstallError::Fs(error)),
    };
    match directory.read_stable(
        OsStr::new("receipt.json"),
        provenant_records::Limits::CONFIG.max_bytes,
    ) {
        Ok(bytes) => {
            provenant_records::decode_v1(bytes.as_slice(), provenant_records::Limits::CONFIG)
                .map(Some)
                .map_err(InstallError::ReceiptDecode)
        }
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(InstallError::Fs(error)),
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
            if !record.harness.spec().account_config_dir {
                return false;
            }
            let mut defaults = Targets::new(environment.roots.clone(), &home, &[record.harness]);
            let Some(default_dir) = defaults
                .harnesses
                .first()
                .and_then(|target| target.skills_dir.parent())
                .map(canonical)
            else {
                return false;
            };
            if defaults
                .apply_environment(
                    environment.claude_config.as_deref(),
                    environment.codex_home.as_deref(),
                )
                .is_err()
            {
                return false;
            }
            let environment_dir = defaults
                .harnesses
                .first()
                .and_then(|target| target.skills_dir.parent())
                .map(canonical);
            let Ok(skills) = record.skills_dir.to_path_buf() else {
                return false;
            };
            let Some(directory) = skills.parent().map(canonical) else {
                return false;
            };
            base.accounts.iter().any(|a| a.harness == record.harness)
                && directory != default_dir
                && environment_dir.is_none_or(|dir| dir != directory)
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
fn install_batches(environment: &Resolved, args: &cli::Install) -> Result<Batches, EdgeError> {
    let mut targets = detected_batches(environment, args)?;
    let base = selected_targets_in(environment, &[])?;
    let mut findings = Vec::new();
    if args.harness.is_empty()
        && let Some(receipt) = read_install_receipt(&base).map_err(EdgeError::Install)?
    {
        for record in orphaned_targets(environment, &base, &receipt) {
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
    environment: &Resolved,
    args: &cli::Install,
) -> Result<Vec<Targets>, EdgeError> {
    if !args.harness.is_empty() {
        return selected_targets_in(environment, &args.harness).map(|targets| vec![targets]);
    }
    // Resolve config and roots without validating an empty selection.
    let base = selected_targets_in(environment, &[])?;
    if let Some(receipt) = read_install_receipt(&base).map_err(EdgeError::Install)? {
        let bin = receipt
            .bin_dir
            .to_path_buf()
            .map_err(InstallError::from)
            .map_err(EdgeError::Install)?;
        let orphans = orphaned_targets(environment, &base, &receipt);
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
                        config_file: record.config_file.to_path_buf()?,
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
    for spec in provenant_records::ADAPTER_SPECS
        .iter()
        .filter(|spec| spec.install.is_some())
    {
        if std::env::split_paths(&path)
            .any(|dir| provenant_os::sys::is_executable(&dir.join(spec.binary)))
        {
            batches.push(selected_targets_in(environment, &[spec.id])?);
        }
    }
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(&environment.home)
        .map_err(|source| EdgeError::Install(InstallError::Smoke(source)))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
    {
        let name = entry.file_name();
        for spec in provenant_records::ADAPTER_SPECS
            .iter()
            .filter(|spec| spec.account_config_dir && spec.install.is_some())
        {
            if spec
                .account_directory_prefix
                .is_some_and(|prefix| name.as_encoded_bytes().starts_with(prefix.as_bytes()))
            {
                directories.push((entry.path(), spec.id));
            }
        }
    }
    directories.sort_by(|left, right| left.0.cmp(&right.0));
    for (directory, harness) in directories {
        let mut batch = base.clone();
        let mut defaults = Targets::new(environment.roots.clone(), &environment.home, &[harness]);
        defaults
            .apply_account_environment(harness, &directory)
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
    tick_agent: Option<super::tick_agent::Installed>,
    state_copy: Option<crate::state_handoff::Handoff>,
}

/// What a sync does beyond reapplying the release.
#[derive(Clone, Copy, Default)]
struct SyncOptions {
    /// Overwrite shared settings edited in an account since the last sync.
    force_settings: bool,
}

pub(super) fn run_install(args: cli::Install, globals: &cli::Globals) -> u8 {
    let mut args = args;
    if args.upgrade_preflight {
        args.dry_run = true;
    }
    match apply_install(args, globals, true, SyncOptions::default()) {
        Ok(applied) => output_install("install", applied, globals),
        Err(refused) => refused.emit("install", globals),
    }
}

struct PreparedInstall {
    batches: Vec<Targets>,
    summaries: Vec<HarnessTarget>,
    preview: InstallOutcome,
}

fn supports_hooks(batch: &Targets) -> bool {
    batch.harnesses.iter().any(|target| {
        batch
            .hook_specs
            .iter()
            .any(|spec| spec.harness == target.harness)
    })
}

fn batch_args(args: &cli::Install, batch: &Targets, dry_run: bool) -> cli::Install {
    cli::Install {
        bin_dir: Some(batch.bin_dir.clone()),
        hooks: if matches!(args.hooks, Some(cli::Switch::On)) && !supports_hooks(batch) {
            None
        } else {
            args.hooks
        },
        dry_run,
        ..args.clone()
    }
}

/// All work that can fail against staged owner configuration runs here, before
/// any service is stopped or the candidate root is published.
fn preflight_install(
    environment: &Resolved,
    destination: &provenant_core::Roots,
    args: &cli::Install,
    options: SyncOptions,
    context: &InstallContext<'_>,
    smoke: &SystemSmoke,
) -> Result<PreparedInstall, Refused> {
    let Batches {
        targets: mut batches,
        findings: skipped,
    } = install_batches(environment, args)?;
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
    if batches.is_empty() {
        return Err(Refused::Fail(
            Outcome::Invalid,
            "NO_HARNESSES",
            "No harness CLIs, Claude config directories or registered accounts found; select --harness".into(),
        ));
    };
    for batch in &mut batches {
        prepare_agents_at(batch, destination)?;
    }
    if matches!(args.hooks, Some(cli::Switch::On)) && !batches.iter().any(supports_hooks) {
        return Err(InstallError::HooksUnavailable.into());
    }
    let mut preview = InstallOutcome {
        actions: Vec::new(),
        findings: skipped,
        manual_includes: Vec::new(),
        dry_run: true,
    };
    let mut reporter = PreviewReporter::default();
    for batch in &batches {
        let outcome = cli::install::execute_install(
            &batch_args(args, batch, true),
            batch.clone(),
            context,
            &mut reporter,
            smoke,
            env!("PROVENANT_TARGET_TRIPLE"),
        )?;
        preview.actions.extend(outcome.actions);
        preview.findings.extend(outcome.findings);
        preview.manual_includes.extend(outcome.manual_includes);
    }
    // Smoke the source before the handoff may stop 1.x or publish the new root.
    verify_release(&args.source, smoke, context.cancel)?;
    Ok(PreparedInstall {
        batches,
        summaries,
        preview,
    })
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
    let environment = environment(globals)?;
    // Identify an explicit 1.x root before any current receipt reader sees it.
    let plan = crate::state_handoff::plan(environment.roots.state.as_path(), &environment.home)
        .map_err(|error| {
            let (outcome, code) = match &error {
                crate::state_handoff::PlanError::StateFormatBreak { .. } => {
                    (Outcome::Refused, "STATE_FORMAT_BREAK")
                }
                _ => (Outcome::Error, "STATE_COPY"),
            };
            Refused::Fail(outcome, code, error.to_string())
        })?;
    refuse_newer_state(globals)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = context(&clock, &cancel)
        .map_err(|error| Refused::Fail(Outcome::Error, "WAIT_BUDGET", error))?;
    let smoke = SystemSmoke::new(None)
        .map_err(|error| Refused::Fail(Outcome::Error, "SMOKE_EXECUTOR", error.to_string()))?;
    let mut reporter = PreviewReporter::default();
    let mut prepared = None;
    let state_copy = if let Some(plan) = plan {
        let preflight = |candidate: &provenant_core::Roots| {
            let staged = Resolved {
                roots: candidate.clone(),
                ..environment.clone()
            };
            prepared = Some(
                preflight_install(
                    &staged,
                    &environment.roots,
                    &args,
                    options,
                    &context,
                    &smoke,
                )
                .map_err(|error| io::Error::other(error.message()))?,
            );
            Ok(())
        };
        if args.dry_run {
            crate::state_handoff::preview(&plan, &environment.roots, preflight)
                .map_err(|error| Refused::Fail(Outcome::Error, "STATE_COPY", error.to_string()))?;
            Some(plan)
        } else {
            match crate::state_handoff::run(&plan, &environment.roots, preflight, || {
                reporter.stop_legacy_agents(&plan.to, &environment.home)
            }) {
                Ok(copied) => Some(copied),
                Err(error) => {
                    if let Some(message) = reporter.stopped_report(&plan.to) {
                        let _ = writeln!(io::stderr().lock(), "{message}");
                    }
                    return Err(Refused::Fail(
                        Outcome::Error,
                        "STATE_COPY",
                        error.to_string(),
                    ));
                }
            }
        }
    } else {
        None
    };
    let PreparedInstall {
        mut batches,
        summaries,
        preview,
    } = match prepared {
        Some(prepared) => prepared,
        None => preflight_install(
            &environment,
            &environment.roots,
            &args,
            options,
            &context,
            &smoke,
        )?,
    };
    if args.dry_run {
        return Ok(Applied {
            result: Ok(preview),
            summaries,
            peer_agent: None,
            tick_agent: None,
            state_copy,
        });
    }
    // Configuration has been validated at the staged root; every actual write
    // now targets its published name. Agent bytes already name that destination.
    for batch in &mut batches {
        batch.roots = environment.roots.clone();
    }
    let targets = batches
        .last()
        .cloned()
        .ok_or(InstallError::InvalidConfig("no prepared install batches"))?;
    let state = targets.roots.state.as_path().to_path_buf();
    let home = targets.home.clone();
    let tick_enabled = targets.tick_agent.is_some();
    reporter.home = Some(home.clone());
    let result: Result<_, Refused> = (|| {
        let mut combined = InstallOutcome {
            actions: Vec::new(),
            findings: preview.findings,
            manual_includes: Vec::new(),
            dry_run: args.dry_run,
        };
        if agents && !args.dry_run && provenant_fabric_runtime::peer_agent::eligible(&state, &home)
        {
            retire_legacy_agents(&home)
                .map_err(|error| InstallError::AgentRetirement(error.to_string()))?;
        }
        for batch in batches {
            // Publish one receipt target at a time, using config's existing lock and ownership rules.
            let settings = batch_args(&args, &batch, args.dry_run);
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
                return Err(error.into());
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
    // Covers receipt publication and final inspection failures alike.
    // The tracked set also covers partial shutdown and earlier agent removals.
    if result.is_err()
        && let Some(message) = reporter.stopped_report(&state)
    {
        let _ = writeln!(io::stderr().lock(), "{message}");
    }
    let result = match result {
        Ok(outcome) => Ok(outcome),
        Err(Refused::Install(error)) => Err(error),
        Err(error) => return Err(error),
    };
    if let Ok(outcome) = &result
        && !outcome.dry_run
    {
        // A just-installed release is immutable, but old copies can be
        // removed once no process is executing them. An incomplete process
        // census cannot prove that safe, so retention fails closed.
        if let Ok(Some(receipt)) = read_install_receipt(&targets) {
            let _ = prune_old_releases(&state, receipt.source_digest);
        }
    }
    // The release is installed first; the agent then serves the new binary.
    let peer_agent = match &result {
        Ok(outcome)
            if !outcome.dry_run
                && (agents || reporter.stopped_agents.contains("dev.provenant.peer")) =>
        {
            Some(provenant_fabric_runtime::peer_agent::install(
                &state,
                &home,
                outcome
                    .actions
                    .iter()
                    .any(|action| action.selector == "peer-agent")
                    || reporter.stopped_agents.contains("dev.provenant.peer"),
            ))
        }
        _ => None,
    };
    let tick_agent = match &result {
        Ok(outcome)
            if !outcome.dry_run
                && (agents || reporter.stopped_agents.contains("dev.provenant.tick")) =>
        {
            Some(super::tick_agent::install(
                &state,
                &home,
                outcome.actions.iter().any(|action| {
                    matches!(action.selector.as_str(), "tick-agent" | "tick-service")
                }) || reporter.stopped_agents.contains("dev.provenant.tick"),
                tick_enabled,
            ))
        }
        _ => None,
    };
    if matches!(
        peer_agent,
        Some(provenant_fabric_runtime::peer_agent::Installed::Started)
    ) {
        reporter.stopped_agents.remove("dev.provenant.peer");
    }
    if matches!(tick_agent, Some(super::tick_agent::Installed::Started)) {
        reporter.stopped_agents.remove("dev.provenant.tick");
    }
    if result.is_ok()
        && let Some(message) = reporter.stopped_report(&state)
    {
        let _ = writeln!(io::stderr().lock(), "{message}");
    }
    Ok(Applied {
        result,
        summaries,
        peer_agent,
        tick_agent,
        state_copy,
    })
}

fn prune_old_releases(state: &Path, active: provenant_records::SourceDigest) -> io::Result<()> {
    use provenant_os::pid::CwdObserver;

    let releases = state.join("releases");
    let census = match provenant_supervisor::census::SystemProcessObserver
        .census(provenant_supervisor::census::MAX_CENSUS_ENTRIES)
    {
        Ok(census) if census.listing_is_complete() => census,
        _ => return Ok(()),
    };
    let processes = provenant_os::pid::SystemCwdObserver;
    let mut in_use = Vec::new();
    let pids = census
        .facts()
        .filter(|fact| !fact.zombie)
        .map(|fact| fact.identity.pid)
        .chain(census.unknown_pids().keys().copied());
    for pid in pids {
        let Some(executable) = processes.executable(pid) else {
            // A process can exit between listing and path lookup; inaccessible
            // system executables cannot use this private managed release tree.
            continue;
        };
        if executable.starts_with(&releases)
            && let Some(release) = executable.parent().and_then(Path::parent)
        {
            in_use.push(release.to_path_buf());
        }
    }
    prune_release_directories(&releases, active.to_string().as_str(), &in_use)
}

fn prune_release_directories(
    releases: &Path,
    active: &str,
    in_use: &[std::path::PathBuf],
) -> io::Result<()> {
    const PREVIOUS_RELEASES_TO_KEEP: usize = 2;
    let entries = match std::fs::read_dir(releases) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || name.len() != 64
            || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !path.join("release.json").is_file()
        {
            continue;
        }
        candidates.push((
            name.to_owned(),
            metadata
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            path,
        ));
    }
    candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
    let mut keep: std::collections::BTreeSet<String> = candidates
        .iter()
        .filter(|(name, _, _)| name != active)
        .take(PREVIOUS_RELEASES_TO_KEEP)
        .map(|(name, _, _)| name.clone())
        .collect();
    keep.insert(active.to_owned());
    for protected in in_use {
        if let Some(name) = protected.file_name().and_then(|name| name.to_str()) {
            keep.insert(name.to_owned());
        }
    }
    for (name, _, path) in candidates {
        if !keep.contains(&name) {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(dead_code, unused_imports)]
mod release_retention_tests {
    use super::prune_release_directories;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn release_pruning_keeps_recent_active_and_running_releases()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let releases = temp.path().join("releases");
        fs::create_dir(&releases)?;
        let names: Vec<_> = (1..=6).map(|n| format!("{n:064x}")).collect();
        for name in &names {
            let release = releases.join(name);
            fs::create_dir(&release)?;
            fs::write(release.join("release.json"), b"managed release")?;
        }
        // An unrelated symlink under releases is not a managed release and
        // must never be followed or removed by retention.
        let outside = temp.path().join("outside");
        fs::create_dir(&outside)?;
        fs::write(outside.join("release.json"), b"outside")?;
        symlink(&outside, releases.join(format!("{:064x}", 7)))?;

        prune_release_directories(&releases, &names[0], &[releases.join(&names[1])])?;

        for name in [&names[0], &names[1], &names[4], &names[5]] {
            assert!(
                releases.join(name).exists(),
                "expected retained release {name}"
            );
        }
        assert!(
            !releases.join(&names[2]).exists() && !releases.join(&names[3]).exists(),
            "old unused releases should be removed"
        );
        assert!(
            releases
                .join(format!("{:064x}", 7))
                .symlink_metadata()?
                .file_type()
                .is_symlink()
        );
        assert!(outside.join("release.json").exists());
        Ok(())
    }
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
        upgrade_preflight: false,
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
    let config = provenant_cli::config_reads::read_for(&targets.roots, "install")
        .map_err(|error| error.to_string())?;
    let accounts = &config.domains.accounts;
    let id = cli::accounts::resolve(accounts, account)?;
    let entry = accounts.entries.get(&id).ok_or("unknown account")?;
    if !provenant_records::AdapterSpec::get(&entry.adapter)
        .is_some_and(|spec| spec.shared_account_settings)
    {
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
                provenant_records::OwnedValue::Selector { value, .. } => e
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
        targets
            .apply_account_environment(record.harness, &directory)
            .map_err(|e| e.to_string())?;
        let mut reporter = PreviewReporter {
            home: Some(targets.home.clone()),
            ..Default::default()
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

/// Refuses to install over state a newer binary wrote. A missing or older
/// receipt is fine: 2.0 never reads 1.x stores, which stay in the 1.x root.
fn refuse_newer_state(globals: &cli::Globals) -> Result<(), Refused> {
    let (_, targets) = selected_targets(globals, &[])?;
    let receipt = read_install_receipt(&targets)?.map(|receipt| receipt.state_format_version);
    if receipt.is_some_and(|version| version > provenant_records::STATE_FORMAT_VERSION) {
        return Err(Refused::Fail(
            Outcome::Refused,
            "STATE_FORMAT_NEWER",
            "The installed state format is newer than this binary; downgrade refused".into(),
        ));
    }
    Ok(())
}

/// The release at `source` is whole and its binary is the one it describes.
fn verify_release(
    source: &Path,
    smoke: &SystemSmoke,
    cancel: &CancelToken,
) -> Result<(), InstallError> {
    let release = provenant_config::Release::read(source, env!("PROVENANT_TARGET_TRIPLE"))?;
    let identity = smoke.check(&release, cancel).map_err(InstallError::Smoke)?;
    if identity.source_digest != release.manifest().source_digest
        || identity.asset_digest != release.manifest().asset_digest
        || identity.target != release.manifest().target
        || identity.toolchain != release.manifest().toolchain
    {
        return Err(InstallError::Manifest(
            "binary identity differs from release manifest",
        ));
    }
    Ok(())
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
        let config = provenant_cli::config_reads::read_for(&targets.roots, "install")
            .map_err(|error| error.to_string())?;
        let fabric = config.domains.fabric;
        let runner = UpgradeRunner {
            executor: provenant_os::exec::SystemExecutor::new(1)
                .map_err(|error| error.to_string())?,
            environment: std::env::vars_os().collect(),
            cwd: targets.home.clone(),
        };
        // Resolve latest once so every host downloads the same tag, even during a release.
        let mut install_args: Vec<std::ffi::OsString> =
            vec!["--upgrade".into(), "--non-interactive".into()];
        if args.yes {
            install_args.push("--yes".into());
        }
        let mut source_archive = None;
        let script;
        if let Some(source) = &args.from_source {
            script = std::fs::read(source.join("install.sh")).map_err(|error| error.to_string())?;
            install_args.extend(["--source".into(), source.as_os_str().to_owned()]);
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
        let mut commands: Vec<(String, Vec<std::ffi::OsString>, &[u8])> =
            vec![("local".to_owned(), local, &[])];
        // Construct the entire fleet before running either phase.
        for (name, peer) in fabric.peers {
            let (command, input) = if let Some(archive) = &source_archive {
                (
                    format!(
                        "set -eu; scratch=$(mktemp -d); trap 'rm -rf \"$scratch\"' 0; tar -xzf - -C \"$scratch\"; git -C \"$scratch\" init -q </dev/null; git -C \"$scratch\" add -f . </dev/null; sh \"$scratch/install.sh\" --upgrade --non-interactive {} --source \"$scratch\"",
                        if args.yes { "--yes" } else { "" },
                    ),
                    archive.as_slice(),
                )
            } else {
                let command = format!(
                    "sh -c '{}' install.sh {}",
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
            commands.push((name, argv, input));
        }
        // No host installs until every host's source, target previews and
        // smoke checks have passed.
        let mut results = Vec::new();
        for (name, argv, input) in &commands {
            let mut preflight = argv.clone();
            if name == "local" {
                preflight.push("--preflight-only".into());
            } else {
                let last = preflight.last_mut().ok_or("missing SSH command")?;
                last.push(" --preflight-only </dev/null");
            }
            let host = report(
                name.clone(),
                runner.execute(&preflight, Some(*input), 1_048_576, 3600),
            );
            if !host.success {
                results.push(host);
                return Ok(results);
            }
        }
        for (name, argv, input) in commands {
            results.push(report(
                name,
                runner.execute(&argv, Some(input), 1_048_576, 3600),
            ));
            if !results.last().is_some_and(|host| host.success) {
                break;
            }
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
                    diagnostic: None,
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
    if let Err(error) = prepare_agents(&mut targets) {
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
        ..Default::default()
    };
    if provenant_fabric_runtime::peer_agent::eligible(targets.roots.state.as_path(), &targets.home)
        && let Err(error) = retire_legacy_agents(&targets.home)
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
    // The worktree driver includes this module without registering these tests.
    #[allow(dead_code)]
    #[test]
    fn home_refusal_has_install_repair() {
        let result = super::home_error_result("install", "HOME must be absolute");
        let error = result.error.unwrap();
        assert_eq!(error.code, "HOME");
        assert!(error.repair.unwrap().contains("provenant install"));
    }

    #[test]
    fn orphan_detection_keeps_default_environment_and_registered_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use provenant_records::{
            AbsolutePath, FileDigest, InstallReceipt, ResolvedHarnessTarget, SourceDigest,
        };
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&project)?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(tmp.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let environment = Resolved {
            assets: home.clone(),
            home: home.clone(),
            project,
            roots: roots.clone(),
            claude_config: Some(home.join("environment")),
            codex_home: None,
        };
        let mut base = Targets::new(roots, &home, &[]);
        base.accounts.push(provenant_config::AccountHome {
            harness: Harness::Claude,
            label: "registered".into(),
            home: home.join("registered"),
        });
        let target =
            |directory: &str| -> Result<ResolvedHarnessTarget, Box<dyn std::error::Error>> {
                let dir = home.join(directory);
                Ok(ResolvedHarnessTarget {
                    extra: Default::default(),
                    harness: Harness::Claude,
                    skills_dir: AbsolutePath::new(&dir.join("skills"))?,
                    config_file: AbsolutePath::new(&dir.join("settings.json"))?,
                    hook_file: None,
                    instruction: None,
                    hooks_relinquished: false,
                })
            };
        let receipt = InstallReceipt {
            extra: Default::default(),
            schema_version: provenant_records::INSTALL_RECEIPT_SCHEMA_VERSION,
            state_format_version: provenant_records::STATE_FORMAT_VERSION,
            source_digest: SourceDigest::from_bytes([1; 32]),
            manifest_digest: FileDigest::from_bytes([2; 32]),
            asset_digest: SourceDigest::from_bytes([3; 32]),
            target: "fixture".into(),
            toolchain: "fixture".into(),
            bin_dir: AbsolutePath::new(&home.join("bin"))?,
            harnesses: vec![Harness::Claude],
            harness_targets: vec![
                target(".claude")?,
                target("environment")?,
                target("registered")?,
                target("orphan")?,
            ],
            entries: vec![],
        };
        let orphans = orphaned_targets(&environment, &base, &receipt);
        assert_eq!(orphans.len(), 1);
        assert_eq!(
            orphans[0].skills_dir.to_path_buf()?,
            home.join("orphan/skills")
        );
        Ok(())
    }

    #[test]
    fn post_shutdown_recovery_names_each_agent_state() {
        let mut reporter = PreviewReporter::default();
        assert!(reporter.stopped_report(Path::new("/state")).is_none());
        reporter.stopped("dev.provenant.peer");
        reporter.stopped("dev.provenant.tick");
        let report = reporter
            .stopped_report(Path::new("/state"))
            .expect("stopped agents");
        for name in ["dev.provenant.peer", "dev.provenant.tick"] {
            assert!(report.contains(name));
        }
    }

    #[test]
    fn shell_quoted_path_round_trips_through_posix_shell() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let value = temp.path().join("state's data; touch marker");
        let text = value.to_str().ok_or("fixture path is not UTF-8")?;
        let command = format!("printf %s {}", shell_quote(text));
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(temp.path())
            .output()?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?, text);
        assert!(!temp.path().join("marker").exists());
        Ok(())
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
        let mut reporter = PreviewReporter {
            home: Some(alias),
            ..Default::default()
        };
        let mut action = Action {
            path: AbsolutePath::new(&provenant_fabric_runtime::peer_agent::plist(&home))?,
            disposition: Disposition::Remove,
            selector: "peer-agent".into(),
            before: None,
            after: None,
            identity: None,
            edits: Vec::new(),
        };
        assert!(
            reporter
                .before_remove_with(&action, |_| Err(io::Error::other("stop failed")))
                .is_err()
        );
        assert!(reporter.stopped_agents.is_empty());
        let report = reporter
            .stopped_report(Path::new("/state"))
            .expect("uncertain stop");
        assert!(report.contains("may still be running") && report.contains("dev.provenant.peer"));
        reporter.before_remove_with(&action, |_| Ok(false))?;
        assert!(reporter.stopped_agents.is_empty());
        let mut stops = 0;
        reporter.before_remove_with(&action, |cwd| {
            assert_eq!(cwd, home);
            stops += 1;
            Ok(true)
        })?;
        assert_eq!(stops, 1);
        action.path = AbsolutePath::new(&home.join("foreign.plist"))?;
        reporter.before_remove_with(&action, |cwd| {
            assert_eq!(cwd, home);
            stops += 1;
            Ok(true)
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
                    Ok(true)
                })
                .is_err()
        );
        assert_eq!(stops, 2);
        Ok(())
    }
}

/// Retire every declared 1.x-only service by name, including a loaded service
/// whose plist is already absent. A failed unload preserves its plist for retry.
fn retire_legacy_agents(home: &Path) -> Result<(), AgentError> {
    retire_legacy_agents_with(home, launchctl)
}

fn retire_legacy_agents_with(
    home: &Path,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    for artifact in provenant_config::LEGACY_1X_ARTIFACTS {
        let provenant_config::LegacyArtifact::LaunchAgent {
            label,
            plist,
            disposition: provenant_config::ArtifactDisposition::Remove,
            ..
        } = artifact
        else {
            continue;
        };
        provenant_fabric_runtime::peer_agent::stop_service_with(home, label, &mut run)?;
        let plist = home.join(plist);
        match std::fs::remove_file(&plist) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod retired_agent_tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::*;

    fn service_output(loaded: bool, label: &str) -> (bool, String) {
        if loaded {
            (true, "state = running\n".into())
        } else {
            (
                false,
                format!("exit 113: Could not find service \"{label}\" in domain gui"),
            )
        }
    }

    #[test]
    fn declared_legacy_agents_are_unloaded_before_plists_are_removed()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_config::{ArtifactDisposition, LEGACY_1X_ARTIFACTS, LegacyArtifact};
        let home = tempfile::tempdir()?;
        let mut loaded = std::collections::BTreeSet::new();
        for artifact in LEGACY_1X_ARTIFACTS {
            if let LegacyArtifact::LaunchAgent { label, plist, .. } = artifact {
                let path = home.path().join(plist);
                std::fs::create_dir_all(path.parent().ok_or("plist parent")?)?;
                std::fs::write(path, label)?;
                loaded.insert(*label);
            }
        }
        for _ in 0..2 {
            retire_legacy_agents_with(home.path(), |args, _| {
                let service = args[1].to_string_lossy();
                let label = service.rsplit('/').next().unwrap_or_default();
                if args[0] == "bootout" {
                    let plist = LEGACY_1X_ARTIFACTS
                        .iter()
                        .find_map(|artifact| match artifact {
                            LegacyArtifact::LaunchAgent {
                                label: known,
                                plist,
                                ..
                            } if *known == label => Some(plist),
                            _ => None,
                        })
                        .expect("declared service");
                    assert!(home.path().join(plist).exists(), "unload precedes removal");
                    loaded.remove(label);
                    Ok((true, String::new()))
                } else {
                    Ok(service_output(loaded.contains(label), label))
                }
            })?;
        }
        for artifact in LEGACY_1X_ARTIFACTS {
            if let LegacyArtifact::LaunchAgent {
                label,
                plist,
                disposition,
                ..
            } = artifact
            {
                let retained = *disposition == ArtifactDisposition::Replace;
                assert_eq!(home.path().join(plist).exists(), retained);
                assert_eq!(loaded.contains(label), retained);
            }
        }
        Ok(())
    }

    #[test]
    fn failed_unload_preserves_declared_plist_for_retry() -> Result<(), Box<dyn std::error::Error>>
    {
        for artifact in provenant_config::LEGACY_1X_ARTIFACTS {
            let provenant_config::LegacyArtifact::LaunchAgent {
                label,
                plist,
                disposition: provenant_config::ArtifactDisposition::Remove,
                ..
            } = artifact
            else {
                continue;
            };
            let home = tempfile::tempdir()?;
            let path = home.path().join(plist);
            std::fs::create_dir_all(path.parent().ok_or("plist parent")?)?;
            std::fs::write(&path, b"old plist")?;
            let result = retire_legacy_agents_with(home.path(), |args, _| {
                let service = args[1].to_string_lossy();
                if args[0] == "print" {
                    let queried = service.rsplit('/').next().unwrap_or_default();
                    Ok(service_output(queried == *label, queried))
                } else {
                    Ok((false, "busy".into()))
                }
            });
            assert!(matches!(result, Err(AgentError::Quiescence { .. })));
            assert!(path.exists());
        }
        Ok(())
    }
}
