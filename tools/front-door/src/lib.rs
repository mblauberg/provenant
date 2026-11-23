mod graph_interface;
mod graph_support;
// The process edge composes typed owners and owns ordinary result delivery.
// `main.rs` parses and calls `dispatch`; the worktree driver includes this
// file and dispatches with its fixture seams. Plain comments only: `include!`
// refuses inner attributes.
mod admit;
mod apps;
mod build_cmd;
mod chair;
mod check_providers;
mod checks;
mod ci;
mod cloud;
pub mod commands;
#[cfg(test)]
mod compose_tests;
mod console;
mod custody;
mod disk;
mod disk_guard;
mod fabric;
mod notes;
mod tick;
mod tick_agent;
mod tracker;

mod chair_log_retention;
mod checkpoint_retention;
mod event_delivery;
mod events;
mod gc_support;
mod gc_sweep;
mod handoff;
mod hook_diagnostics;
mod hook_state;
mod hook_worktree;
mod hooks;
mod hosts;
mod install;
mod lane_cleanup;
pub mod local_answers;
mod overflow;
mod port_receipt_retention;
mod pr;
#[cfg(test)]
mod pr_land_tests;
mod process_tree;
mod provider_retention;
mod release_smoke;
mod retention_owner;
mod roots;
mod scratch_dir;
mod scratch_owner;
mod settlement;
mod staging_gc;
mod state_handoff;
mod status_wait;
mod stranded_work;
mod usage;
mod verify;
#[cfg(test)]
pub mod worktree_tests;
mod worktrees;
use provenant_cli::{Command, ParseOutcome, RenderError, RenderMode};
use provenant_records::{CommandError, CommandResult, Outcome, VersionInfo};
use roots::{Product, Workspace};
use serde::Serialize;
use std::{
    ffi::OsString,
    io::{self, Write},
};

/// Process-wide seams the entry point selects once. The trait lives in the CLI
/// crate because a registry `Invocation` carries it; the binary re-exports it
/// so `provenant::Process` stays the name the entry point and fixtures use.
pub use provenant_cli::Process;
/// The installed binary's seams.
pub struct System;
impl Process for System {}

fn identity() -> Result<VersionInfo, Box<dyn std::error::Error>> {
    Ok(VersionInfo {
        extra: Default::default(),
        source_digest: provenant_records::SourceDigest::parse(env!("PROVENANT_SOURCE_DIGEST"))?,
        asset_digest: provenant_records::SourceDigest::parse(env!("PROVENANT_ASSET_DIGEST"))?,
        target: env!("PROVENANT_TARGET_TRIPLE").into(),
        toolchain: env!("PROVENANT_TOOLCHAIN").into(),
        record_schema_version: 1,
        config_schema_version: provenant_config::CONFIG_SCHEMA_VERSION,
    })
}
fn human_inbox(
    command: provenant_cli::inbox::HumanCommand,
    path: &str,
    globals: &provenant_cli::Globals,
) -> u8 {
    match roots::resolve(globals, Workspace::Project, Product::Required) {
        Ok(resolved) => provenant_cli::inbox::run(
            command,
            path,
            globals,
            &resolved.roots,
            &resolved.home,
            &resolved.project,
        ),
        Err(error) => fail(
            path,
            Outcome::Error,
            "ROOT",
            &error.to_string(),
            REPAIR_ROOT,
            globals,
        ),
    }
}
fn emit<T: Serialize>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
) -> u8 {
    emit_summarised(result, globals, human, read_only, None)
}
/// `emit` for an owner that supplies its own compact view.
pub(crate) fn emit_summarised<T: Serialize>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
    summary: Option<&provenant_cli::Summary>,
) -> u8 {
    emit_with_writers(
        result,
        globals,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        human,
        read_only,
        summary,
    )
}

/// The emission seam shared by production and targeted tests: a refusal's
/// rendered bytes go to the supplied writers, so a test can drive the same
/// `fail_with_cause` path instead of hand-building a `CommandResult`.
pub(crate) fn emit_with_writers<T: Serialize, Wout: Write, Werr: Write>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    stdout: &mut Wout,
    stderr: &mut Werr,
    human: &str,
    read_only: bool,
    summary: Option<&provenant_cli::Summary>,
) -> u8 {
    let explanation = if summary.is_none() && result.command == "land" {
        serde_json::to_value(&result.data)
            .ok()
            .and_then(|data| land_explanation(&data, globals))
    } else {
        None
    };
    let summary = summary.or(explanation.as_ref());
    let mode = if !globals.json && matches!(result.command.as_str(), "help" | "version") {
        RenderMode::Human {
            text: human,
            read_only,
            summary: None,
        }
    } else {
        globals.render_mode(human, read_only, summary)
    };
    let rendered = provenant_cli::render_or_publish(
        result,
        mode,
        stdout,
        stderr,
        |value| overflow::publish(value, globals),
        // A published overflow keeps the result's status in its envelope, so
        // the exit code matches it; only a real render/IO failure exits 3.
        provenant_cli::OverflowPolicy::Proceed,
    );
    match rendered {
        Ok(status) => provenant_cli::exit_status(&status),
        Err(_) => {
            let _ = io::stderr().write_all(b"provenant: IO: cannot write command result\n");
            3
        }
    }
}

