//! The binary edge's output seam. One sink per command collects typed warnings,
//! prints them as they arrive in human mode, or carries them in the JSON
//! envelope. Emission and the dedup store live here so no library crate writes
//! to either standard stream.
//!
//! Spec: docs/specs/cli.md#warnings-are-typed-values
#![allow(clippy::disallowed_methods)] // The only module allowed to write the standard streams.

use crate::overflow;
use provenant_cli::{OverflowPolicy, RenderError, RenderMode, Summary};
use provenant_records::CommandResult;
use provenant_records::command_result::NextStep;
use provenant_records::warning::{self, Dedup, RESULT_TOO_LARGE, Sink, Warning};
use provenant_workflow::session::{CommandDefaults, CommandPrefix};
use serde::Serialize;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// What the environment resolves with no `--state-dir` or `--project` flag.
/// The state root is the environment override or the home-relative default;
/// the project is the directory the caller is running in.
pub(crate) fn command_defaults(cwd: &Path) -> CommandDefaults {
    let state_dir = std::env::var_os(provenant_core::STATE_DIRECTORY_ENV)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(provenant_core::consumer_env::HOME)
                .map(|home| PathBuf::from(home).join(provenant_core::DEFAULT_STATE_SUFFIX))
        });
    CommandDefaults {
        state_dir,
        project: Some(std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned())),
    }
}

/// The one formatter for suggested commands. Flags that equal the caller's
/// environment defaults are omitted from every command it renders.
pub(crate) fn command_prefix(globals: &provenant_cli::Globals, cwd: &Path) -> CommandPrefix {
    CommandPrefix::with_defaults(
        globals.state_dir.as_deref(),
        globals.project.as_deref(),
        cwd,
        &command_defaults(cwd),
    )
}

/// The command's sink, installed once before any path can raise a warning.
static COMMAND_SINK: RwLock<Option<Arc<CommandSink>>> = RwLock::new(None);
/// Set as soon as the state root is known, before root and config warnings fire.
/// The retention window and the collapse setting are resolved with the root so
/// config loading never runs while the sink holds its mutex.
/// Spec: docs/specs/cli.md#warnings-are-typed-values
static STATE_ROOT: RwLock<Option<(PathBuf, u64, bool)>> = RwLock::new(None);

struct SinkState {
    json: bool,
    /// `--full` lists every warning instead of the collapsed line.
    full: bool,
    /// Warnings held for the envelope; the human path renders them once at emit.
    warnings: Vec<Warning>,
}

struct CommandSink {
    state: Mutex<SinkState>,
}

impl CommandSink {
    /// Whether this command's warnings travel as data rather than being
    /// printed. JSON mode must not let one invocation's memory suppress a
    /// warning a later consumer still needs to read.
    fn is_json(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .json
    }

    /// Show one warning through the supplied writer, then record its per-change
    /// key only when the message actually reached the reader. A failed write
    /// leaves the key unseen so the same warning can be retried on the next
    /// invocation instead of being silently swallowed.
    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    fn display_with(&self, warning: Warning, err: &mut dyn Write) {
        let delivered = writeln!(err, "warning: {}", warning.message).is_ok();
        for next in &warning.next {
            let text = next.display_text();
            if !text.is_empty() {
                delivered &= writeln!(err, "  next: {text}").is_ok();
            }
        }
        if delivered {
            remember_for_change(&warning);
        }
    }

    /// Render a batch of repeated warnings as one line. The first warning keeps
    /// its message and `next` steps; the count and first subject are named so
    /// `--full` can list every instance.
    /// Spec: docs/specs/agent-native.md#35-short-output
    fn display_collapsed(&self, group: provenant_cli::WarningGroup, err: &mut dyn Write) {
        let first = group.first;
        let subject = first.subject.as_deref().unwrap_or_default();
        let mut delivered = if subject.is_empty() {
            writeln!(
                err,
                "warning [{}] ×{}: {} — --full lists all",
                first.code, group.count, first.message
            )
            .is_ok()
        } else {
            writeln!(
                err,
                "warning [{}] ×{}: {} (first: {}) — --full lists all",
                first.code,
                group.count,
                bounded_text(&first.message, provenant_cli::limits::SUMMARY_ROW_BYTES),
                bounded_text(subject, provenant_cli::limits::SUMMARY_ROW_BYTES)
            )
            .is_ok()
        };
        for next in &first.next {
            let text = next.display_text();
            if !text.is_empty() {
                delivered &= writeln!(err, "  next: {text}").is_ok();
            }
        }
        if delivered {
            remember_for_change(&first);
        }
    }

    /// The human renderer, driven by one emit path so human and JSON read the
    /// same held warnings. A per-change warning already shown in this state
    /// root is not shown again; a repeat within this invocation collapses.
    fn display_human(&self, warnings: &[Warning], err: &mut dyn Write) {
        let (state_root, retention_days, collapse) = STATE_ROOT
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| (PathBuf::new(), 30, true));
        let full = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .full;
        let fresh: Vec<Warning> = warnings
            .iter()
            .filter(|warning| {
                warning.dedup != Dedup::PerChange
                    || state_root.as_os_str().is_empty()
                    || !dedup::was_seen(&state_root, &warning.key(), retention_days)
            })
            .cloned()
            .collect();
        if full || !collapse {
            for warning in fresh {
                self.display_with(warning, err);
            }
        } else {
            for group in provenant_cli::collapse_warnings(&fresh) {
                if group.count == 1 {
                    self.display_with(group.first, err);
                } else {
                    self.display_collapsed(group, err);
                }
            }
        }
    }

    /// The production seam: typed warnings are the only thing this module
    /// writes to stderr, and only after the sink no longer holds its lock.
    fn display(&self, warnings: &[Warning]) {
        let mut err = io::stderr().lock();
        self.display_human(warnings, &mut err);
    }

    /// Take warnings still held for an envelope that never arrived.
    fn take_held(&self) -> Vec<Warning> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut state.warnings)
    }
}

impl Sink for CommandSink {
    /// A warning arrives here from `records::warning::warn`. Every warning is
    /// held, repeats included: under `--json` it travels in the envelope, and
    /// in human mode the one emit path collapses repeats and skips a per-change
    /// key already shown in an earlier invocation. Nothing called under this
    /// lock loads config or raises a warning, so a malformed setting cannot
    /// re-enter `accept` and deadlock the mutex.
    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    fn accept(&self, warning: Warning) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.warnings.push(warning);
    }
}

/// Restores the previous sink on drop and drains warnings still held for an
/// envelope that never arrived (a command that exits without emitting). Those
/// warnings are shown once here instead of being swallowed.
pub(crate) struct CommandSinkGuard {
    _inner: warning::SinkGuard,
}

impl Drop for CommandSinkGuard {
    fn drop(&mut self) {
        let sink = COMMAND_SINK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(sink) = sink {
            let warnings = sink.take_held();
            sink.display(&warnings);
        }
    }
}

