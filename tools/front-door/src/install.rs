//! Process-edge wiring for installation. State operations stay in config;
//! every child is supervised with a bounded wait and isolated environment.
use crate::apps;
use crate::release_smoke::SystemSmoke;
use crate::roots::{self, Product, Resolved, Workspace};
use provenant_cli as cli;
use provenant_config::shared_settings::SharedSettings;
use provenant_config::{
    AccountHome, Action, HarnessTarget, InstallContext, InstallError, InstallFinding,
    InstallOutcome, InstallReporter, ManualInclude, ReleaseSmoke, Targets,
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_os::fs::Directory;
use provenant_records::{
    CommandError, CommandResult, Harness, Outcome,
    warning::{
        GIT_OPERATION_RETAINED, LIVE_RECORD_FORMAT_CHANGED, STATE_ROOT_OVERLAP, Warning, warn,
    },
};
use serde::Serialize;
use std::ffi::OsStr;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

mod migrations;

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
                peer_agent_registry_name,
            )
            .map_err(EdgeError::Peer)?,
        );
    }
    if super::tick_agent::eligible(state, &targets.home) {
        let program = targets.bin_dir.join(provenant_config::STABLE_BINARY_NAME);
        let search = provenant_fabric_runtime::peer_agent::scheduler_path(
            &program,
            &targets.roots,
            peer_agent_registry_name,
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
                "configuration ignored for uninstall ({error}); removing recorded targets only"
            ));
            // Uninstall is host-only, including its configuration fallback.
            let policy = std::sync::Arc::new(provenant_config::policy::read(
                environment.roots.state.as_path(),
                "",
                &Default::default(),
            ));
            let mut targets = Targets::new(environment.roots, &environment.home, harnesses, policy);
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
        let state = format!("'{}'", state.display().to_string().replace('\'', "'\\''"));
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
    /// Advisory when this release changes live-attempt records under live lanes.
    #[serde(skip_serializing_if = "Option::is_none")]
    live_record_warning: Option<String>,
    /// Version and staleness of the installed bundled app.
    #[serde(skip_serializing_if = "Option::is_none")]
    app: Option<apps::Status>,
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
            live_record_warning: None,
            app: None,
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
            live_record_warning: None,
            app: None,
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
                live_record_warning: None,
                app: None,
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
        InstallError::Fs(_) | InstallError::Preview(_) | InstallError::Smoke(_) => {
            (Outcome::Error, "INSTALL_ERROR")
        }
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
    let diagnostic = projection.as_ref().map(Into::into);
    let repair = projection
        .as_ref()
        .and_then(|projection| projection.next.as_deref())
        .map(str::to_owned)
        .or_else(|| {
            matches!(error, InstallError::Partial { .. })
                .then(|| "Resolve the failure and re-apply the same install command".to_owned())
        })
        .unwrap_or_else(|| {
            if code == provenant_config::shared_settings::NEVER_SYNCED_CODE {
                "run `provenant accounts sync`".to_owned()
            } else if code == "INSTALL_INVALID" {
                crate::REPAIR_CONFIG.to_owned()
            } else {
                "resolve the reported install failure and rerun the install command".to_owned()
            }
        });
    let mut failure = CommandError::new(code, error.to_string(), None, repair);
    failure.diagnostic = diagnostic;
    result.error = Some(failure);
    result
}
const HOME_REPAIR: &str =
    "Set HOME to your absolute home directory, then rerun `provenant install`.";

fn home_error_result(command: &str, message: &str) -> CommandResult<()> {
    let mut result = CommandResult::new(command, Outcome::Invalid, ());
    result.error = Some(CommandError::new("HOME", message, None, HOME_REPAIR));
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
            crate::REPAIR_ROOT,
            globals,
        ),
        EdgeError::Peer(error) => super::fail(
            command,
            Outcome::Error,
            "PEER_AGENT",
            &error.to_string(),
            "resolve the reported peer agent error, then rerun the command",
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
    let live_record_warning = applied.live_record_warning;
    output_install_with(
        &notes,
        command,
        &applied.summaries,
        applied.result,
        applied.peer_agent,
        applied.tick_agent,
        |data| {
            data.state_copy = state_copy;
            data.live_record_warning = live_record_warning;
        },
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
            let app_report = apps::status(&apps::home(), env!("CARGO_PKG_VERSION"));
            // install.sh swaps the app after this install and prints the app line itself.
            let installer_owns_app = std::env::var_os("PROVENANT_INSTALLER_OWNS_APP")
                .is_some_and(|value| value == "true");
            if let Some(app) = app_report.as_ref().filter(|_| !installer_owns_app) {
                human.push_str(&format!(
                    "  app: {} ({})\n",
                    app.path,
                    app.version.as_deref().unwrap_or("version unknown")
                ));
                if app.warning.is_some() {
                    human.push_str(&format!("  {}\n", apps::advisory(app)));
                }
            }
            let read_only = outcome.dry_run;
            let mut data = InstallData::success(outcome);
            data.peer_agent = peer_agent;
            data.tick_agent = tick_agent;
            data.app = app_report;
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
    read_receipt_at(targets.roots.state.as_path())
}

/// Read the receipt a state root currently owns. The live-record advisory
/// reads it from the installed root, before the candidate root is published.
fn read_receipt_at(
    state: &Path,
) -> Result<Option<provenant_records::InstallReceipt>, InstallError> {
    let path = state.join("install");
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
            let receipt: provenant_records::InstallReceipt =
                provenant_records::decode(bytes.as_slice(), provenant_records::Limits::CONFIG)
                    .map_err(InstallError::ReceiptDecode)?;
            if receipt.schema_version != provenant_records::INSTALL_RECEIPT_SCHEMA_VERSION {
                return Err(InstallError::Manifest("unsupported receipt schema version"));
            }
            Ok(Some(receipt))
        }
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(InstallError::Fs(error)),
    }
}