/// A policy explanation is a structured result; compact output names omitted rows.
fn land_explanation(
    data: &serde_json::Value,
    globals: &provenant_cli::Globals,
) -> Option<provenant_cli::Summary> {
    let fields = data.get("fields")?.as_object()?;
    let mut rows = Vec::new();
    for (key, label) in [
        ("gate.kind", "gate"),
        ("gate.checks", "checks"),
        ("gate.commands", "commands"),
        ("approval_sources", "approval"),
        ("approval_tier", "approval tier"),
        ("approval_carry", "approval carry"),
        ("local_gate.mode", "local gate"),
        ("integration", "integration"),
        ("publish", "publication"),
        ("budget.advisory_micros", "budget advisory (millionths)"),
        ("budget.cap_micros", "budget cap (millionths)"),
    ] {
        if let Some(field) = fields.get(key) {
            let value = field.get("value")?;
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            let source = field
                .pointer("/source/layer")
                .and_then(serde_json::Value::as_str)?;
            rows.push(format!("{label}: {value} ({source})"));
        }
    }
    if let Some(patience) = data.get("ci_patience_seconds") {
        let source = data["ci_patience_source"].as_str()?;
        rows.push(format!("CI patience: {patience}s ({source})"));
    }
    if let Some(warnings) = data["warnings"].as_array() {
        rows.splice(
            0..0,
            warnings
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|warning| format!("warning: {warning}")),
        );
    }
    let mut detail = String::from("provenant");
    for (flag, path) in [
        ("--state-dir", globals.state_dir.as_ref()),
        ("--project", globals.project.as_ref()),
    ] {
        if let Some(path) = path {
            detail.push_str(&format!(
                " {flag} '{}'",
                path.to_string_lossy().replace('\'', "'\\''")
            ));
        }
    }
    detail.push_str(" land --explain --json");
    Some(provenant_cli::Summary::new(
        "Landing policy",
        rows,
        Some(detail),
    ))
}
/// `emit_summarised` for a result the caller narrows rather than reads from an
/// artefact: an oversized result is bounded with a warning, never published, so
/// no file is left in the results area on any path.
fn emit_unpublished<T: Serialize>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
    summary: Option<&provenant_cli::Summary>,
) -> u8 {
    let rendered = render_unpublished(
        result,
        globals,
        human,
        read_only,
        summary,
        provenant_cli::MAX_RESULT_BYTES,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    );
    const REPAIR: &str = "narrow it with --since and --until";
    match rendered {
        Ok(omitted) => {
            if omitted > 0 {
                let _ = writeln!(
                    io::stderr().lock(),
                    "warning: [RESULT_TOO_LARGE] {} exceeds 1 MiB (source: COMMAND_RESULT_MAX_BYTES); showing a bounded view with {omitted} bytes omitted; {REPAIR}",
                    result.command
                );
            }
            provenant_cli::exit_status(&result.status)
        }
        Err(RenderError::TooLarge) => {
            // JSON cannot be clipped mid-object: emit a bounded marker instead.
            let omitted = serde_json::to_vec(result)
                .map(|bytes| bytes.len().saturating_sub(provenant_cli::MAX_RESULT_BYTES))
                .unwrap_or_default();
            let _ = writeln!(
                io::stderr().lock(),
                "warning: [RESULT_TOO_LARGE] {} exceeds 1 MiB (source: COMMAND_RESULT_MAX_BYTES); showing a bounded view with {omitted} bytes omitted; {REPAIR}",
                result.command
            );
            let marker = serde_json::json!({
                "truncated": true,
                "omitted_bytes": omitted,
                "source": "COMMAND_RESULT_MAX_BYTES",
                "repair": REPAIR,
            });
            let mut bounded = CommandResult::new(&result.command, result.status, marker);
            // The marker replaces the oversized data, not the refusal. Keep the
            // original typed error so an oversized failure still carries its
            // code, repair and diagnostic, as the console and disk edges do.
            bounded.error = result
                .error
                .as_ref()
                .map(|error| provenant_cli::bounded_error(error, 512));
            let _ = provenant_cli::render(&bounded, RenderMode::Json, &mut io::stdout().lock());
            provenant_cli::exit_status(&result.status)
        }
        Err(_) => {
            let _ = io::stderr().write_all(b"provenant: IO: cannot write command result\n");
            3
        }
    }
}
/// Render a result within `limit`, bounding the human view with an explicit
/// omitted-size marker when it is larger. Returns the bytes omitted from the
/// owner's text (zero when the result fit).
#[allow(clippy::too_many_arguments)]
fn render_unpublished<T: Serialize>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
    summary: Option<&provenant_cli::Summary>,
    limit: usize,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<usize, RenderError> {
    let full_bytes = serde_json::to_vec(result)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    // The size bounds the result, not its presentation: a compact view of an
    // oversized result is still an oversized result.
    let oversized = !globals.json && full_bytes > limit;
    let clipped;
    let mut omitted = 0;
    let mode = if oversized {
        let (text, count) = provenant_cli::clip_for_output(human, limit);
        clipped = text;
        omitted = count;
        globals.render_mode(&clipped, read_only, summary)
    } else {
        globals.render_mode(human, read_only, summary)
    };
    if !globals.json && result.error.is_some() {
        provenant_cli::render_within(result, mode, stderr, limit)?;
    } else {
        provenant_cli::render_within(result, mode, stdout, limit)?;
    }
    Ok(omitted)
}
/// Accounts by adapter, then the registered rows; none needs action.
fn accounts_summary(settings: &provenant_providers::accounts::Settings) -> provenant_cli::Summary {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for entry in settings.entries.values() {
        *counts.entry(entry.adapter.as_str()).or_default() += 1;
    }
    let total = settings.entries.len();
    let headline = if total == 0 {
        "no accounts registered".to_owned()
    } else {
        format!(
            "{total} account{}: {}",
            if total == 1 { "" } else { "s" },
            provenant_cli::Summary::tally(&counts.into_iter().collect::<Vec<_>>())
        )
    };
    let rows = settings
        .entries
        .values()
        .map(|entry| {
            format!(
                "{} {} {}{}",
                entry.adapter,
                entry.label,
                entry.key,
                if entry.hidden { " hidden" } else { "" }
            )
        })
        .collect();
    provenant_cli::Summary::new(headline, rows, None)
}
/// Reusable next steps for the common refusals. Every `fail` call supplies a
/// repair explicitly, so the renderer never has to synthesise one.
const REPAIR_ROOT: &str = "run from a project checkout or pass `--project PATH`";
const REPAIR_CONFIG: &str = "run `provenant check config --json` and correct the reported setting";
const REPAIR_IO: &str = "check the reported path and file permissions, then retry";