/// Install the command's sink. The guard restores the previous sink on drop,
/// so nested dispatch in tests cannot leak one command's warnings into another.
pub(crate) fn install_command_sink(json: bool) -> CommandSinkGuard {
    install_command_sink_with(json, false)
}

/// `install_command_sink` with the `--full` decision: a full human view lists
/// every warning, a compact one collapses repeats into a single counted line.
pub(crate) fn install_command_sink_with(json: bool, full: bool) -> CommandSinkGuard {
    let sink = Arc::new(CommandSink {
        state: Mutex::new(SinkState {
            json,
            full,
            warnings: Vec::new(),
        }),
    });
    if let Ok(mut slot) = COMMAND_SINK.write() {
        *slot = Some(sink.clone());
    }
    CommandSinkGuard {
        _inner: warning::install(sink),
    }
}

/// Carry warnings captured while parsing into the command sink. Parsing runs
/// before dispatch installs the sink, so these are the only warnings raised
/// before the command's envelope exists.
pub(crate) fn replay_warnings(warnings: Vec<Warning>) {
    for warning in warnings {
        warning::warn(warning);
    }
}

/// Name the state root used by per-change dedup and resolve its retention
/// window once, before the sink can see the root. Config loading raises typed
/// warnings; doing it here — with no sink lock held — keeps that warning out of
/// the sink's reentrant path. Called as soon as roots are resolved, before root
/// warnings are raised. Spec: docs/specs/cli.md#warnings-are-typed-values
pub(crate) fn set_state_root(root: &Path) {
    let retention_days = host_settings::dedup_retention_days(root);
    let collapse = host_settings::collapse_warnings(root);
    if let Ok(mut slot) = STATE_ROOT.write() {
        *slot = Some((root.to_owned(), retention_days, collapse));
    }
}

/// Record a per-change key after its warning has been shown. Invocation-scoped
/// keys need no memory: one command shows each such warning at most once.
fn remember_for_change(warning: &Warning) {
    if warning.dedup != Dedup::PerChange {
        return;
    }
    let state_root = STATE_ROOT
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some((root, retention_days, _)) = state_root {
        dedup::remember(&root, &warning.key(), warning.code.as_ref(), retention_days);
    }
}

/// Take the warnings collected for the current command's envelope. Taking only
/// moves them out of the sink. A per-change key is remembered once the reader
/// has the envelope (`remember_warnings`) or once the guard shows the warning on
/// a path that never emits — never before it has been delivered.
pub(crate) fn take_warnings() -> Vec<Warning> {
    let sink = COMMAND_SINK
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    sink.map(|sink| sink.take_held()).unwrap_or_default()
}

/// Remember a batch of per-change keys after their envelope reached the reader.
/// An undelivered warning must not be marked seen just because it was drained;
/// a JSON-delivered warning is never remembered, so a later human run still
/// shows it and a later envelope still carries it.
fn remember_warnings(warnings: &[Warning]) {
    let json = COMMAND_SINK
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .is_some_and(|sink| sink.is_json());
    if json {
        return;
    }
    for warning in warnings {
        remember_for_change(warning);
    }
}

/// The drain the CLI crate's shared emitter seam uses. The binary edge owns
/// the sink; a library command surface only carries the warnings as data onto
/// the envelope it renders, so none is printed bare under `--json`.
/// Spec: docs/specs/cli.md#warnings-are-typed-values
pub(crate) fn command_seam() -> provenant_cli::CommandSeam {
    provenant_cli::CommandSeam {
        take: take_warnings,
        remember: remember_warnings,
        give_back: return_warnings,
        display_warnings: display_held_warnings,
        report: |result| {
            write_report(result)
                .map(|path| path.to_string_lossy().into_owned())
                .ok()
        },
        streams: || (Box::new(io::stdout().lock()), Box::new(io::stderr().lock())),
    }
}

/// The human warning renderer for a library command surface. JSON keeps the
/// warnings as data and never calls this.
fn display_held_warnings(warnings: &[Warning]) {
    let sink = COMMAND_SINK
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(sink) = sink {
        sink.display(warnings);
    }
}

/// Return warnings to the sink when no envelope reached the reader, so the
/// guard shows them on exit (and remembers them then) instead of swallowing
/// them or remembering an undelivered warning here.
fn return_warnings(warnings: Vec<Warning>) {
    let sink = COMMAND_SINK
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(sink) = sink {
        let mut state = sink
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.warnings.extend(warnings);
    }
}

pub(crate) fn emit<T: Serialize>(
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
    summary: Option<&Summary>,
) -> u8 {
    emit_with_writers_and_tty(
        result,
        globals,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        human,
        read_only,
        summary,
        io::stdout().is_terminal(),
    )
}

/// Human text with terminal escape sequences removed. A pipe or a file is not
/// a terminal: colour and cursor controls there cost bytes and let a captured
/// line reorder or hide text from the reader. Everything else, including line
/// breaks and tabs, is kept as written.
pub(crate) fn strip_ansi(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    if !bytes.contains(&0x1b) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            let start = index;
            while index < bytes.len() && bytes[index] != 0x1b {
                index += 1;
            }
            // The slice ends before an ASCII byte, so it is a UTF-8 boundary.
            out.push_str(&text[start..index]);
            continue;
        }
        index += 1;
        match bytes.get(index) {
            // CSI: parameters and intermediates, then one final byte.
            Some(b'[') => {
                index += 1;
                while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                    index += 1;
                }
                index = index.saturating_add(1).min(bytes.len());
            }
            // OSC: ends at BEL or at the string terminator ESC backslash.
            Some(b']') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            // String terminator: ESC backslash can end an OSC or stand alone
            // (for example after an OSC already closed with BEL). Drop both
            // bytes so the next printable byte is kept verbatim.
            Some(b'\\') => {
                index = (index + 1).min(bytes.len());
            }
            // Two-byte character set and private sequences.
            Some(b'(' | b')' | b'#' | b'*' | b'+') => {
                index = (index + 2).min(bytes.len());
            }
            _ => {}
        }
    }
    Cow::Owned(out)
}

/// The emission seam shared by production and targeted tests: a refusal's
/// rendered bytes go to the supplied writers, so a test can drive the same
/// `fail_with_cause` path instead of hand-building a `CommandResult`. Buffers
/// are not terminals, so human text is stripped for them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_with_writers<T: Serialize, Wout: Write, Werr: Write>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    stdout: &mut Wout,
    stderr: &mut Werr,
    human: &str,
    read_only: bool,
    summary: Option<&Summary>,
) -> u8 {
    emit_with_writers_and_tty(
        result, globals, stdout, stderr, human, read_only, summary, false,
    )
}