/// The advisory text when this release changes live-attempt records while
/// attempts are still live. Silent unless both conditions hold; never refuses.
fn live_record_warning(installed: Option<u32>, live: &[String]) -> Option<String> {
    if installed == Some(provenant_records::LIVE_RECORD_FORMAT) {
        return None;
    }
    if live.is_empty() {
        return None;
    }
    let from = installed
        .map(|format| format.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    Some(format!(
        "release changes live-attempt records (format {from} → {}); live attempts: {}; drain them or expect them to settle under the new release",
        provenant_records::LIVE_RECORD_FORMAT,
        live.join(", ")
    ))
}

/// Enumerate live attempts under the installed state root without resolving a
/// checkout. The cross-project query ignores the store's calling identity, so
/// no project registration or current directory is needed; a missing store is
/// an empty read, not an inspection failure.
fn inspect_live_attempts(
    destination: &provenant_core::Roots,
    call: &provenant_run::Request<'_>,
) -> provenant_run::Result<Vec<String>> {
    let execution = provenant_run::ExecutionContext {
        state_root: destination.state.clone(),
        project_id: provenant_core::ProjectId::from_bytes([0; 16]),
        actor_id: provenant_core::ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let Some(store) = provenant_run::RunStore::open_read_only(&execution, call)? else {
        return Ok(Vec::new());
    };
    provenant_cli::run::live_attempt_ids(&store, call)
}

/// Best-effort advisory for retained writer custody predating guardian schema
/// 2. The custody owner classifies legacy evidence; install only reports its
/// count and a representative path. This warning never blocks preflight.
fn warn_legacy_custody(destination: &provenant_core::Roots) -> Option<String> {
    let worktrees_path = destination.state.as_path().join("worktrees");
    let directory = match Directory::open_anchor(&worktrees_path) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return None,
        Err(error) => {
            let message = format!(
                "could not inspect retained writer custody under {}: {error}",
                worktrees_path.display()
            );
            warn(
                Warning::new(GIT_OPERATION_RETAINED, message.clone())
                    .subject(worktrees_path.display().to_string())
                    .once_per_change(),
            );
            return Some(message);
        }
    };
    let scan = match provenant_supervisor::custody::list_legacy_guardian_records_in_worktrees(
        &directory,
    ) {
        Ok(scan) => scan,
        Err(error) => {
            let message = format!(
                "could not inspect retained writer custody under {}: {error}",
                worktrees_path.display()
            );
            warn(
                Warning::new(GIT_OPERATION_RETAINED, message.clone())
                    .subject(worktrees_path.display().to_string())
                    .once_per_change(),
            );
            return Some(message);
        }
    };
    let paths: Vec<_> = scan
        .records
        .into_iter()
        .map(|record| worktrees_path.join(record))
        .collect();
    let inspection_issue = scan.issues.first().map(|issue| {
        format!(
            "could not inspect {}: {}",
            worktrees_path.join(&issue.path).display(),
            issue.reason
        )
    });
    let message = if let Some(example) = paths.first() {
        let mut message = format!(
            "retained writer custody includes {} attempt(s) with guardian-lock.json schema below 2; example: {}; these attempts remain fenced but no longer block launches",
            paths.len(),
            example.display()
        );
        if let Some(issue) = inspection_issue {
            message.push_str(&format!("; {issue}"));
        }
        message
    } else {
        inspection_issue?
    };
    warn(
        Warning::new(GIT_OPERATION_RETAINED, message.clone())
            .subject(worktrees_path.display().to_string())
            .once_per_change(),
    );
    Some(message)
}

/// Best-effort preflight advisory: compare the installed receipt's live-record
/// format with this release's and name live attempts on stderr. Live attempts
/// are enumerated across the whole installed state root, independent of the
/// current checkout. When they cannot be inspected the failure is itself a
/// visible warning rather than silence; this never blocks an install.
fn warn_live_record_change(
    destination: &provenant_core::Roots,
    context: &InstallContext<'_>,
) -> Option<String> {
    let receipt = read_receipt_at(destination.state.as_path())
        .ok()
        .flatten()?;
    let installed = receipt.live_record_format;
    if installed == Some(provenant_records::LIVE_RECORD_FORMAT) {
        return None;
    }
    let call = provenant_run::Request {
        clock: context.clock,
        cancel: context.cancel,
    };
    let message = match inspect_live_attempts(destination, &call) {
        Ok(live) => live_record_warning(installed, &live)?,
        Err(reason) => format!(
            "release changes live-attempt records (format {} → {}); could not inspect live attempts under {}: {reason}",
            installed
                .map(|format| format.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            provenant_records::LIVE_RECORD_FORMAT,
            destination.state.as_path().display()
        ),
    };
    warn(
        Warning::new(LIVE_RECORD_FORMAT_CHANGED, message.clone())
            .subject(destination.state.as_path().display().to_string())
            .once_per_change(),
    );
    Some(message)
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
            let mut defaults = Targets::new(
                environment.roots.clone(),
                &home,
                &[record.harness],
                base.policy.clone(),
            );
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
        let mut defaults = Targets::new(
            environment.roots.clone(),
            &environment.home,
            &[harness],
            base.policy.clone(),
        );
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
            Self::Fail(status, code, message) => super::fail(
                command,
                status,
                code,
                &message,
                "correct the reported condition and rerun the command",
                globals,
            ),
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
    live_record_warning: Option<String>,
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
    live_record_warning: Option<String>,
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
        // Batches clone one base and then take their own harness, so hook
        // registrations follow the harness each batch actually installs.
        let harnesses: Vec<_> = batch
            .harnesses
            .iter()
            .map(|target| target.harness)
            .collect();
        batch.hook_specs = provenant_config::hook_registrations(&harnesses);
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
        batches[0].policy.warn_or(
            "INSTALL_HOOKS_UNAVAILABLE",
            provenant_records::policy::Facts::violation(provenant_config::HOOKS_UNAVAILABLE_DETAIL),
            |_| InstallError::HooksUnavailable,
        )?;
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
    let live_record_warning = warn_live_record_change(destination, context);
    let _ = warn_legacy_custody(destination);
    Ok(PreparedInstall {
        batches,
        summaries,
        preview,
        live_record_warning,
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
            match crate::state_handoff::run(&plan, &environment.roots, preflight) {
                Ok(copied) => Some(copied),
                Err(error) => {
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
        live_record_warning,
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
            live_record_warning,
        });
    }
    // Configuration has been validated at the staged root; every actual write
    // now targets its published name. Agent bytes already name that destination.
    // Non-SQLite state migrates once here, after the live-attempt advisory and
    // before any release is swapped in.
    let migrated = migrations::run(environment.roots.state.as_path());
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
        warn(
            Warning::new(GIT_OPERATION_RETAINED, message.clone())
                .subject(state.display().to_string())
                .once_per_change(),
        );
    }
    let result = match result {
        Ok(outcome) => Ok(outcome),
        Err(Refused::Install(error)) => Err(error),
        Err(error) => return Err(error),
    };
    if let Ok(outcome) = &result
        && !outcome.dry_run
    {
        // A step that ran before this install had no receipt to record into;
        // record its id on the receipt the install just published.
        migrations::record(&state, &migrated);
        // The seeding feature is gone; a state root left by an older release
        // still pays for its seeds until install retires them once.
        let _ = retire_cargo_target_seeds(&state);
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
        warn(
            Warning::new(GIT_OPERATION_RETAINED, message.clone())
                .subject(state.display().to_string())
                .once_per_change(),
        );
    }
    Ok(Applied {
        result,
        summaries,
        peer_agent,
        tick_agent,
        state_copy,
        live_record_warning,
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

const RETIRED_CARGO_TARGET_SEEDS: &str = "cargo-target-seeds";

/// Sum the bytes of a retired tree without following links out of the state
/// root. An unreadable entry contributes zero; removal never depends on it.
fn retired_tree_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) if metadata.is_dir() => retired_tree_bytes(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Remove the directory left by the retired seeding feature, reporting its
/// path and size. A missing directory is the normal case and stays silent.
// A successful retirement is reported on stderr as the install's progress trace.
#[allow(clippy::disallowed_methods)]
fn retire_cargo_target_seeds(state: &Path) -> Option<String> {
    let path = state.join(RETIRED_CARGO_TARGET_SEEDS);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            let message = format!(
                "could not inspect retired cargo target seeds at {}: {error}",
                path.display()
            );
            warn(
                Warning::new(STATE_ROOT_OVERLAP, message.clone())
                    .subject(path.display().to_string())
                    .once_per_change(),
            );
            return Some(message);
        }
    };
    if !metadata.is_dir() {
        let message = format!(
            "retired cargo target seeds at {} is not a directory; left untouched",
            path.display()
        );
        warn(
            Warning::new(STATE_ROOT_OVERLAP, message.clone())
                .subject(path.display().to_string())
                .once_per_change(),
        );
        return Some(message);
    }
    let bytes = retired_tree_bytes(&path);
    let message = match std::fs::remove_dir_all(&path) {
        Ok(()) => format!(
            "removed retired cargo target seeds at {} ({bytes} bytes)",
            path.display()
        ),
        Err(error) => format!(
            "could not remove retired cargo target seeds at {}: {error}",
            path.display()
        ),
    };
    if message.starts_with("could not remove") {
        warn(
            Warning::new(STATE_ROOT_OVERLAP, message.clone())
                .subject(path.display().to_string())
                .once_per_change(),
        );
    } else {
        let _ = writeln!(io::stderr().lock(), "{message}");
    }
    Some(message)
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
                        "resolve the reported adopt error and rerun `provenant accounts sync --adopt <account>`",
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
                    "resolve the reported prune error and rerun `provenant accounts sync`",
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
            "run `provenant install` first, then rerun `provenant accounts sync`",
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

fn verify_graph_name() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "smoke-verify-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

struct SmokeProject {
    directory: crate::scratch_dir::ScratchDir,
    /// The commit the fixture's change sits on, so the review nodes have a
    /// non-empty range to inspect.
    base: String,
}

impl SmokeProject {
    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn base(&self) -> &str {
        &self.base
    }
}

/// The binding the smoke child creates for `project`. Resolve it while the
/// scratch exists: it is the canonical common directory the child's `run`
/// binds, not the scratch path's own spelling, and cleanup must still name the
/// record after the graph or the fixture has removed the checkout. An
/// unresolved binding is an error, never an empty success.
fn smoke_project_binding(
    state_dir: &Path,
    project: &Path,
) -> Result<provenant_core::ProjectId, String> {
    let directory = provenant_config::host_project_dir(state_dir, project).ok_or_else(|| {
        format!(
            "cannot resolve the host project binding for {}",
            project.display()
        )
    })?;
    directory
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| provenant_core::ProjectId::parse(name).ok())
        .ok_or_else(|| "smoke project binding name is invalid".to_owned())
}

/// Remove the record the smoke child published for the scratch project, when
/// the run ends, success or failure, so deleting the fixture cannot leave a
/// registered project whose layout no longer resolves. A missing record means
/// the child never bound (a refused launch); an unreadable registry or a
/// refused removal is reported to the caller.
fn forget_smoke_binding(
    state_dir: &Path,
    project: provenant_core::ProjectId,
) -> Result<(), String> {
    provenant_config::forget_project_at_state(state_dir, project)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn runner_for_verify(targets: &provenant_config::Targets) -> Result<UpgradeRunner, String> {
    Ok(UpgradeRunner {
        executor: provenant_os::exec::SystemExecutor::new(1).map_err(|error| error.to_string())?,
        environment: std::env::vars_os().collect(),
        cwd: targets.home.clone(),
    })
}

/// The binary the install published, not the process that started the upgrade.
/// Every verify child names this path, so a fix to the verifier ships with the
/// release that installs it instead of waiting for the following upgrade.
fn installed_destination(targets: &provenant_config::Targets) -> PathBuf {
    targets.bin_dir.join(provenant_config::STABLE_BINARY_NAME)
}

fn verify_smoke(
    targets: &provenant_config::Targets,
    args: &cli::Upgrade,
) -> Result<String, String> {
    let runner = runner_for_verify(targets)?;
    verify_smoke_with(
        &runner,
        &installed_destination(targets),
        targets.roots.state.as_path(),
        &std::env::temp_dir(),
        args,
    )
}

/// Re-exec the installed binary so its own verifier drives the smoke graph,
/// wait, status and cleanup. The runner is injected so a test can name the
/// program the verify invocation actually used.
fn verify_installed_smoke(
    runner: &UpgradeRunner,
    environment: &Resolved,
    targets: &provenant_config::Targets,
    args: &cli::Upgrade,
) -> Result<String, String> {
    let program = installed_destination(targets);
    // JSON keeps the child's envelopes typed. In compact human mode the
    // verifier's own rows are clipped before the parent could read them, so
    // both the innermost error and the smoke verdict would be lost.
    let mut argv = vec![
        program.into_os_string(),
        "--json".into(),
        "--state-dir".into(),
        environment.roots.state.as_path().as_os_str().to_owned(),
    ];
    // The verifier builds its own scratch checkout; the caller's project is
    // only needed for config and root resolution. Passing a non-checkout makes
    // the child refuse with INVALID before the smoke can start, which is why a
    // `upgrade --verify` run from $HOME failed only its smoke host. Resolve from
    // the child's own cwd instead when the selected project is not a checkout.
    if provenant_os::git::repository_path(
        &environment.project,
        provenant_os::git::RepositoryPath::Toplevel,
    )
    .is_ok()
    {
        argv.extend([
            "--project".into(),
            environment.project.as_os_str().to_owned(),
        ]);
    }
    argv.extend(["upgrade".into(), "--verify".into(), "--verify-only".into()]);
    if let Some(host) = &args.host {
        argv.extend(["--host".into(), host.clone().into()]);
    }
    if let Some(route) = &args.route {
        argv.extend(["--route".into(), route.clone().into()]);
    }
    for setting in &args.set {
        argv.extend(["--set".into(), setting.clone().into()]);
    }
    let output = runner.execute(&argv, None, 1_048_576, 3700)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// The installed binary's own verify phase. It never installs or downloads;
/// it reports the version that actually ran verification.
fn run_verify_only(
    targets: &provenant_config::Targets,
    args: &cli::Upgrade,
    globals: &cli::Globals,
) -> u8 {
    let result = verify_smoke(targets, args);
    let version = env!("CARGO_PKG_VERSION");
    match result {
        Ok(output) => {
            let human = format!("verified by provenant {version} (installed binary):\n{output}");
            let verdict = graph_verdict(&output);
            super::emit(
                &CommandResult::new(
                    "upgrade",
                    Outcome::Ok,
                    serde_json::json!({
                        "verify": "installed",
                        "version": version,
                        "output": output,
                        "verdict": verdict,
                    }),
                ),
                globals,
                &human,
                false,
            )
        }
        Err(report) => {
            // Publish the innermost typed reason, not the nested `Command
            // failed` wrappers the child executor stacks around a smoke error.
            let (code, message) = innermost_error_text(&report);
            super::fail(
                "upgrade",
                Outcome::Error,
                &code,
                &message,
                "inspect the reported smoke failure, resolve it, then rerun `upgrade --verify`",
                globals,
            )
        }
    }
}

fn smoke_git(runner: &UpgradeRunner, project: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut argv = vec![
        provenant_os::git::git_executable()
            .ok_or("Git unavailable")?
            .into_os_string(),
    ];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    runner.execute_at(&argv, project, None, 65_536, 30)
}

fn smoke_project(runner: &UpgradeRunner, scratch: &Path) -> Result<SmokeProject, String> {
    let directory = crate::scratch_dir::ScratchDir::new_in(scratch, "provenant-smoke-project-")
        .map_err(|error| error.to_string())?;
    let project = directory.path();
    // Two commits: the review nodes inspect the fixture change, so their base
    // and subject must be different commits even though the writer starts at
    // HEAD.
    std::fs::write(project.join("fixture.txt"), "smoke base\n")
        .map_err(|error| error.to_string())?;
    for args in [
        vec!["init", "-q"],
        vec!["add", "fixture.txt"],
        vec![
            "-c",
            "user.name=Provenant Smoke",
            "-c",
            "user.email=smoke@localhost",
            "commit",
            "-qm",
            "smoke base",
        ],
    ] {
        smoke_git(runner, project, &args)?;
    }
    let base = String::from_utf8_lossy(&smoke_git(runner, project, &["rev-parse", "HEAD"])?)
        .trim()
        .to_owned();
    if base.is_empty() {
        return Err("smoke fixture has no base commit".to_owned());
    }
    std::fs::write(project.join("fixture.txt"), "smoke fixture\n")
        .map_err(|error| error.to_string())?;
    for args in [
        vec!["add", "fixture.txt"],
        vec![
            "-c",
            "user.name=Provenant Smoke",
            "-c",
            "user.email=smoke@localhost",
            "commit",
            "-qm",
            "smoke fixture",
        ],
    ] {
        smoke_git(runner, project, &args)?;
    }
    Ok(SmokeProject { directory, base })
}

fn verify_smoke_with(
    runner: &UpgradeRunner,
    program: &Path,
    state_dir: &Path,
    scratch: &Path,
    args: &cli::Upgrade,
) -> Result<String, String> {
    let project = smoke_project(runner, scratch)?;
    // Resolve the binding before the graph launches: the child binds this same
    // canonical common directory, and the scratch may not survive the run. An
    // unresolved binding is reported by cleanup, never hidden.
    let binding = smoke_project_binding(state_dir, project.path());
    let program = program.as_os_str().to_owned();
    let graph = verify_graph_name();
    let mut run = vec![
        program.clone(),
        "--project".into(),
        project.path().as_os_str().to_owned(),
        "--state-dir".into(),
        state_dir.as_os_str().to_owned(),
        "--json".into(),
        "run".into(),
        "smoke".into(),
        "--name".into(),
        graph.clone().into(),
    ];
    let remote = args
        .host
        .as_deref()
        .is_some_and(|host| !matches!(host, "" | "local" | "localhost"));
    if !remote {
        // The writer node needs a linked worktree, which a primary checkout
        // is not; ask the launch to create one on a branch of the fixture.
        run.extend([
            "--branch".into(),
            format!("smoke/{graph}").into(),
            "--base".into(),
            "HEAD".into(),
        ]);
    }
    if let Some(host) = &args.host {
        run.extend(["--host".into(), host.into()]);
    }
    if let Some(route) = &args.route {
        run.extend(["--route".into(), route.into()]);
    }
    for setting in &args.set {
        if setting.starts_with("worktree=") || setting.starts_with("diff_base=") {
            continue;
        }
        run.extend(["--set".into(), setting.into()]);
    }
    // The review nodes inspect the fixture's change; give them the base the
    // fixture committed before it, not the writer's start point.
    run.extend([
        "--set".into(),
        format!("diff_base={}", project.base()).into(),
    ]);
    let outcome = match runner.execute(&run, None, 1_048_576, 3600) {
        Ok(launched) => {
            let wait = runner.execute(
                &[
                    program.clone(),
                    "--project".into(),
                    project.path().as_os_str().to_owned(),
                    "--state-dir".into(),
                    state_dir.as_os_str().to_owned(),
                    "--json".into(),
                    "wait".into(),
                    graph.clone().into(),
                    "--timeout".into(),
                    "3600".into(),
                ],
                None,
                1_048_576,
                3700,
            );
            let status = runner.execute(
                &[
                    program,
                    "--project".into(),
                    project.path().as_os_str().to_owned(),
                    "--state-dir".into(),
                    state_dir.as_os_str().to_owned(),
                    "--json".into(),
                    "status".into(),
                    graph.into(),
                ],
                None,
                1_048_576,
                60,
            );
            match (wait, status) {
                (Ok(wait), Ok(status)) => Ok(format!(
                    "{}{}{}",
                    String::from_utf8_lossy(&launched),
                    String::from_utf8_lossy(&wait),
                    String::from_utf8_lossy(&status)
                )),
                (Err(error), status) => Err(format!(
                    "{}\nsmoke node report: {}",
                    error,
                    status
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_else(|err| err)
                )),
                (Ok(_), Err(error)) => Err(format!(
                    "{error}\nsmoke launch: {}",
                    String::from_utf8_lossy(&launched)
                )),
            }
        }
        Err(error) => Err(error),
    };
    report_smoke_cleanup(
        outcome,
        binding.and_then(|project| forget_smoke_binding(state_dir, project)),
    )
}

fn report_smoke_cleanup<T>(
    outcome: Result<T, String>,
    cleanup: Result<(), String>,
) -> Result<T, String> {
    // Cleanup is secondary to the smoke result, but leaving a scratch binding
    // behind is never silent: warn on stderr and keep the warning in a failed
    // smoke's report.
    let Err(error) = cleanup else {
        return outcome;
    };
    let warning = format!("smoke scratch project binding was not removed: {error}");
    warn(
        Warning::new(
            provenant_records::warning::SMOKE_BINDING_RETAINED,
            warning.clone(),
        )
        .once_per_change(),
    );
    match outcome {
        Ok(output) => Ok(output),
        Err(error) => Err(format!("{error}\n{warning}")),
    }
}

fn preview_smoke(
    runner: &UpgradeRunner,
    program: &Path,
    state_dir: &Path,
    args: &cli::Upgrade,
) -> Result<Vec<u8>, String> {
    let project = smoke_project(runner, &std::env::temp_dir())?;
    let binding = smoke_project_binding(state_dir, project.path());
    let mut argv = vec![
        program.as_os_str().to_owned(),
        "--project".into(),
        project.path().as_os_str().to_owned(),
        "--state-dir".into(),
        state_dir.as_os_str().to_owned(),
        "--json".into(),
        "run".into(),
        "smoke".into(),
        "--explain".into(),
    ];
    if let Some(host) = &args.host {
        argv.extend(["--host".into(), host.into()]);
    }
    if let Some(route) = &args.route {
        argv.extend(["--route".into(), route.into()]);
    }
    for setting in &args.set {
        if setting.starts_with("diff_base=") {
            continue;
        }
        argv.extend(["--set".into(), setting.into()]);
    }
    argv.extend([
        "--set".into(),
        format!("diff_base={}", project.base()).into(),
    ]);
    let outcome = runner.execute(&argv, None, 1_048_576, 60);
    report_smoke_cleanup(
        outcome,
        binding.and_then(|project| forget_smoke_binding(state_dir, project)),
    )
}

impl UpgradeRunner {
    fn execute(
        &self,
        argv: &[std::ffi::OsString],
        input: Option<&[u8]>,
        limit: usize,
        seconds: u64,
    ) -> Result<Vec<u8>, String> {
        self.execute_at(argv, &self.cwd, input, limit, seconds)
    }

    fn execute_at(
        &self,
        argv: &[std::ffi::OsString],
        cwd: &Path,
        input: Option<&[u8]>,
        limit: usize,
        seconds: u64,
    ) -> Result<Vec<u8>, String> {
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
        let mut options = ExecOptions::new(argv, cwd, std::time::Duration::from_secs(seconds));
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

/// What a host report attests to. Drives the status verb in the human view;
/// a typed signal keeps the renderer from matching on the host's name or prose.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostPhase {
    Upgrade,
    Verify,
}

#[derive(Serialize)]
struct HostUpgrade {
    host: String,
    success: bool,
    output: String,
    #[serde(skip)]
    phase: HostPhase,
    /// Installed app version, reported for the local host.
    #[serde(skip_serializing_if = "Option::is_none")]
    app: Option<apps::Status>,
}

#[path = "install_report.rs"]
mod report;
use report::{first_error_line, graph_verdict, innermost_error_text, is_envelope, smoke_verdict};

/// Refusal detail: name every failing host once with its bounded one-line
/// reason. The full per-host view lives in `--verify --json`; it must never
/// enter `error.message`, where a large capture could be paged away by the
/// renderer.
fn failed_hosts_message(hosts: &[HostUpgrade]) -> String {
    let failed = hosts.iter().filter(|host| !host.success).count();
    let mut message = format!("{failed} host(s) failed:\n");
    for host in hosts.iter().filter(|host| !host.success) {
        message.push_str(&format!(
            "{}: {}\n",
            host.host,
            first_error_line(&host.output)
        ));
    }
    message
}

/// The `UPGRADE_FAILED` refusal. The renderer drops human output when an error
/// is set, so the next step points at the typed per-host detail in `--json`,
/// not at `--full`.
fn failed_upgrade_result(hosts: Vec<HostUpgrade>) -> CommandResult<Vec<HostUpgrade>> {
    let message = failed_hosts_message(&hosts);
    let mut result = CommandResult::new("upgrade", Outcome::Error, hosts);
    result.error = Some(CommandError::new(
        "UPGRADE_FAILED",
        message,
        None,
        "run `provenant upgrade --verify --json` for the full per-host detail, resolve the failing host, then rerun `provenant upgrade`",
    ));
    result
}

/// Compact detail for one host: capability notices become a count, smoke
/// envelopes become a verdict, and no other line is a raw JSON envelope.
fn compact_host_detail(host: &HostUpgrade) -> String {
    if host.phase == HostPhase::Verify {
        return smoke_verdict(&host.output);
    }
    let mut notices = 0usize;
    let mut detail = String::new();
    for line in host.output.lines() {
        if line.contains(provenant_config::NATIVE_CAPABILITY_UNAVAILABLE) {
            notices += 1;
        } else if !is_envelope(line) {
            detail.push_str(line);
            detail.push('\n');
        }
    }
    if notices > 0 {
        detail.push_str(&format!(
            "  {notices} native capability notice{}; use --full for detail\n",
            if notices == 1 { "" } else { "s" }
        ));
    }
    detail
}

/// One leading status line per host, followed by compact detail. `--full`
/// keeps every captured line instead.
fn render_upgrade_hosts(hosts: &[HostUpgrade], full: bool) -> String {
    let mut human = String::new();
    for host in hosts {
        let status = match (host.success, host.phase) {
            (true, HostPhase::Verify) => "verified".to_owned(),
            (true, HostPhase::Upgrade) => "upgraded".to_owned(),
            (false, _) => format!("failed - {}", first_error_line(&host.output)),
        };
        human.push_str(&format!("{}: {}\n", host.host, status));
        if full {
            human.push_str(&host.output);
            if !host.output.ends_with('\n') {
                human.push('\n');
            }
        } else {
            human.push_str(&compact_host_detail(host));
        }
        if let Some(app) = &host.app {
            human.push_str(&format!(
                "  app: {} ({})\n",
                app.path,
                app.version.as_deref().unwrap_or("version unknown")
            ));
            if app.warning.is_some() {
                human.push_str(&format!("  {}\n", apps::advisory(app)));
            }
        }
    }
    human
}

pub(super) fn run_upgrade(args: cli::Upgrade, globals: &cli::Globals) -> u8 {
    let (environment, targets) = match selected_targets(globals, &[]) {
        Ok(value) => value,
        Err(error) => return edge_error("upgrade", error, globals),
    };
    if args.verify_only {
        return run_verify_only(&targets, &args, globals);
    }
    if args.dry_run {
        let runner = match runner_for_verify(&targets) {
            Ok(runner) => runner,
            Err(error) => {
                return super::fail(
                    "upgrade",
                    Outcome::Error,
                    "UPGRADE_VERIFY",
                    &error,
                    "configure a valid verify runner, then rerun `provenant upgrade --dry-run`",
                    globals,
                );
            }
        };
        let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("provenant"));
        let output = preview_smoke(&runner, &program, targets.roots.state.as_path(), &args);
        return match output {
            Ok(bytes) => {
                let human = format!(
                    "Smoke graph nodes from the running binary (not launched):\n{}",
                    String::from_utf8_lossy(&bytes)
                );
                super::emit(
                    &CommandResult::new(
                        "upgrade",
                        Outcome::Ok,
                        serde_json::json!({"verify": "dry_run", "preview": String::from_utf8_lossy(&bytes)}),
                    ),
                    globals,
                    &human,
                    false,
                )
            }
            Err(error) => super::fail(
                "upgrade",
                Outcome::Error,
                "UPGRADE_VERIFY",
                &error,
                "fix the reported smoke preview failure, then rerun `provenant upgrade --dry-run`",
                globals,
            ),
        };
    }
    match read_install_receipt(&targets) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return super::fail(
                "upgrade",
                Outcome::Invalid,
                "UPGRADE_RECEIPT",
                "No install receipt; run install first",
                "run `provenant install` first, then rerun `provenant upgrade`",
                globals,
            );
        }
        Err(error) => return install_error("upgrade", &error, globals),
    }
    // The release tag or source manifest version is the version being installed.
    // Keep it so the final report can compare the app with the CLI that was just
    // installed, not with the version of the running binary.
    let mut upgrade_version: Option<String> = None;
    let mut version_warning: Option<String> = None;
    let upgraded = (|| -> Result<Vec<HostUpgrade>, String> {
        let config = provenant_cli::config_reads::read_for(&targets.roots, "install")
            .map_err(|error| error.to_string())?;
        let fabric = config.domains.fabric;
        let mut environment: Vec<_> = std::env::vars_os().collect();
        // The Rust report owns the app line; keep install.sh from printing it twice.
        environment.push(("PROVENANT_UPGRADE_QUIET_APP".into(), "true".into()));
        let runner = UpgradeRunner {
            executor: provenant_os::exec::SystemExecutor::new(1)
                .map_err(|error| error.to_string())?,
            environment,
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
            let manifest = source.join("tools/front-door/Cargo.toml");
            upgrade_version = std::fs::read_to_string(&manifest)
                .ok()
                .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
                .and_then(|document| {
                    document
                        .get("package")
                        .and_then(|package| package.get("version"))
                        .and_then(|version| version.as_str())
                        .map(str::to_owned)
                });
            if upgrade_version.is_none() {
                version_warning = Some(format!(
                    "could not read the installed CLI version from {}; the app comparison was skipped",
                    manifest.display()
                ));
            }
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
            upgrade_version = Some(tag.trim_start_matches('v').to_owned());
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
                phase: HostPhase::Upgrade,
                app: None,
            },
            Err(output) => HostUpgrade {
                host,
                success: false,
                output,
                phase: HostPhase::Upgrade,
                app: None,
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
            "inspect the reported failure, resolve it, then rerun `provenant upgrade`",
            globals,
        ),
        Ok(mut hosts) => {
            if args.verify && hosts.iter().all(|host| host.success) {
                let verified = runner_for_verify(&targets).and_then(|runner| {
                    verify_installed_smoke(&runner, &environment, &targets, &args)
                });
                match verified {
                    Ok(output) => hosts.push(HostUpgrade {
                        host: "smoke".into(),
                        success: true,
                        output,
                        phase: HostPhase::Verify,
                        app: None,
                    }),
                    Err(output) => hosts.push(HostUpgrade {
                        host: "smoke".into(),
                        success: false,
                        output,
                        phase: HostPhase::Verify,
                        app: None,
                    }),
                }
            }
            let local_installed = hosts
                .iter()
                .any(|host| host.host == "local" && host.success);
            let app_status = if local_installed {
                upgrade_version
                    .as_deref()
                    .and_then(|version| apps::status(&targets.home, version))
            } else {
                None
            };
            if let Some(local) = hosts.iter_mut().find(|host| host.host == "local") {
                local.app = app_status;
            }
            let success = hosts.iter().all(|host| host.success);
            let mut human = render_upgrade_hosts(&hosts, globals.full);
            if let Some(warning) = &version_warning {
                human.push_str(&format!("  Warning: {warning}\n"));
            }
            let result = if success {
                CommandResult::new("upgrade", Outcome::Ok, hosts)
            } else {
                failed_upgrade_result(hosts)
            };
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
            return super::fail(
                "uninstall",
                Outcome::Error,
                "WAIT_BUDGET",
                &error,
                "wait for the running operation to release its lease, then rerun `provenant uninstall`",
                globals,
            );
        }
    };
    let mut reporter = PreviewReporter {
        home: Some(targets.home.clone()),
        ..Default::default()
    };
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
#[path = "install_tests.rs"]
mod tests;