/// The one construction of a refusal's transport facts. `cause` is extra
/// evidence beside the short message and is rendered indented in human output
/// and as `error.cause` in JSON; owners whose cause repeats the message let the
/// renderer suppress the duplicate.
pub(crate) fn command_error(
    code: &str,
    message: &str,
    cause: Option<&str>,
    repair: &str,
) -> CommandError {
    CommandError::new(code, message, cause, repair)
}
fn fail(
    command: &str,
    status: Outcome,
    code: &str,
    message: &str,
    repair: &str,
    globals: &provenant_cli::Globals,
) -> u8 {
    fail_with_repair(command, status, code, message, repair, globals)
}
fn fail_with_repair(
    command: &str,
    status: Outcome,
    code: &str,
    message: &str,
    repair: &str,
    globals: &provenant_cli::Globals,
) -> u8 {
    fail_with_cause(command, status, code, message, None, repair, globals)
}
/// `fail_with_repair` with a cause the operator needs to see, such as the
/// bounded output of a failing `build.prepare` command.
fn fail_with_cause(
    command: &str,
    status: Outcome,
    code: &str,
    message: &str,
    cause: Option<&str>,
    repair: &str,
    globals: &provenant_cli::Globals,
) -> u8 {
    fail_with_cause_with_writers(
        command,
        status,
        code,
        message,
        cause,
        repair,
        globals,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

/// The testable seam for `fail_with_cause`: the same refusal, emitted to the
/// supplied writers so a test observes exactly what production renders.
// The argument list mirrors `fail_with_cause` plus the two writers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fail_with_cause_with_writers<Wout: Write, Werr: Write>(
    command: &str,
    status: Outcome,
    code: &str,
    message: &str,
    cause: Option<&str>,
    repair: &str,
    globals: &provenant_cli::Globals,
    stdout: &mut Wout,
    stderr: &mut Werr,
) -> u8 {
    fail_with_command_error_with_writers(
        command,
        status,
        command_error(code, message, cause, repair),
        globals,
        stdout,
        stderr,
    )
}

/// Emit an already-mapped refusal. Owners with a typed error use this with
/// [`crate::worktrees::Error::command_error`] so the mapping lives in one place.
pub(crate) fn fail_with_command_error(
    command: &str,
    status: Outcome,
    error: CommandError,
    globals: &provenant_cli::Globals,
) -> u8 {
    fail_with_command_error_with_writers(
        command,
        status,
        error,
        globals,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

pub(crate) fn fail_with_command_error_with_writers<Wout: Write, Werr: Write>(
    command: &str,
    status: Outcome,
    error: CommandError,
    globals: &provenant_cli::Globals,
    stdout: &mut Wout,
    stderr: &mut Werr,
) -> u8 {
    let mut result = CommandResult::new(command, status, ());
    result.error = Some(error);
    emit_with_writers(&result, globals, stdout, stderr, "", false, None)
}
fn fail_with_diagnostic(
    command: &str,
    status: Outcome,
    code: &str,
    diagnostic: &provenant_records::diagnostic::Diagnostic,
    globals: &provenant_cli::Globals,
) -> u8 {
    let message = diagnostic
        .detail
        .clone()
        .unwrap_or_else(|| diagnostic.code.clone());
    let mut result = CommandResult::new(command, status, ());
    let mut error = command_error(
        code,
        &message,
        diagnostic.detail.as_deref(),
        diagnostic
            .next
            .as_deref()
            .unwrap_or("correct the reported condition and rerun the command"),
    );
    error.diagnostic = Some(diagnostic.into());
    result.error = Some(error);
    emit(&result, globals, "", false)
}
/// Resolve Run's composition paths, then emit the owner's reply.
fn run_edge(
    leaf: &str,
    globals: &provenant_cli::Globals,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> provenant_cli::run::Reply,
) -> u8 {
    run_edge_workspace(leaf, globals, Workspace::Project, action)
}
fn run_edge_workspace(
    leaf: &str,
    globals: &provenant_cli::Globals,
    workspace: Workspace<'_>,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> provenant_cli::run::Reply,
) -> u8 {
    run_edge_emitting(leaf, globals, workspace, true, action)
}
/// `run_edge_workspace`; with `publish` false an oversized result is bounded
/// with a warning and nothing is written to the results area.
fn run_edge_emitting(
    leaf: &str,
    globals: &provenant_cli::Globals,
    workspace: Workspace<'_>,
    publish: bool,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> provenant_cli::run::Reply,
) -> u8 {
    match roots::resolve(globals, workspace, Product::Required) {
        Ok(resolved) => {
            let reply = action(&provenant_cli::run::Edge {
                roots: &resolved.roots,
                home: &resolved.home,
                assets: &resolved.assets,
                cwd: &resolved.project,
            });
            if publish {
                emit_summarised(
                    &reply.result,
                    globals,
                    &reply.human,
                    reply.read_only,
                    reply.summary.as_ref(),
                )
            } else {
                emit_unpublished(
                    &reply.result,
                    globals,
                    &reply.human,
                    reply.read_only,
                    reply.summary.as_ref(),
                )
            }
        }
        Err(error) => {
            let status = match error {
                roots::RootError::Checkout | roots::RootError::State(_) => Outcome::Invalid,
                _ => Outcome::Error,
            };
            fail(
                leaf,
                status,
                "ROOT",
                &error.to_string(),
                REPAIR_ROOT,
                globals,
            )
        }
    }
}
/// Adds each account's sync state to `accounts list`: one line per account
/// and the full findings in `data.sync`.
fn report_account_sync(
    globals: &provenant_cli::Globals,
    accounts: &provenant_providers::accounts::Settings,
    reply: &mut provenant_cli::run::Reply,
) {
    match install::account_status(globals) {
        Ok(statuses) => {
            reply.human = accounts
                .entries
                .values()
                .map(|entry| {
                    let state = statuses
                        .iter()
                        .find(|status| {
                            status.label == entry.label
                                && status.harness.spec().name == entry.adapter
                        })
                        .map_or(provenant_config::AccountState::NotInstalled, |s| s.state);
                    let state = serde_json::to_value(state)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    format!("{} {} {} {state}\n", entry.adapter, entry.label, entry.key)
                })
                .collect();
            reply.result.data["sync"] = serde_json::json!(statuses);
        }
        Err(message) => {
            reply.result.data["sync"] = serde_json::json!({"error": message});
        }
    }
}
/// Replace the registered account's quota reading with a fresh probe and report the result.
fn probe_registered_account(
    edge: &provenant_cli::run::Edge<'_>,
    data: &provenant_providers::accounts::Settings,
    adapter: &str,
    label: &str,
    reply: &mut provenant_cli::run::Reply,
) {
    let Some(entry) = data
        .entries
        .values()
        .find(|entry| entry.adapter == adapter && entry.label == label)
    else {
        return;
    };
    let account = provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: entry.adapter.clone(),
        account_key: entry.key.clone(),
        label: entry.label.clone(),
    };
    if !provenant_providers::REGISTRY
        .iter()
        .any(|r| r.quota_source.is_some() && r.adapter.id() == account.adapter)
    {
        return;
    }
    let result = provenant_cli::config_reads::read_for(edge.roots, "quota")
        .map(|config| config.domains.quota)
        .map_err(|error| error.to_string())
        .and_then(|settings| {
            provenant_providers::quota_live::LiveQuota::new(edge.roots, edge.home, data)
                .map(|effects| {
                    effects
                        .with_refresh_minimum(settings.refresh_min_budget_seconds)
                        .with_refresh_budget(settings.refresh_budget_seconds)
                })
                .map_err(|error| error.to_string())
        })
        .and_then(|effects| {
            provenant_cli::quota::probe_registered(
                edge.roots,
                provenant_providers::REGISTRY,
                &effects,
                &account,
            )
        });
    let row = result.as_ref().ok().and_then(|report| {
        report.accounts.iter().find(|row| {
            row.account.adapter == account.adapter && row.account.account_key == account.account_key
        })
    });
    match (row, &result) {
        (Some(row), _) => {
            let status = serde_json::to_value(row.status).unwrap_or_default();
            let error = row.error.and_then(|error| serde_json::to_value(error).ok());
            reply.human.push_str(&format!(
                "quota probed: {} {}{}\n",
                account.label,
                status.as_str().unwrap_or("unknown"),
                error
                    .as_ref()
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(String::new, |error| format!(" (refresh failed: {error})")),
            ));
            reply.result.data["quota"] = serde_json::json!({"status": status, "error": error});
        }
        (None, Err(error)) => {
            reply.human.push_str(&format!(
                "quota not probed: {error}; run `provenant quota tick`\n"
            ));
            reply.result.data["quota"] = serde_json::json!({"skipped": error});
        }
        (None, Ok(_)) => {}
    }
}
/// Registration is saved before this runs. Install the managed hooks, skills
/// and shared settings into the new account directory, so it matches the
/// others without a further command. A failure leaves the registration in place.
fn sync_registered_account(
    globals: &provenant_cli::Globals,
    reply: &mut provenant_cli::run::Reply,
) {
    match install::sync_after_registration(globals) {
        Ok(Some(outcome)) => {
            reply.human.push_str(&format!(
                "synced hooks, skills and shared settings: {} changes\n",
                outcome.actions.len()
            ));
            for finding in &outcome.findings {
                let path = finding
                    .path
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                let detail = finding
                    .detail
                    .as_ref()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default();
                reply
                    .human
                    .push_str(&format!("  {}: {path}{detail}\n", finding.rule));
            }
            if outcome
                .findings
                .iter()
                .any(|finding| finding.rule == "settings_conflict")
            {
                reply.human.push_str(
                    "  keep an account's value: `provenant accounts sync --adopt <account>`; use the source's: `provenant accounts sync --force-settings`\n",
                );
            }
            reply.result.data["sync"] = serde_json::json!({
                "changes": outcome.actions.len(),
                "findings": outcome.findings,
            });
        }
        Ok(None) => {
            reply
                .human
                .push_str("not synced: no install receipt; run `provenant install`\n");
            reply.result.data["sync"] = serde_json::json!({"skipped": "no install receipt"});
        }
        Err(message) => {
            reply.result.status = Outcome::Error;
            reply.result.error = Some(CommandError::new(
                "ACCOUNT_SYNC",
                format!("account registered, but syncing managed content failed: {message}"),
                None,
                "resolve the failure, then run `provenant accounts sync`",
            ));
        }
    }
}
/// What a config command does: show, describe the schema, edit, or list history.
enum ConfigAction<'a> {
    Show,
    Ports,
    Detect,
    Path {
        dir: Option<&'a std::path::Path>,
    },
    Schema {
        all: bool,
    },
    Edit(provenant_config::KnobEdit<'a>),
    Apply {
        patch: &'a std::path::Path,
        revision: &'a str,
        dry_run: bool,
    },
    History {
        since: Option<jiff::Timestamp>,
        limit: usize,
    },
}
fn read_patch(path: &std::path::Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("patch file exceeds 1 MiB".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
/// The effective verify port for `config show`: each value with the layer that
/// supplied it. An unset base is inferred from the effective remote's HEAD and
/// labelled with the ref it came from; a port table that cannot be read stays
/// a warning instead of stopping a command that does not need verify.
fn config_show_ports(project: &std::path::Path, state: &std::path::Path) -> serde_json::Value {
    use provenant_os::exec::{SystemExecutor, Termination};
    let ports = match provenant_config::PortTable::read_for_inspection(Some(project), state) {
        Ok(ports) => ports,
        Err(error) => {
            return serde_json::json!({
                "warning": format!("port table could not be read: {error}")
            });
        }
    };
    let remote = ports.verify.remote.clone();
    let base = if ports.verify.base.is_empty() {
        let inferred = if remote.is_empty() {
            None
        } else {
            SystemExecutor::new(1)
                .ok()
                .and_then(|executor| {
                    crate::gc_support::host_git_with_stderr(
                        project,
                        &executor,
                        &[
                            "symbolic-ref",
                            "--quiet",
                            &format!("refs/remotes/{remote}/HEAD"),
                        ],
                        std::time::Duration::from_secs(5),
                        false,
                    )
                    .ok()
                })
                .and_then(|outcome| {
                    (outcome.termination == Termination::Exit(0)).then(|| {
                        String::from_utf8_lossy(&outcome.stdout.bytes)
                            .trim()
                            .to_owned()
                    })
                })
                .and_then(|target| provenant_config::infer_verify_base(&remote, &target))
        };
        match inferred {
            Some((base, source)) => serde_json::json!({
                "value": base,
                "source": format!("inferred from {source}"),
            }),
            None => serde_json::json!({
                "value": serde_json::Value::Null,
                "source": "unresolved",
                "remedy": provenant_config::verify_base_unset_error().to_string(),
            }),
        }
    } else {
        serde_json::json!({
            "value": ports.verify.base,
            "source": provenant_config::port_source_layer(ports.verify.source),
        })
    };
    serde_json::json!({
        "verify.base": base,
        "verify.remote": {
            "value": remote,
            "source": provenant_config::port_source_layer(ports.verify.source),
        },
    })
}

/// The effective `build.prepare` list with the leaf's own source. A port's
/// label follows its highest leaf, which may be a different key, so read the
/// provenance of `build.prepare` rather than `table.build.source`.
fn config_ports_prepare(table: &provenant_config::PortTable) -> serde_json::Value {
    let source = table
        .provenance()
        .get("build.prepare")
        .copied()
        .unwrap_or(table.build.source);
    serde_json::json!({
        "value": table.build.prepare,
        "source": provenant_config::port_source_layer(source),
    })
}

#[cfg(test)]
mod config_ports_tests {
    #[test]
    fn reports_effective_prepare_with_its_own_leaf_source() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        let keyed = provenant_config::host_project_dir(&state, &project).unwrap();
        std::fs::create_dir_all(&keyed).unwrap();
        std::fs::write(state.join("ports.json"), r#"{"build":{"strategy":"lane"}}"#).unwrap();
        std::fs::write(
            keyed.join("ports.json"),
            r#"{"build":{"prepare":[["node","gen.mjs"]]}}"#,
        )
        .unwrap();
        let table = provenant_config::PortTable::read_for(Some(&project), &state).unwrap();
        let value = super::config_ports_prepare(&table);
        assert_eq!(value["source"], "host_project");
        assert_eq!(value["value"][0][1], "gen.mjs");

        // A higher layer setting a different leaf must not relabel prepare.
        std::fs::write(
            project.join(".provenant/ports.json"),
            r#"{"build":{"strategy":"lane"}}"#,
        )
        .unwrap();
        let table = provenant_config::PortTable::read_for(Some(&project), &state).unwrap();
        assert_eq!(
            super::config_ports_prepare(&table)["source"],
            "host_project"
        );
    }
}

fn config_setting_value(
    config: &provenant_config::ResolvedConfig<provenant_cli::domains::Domains>,
    sections: &provenant_config::DomainSections<provenant_cli::domains::Domains>,
    name: &str,
) -> serde_json::Value {
    if let Some(knob) = config.knobs.get(name) {
        return serde_json::to_value(knob).unwrap_or_default();
    }
    provenant_config::schema(config, sections, String::new())
        .ok()
        .and_then(|schema| schema.fields.into_iter().find(|field| field.path == name))
        .map(|field| serde_json::json!({"value": field.value, "source": field.source}))
        .unwrap_or_default()
}

/// Run one config action and print the result: the effective config read back
/// for show and edits, or the schema, apply report or history.
fn config_command(command: &str, action: ConfigAction, globals: &provenant_cli::Globals) -> u8 {
    use provenant_config as config;
    let result = (|| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let context = roots::resolve(globals, Workspace::Project, Product::Required)?;
        let roots = context.roots;
        let sections = &provenant_cli::domains::SECTIONS;
        let options = config::EditOptions::new("cli");
        Ok(match action {
            ConfigAction::Ports => {
                let table = config::PortTable::read_for_inspection(
                    Some(&context.project),
                    roots.state.as_path(),
                )?;
                let ports: Vec<_> = table
                    .entries()
                    .map(|(name, source, backend, _, unavailable_reason)| {
                        let backend = match backend {
                            config::PortBackendKind::Builtin => "builtin",
                            config::PortBackendKind::Command => "command",
                            config::PortBackendKind::None => "none",
                        };
                        serde_json::json!({
                            "name": name,
                            "backend": backend,
                            "source": source,
                            "available": unavailable_reason.is_none(),
                            "unavailable_reason": unavailable_reason,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "schema_version": table.schema_version,
                    "ports": ports,
                    "build": {
                        "prepare": config_ports_prepare(&table),
                    },
                })
            }
            ConfigAction::Detect => serde_json::to_value(config::detect::detect(&context.project))?,
            ConfigAction::Path { dir } => {
                let project = dir.unwrap_or(&context.project);
                let path = config::host_project_dir(roots.state.as_path(), project)
                    .ok_or("cannot determine host project directory")?
                    .join("ports.json");
                serde_json::json!({"exists": path.is_file(), "path": path})
            }
            ConfigAction::Show => {
                let (resolved, revision) = config::show(&roots, sections)?;
                let mut value = serde_json::to_value(&resolved)?;
                value["revision"] = revision.into();
                value["admission"] =
                    serde_json::to_value(config::ResolvedAdmissionClass::resolve_all(
                        roots.state.as_path(),
                        Some(&context.project),
                        provenant_config::project_key_for(&context.project).as_deref(),
                    ))?;
                let policy = resolved.domains.policy.resolve_with_knobs(
                    &provenant_os::git::repository_path(
                        &context.project,
                        provenant_os::git::RepositoryPath::CommonDir,
                    )
                    .ok()
                    .and_then(|path| provenant_config::ProjectLocation::new(path).ok())
                    .and_then(|location| {
                        provenant_config::lookup_project(&roots, &location)
                            .ok()
                            .flatten()
                    })
                    .map(|p| p.project_id().to_string())
                    .unwrap_or_default(),
                    &resolved.knobs,
                    &Default::default(),
                );
                value["policy"] = serde_json::to_value(&policy)?;
                value["policy_render"] = policy.render().into();
                value["ports"] = config_show_ports(&context.project, roots.state.as_path());
                if globals.full {
                    let table = config::PortTable::read_for_inspection(
                        Some(&context.project),
                        roots.state.as_path(),
                    )?;
                    let serialized = serde_json::to_value(&table)?;
                    let leaves: serde_json::Map<String, serde_json::Value> = table
                        .provenance()
                        .iter()
                        .filter(|(key, _)| {
                            key.starts_with("build.")
                                || key.starts_with("verify.tiers.")
                                || *key == "verify.default_tier"
                        })
                        .filter_map(|(key, source)| {
                            let mut v = &serialized;
                            for part in key.split('.') {
                                v = v.get(part)?;
                            }
                            Some((
                                key.clone(),
                                serde_json::json!({"value": v, "source": source}),
                            ))
                        })
                        .collect();
                    value["port_leaves"] = leaves.into();
                    value["detection"] =
                        serde_json::to_value(config::detect::detect(&context.project))?;
                }
                value
            }
            ConfigAction::Schema { all } => {
                let (resolved, revision) = config::show(&roots, sections)?;
                let mut schema = config::schema(&resolved, sections, revision)?;
                if !all {
                    schema
                        .fields
                        .retain(|field| config::documented_field(&field.path));
                }
                serde_json::to_value(schema)?
            }
            ConfigAction::Edit(edit) => {
                let mut previous = serde_json::Value::Null;
                let edited = config::edit_config(&roots, sections, &options, |doc, current| {
                    previous = match edit {
                        config::KnobEdit::Set { name, .. } | config::KnobEdit::Unset(name) => {
                            config_setting_value(current, sections, name)
                        }
                        config::KnobEdit::Profile(_) => serde_json::json!(current.profile),
                    };
                    config::apply_edit(edit, doc, current, sections)
                })?;
                match edit {
                    config::KnobEdit::Set { name, .. } | config::KnobEdit::Unset(name) => {
                        serde_json::json!({"key": name, "previous": previous,
                            "effective": config_setting_value(&edited.config, sections, name), "revision": edited.after_revision})
                    }
                    config::KnobEdit::Profile(_) => serde_json::json!({"key": "profile",
                        "previous": previous, "effective": edited.config.profile,
                        "source": if matches!(edited.config.origins.get("profile"), Some(config::Origin::User { .. })) { "user" } else { "default" },
                        "revision": edited.after_revision}),
                }
            }
            ConfigAction::Apply {
                patch,
                revision,
                dry_run,
            } => {
                let patch = read_patch(patch)?;
                let options = config::EditOptions {
                    expect_revision: Some(revision),
                    dry_run,
                    ..options
                };
                let edited = config::edit_config(&roots, sections, &options, |doc, current| {
                    config::apply_patch(&patch, doc, current, sections)
                })?;
                serde_json::json!({
                    "dry_run": edited.dry_run,
                    "before_revision": edited.before_revision,
                    "after_revision": edited.after_revision,
                    "changes": edited.changes,
                })
            }
            ConfigAction::History { since, limit } => {
                serde_json::json!({ "entries": config::history(&roots, since, limit)? })
            }
        })
    })();
    match result {
        Ok(value) => {
            let human = if command == "config show" {
                if globals.full {
                    serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
                } else {
                    let sections = value
                        .as_object()
                        .map(|object| {
                            object
                                .keys()
                                .filter(|key| !matches!(key.as_str(), "origins" | "policy_render"))
                                .map(String::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    let profile = value["profile"].as_str().unwrap_or("unknown");
                    let revision = value["revision"].as_str().unwrap_or("unknown");
                    let warnings = value["warnings"].as_array().map_or(0, Vec::len);
                    format!(
                        "effective configuration: profile={profile} revision={revision} warnings={warnings}\nsections: {sections}\nUse `provenant config show --full` for values and sources, or `--json` for structured data.\n"
                    )
                }
            } else if command == "config set" || command == "config unset" {
                format!(
                    "{}: {}{} (previous: {})\n",
                    value["key"].as_str().unwrap_or("profile"),
                    value["effective"],
                    value["source"]
                        .as_str()
                        .map(|source| format!(" (source: {source})"))
                        .unwrap_or_default(),
                    value["previous"]
                )
            } else {
                provenant_cli::terse(&value)
            };
            emit(
                &CommandResult::new(command, Outcome::Ok, value),
                globals,
                &human,
                true,
            )
        }
        Err(error) => {
            let status = match error.downcast_ref::<provenant_config::ConfigError>() {
                Some(
                    provenant_config::ConfigError::Read { .. }
                    | provenant_config::ConfigError::ProjectBindingFilesystem(_),
                )
                | None => Outcome::Error,
                Some(_) => Outcome::Invalid,
            };
            let code = match error.downcast_ref::<provenant_config::ConfigError>() {
                Some(provenant_config::ConfigError::StaleRevision { .. }) => "STALE_REVISION",
                Some(error) if error.refusal_code().is_some() => error
                    .refusal_code()
                    .unwrap_or(provenant_config::shared_settings::NEVER_SYNCED_CODE),
                _ => "CONFIG",
            };
            if code == provenant_config::shared_settings::NEVER_SYNCED_CODE {
                let projection = provenant_config::shared_settings::never_synced_diagnostic();
                let mut result = CommandResult::new(command, status, ());
                let mut error = CommandError::new(
                    projection.code.clone(),
                    error.to_string(),
                    projection.detail.as_deref(),
                    projection
                        .next
                        .as_deref()
                        .unwrap_or("run `provenant accounts sync`"),
                );
                error.diagnostic = Some((&projection).into());
                result.error = Some(error);
                return emit(&result, globals, "", false);
            }
            fail(
                command,
                status,
                code,
                &error.to_string(),
                REPAIR_CONFIG,
                globals,
            )
        }
    }
}
/// Unsupported pool efforts are advisory at load and remain visible in
/// routine diagnostics with the origin recorded for each configured value.
pub(crate) fn pool_effort_warnings(roots: &provenant_core::Roots) -> Vec<String> {
    let Ok(config) = provenant_cli::config_reads::read_for(roots, "route preferences") else {
        return Vec::new();
    };
    let Ok(catalogue) = provenant_routing::Catalogue::bundled() else {
        return Vec::new();
    };
    config
        .domains
        .routing
        .pool_effort_warnings_with_sources(&catalogue, |key| match config.origins.get(key) {
            Some(provenant_config::Origin::User { .. }) => {
                format!("config.toml ({key})")
            }
            Some(provenant_config::Origin::Compiled { profile }) => {
                format!("compiled {profile:?} profile ({key})")
            }
            None => format!("compiled defaults ({key})"),
        })
}
/// Registered account settings, read through the binary's domain aggregate.
fn account_settings(
    roots: &provenant_core::Roots,
) -> Result<provenant_providers::accounts::Settings, provenant_config::ConfigError> {
    provenant_cli::config_reads::read_for(roots, "accounts").map(|config| config.domains.accounts)
}
/// Explicit Claude registrations only, never implicit directory discovery.
pub(crate) fn registered_claude(
    roots: &provenant_core::Roots,
) -> Result<Vec<provenant_providers::identity::RegisteredAccount>, Box<dyn std::error::Error>> {
    provenant_providers::quota_live::registered_claude(&account_settings(roots)?)
}
fn live_quota(
    resolved: &roots::Resolved,
) -> Result<provenant_providers::quota_live::LiveQuota, Box<dyn std::error::Error>> {
    let settings = provenant_cli::config_reads::read_for(&resolved.roots, "quota")?.domains;
    Ok(provenant_providers::quota_live::LiveQuota::new(
        &resolved.roots,
        &resolved.home,
        &settings.accounts,
    )?
    .with_refresh_minimum(settings.quota.refresh_min_budget_seconds)
    .with_refresh_budget(settings.quota.refresh_budget_seconds))
}
/// The LaunchAgent's long-running entrypoint, `fabric peer-agent`.
fn peer_agent(globals: &provenant_cli::Globals) -> u8 {
    use provenant_fabric_runtime::peer_agent;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if provenant_cli::fabric::in_lane() {
            return Err(
                provenant_fabric_runtime::coordination::CoordinationError::LaneMutation.into(),
            );
        }
        let resolved = roots::resolve(globals, Workspace::Home, Product::Required)?;
        Ok(peer_agent::serve(
            &std::env::current_exe()?,
            resolved.roots.state.as_path(),
            &resolved.home,
        )?)
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            peer_agent::log(&error.to_string());
            3
        }
    }
}
/// The guardian entry for one launched attempt, then any settled graph step.
fn supervise(descriptors: &[u64], globals: &provenant_cli::Globals, process: &dyn Process) -> u8 {
    let descriptors: Vec<_> = descriptors
        .iter()
        .map(|fd| OsString::from(fd.to_string()))
        .collect();
    let registry = provenant_cli::run::Registry::default();
    let holder = provenant_run::RunHolder {
        providers: &registry,
        resources: process.admission_resources(),
        clock: process.clock(),
    };
    process.holder(&holder, &mut |holder| {
        let workers = process.workers();
        let holder = provenant_cli::run::HostHolder::new(holder)
            .with_graph_wake(move |context, id| {
                fabric::advance_live_graph(context, id, workers).map_err(|error| error.to_string())
            })
            .with_graph_read(move |context, input, cancel, within| {
                fabric::read_lane_graph(context, input, cancel, within, workers)
            });
        let bind_publication =
            |environment: &[(Vec<u8>, Vec<u8>)],
             secret: Option<&provenant_os::exec::SecretInput>,
             inherited: &provenant_os::exec::SecretValues| {
                let mut values = provenant_redact::launch_values(
                    environment,
                    secret.map(|value| value.bytes.as_slice()),
                )
                .map_err(|_| provenant_os::exec::FilterError)?;
                values.extend(inherited.0.iter().cloned());
                let scope = provenant_redact::publication_scope(values)
                    .map_err(|_| provenant_os::exec::FilterError)?;
                Ok(Box::new(scope) as Box<dyn std::any::Any>)
            };
        let hooks = provenant_supervisor::GuardianHooks {
            publication_scope: Some(&bind_publication),
            holder: Some(&holder),
            stdout_decoder: Some(holder.stdout_decoder()),
            stderr_decoder: Some(holder.stderr_decoder()),
            ..Default::default()
        };
        match process.supervise(&descriptors, hooks) {
            Ok(()) => match fabric::advance_settled_graphs(&holder, process.workers()) {
                Ok(()) => 0,
                Err(error) => fail(
                    "internal supervise",
                    Outcome::Error,
                    "GRAPH_ADVANCE",
                    &error.to_string(),
                    "run `provenant tick` after the reported cause is resolved",
                    globals,
                ),
            },
            Err(error) => {
                let (status, code) = error.outcome();
                fail(
                    "internal supervise",
                    status,
                    code,
                    &error.to_string(),
                    "rerun the command; if it persists, report its `--json` refusal",
                    globals,
                )
            }
        }
    })
}
fn run(inv: provenant_cli::Invocation, process: &dyn Process) -> u8 {
    let globals = &inv.globals;
    let command = &inv.command;
    if let Command::Hook(hook) = command {
        return hooks::run(hook.clone(), globals);
    }
    if let Command::ProcessTree { root } = command {
        return process_tree::run(*root, globals);
    }
    // Lanes may read quota and routing state (`quota show`, `quota history`
    // and `route resolve`), but must not mutate account state: the mutating
    // commands run provider probes with the chair's credentials and write the
    // chair's quota record. Each refusal names the command to run outside the
    // lane instead of sending the agent in a circle.
    let mutating_quota_repair = match command {
        Command::Quota {
            command: Some(provenant_cli::Quota::Tick { .. }),
        } => Some("run `provenant quota tick` from the chair's session"),
        Command::Quota {
            command: Some(provenant_cli::Quota::Clear { .. }),
        } => Some("run `provenant quota clear` from the chair's session"),
        Command::Quota {
            command: Some(provenant_cli::Quota::Display { .. }),
        } => Some("run `provenant quota display` from the chair's session"),
        _ => None,
    };
    let in_lane = std::env::var_os(provenant_core::consumer_env::LANE_DEPTH)
        .is_some_and(|depth| depth != "0");
    if in_lane && let Some(repair) = mutating_quota_repair {
        return fail(
            &inv.leaf,
            Outcome::Refused,
            "QUOTA_UNAVAILABLE_IN_LANE",
            "mutating quota commands are unavailable inside a lane; run them from the chair's session",
            repair,
            globals,
        );
    }
    let session_save = matches!(
        command,
        Command::Workflow(provenant_cli::Workflow::Session(
            provenant_cli::workflow::Session::Save { .. }
        ))
    );
    if matches!(command, Command::Install(_)) {
        let selected_state = globals
            .state_dir
            .as_deref()
            .map(std::path::Path::to_path_buf)
            .or_else(|| {
                std::env::var_os(provenant_core::STATE_DIRECTORY_ENV).map(std::path::PathBuf::from)
            });
        if let Some(state) = selected_state
            && state.is_absolute()
            && let Ok(home) = roots::home()
            && let Err(error) = state_handoff::validate_selected_root(&state, &home)
        {
            let (status, code) = match &error {
                state_handoff::PlanError::StateFormatBreak { .. } => {
                    (Outcome::Refused, "STATE_FORMAT_BREAK")
                }
                state_handoff::PlanError::Io(_) => (Outcome::Error, "STATE_COPY"),
            };
            return fail(
                &inv.leaf,
                status,
                code,
                &error.to_string(),
                REPAIR_ROOT,
                globals,
            );
        }
    }
    if let Err(error) = roots::validate_globals(globals, session_save) {
        // A bad selected root is a usage fault; the environment is not.
        let status = match error {
            roots::RootError::Home if inv.leaf == "install" => Outcome::Invalid,
            roots::RootError::Checkout | roots::RootError::State(_) => Outcome::Invalid,
            _ => Outcome::Error,
        };
        let code = if matches!(error, roots::RootError::Home) && inv.leaf == "install" {
            "HOME"
        } else {
            error.code()
        };
        if matches!(error, roots::RootError::Home) && inv.leaf == "install" {
            return install::home_error(&inv.leaf, &error.to_string(), globals);
        }
        return fail(
            &inv.leaf,
            status,
            code,
            &error.to_string(),
            REPAIR_ROOT,
            globals,
        );
    }
    let Some(verb) = commands::find(&inv.leaf) else {
        return fail(
            &inv.leaf,
            Outcome::Error,
            "INTERNAL",
            "the parsed command has no registry verb",
            "report this internal dispatch error; rerun the command",
            globals,
        );
    };
    let invocation = provenant_cli::Invocation {
        command,
        globals,
        leaf: inv.leaf.as_str(),
        process,
    };
    match (verb.run)(&invocation) {
        Ok(outcome) => provenant_cli::exit_status(&outcome),
        Err(error) => fail_with_command_error(&inv.leaf, Outcome::Error, error, globals),
    }
}

/// Disk observations are best-effort and may stall on an unhealthy volume.
fn bounded_session_warning(
    observe: impl FnOnce() -> Option<String> + Send + 'static,
) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("session-disk-warning".into())
        .spawn(move || {
            let _ = tx.send(observe());
        })
        .ok()?;
    rx.recv_timeout(std::time::Duration::from_millis(250))
        .ok()
        .flatten()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod session_warning_tests {
    #[test]
    fn stalled_observation_does_not_hold_save() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let warning = super::bounded_session_warning(move || {
            let _ = rx.recv();
            Some("late".to_owned())
        });
        assert_eq!(warning, None);
        drop(tx);
    }
}

/// Run one parsed command line and count it, returning the exit status.
// CLI reporting measures real elapsed time for operational diagnostics.
#[allow(clippy::disallowed_methods)]
pub fn dispatch(parsed: ParseOutcome, process: &dyn Process) -> u8 {
    commands::install();
    let started = std::time::Instant::now();
    // Lets attempt context match an installed release without hashing the
    // executable on the launch path; an unparsable identity stays unknown.
    if let Ok(identity) = identity() {
        // The guardian may be a different build from the CLI that created the
        // attempt. Hand it this binary's source digest so its launch record
        // names the build that actually launched the attempt.
        provenant_supervisor::set_build_source_digest(identity.source_digest.to_string());
        provenant_run::set_build_identity(identity);
    }
    let (verb, globals) = match &parsed {
        ParseOutcome::Invocation(inv) => (inv.leaf.as_str(), &inv.globals),
        ParseOutcome::Invalid { globals, .. } => ("parse", globals),
        ParseOutcome::Help { globals, .. } => ("help", globals),
        ParseOutcome::Version { globals } => ("version", globals),
    };
    let verb = verb.to_owned();
    // A command admitted under an admission class runs directly under that slot:
    // it needs no roots, telemetry or result envelope, and ends with the command's exit code.
    if let ParseOutcome::Invocation(inv) = &parsed
        && let Command::Admit { class, command } = &inv.command
    {
        let mut env: Vec<_> = std::env::vars_os().collect();
        if !env
            .iter()
            .any(|(key, _)| key == provenant_core::consumer_env::ADMISSION_SLOT_STATE)
        {
            let resolved = roots::resolve(globals, Workspace::Checkout, Product::Optional)
                .or_else(|_| roots::resolve(globals, Workspace::Directory, Product::Optional));
            if let Ok(resolved) = resolved {
                let state = resolved.roots.state.as_path();
                let config = provenant_config::ResolvedAdmissionClass::resolve(
                    class,
                    state,
                    Some(&resolved.project),
                    provenant_config::project_key_for(&resolved.project).as_deref(),
                );
                if let Err(error) =
                    provenant_os::admission::AdmissionSlots::provision(state, class, config.slots)
                {
                    let _ = writeln!(
                        io::stderr().lock(),
                        "provenant: admission slots unavailable; command may proceed without them: {error}"
                    );
                }
                env.push((
                    provenant_core::consumer_env::ADMISSION_SLOT_STATE.into(),
                    state.as_os_str().to_owned(),
                ));
                if let Ok(json) = serde_json::to_string(&config) {
                    env.push(("PROVENANT_ADMISSION_CONFIG".into(), json.into()));
                }
            }
        }
        return u8::try_from(admit::run(class, command, &env)).unwrap_or(1);
    }
    // No product installation, Git probe or provider access is needed to count.
    // Read-only catalogue and checkpoint observation must not record demand
    // telemetry, including on an otherwise empty state root.
    let resolved = roots::resolve(globals, Workspace::Directory, Product::Optional).ok();
    // A guardian receives its launcher's resolved mode in the private descriptor.
    if !matches!(&parsed, ParseOutcome::Invocation(inv) if matches!(&inv.command, Command::Internal(provenant_cli::Internal::Supervise { .. })))
    {
        let mode = resolved
            .as_ref()
            .and_then(|resolved| {
                provenant_cli::config_reads::read_for(&resolved.roots, "help").ok()
            })
            .and_then(|config| {
                let knob = config.knobs.get("redaction")?;
                provenant_redact::Mode::parse(knob.value.as_str()?)
            })
            .unwrap_or_default();
        provenant_redact::init(mode);
    }
    if let Some(timeout) = resolved.as_ref().and_then(|resolved| {
        provenant_config::StoreSettings::read(resolved.roots.state.as_path())
            .ok()?
            .busy_timeout()
    }) {
        provenant_sqlite::set_busy_timeout(timeout);
    }
    // Whether a status read waited: telemetry separates waits from reads.
    let status_wait = match &parsed {
        ParseOutcome::Invocation(inv) => match &inv.command {
            Command::Status(args) if !args.session_stream => Some(false),
            Command::Wait(_) => Some(true),
            _ => None,
        },
        _ => None,
    };
    let read_only_graph = match &parsed {
        ParseOutcome::Invocation(inv) => {
            matches!(&inv.command, Command::Run(args) if args.explain)
                || matches!(&inv.command, Command::Status(args) if args.session_stream)
                || matches!(&inv.command, Command::Templates(_))
        }
        _ => false,
    };
    let usage_roots = if read_only_graph
        || matches!(
            verb.as_str(),
            "workflow session show"
                | "workflow session diff"
                | "chair summary show"
                | "chair summary diff"
                | "console changes"
                | "console projects"
                | "console sessions"
                | "console overview"
                | "console board"
                | "console tree"
        ) {
        None
    } else {
        resolved
    };
    let usage_project = usage_roots.as_ref().and_then(usage::project_id);
    let status = match parsed {
        ParseOutcome::Invocation(inv) => run(inv, process),
        ParseOutcome::Invalid { error, globals } => fail_with_repair(
            "parse",
            Outcome::Invalid,
            "INVALID",
            &error.to_string(),
            error
                .repair()
                .unwrap_or("correct the reported argument and rerun the command"),
            &globals,
        ),
        ParseOutcome::Help {
            text,
            catalog,
            globals,
        } => emit(
            &CommandResult::new("help", Outcome::Ok, catalog),
            &globals,
            &text,
            true,
        ),
        ParseOutcome::Version { globals } => match identity() {
            Ok(identity) => {
                #[derive(Serialize)]
                struct BuildIdentity {
                    package_version: &'static str,
                    #[serde(flatten)]
                    installation: VersionInfo,
                }
                emit(
                    &CommandResult::new(
                        "version",
                        Outcome::Ok,
                        BuildIdentity {
                            package_version: env!("CARGO_PKG_VERSION"),
                            installation: identity,
                        },
                    ),
                    &globals,
                    concat!(
                        "provenant ",
                        env!("CARGO_PKG_VERSION"),
                        " ",
                        env!("PROVENANT_SOURCE_DIGEST"),
                        "\n"
                    ),
                    true,
                )
            }
            Err(error) => fail(
                "version",
                Outcome::Error,
                "IDENTITY",
                &error.to_string(),
                "report this identity error; rerun `provenant --version`",
                &globals,
            ),
        },
    };
    if let Some(resolved) = usage_roots {
        provenant_telemetry::usage::record_cli_context(
            &resolved.roots,
            &provenant_os::clock::SystemClock,
            &verb,
            status,
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            usage_project,
            provenant_cli::fabric::current_session().as_deref(),
            std::env::var(provenant_core::consumer_env::PARENT_ATTEMPT_ID)
                .ok()
                .and_then(|id| id.parse().ok()),
            status_wait,
        );
    }
    status
}

#[cfg(test)]
mod unpublished_result_tests {
    #[test]
    fn landing_explanation_projects_sourced_facts_and_preserves_warnings() {
        let data = serde_json::json!({
            "fields": {
                "gate.kind": {"value": "commands", "source": {"layer": "project"}},
                "approval_sources": {"value": ["forge"], "source": {"layer": "host"}}
            },
            "ci_patience_seconds": 45,
            "ci_patience_source": "--wait",
            "warnings": ["inspect the selected gate"]
        });
        let summary = super::land_explanation(&data, &provenant_cli::Globals::default()).unwrap();
        let text = summary.lines.join("\n");
        for field in data["fields"].as_object().unwrap().values() {
            assert!(text.contains(field["source"]["layer"].as_str().unwrap()));
        }
        for warning in data["warnings"].as_array().unwrap() {
            assert!(text.contains(warning.as_str().unwrap()));
        }
        assert!(
            summary
                .next
                .as_deref()
                .is_some_and(|next| next.ends_with("land --explain --json"))
        );
        assert_eq!(summary.omitted, 0);
    }

    #[test]
    fn an_oversized_human_result_is_bounded_and_json_signals_too_large() {
        let text = "x".repeat(4096);
        let limit = 1024;
        let result = provenant_records::CommandResult::new(
            "telemetry stats",
            provenant_records::Outcome::Ok,
            text.clone(),
        );
        // Human output is bounded with an explicit omitted count so the caller
        // can warn and proceed; it never refuses the whole result.
        let globals = provenant_cli::Globals {
            json: false,
            full: true,
            ..Default::default()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let rendered = super::render_unpublished(
            &result, &globals, &text, false, None, limit, &mut out, &mut err,
        );
        let omitted = rendered.expect("an oversized human result is bounded, not refused");
        assert!(omitted > 0, "the omitted bytes are reported to the caller");
        assert!(!out.is_empty());
        assert!(out.len() <= limit, "{} exceeds {limit}", out.len());
        assert!(err.is_empty());

        // JSON cannot be clipped mid-object, so this helper signals TooLarge
        // for the caller to page a bounded marker instead of a refusal.
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let rendered = super::render_unpublished(
            &result, &globals, &text, false, None, limit, &mut out, &mut err,
        );
        assert!(
            matches!(rendered, Err(provenant_cli::RenderError::TooLarge)),
            "JSON oversize must reach the caller's bounded-marker path"
        );
        assert!(out.is_empty() && err.is_empty());
    }

    #[test]
    fn a_result_within_the_bound_is_rendered() {
        let result = provenant_records::CommandResult::new(
            "telemetry stats",
            provenant_records::Outcome::Ok,
            "x".repeat(64),
        );
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        super::render_unpublished(
            &result,
            &globals,
            "",
            false,
            None,
            1024,
            &mut out,
            &mut Vec::new(),
        )
        .expect("within the bound");
        assert!(!out.is_empty());
    }
}