/// `emit_with_writers` with the caller's terminal decision. The binary edge
/// passes whether stdout is a TTY; tests pass the piped case explicitly.
#[allow(clippy::too_many_arguments)]
fn emit_with_writers_and_tty<T: Serialize, Wout: Write, Werr: Write>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    stdout: &mut Wout,
    stderr: &mut Werr,
    human: &str,
    read_only: bool,
    summary: Option<&Summary>,
    tty: bool,
) -> u8 {
    let human = if tty {
        Cow::Borrowed(human)
    } else {
        strip_ansi(human)
    };
    let mut warnings = take_warnings();
    // An oversized result also gets a private report; its path travels on the
    // cap warning so both the human line and the JSON envelope name it.
    if let Ok(data) = serde_json::to_value(&result.data)
        && provenant_cli::oversized(result, &data)
    {
        let envelope = CommandResult {
            schema_version: result.schema_version,
            command: result.command.clone(),
            status: result.status,
            data,
            error: result.error.clone(),
            warnings: warnings.clone(),
            next: result.next.clone(),
        };
        if let Some(path) = write_report(&envelope) {
            warnings.push(provenant_cli::report_warning(
                &result.command,
                &path.to_string_lossy(),
            ));
        }
    }
    if !globals.json {
        let sink = COMMAND_SINK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(sink) = sink {
            sink.display_human(&warnings, stderr);
        }
    }
    let explanation = if summary.is_none() && result.command == "land" {
        serde_json::to_value(&result.data)
            .ok()
            .and_then(|data| land_explanation(&data, globals))
    } else {
        None
    };
    // The land explanation's suggested command is a typed step on the envelope,
    // never a formatted line inside a summary. An owner-supplied array wins.
    let steps: Cow<'_, [NextStep]> = if result.next.is_empty() {
        Cow::Owned(
            explanation
                .as_ref()
                .map(|(_, step)| vec![step.clone()])
                .unwrap_or_default(),
        )
    } else {
        Cow::Borrowed(result.next.as_slice())
    };
    let summary = summary
        .or(explanation.as_ref().map(|(summary, _)| summary))
        .map(|summary| {
            if tty {
                summary.clone()
            } else {
                // A compact view prints the summary rows, not the raw text, so
                // the same terminal-safety rule has to reach every row.
                Summary {
                    lines: summary
                        .lines
                        .iter()
                        .map(|line| strip_ansi(line).into_owned())
                        .collect(),
                    omitted: summary.omitted,
                }
            }
        });
    let mode = if !globals.json && matches!(result.command.as_str(), "help" | "version") {
        RenderMode::Human {
            text: human.as_ref(),
            read_only,
            summary: None,
        }
    } else {
        globals.render_mode(human.as_ref(), read_only, summary.as_ref())
    };
    // Build one resolved envelope carrying the steps the owner chose (or the
    // land explanation's step) so JSON and human mode read the same next array.
    // The one renderer turns that array into human lines after the summary, so
    // a compact view never drops the line JSON carries.
    let resolved = CommandResult {
        schema_version: result.schema_version,
        command: result.command.clone(),
        status: result.status,
        data: &result.data,
        error: result.error.clone(),
        warnings: warnings.clone(),
        next: steps.to_vec(),
    };
    // JSON prints nothing to stderr: the sink held every warning, and this is
    // the only place they reach the consumer. Serialising `T` to a Value lets
    // an owner keep its own data type while the envelope gains one field.
    let rendered = provenant_cli::render_or_publish(
        &resolved,
        mode,
        stdout,
        stderr,
        |value| overflow::publish(value, globals),
        // A published overflow keeps the result's status in its envelope,
        // so the exit code matches it; only a real render/IO failure exits 3.
        OverflowPolicy::Proceed,
    );
    match rendered {
        Ok(status) => {
            // A JSON envelope delivered the warnings; a human reader already
            // had them rendered above, which remembered only the keys actually
            // shown. Remembering again here would count one sighting twice.
            if globals.json {
                remember_warnings(&warnings);
            }
            provenant_cli::exit_status(&status)
        }
        Err(_) => {
            // Under JSON no envelope was written, so the guard still owns the
            // warnings. Human warnings were already rendered above and must not
            // be shown twice.
            if globals.json {
                return_warnings(warnings);
            }
            let _ = io::stderr().write_all(b"provenant: IO: cannot write command result\n");
            3
        }
    }
}

/// A policy explanation is a structured result; compact output names omitted
/// rows. Its suggested command is a typed step, not a formatted summary line.
pub(crate) fn land_explanation(
    data: &Value,
    _globals: &provenant_cli::Globals,
) -> Option<(Summary, NextStep)> {
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
            let source = field.pointer("/source/layer").and_then(Value::as_str)?;
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
                .filter_map(Value::as_str)
                .map(|warning| format!("warning: {warning}")),
        );
    }
    let step = NextStep::from_args(
        &["provenant", "land", "--explain", "--json"],
        "review the landing policy with full detail",
    );
    Some((Summary::new("Landing policy", rows), step))
}

/// `emit_summarised` for a result the caller narrows rather than reads from an
/// artefact: an oversized result is bounded with a warning, never published, so
/// no file is left in the results area on any path.
pub(crate) fn emit_unpublished<T: Serialize>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
    summary: Option<&Summary>,
) -> u8 {
    emit_unpublished_with_writers(
        result,
        globals,
        human,
        read_only,
        summary,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

/// The emission seam for `emit_unpublished`: the bounded marker and its
/// envelope go to the supplied writers so a test can drive the overflow path.
#[allow(clippy::too_many_arguments)]
fn emit_unpublished_with_writers<T: Serialize, Wout: Write, Werr: Write>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
    summary: Option<&Summary>,
    stdout: &mut Wout,
    stderr: &mut Werr,
) -> u8 {
    const REPAIR: &str = "narrow it with --since and --until";
    // Drain the held warnings first. They travel in the JSON envelope (or reach
    // stderr on the human path) before any per-change key is remembered.
    let mut warnings = take_warnings();
    if let Ok(data) = serde_json::to_value(&result.data)
        && provenant_cli::oversized(result, &data)
    {
        let envelope = CommandResult {
            schema_version: result.schema_version,
            command: result.command.clone(),
            status: result.status,
            data,
            error: result.error.clone(),
            warnings: warnings.clone(),
            next: result.next.clone(),
        };
        if let Some(path) = write_report(&envelope) {
            warnings.push(provenant_cli::report_warning(
                &result.command,
                &path.to_string_lossy(),
            ));
        }
    }
    if !globals.json {
        let sink = COMMAND_SINK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(sink) = sink {
            sink.display_human(&warnings, stderr);
        }
    }
    // Build the envelope once so the overflow measurement and the bounded
    // marker describe the bytes actually attempted, including these warnings.
    // Measuring the bare result instead reports 0 omitted bytes when the
    // warnings are what pushed the envelope past the cap.
    let envelope = if globals.json {
        let data = serde_json::to_value(&result.data).unwrap_or(Value::Null);
        Some(CommandResult {
            schema_version: result.schema_version,
            command: result.command.clone(),
            status: result.status,
            data,
            error: result.error.clone(),
            warnings: warnings.clone(),
            next: result.next.clone(),
        })
    } else {
        None
    };
    let rendered = match envelope.as_ref() {
        Some(envelope) => render_unpublished(
            envelope,
            globals,
            human,
            read_only,
            summary,
            provenant_cli::MAX_RESULT_BYTES,
            &mut *stdout,
            &mut *stderr,
        ),
        None => render_unpublished(
            result,
            globals,
            human,
            read_only,
            summary,
            provenant_cli::MAX_RESULT_BYTES,
            &mut *stdout,
            &mut *stderr,
        ),
    };
    match rendered {
        Ok(omitted) => {
            if !globals.json && omitted > 0 {
                // Human mode: the sink prints this as it arrives and remembers it.
                warning::warn(too_large_warning(&result.command, omitted, REPAIR));
            }
            if globals.json {
                remember_warnings(&warnings);
            }
            provenant_cli::exit_status(&result.status)
        }
        Err(RenderError::TooLarge) => {
            // JSON cannot be clipped mid-object: emit a bounded marker instead.
            let omitted = envelope
                .as_ref()
                .and_then(|envelope| serde_json::to_vec(envelope).ok())
                .map(|bytes| bytes.len().saturating_sub(provenant_cli::MAX_RESULT_BYTES))
                .unwrap_or_else(|| {
                    serde_json::to_vec(result)
                        .map(|bytes| bytes.len().saturating_sub(provenant_cli::MAX_RESULT_BYTES))
                        .unwrap_or_default()
                });
            let marker = json!({
                "truncated": true,
                "omitted_bytes": omitted,
                "source": "COMMAND_RESULT_MAX_BYTES",
                "repair": REPAIR,
            });
            // The marker replaces the oversized data, not the refusal: carry the
            // command's warnings and typed error onto it so neither is dropped.
            let mut bounded = CommandResult {
                schema_version: result.schema_version,
                command: result.command.clone(),
                status: result.status,
                data: marker,
                error: result
                    .error
                    .as_ref()
                    .map(|error| provenant_cli::bounded_error(error, 512)),
                warnings,
                next: result.next.clone(),
            };
            if !bounded
                .warnings
                .iter()
                .any(|warning| warning.code.as_ref() == RESULT_TOO_LARGE.as_str())
            {
                bounded
                    .warnings
                    .push(too_large_warning(&result.command, omitted, REPAIR));
            }
            let written = provenant_cli::render(&bounded, RenderMode::Json, &mut *stdout);
            if written.is_ok() {
                remember_warnings(&bounded.warnings);
            } else {
                return_warnings(bounded.warnings);
            }
            provenant_cli::exit_status(&result.status)
        }
        Err(_) => {
            // Human warnings were already displayed above; under JSON the guard
            // still owns them because no envelope reached the reader.
            if globals.json {
                return_warnings(warnings);
            }
            let _ = stderr.write_all(b"provenant: IO: cannot write command result\n");
            3
        }
    }
}

/// The typed cap notice. The bracketed code in the message keeps the human
/// stderr line recognisable while the code itself travels in the envelope.
fn too_large_warning(command: &str, omitted: usize, repair: &str) -> Warning {
    Warning::new(
        RESULT_TOO_LARGE,
        format!(
            "[RESULT_TOO_LARGE] {command} exceeds 1 MiB (source: COMMAND_RESULT_MAX_BYTES); showing a bounded view with {omitted} bytes omitted"
        ),
     provenant_records::warning::detected_source(RESULT_TOO_LARGE))
    .repair(repair)
}

/// Render a result within `limit`, bounding the human view with an explicit
/// omitted-size marker when it is larger. Returns the bytes omitted from the
/// owner's text (zero when the result fit).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_unpublished<T: Serialize>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
    summary: Option<&Summary>,
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

/// Per-change memory: one STRICT row per dedup key in `<state>/warnings.sqlite3`.
/// A seen key is never shown again; every write prunes rows older than the
/// configured retention window. Failures fail open — the warning is shown.
mod dedup {
    use super::*;
    use provenant_sqlite::{Schema, ensure_schema, open_connection};
    use std::ffi::OsStr;

    pub(super) const WARNINGS_SCHEMA: Schema = Schema {
        sql: "CREATE TABLE seen (key TEXT PRIMARY KEY, code TEXT NOT NULL, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, count INTEGER NOT NULL) STRICT; PRAGMA user_version=5;",
        version: 5,
        steps: &[],
        migrations: &[],
    };

    /// Open the dedup store. `open_connection` never creates a file (it opens
    /// with `SQLITE_OPEN_READ_WRITE`, not `CREATE`), so a write path must create
    /// the file first or every key fails open and nothing is ever remembered.
    /// Creation goes through the confined directory so a symlink cannot
    /// redirect the store outside the state root.
    fn open(state_root: &Path, write: bool) -> Option<rusqlite::Connection> {
        if write {
            let root = provenant_os::fs::Directory::open_anchor(state_root).ok()?;
            match root.create_new(OsStr::new("warnings.sqlite3")) {
                Ok(file) => {
                    let _ = file.sync();
                }
                Err(error) if error.is_exists() => {}
                Err(_) => return None,
            }
        }
        open_connection(&state_root.join("warnings.sqlite3"), !write).ok()
    }

    /// Whether `key` has already been shown. A read-only check: it does not
    /// mark the key, so a warning can be held for the envelope before it is
    /// remembered at display time.
    pub(super) fn was_seen(state_root: &Path, key: &str, retention_days: u64) -> bool {
        let Some(db) = open(state_root, false) else {
            return false;
        };
        if ensure_schema(&db, WARNINGS_SCHEMA).is_err() {
            return false;
        }
        use provenant_core::Clock as _;
        let now = provenant_os::clock::SystemClock.unix_seconds().max(0);
        let window = i64::try_from(retention_days.saturating_mul(86_400)).unwrap_or(i64::MAX);
        let cutoff = now.saturating_sub(window);
        // An expired key is new again: its row will be pruned on the next write.
        db.query_row(
            "SELECT 1 FROM seen WHERE key = ?1 AND last_seen >= ?2",
            rusqlite::params![key, cutoff],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// Record `key` and report whether it is new. A fresh row starts at one;
    /// only an existing row advances its count and last-seen time. Stale rows
    /// are pruned on every write.
    pub(super) fn remember(state_root: &Path, key: &str, code: &str, retention_days: u64) -> bool {
        let Some(mut db) = open(state_root, true) else {
            return true;
        };
        if ensure_schema(&db, WARNINGS_SCHEMA).is_err() {
            return true;
        }
        use provenant_core::Clock as _;
        // SQLite stores signed integers; seconds before the epoch clamp to zero.
        let now = provenant_os::clock::SystemClock.unix_seconds().max(0);
        let window = i64::try_from(retention_days.saturating_mul(86_400)).unwrap_or(i64::MAX);
        let cutoff = now.saturating_sub(window);
        let tx = match db.transaction() {
            Ok(tx) => tx,
            Err(_) => return true,
        };
        let existed = tx
            .query_row(
                "SELECT 1 FROM seen WHERE key = ?1",
                rusqlite::params![key],
                |_| Ok(()),
            )
            .is_ok();
        if existed {
            let _ = tx.execute(
                "UPDATE seen SET last_seen=?1, count=count+1 WHERE key=?2",
                rusqlite::params![now, key],
            );
        } else {
            let _ = tx.execute(
                "INSERT INTO seen (key, code, first_seen, last_seen, count) VALUES (?1, ?2, ?3, ?3, 1)",
                rusqlite::params![key, code, now],
            );
        }
        let _ = tx.execute(
            "DELETE FROM seen WHERE last_seen < ?1",
            rusqlite::params![cutoff],
        );
        let _ = tx.commit();
        !existed
    }
}

/// The host setting that bounds per-change warning memory, with its source.
mod host_settings {
    use super::*;

    /// `warnings.dedup_retention_days`, default 30. The warning that names a
    /// stale setting carries its provenance; the value itself is read through
    /// the config owner so the layer that set it is the one reported.
    pub(super) fn dedup_retention_days(state_root: &Path) -> u64 {
        provenant_config::warnings_dedup_retention_days(state_root)
    }

    /// `domains.cli.collapse_warnings`, host or launch layer, default `on`.
    /// A missing or unreadable config keeps the compiled default.
    pub(super) fn collapse_warnings(state_root: &Path) -> bool {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Cli {
            collapse_warnings: String,
        }
        if state_root.as_os_str().is_empty() {
            return true;
        }
        provenant_config::read_domain::<Cli>(state_root, "cli")
            .map(|settings| !settings.collapse_warnings.eq_ignore_ascii_case("off"))
            .unwrap_or(true)
    }
}

/// Private reports for oversized results. The full result is written under
/// `<state>/reports/<verb>-<ts>.json` mode 0600 with the same redaction as the
/// envelope; stale reports are pruned on each write (seven-day retention).
mod reports {
    use super::*;
    use provenant_core::Clock as _;
    use std::ffi::OsStr;

    const RETENTION_DAYS: i64 = 7;

    pub(super) fn write_report(
        root: &Path,
        result: &CommandResult<serde_json::Value>,
    ) -> Option<PathBuf> {
        if root.as_os_str().is_empty() {
            return None;
        }
        let anchor = provenant_os::fs::Directory::open_anchor(root).ok()?;
        match anchor.mkdir(OsStr::new("reports"), 0o700) {
            Ok(()) => {}
            Err(error) if error.is_exists() => {}
            Err(_) => return None,
        }
        let reports = anchor.open_directory(OsStr::new("reports")).ok()?;
        let at = provenant_os::clock::SystemClock
            .wall_now()
            .map(|at| at.to_string())
            .unwrap_or_else(|_| "1970-01-01T00:00:00.000000Z".into())
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>();
        let verb = result
            .command
            .split_whitespace()
            .next()
            .unwrap_or("result")
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let name = format!("{verb}-{at}.json");
        let bytes = serde_json::to_vec(result).ok()?;
        reports
            .atomic_write_with_permissions(
                OsStr::new(&name),
                ".report-",
                &bytes,
                0o600,
                provenant_os::fs::PublishMode::NoReplace,
            )
            .ok()?;
        let _ = reports.sync();
        prune(&root.join("reports"));
        Some(root.join("reports").join(name))
    }

    /// Remove reports whose modification time is older than the retention
    /// window. A failure to inspect one file leaves it in place.
    pub(super) fn prune(reports: &Path) {
        let now = provenant_os::clock::SystemClock.unix_seconds();
        let cutoff = now.saturating_sub(RETENTION_DAYS.saturating_mul(86_400));
        let Ok(entries) = std::fs::read_dir(reports) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let age = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|age| age.as_secs() as i64)
                .unwrap_or(0);
            if age < cutoff {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Write the report for an oversized result and return its path for the cap
/// warning. The state root is resolved once at command start.
fn write_report(result: &CommandResult<serde_json::Value>) -> Option<PathBuf> {
    let root = STATE_ROOT
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .map(|(root, _, _)| root);
    let root = root?;
    reports::write_report(&root, result)
}

/// Prune reports older than the seven-day window. The writer calls this on
/// every write; a gc sweep can call the same seam without duplicating the rule.
#[allow(dead_code)]
pub(crate) fn prune_reports(root: &Path) {
    reports::prune(&root.join("reports"));
}

#[cfg(test)]
#[allow(dead_code, unused_imports)]
mod tests {
    use super::*;
    use provenant_records::Outcome;
    use provenant_records::warning::{NAME_CASE_FOLDED, WarningCode, capture};
    use provenant_sqlite::{ensure_schema, open_connection};
    use std::ffi::OsString;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_root(f: impl FnOnce(&Path)) {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("tempdir");
        f(dir.path());
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn piped_stdout_has_no_ansi_escapes_but_a_terminal_keeps_them() {
        let result = CommandResult::new("output", Outcome::Ok, json!({"state": "succeeded"}));
        let globals = provenant_cli::Globals {
            full: true,
            ..Default::default()
        };
        let human = "settlement: succeeded\n\u{1b}[31mred findings\u{1b}[0m \u{1b}]8;;https://example.invalid\u{7}link\u{1b}\\ tail\n";
        let mut piped = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            emit_with_writers_and_tty(
                &result, &globals, &mut piped, &mut err, human, true, None, false,
            ),
            0
        );
        let printed = String::from_utf8(piped).expect("utf8");
        assert!(!printed.contains('\u{1b}'), "{printed:?}");
        assert!(printed.contains("red findings"), "{printed:?}");
        assert!(printed.contains("link tail"), "{printed:?}");
        let mut terminal = Vec::new();
        let mut err = Vec::new();
        emit_with_writers_and_tty(
            &result,
            &globals,
            &mut terminal,
            &mut err,
            human,
            true,
            None,
            true,
        );
        assert!(
            String::from_utf8(terminal)
                .expect("utf8")
                .contains("\u{1b}[31m"),
            "a terminal keeps the escapes"
        );

        // The compact view prints summary rows, so those must be clean too.
        let compact_globals = provenant_cli::Globals::default();
        let summary = Summary {
            lines: vec!["\u{1b}[1msettlement: succeeded\u{1b}[0m".into()],
            omitted: 0,
        };
        let mut compact = Vec::new();
        emit_with_writers_and_tty(
            &result,
            &compact_globals,
            &mut compact,
            &mut Vec::new(),
            "",
            true,
            Some(&summary),
            false,
        );
        let compact = String::from_utf8(compact).expect("utf8");
        assert!(!compact.contains('\u{1b}'), "{compact:?}");
        assert!(compact.contains("settlement: succeeded"), "{compact:?}");
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn json_mode_carries_warnings_in_the_envelope_and_not_on_stderr() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        warning::warn(Warning::new(
            WarningCode("TEST"),
            "a typed warning",
            provenant_records::warning::detected_source(WarningCode("TEST")),
        ));
        let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert_eq!(envelope["warnings"][0]["code"], "TEST");
        assert_eq!(envelope["warnings"][0]["message"], "a typed warning");
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn json_mode_carries_next_steps_as_argv_arrays() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        let result =
            CommandResult::new("fixture", Outcome::Ok, json!({"ok": true})).with_next(vec![
                NextStep::new(
                    vec![
                        "provenant".into(),
                        "pr".into(),
                        "status".into(),
                        "12".into(),
                    ],
                    "check CI",
                ),
            ]);
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert_eq!(
            envelope["next"][0]["argv"],
            json!(["provenant", "pr", "status", "12"])
        );
        assert_eq!(envelope["next"][0]["why"], "check CI");
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    }

    /// The envelope's `next` array and the one human line the edge prints are
    /// two views of the same typed step. Every owner family pins this.
    fn assert_next_agrees(command: &str, steps: Vec<NextStep>) {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = CommandResult::new(command, Outcome::Ok, json!({})).with_next(steps.clone());

        let json_globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let sink = install_command_sink(true);
        let mut json_out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(
            &result,
            &json_globals,
            &mut json_out,
            &mut err,
            "",
            false,
            None,
        );
        let envelope: Value = serde_json::from_slice(&json_out).expect("envelope");
        let on_wire: Vec<NextStep> =
            serde_json::from_value(envelope["next"].clone()).expect("next steps");
        assert_eq!(on_wire, steps, "JSON must carry the exact argv and why");
        drop(sink);

        let human_globals = provenant_cli::Globals {
            json: false,
            full: true,
            ..Default::default()
        };
        let _sink = install_command_sink(false);
        let mut human_out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(
            &result,
            &human_globals,
            &mut human_out,
            &mut err,
            "",
            false,
            None,
        );
        let text = String::from_utf8(human_out).expect("human text");
        let prefix = provenant_workflow::session::CommandPrefix::default();
        assert_eq!(
            text,
            provenant_cli::next_steps_text(&steps, &prefix),
            "the human line must be rendered from the same array"
        );
        assert_eq!(text.lines().count(), steps.len());
        for step in &steps {
            let rendered = prefix.render_argv(&step.argv).expect("renderable argv");
            assert!(
                text.lines()
                    .any(|line| line.ends_with(&format!("; {rendered}"))
                        || line == format!("next: {rendered}")),
                "no human line matches argv {:?}: {text}",
                step.argv
            );
        }
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_launch() {
        assert_next_agrees(
            "run",
            vec![NextStep::from_args(
                &["provenant", "wait", "review-1"],
                "the run is still running",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_wait() {
        assert_next_agrees(
            "wait",
            vec![NextStep::from_command(
                "provenant status mixed/fast mixed/slow",
                "mixed/fast settled ok, mixed/slow failed",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_status() {
        assert_next_agrees(
            "status",
            vec![NextStep::from_args(
                &[
                    "provenant",
                    "status",
                    "--all",
                    "--limit",
                    "50",
                    "--cursor",
                    "7",
                ],
                "read the next page of graphs",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_steer() {
        assert_next_agrees(
            "steer",
            vec![NextStep::from_command(
                "provenant status fix-2",
                "re-read the graph",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_pr() {
        assert_next_agrees(
            "pr status",
            vec![NextStep::from_command(
                "provenant ci failures pr 1669",
                "inspect the failing checks",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_worktree() {
        assert_next_agrees(
            "worktree gc",
            vec![NextStep::from_command(
                "provenant gc --apply",
                "apply the pending retention",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_ci() {
        assert_next_agrees(
            "ci failures",
            vec![NextStep::from_command(
                "gh run rerun 42 --failed",
                "rerun the failed jobs",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn a_real_command_owner_carries_a_non_empty_next_array() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A real graph-status projection, not a hand-built fixture: the owner's
        // own builder produces the step from the row data a failing graph emits.
        let rows = vec![json!({
            "kind": "graph",
            "id": "g",
            "state": "failed",
            "next": "provenant retry g",
        })];
        let steps: Vec<NextStep> = crate::fabric::unified::graph_status_next(&rows)
            .into_iter()
            .collect();
        assert!(
            !steps.is_empty(),
            "the real owner must suggest a next command"
        );
        assert_eq!(steps[0].argv, ["provenant", "retry", "g"]);
        let result = CommandResult::new("status", Outcome::Ok, json!({})).with_next(steps);
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let sink = install_command_sink(true);
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        drop(sink);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert!(envelope["next"].as_array().is_some_and(|a| !a.is_empty()));
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn an_owner_capture_keeps_warnings_out_of_the_command_sink() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ((), captured) = capture(|| {
            warning::warn(Warning::new(
                WarningCode("TEST"),
                "a configured key was ignored",
                provenant_records::warning::detected_source(WarningCode("TEST")),
            ));
        });
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code.as_ref(), "TEST");
        // No sink was installed: nothing was taken for an envelope.
        assert!(take_warnings().is_empty());
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_per_change_key_is_remembered_but_a_changed_key_is_shown() {
        with_root(|root| {
            set_state_root(root);
            assert!(dedup::remember(root, "sha256:a", "A", 30));
            assert!(!dedup::remember(root, "sha256:a", "A", 30));
            assert!(dedup::remember(root, "sha256:b", "A", 30));
            assert!(root.join("warnings.sqlite3").exists());
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn pruning_removes_only_stale_rows() {
        with_root(|root| {
            let Ok(db) = open_connection(&root.join("warnings.sqlite3"), false) else {
                return;
            };
            assert!(ensure_schema(&db, dedup::WARNINGS_SCHEMA).is_ok());
            use provenant_core::Clock as _;
            let now = provenant_os::clock::SystemClock.unix_seconds();
            let stale = now.saturating_sub(400 * 86_400);
            assert!(db
                .execute(
                    "INSERT INTO seen (key, code, first_seen, last_seen, count) VALUES ('old', 'A', ?1, ?1, 1)",
                    rusqlite::params![stale],
                )
                .is_ok());
            assert!(dedup::remember(root, "sha256:fresh", "A", 30));
            let remaining: Vec<String> = db
                .prepare("SELECT key FROM seen ORDER BY key")
                .expect("prepare")
                .query_map([], |row| row.get(0))
                .expect("query")
                .map_while(Result::ok)
                .collect();
            assert_eq!(remaining, ["sha256:fresh"]);
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn parse_warnings_replayed_before_emit_reach_the_json_envelope() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A capital target is folded by clap's value parser, which runs before
        // dispatch installs the command sink.
        let (parsed, parse_warnings) =
            capture(|| provenant_cli::parse(["stop", "Fix-Login", "--json"].map(OsString::from)));
        assert!(
            matches!(parsed, provenant_cli::ParseOutcome::Invocation(_)),
            "the command must parse"
        );
        assert!(
            parse_warnings
                .iter()
                .any(|warning| warning.code.as_ref() == NAME_CASE_FOLDED.as_str()),
            "{parse_warnings:?}"
        );
        let _sink = install_command_sink(true);
        replay_warnings(parse_warnings);
        let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert!(
            envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning["code"] == NAME_CASE_FOLDED.as_str()),
            "{envelope}"
        );
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn an_oversized_json_result_carries_warnings_on_the_bounded_marker() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        let result = CommandResult::new(
            "fixture",
            Outcome::Ok,
            "x".repeat(provenant_cli::MAX_RESULT_BYTES + 128),
        );
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status =
            emit_unpublished_with_writers(&result, &globals, "", false, None, &mut out, &mut err);
        assert_eq!(status, 0);
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        let envelope: Value = serde_json::from_slice(&out).expect("bounded marker envelope");
        assert_eq!(envelope["data"]["truncated"], true);
        assert!(envelope["data"]["omitted_bytes"].as_u64().unwrap() > 0);
        assert!(
            envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning["code"] == RESULT_TOO_LARGE.as_str()),
            "{envelope}"
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn omitted_bytes_measure_the_envelope_not_the_bare_result() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        // Size the bare result just under the cap so a modest held warning is
        // what pushes the envelope over it. The marker and its warning must
        // report that overflow, not "0 bytes omitted".
        let empty = CommandResult::new("fixture", Outcome::Ok, String::new());
        let overhead = serde_json::to_vec(&empty).expect("encode").len();
        let data = "x".repeat(provenant_cli::MAX_RESULT_BYTES - overhead - 256);
        let result = CommandResult::new("fixture", Outcome::Ok, data);
        assert!(
            serde_json::to_vec(&result).expect("encode").len() < provenant_cli::MAX_RESULT_BYTES,
            "the bare result fits"
        );
        warning::warn(Warning::new(
            WarningCode("TEST"),
            "w".repeat(512),
            provenant_records::warning::detected_source(WarningCode("TEST")),
        ));
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status =
            emit_unpublished_with_writers(&result, &globals, "", false, None, &mut out, &mut err);
        assert_eq!(status, 0);
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        let envelope: Value = serde_json::from_slice(&out).expect("bounded marker envelope");
        let omitted = envelope["data"]["omitted_bytes"]
            .as_u64()
            .expect("omitted_bytes") as usize;
        assert!(omitted > 0, "the envelope overflowed: {envelope}");
        assert!(
            envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| {
                    warning["code"] == RESULT_TOO_LARGE.as_str()
                        && warning["message"].as_str().is_some_and(|message| {
                            message.contains(&format!("{omitted} bytes omitted"))
                        })
                }),
            "the warning reports the same honest count as the marker: {envelope}"
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_json_delivered_warning_is_kept_but_not_remembered() {
        with_root(|root| {
            set_state_root(root);
            let shown = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Reader` to `reader`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("reader-shown")
            .once_per_change();
            let shown_key = shown.key();
            let fresh = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("writer-fresh")
            .once_per_change();
            let fresh_key = fresh.key();
            // A prior human display must not hide the warning from a consumer.
            assert!(dedup::remember(
                root,
                &shown_key,
                NAME_CASE_FOLDED.as_str(),
                30
            ));
            let sink = install_command_sink(true);
            warning::warn(shown);
            warning::warn(fresh);
            let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
            let globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None),
                0
            );
            let envelope: Value = serde_json::from_slice(&out).expect("envelope");
            let codes: Vec<_> = envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .map(|warning| warning["code"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(codes, [NAME_CASE_FOLDED.as_str(); 2], "{envelope}");
            assert!(
                dedup::was_seen(root, &shown_key, 30),
                "a JSON delivery does not clear prior human memory"
            );
            assert!(
                !dedup::was_seen(root, &fresh_key, 30),
                "a JSON delivery never marks a key as shown; a later person still sees it"
            );
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
            drop(sink);
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_failed_write_does_not_remember_an_undelivered_warning() {
        with_root(|root| {
            set_state_root(root);
            let warning = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("writer-bad")
            .once_per_change();
            let key = warning.key();
            let sink = install_command_sink(true);
            warning::warn(warning);
            let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
            let globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };
            let mut out = BrokenWriter;
            let mut err = Vec::new();
            assert_eq!(
                emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None),
                3
            );
            assert!(
                !dedup::was_seen(root, &key, 30),
                "a warning no envelope delivered is not remembered on an IO failure"
            );
            // The guard shows the returned warning on exit and remembers it then.
            drop(sink);
        });
    }

    /// A writer that always fails, driving `emit_with_writers` into its exit-3
    /// arm while keeping the test inside the in-process emission seam.
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn an_exit_without_emitting_shows_and_remembers_held_warnings() {
        with_root(|root| {
            set_state_root(root);
            let warning = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("writer")
            .once_per_change();
            let key = warning.key();
            {
                let _sink = install_command_sink(true);
                warning::warn(warning);
                assert!(!dedup::was_seen(root, &key, 30));
            }
            assert!(
                dedup::was_seen(root, &key, 30),
                "the guard drains held warnings on exit so they are shown once"
            );
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_fresh_per_change_key_starts_at_one() {
        with_root(|root| {
            assert!(dedup::remember(root, "sha256:fresh", "A", 30));
            let db = open_connection(&root.join("warnings.sqlite3"), true).expect("db");
            let count: i64 = db
                .query_row(
                    "SELECT count FROM seen WHERE key = 'sha256:fresh'",
                    [],
                    |row| row.get(0),
                )
                .expect("count");
            assert_eq!(count, 1, "a new row is shown once and counted once");
            drop(db);
            assert!(!dedup::remember(root, "sha256:fresh", "A", 30));
            let db = open_connection(&root.join("warnings.sqlite3"), true).expect("db");
            let count: i64 = db
                .query_row(
                    "SELECT count FROM seen WHERE key = 'sha256:fresh'",
                    [],
                    |row| row.get(0),
                )
                .expect("count");
            assert_eq!(count, 2, "only a repeat advances the count");
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_malformed_warnings_setting_does_not_reenter_the_sink_under_its_lock() {
        with_root(|root| {
            std::fs::write(
                root.join("config.toml"),
                "schema_version = 1\n[warnings]\ndedup_retention_days = 0\n",
            )
            .expect("config");
            set_state_root(root);
            let _sink = install_command_sink(true);
            // Resolving the malformed setting raises a per-change config warning.
            // If the sink re-entered while holding its mutex on this path the
            // command would hang; it must instead reach the same envelope.
            warning::warn(
                Warning::new(
                    NAME_CASE_FOLDED,
                    "folded `Sink` to `sink`",
                    provenant_records::warning::detected_source(NAME_CASE_FOLDED),
                )
                .subject("sink")
                .once_per_change(),
            );
            let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
            let globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
            let envelope: Value = serde_json::from_slice(&out).expect("envelope");
            assert!(
                envelope["warnings"]
                    .as_array()
                    .expect("warnings")
                    .iter()
                    .any(|warning| warning["code"] == NAME_CASE_FOLDED.as_str()),
                "{envelope}"
            );
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_failed_stderr_write_does_not_remember_a_per_change_warning() {
        with_root(|root| {
            set_state_root(root);
            let first = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Sink` to `sink`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("sink-write")
            .once_per_change();
            let key = first.key();
            let _sink = install_command_sink(false);
            let sink = COMMAND_SINK
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .expect("sink");
            let mut broken = BrokenWriter;
            sink.display_with(first, &mut broken);
            assert!(
                !dedup::was_seen(root, &key, 30),
                "an undelivered warning must not be remembered as shown"
            );

            let second = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Sink` to `sink`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("sink-write")
            .once_per_change();
            let mut delivered = Vec::new();
            sink.display_with(second, &mut delivered);
            assert_eq!(
                String::from_utf8_lossy(&delivered),
                "warning: folded `Sink` to `sink`\n"
            );
            assert!(
                dedup::was_seen(root, &key, 30),
                "a warning the reader received is remembered"
            );
        });
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    /// Two launches in one state root show an observational policy warning once
    /// to a person, while a later JSON envelope still carries it with its code
    /// and source. The warning is the real one `Policy::check` produces.
    #[test]
    fn writer_census_is_shown_once_but_every_envelope_carries_it() {
        use provenant_records::policy::{Effect, Facts, Policy};
        with_root(|root| {
            set_state_root(root);
            let Effect::Warn(warning) = Policy::default().check(
                "WRITER_CENSUS",
                Facts::violation("process census reports an occupant"),
            ) else {
                panic!("WRITER_CENSUS is an observational rule")
            };
            let result = CommandResult::new("run", Outcome::Ok, json!({}));
            let human = provenant_cli::Globals {
                json: false,
                ..Default::default()
            };
            let json_globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };

            let key = warning.key();
            for _ in 0..2 {
                let _sink = install_command_sink(false);
                warning::warn(warning.clone());
                let mut out = Vec::new();
                let mut err = Vec::new();
                assert_eq!(
                    emit_with_writers(&result, &human, &mut out, &mut err, "", false, None),
                    0
                );
            }
            // A human warning prints to the process stderr as it arrives, so it
            // never appears in the emitter's writer. Its per-change row is
            // advanced only after a successful display, which makes the row's
            // count the number of times a person actually saw the notice.
            let db = open_connection(&root.join("warnings.sqlite3"), true).expect("db");
            let shown: i64 = db
                .query_row(
                    "SELECT count FROM seen WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get(0),
                )
                .expect("the census notice was shown");
            assert_eq!(shown, 1, "a person sees the census notice once");
            drop(db);

            let _sink = install_command_sink(true);
            warning::warn(warning);
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                emit_with_writers(&result, &json_globals, &mut out, &mut err, "", false, None),
                0
            );
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
            let envelope: Value = serde_json::from_slice(&out).expect("envelope");
            let carried = envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|entry| {
                    entry["code"] == "WRITER_CENSUS"
                        && entry["source"]["layer"] == "default"
                        && entry["source"]["origin"] == "compiled policy table"
                });
            assert!(
                carried,
                "the second envelope still lists the warning: {envelope}"
            );
        });
    }

    fn shared_warning(subject: &str) -> Warning {
        let setting = provenant_records::command_result::SettingRef {
            key: "domains.cli.collapse_warnings".into(),
            value: "on".into(),
            layer: provenant_records::Layer::Host,
            origin: "config.toml:12".into(),
        };
        let code = provenant_records::WarningCode("SHARED");
        Warning::new(
            code,
            "7 lanes share a tree",
            provenant_records::warning::detected_source(code),
        )
        .subject(subject)
        .setting(setting)
    }

    #[test]
    // Spec: docs/specs/agent-native.md#35-short-output
    fn same_template_warnings_render_once_json_keeps_all() {
        with_root(|root| {
            set_state_root(root);
            let warnings: Vec<Warning> = (0..7)
                .map(|index| shared_warning(&format!("lane-{index}")))
                .collect();
            let result = CommandResult::new("fixture", Outcome::Ok, json!({}));
            let human_globals = provenant_cli::Globals {
                json: false,
                ..Default::default()
            };
            {
                let _sink = install_command_sink_with(false, false);
                for warning in warnings.clone() {
                    warning::warn(warning);
                }
                let mut out = Vec::new();
                let mut err = Vec::new();
                assert_eq!(
                    emit_with_writers(&result, &human_globals, &mut out, &mut err, "", false, None,),
                    0
                );
                let text = String::from_utf8(err).expect("utf8");
                assert!(text.contains("warning [SHARED] ×7:"), "{text}");
                assert!(text.contains("(first: lane-0)"), "{text}");
                assert!(text.contains("--full lists all"), "{text}");
                assert_eq!(
                    text.lines()
                        .filter(|line| line.starts_with("warning [SHARED]"))
                        .count(),
                    1,
                    "{text}"
                );
            }
            {
                let _sink = install_command_sink(true);
                for warning in warnings {
                    warning::warn(warning);
                }
                let globals = provenant_cli::Globals {
                    json: true,
                    ..Default::default()
                };
                let mut out = Vec::new();
                let mut err = Vec::new();
                assert_eq!(
                    emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None),
                    0
                );
                let envelope: Value = serde_json::from_slice(&out).expect("envelope");
                assert_eq!(envelope["warnings"].as_array().map(Vec::len), Some(7));
            }
        });
    }

    #[test]
    // Spec: docs/specs/agent-native.md#35-short-output
    fn large_result_keeps_outcome_and_writes_private_report() {
        with_root(|root| {
            set_state_root(root);
            let _sink = install_command_sink(false);
            let result = CommandResult::new(
                "status",
                Outcome::Ok,
                json!({"blob": "x".repeat(provenant_cli::MAX_RESULT_BYTES)}),
            );
            let globals = provenant_cli::Globals {
                json: false,
                ..Default::default()
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                emit_unpublished_with_writers(
                    &result,
                    &globals,
                    "status: ok\n",
                    false,
                    None,
                    &mut out,
                    &mut err,
                ),
                0
            );
            let entries: Vec<_> = std::fs::read_dir(root.join("reports"))
                .expect("reports directory")
                .flatten()
                .collect();
            assert_eq!(entries.len(), 1, "one private report for the result");
            let path = entries[0].path();
            assert!(path.to_string_lossy().contains("status-"), "{path:?}");
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("report metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let text = String::from_utf8(err).expect("utf8");
            assert!(text.contains("RESULT_TOO_LARGE"), "{text}");
            assert!(text.contains(&path.to_string_lossy().to_string()), "{text}");
            let saved: Value =
                serde_json::from_slice(&std::fs::read(&path).expect("report bytes")).expect("json");
            assert_eq!(saved["command"], "status");
            assert_eq!(saved["status"], "ok");
        });
    }
}
