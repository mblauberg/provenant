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
use provenant_records::warning::{self, Dedup, RESULT_TOO_LARGE, Sink, Warning};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// The command's sink, installed once before any path can raise a warning.
static COMMAND_SINK: RwLock<Option<Arc<CommandSink>>> = RwLock::new(None);
/// Set as soon as the state root is known, before root and config warnings fire.
static STATE_ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

struct SinkState {
    json: bool,
    /// Per-invocation dedup: the keys this command has already handled.
    seen: HashSet<String>,
    /// Warnings held for the JSON envelope (human mode prints instead).
    warnings: Vec<Warning>,
}

struct CommandSink {
    state: Mutex<SinkState>,
}

impl CommandSink {
    /// Show an accepted warning, then record its per-change key. Recording only
    /// after the reader has it keeps a warning swallowed on an exit path that
    /// never emits from being marked seen before it was displayed.
    fn display(&self, warning: Warning) {
        {
            let mut err = io::stderr().lock();
            let _ = writeln!(err, "warning: {}", warning.message);
            if let Some(repair) = warning.repair.as_deref() {
                let _ = writeln!(err, "  repair: {repair}");
            }
        }
        remember_for_change(&warning);
    }

    /// Take warnings still held for an envelope that never arrived.
    fn take_held(&self) -> Vec<Warning> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.seen.clear();
        std::mem::take(&mut state.warnings)
    }
}

impl Sink for CommandSink {
    /// A warning arrives here from `records::warning::warn`. Collapse repeats by
    /// key; skip per-change keys already remembered in the state root; then
    /// either print to stderr as it arrives or keep it for the envelope.
    fn accept(&self, warning: Warning) {
        let key = warning.key();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.seen.insert(key.clone()) {
            return;
        }
        if warning.dedup == Dedup::PerChange {
            let root = STATE_ROOT
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(root) = root
                && dedup::was_seen(&root, &key)
            {
                return;
            }
        }
        if state.json {
            state.warnings.push(warning);
        } else {
            drop(state);
            self.display(warning);
        }
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
            for warning in sink.take_held() {
                sink.display(warning);
            }
        }
    }
}

/// Install the command's sink. The guard restores the previous sink on drop,
/// so nested dispatch in tests cannot leak one command's warnings into another.
pub(crate) fn install_command_sink(json: bool) -> CommandSinkGuard {
    let sink = Arc::new(CommandSink {
        state: Mutex::new(SinkState {
            json,
            seen: HashSet::new(),
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

/// Name the state root used by per-change dedup. Called as soon as roots are
/// resolved, before config and root warnings are raised.
pub(crate) fn set_state_root(root: &Path) {
    if let Ok(mut slot) = STATE_ROOT.write() {
        *slot = Some(root.to_owned());
    }
}

/// Record a per-change key after its warning has been shown. Invocation-scoped
/// keys need no memory: one command shows each such warning at most once.
fn remember_for_change(warning: &Warning) {
    if warning.dedup != Dedup::PerChange {
        return;
    }
    let root = STATE_ROOT
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(root) = root {
        dedup::remember(&root, &warning.key(), warning.code.as_ref());
    }
}

/// Take the warnings collected for the current command's envelope. Taking only
/// moves them out of the sink. A per-change key is remembered once the reader
/// has the envelope (`remember_warnings`) or once the guard shows the warning on
/// a path that never emits — never before it has been delivered.
fn take_warnings() -> Vec<Warning> {
    let sink = COMMAND_SINK
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    sink.map(|sink| sink.take_held()).unwrap_or_default()
}

/// Remember a batch of per-change keys after their envelope reached the reader.
/// An undelivered warning must not be marked seen just because it was drained.
fn remember_warnings(warnings: &[Warning]) {
    for warning in warnings {
        remember_for_change(warning);
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
    let warnings = take_warnings();
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
    // JSON prints nothing to stderr: the sink held every warning, and this is
    // the only place they reach the consumer. Serialising `T` to a Value lets
    // an owner keep its own data type while the envelope gains one field.
    let rendered = if matches!(mode, RenderMode::Json) {
        let data = serde_json::to_value(&result.data).unwrap_or(Value::Null);
        let json_result = CommandResult {
            schema_version: result.schema_version,
            command: result.command.clone(),
            status: result.status,
            data,
            error: result.error.clone(),
            warnings: warnings.clone(),
        };
        provenant_cli::render_or_publish(
            &json_result,
            mode,
            stdout,
            stderr,
            |value| overflow::publish(value, globals),
            // A published overflow keeps the result's status in its envelope,
            // so the exit code matches it; only a real render/IO failure exits 3.
            OverflowPolicy::Proceed,
        )
    } else {
        provenant_cli::render_or_publish(
            result,
            mode,
            stdout,
            stderr,
            |value| overflow::publish(value, globals),
            OverflowPolicy::Proceed,
        )
    };
    match rendered {
        Ok(status) => {
            // The envelope reached the reader: its warnings are now delivered,
            // so their per-change keys may be remembered.
            remember_warnings(&warnings);
            provenant_cli::exit_status(&status)
        }
        Err(_) => {
            // No envelope was written. Hand the warnings back to the guard,
            // which shows them on exit and remembers them only once shown.
            return_warnings(warnings);
            let _ = io::stderr().write_all(b"provenant: IO: cannot write command result\n");
            3
        }
    }
}

/// A policy explanation is a structured result; compact output names omitted rows.
pub(crate) fn land_explanation(data: &Value, globals: &provenant_cli::Globals) -> Option<Summary> {
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
    Some(Summary::new("Landing policy", rows, Some(detail)))
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
    let warnings = take_warnings();
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
            remember_warnings(&warnings);
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
            return_warnings(warnings);
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
    )
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

    pub(super) const WARNINGS_SCHEMA: Schema = Schema {
        sql: "CREATE TABLE seen (key TEXT PRIMARY KEY, code TEXT NOT NULL, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, count INTEGER NOT NULL) STRICT; PRAGMA user_version=5;",
        version: 5,
        steps: &[],
        migrations: &[],
    };

    /// Whether `key` has already been shown. A read-only check: it does not
    /// mark the key, so a warning can be held for the envelope before it is
    /// remembered at display time.
    pub(super) fn was_seen(state_root: &Path, key: &str) -> bool {
        let Ok(db) = open_connection(&state_root.join("warnings.sqlite3"), false) else {
            return false;
        };
        if ensure_schema(&db, WARNINGS_SCHEMA).is_err() {
            return false;
        }
        use provenant_core::Clock as _;
        let now = provenant_os::clock::SystemClock.unix_seconds().max(0);
        let retention = super::host_settings::dedup_retention_days(state_root);
        let window = i64::try_from(retention.saturating_mul(86_400)).unwrap_or(i64::MAX);
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
    pub(super) fn remember(state_root: &Path, key: &str, code: &str) -> bool {
        let Ok(mut db) = open_connection(&state_root.join("warnings.sqlite3"), false) else {
            return true;
        };
        if ensure_schema(&db, WARNINGS_SCHEMA).is_err() {
            return true;
        }
        use provenant_core::Clock as _;
        // SQLite stores signed integers; seconds before the epoch clamp to zero.
        let now = provenant_os::clock::SystemClock.unix_seconds().max(0);
        let retention = super::host_settings::dedup_retention_days(state_root);
        let window = i64::try_from(retention.saturating_mul(86_400)).unwrap_or(i64::MAX);
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

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn json_mode_carries_warnings_in_the_envelope_and_not_on_stderr() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        warning::warn(Warning::new(WarningCode("TEST"), "a typed warning"));
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
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn an_owner_capture_keeps_warnings_out_of_the_command_sink() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ((), captured) = capture(|| {
            warning::warn(Warning::new(
                WarningCode("TEST"),
                "a configured key was ignored",
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
            assert!(dedup::remember(root, "sha256:a", "A"));
            assert!(!dedup::remember(root, "sha256:a", "A"));
            assert!(dedup::remember(root, "sha256:b", "A"));
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
            assert!(dedup::remember(root, "sha256:fresh", "A"));
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
    // Spec: docs/specs/cli.md#one-bounded-envelope
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
    // Spec: docs/specs/cli.md#one-bounded-envelope
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
        warning::warn(Warning::new(WarningCode("TEST"), "w".repeat(512)));
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
    fn a_per_change_warning_is_remembered_only_after_a_successful_write() {
        with_root(|root| {
            set_state_root(root);
            let warning = Warning::new(NAME_CASE_FOLDED, "folded `Reader` to `reader`")
                .subject("reader")
                .once_per_change();
            let key = warning.key();
            let _sink = install_command_sink(true);
            warning::warn(warning);
            assert!(
                !dedup::was_seen(root, &key),
                "a warning held for the envelope is not remembered before it is shown"
            );
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
            assert!(
                dedup::was_seen(root, &key),
                "delivering the warning in the envelope remembers its key"
            );
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_failed_write_does_not_remember_an_undelivered_warning() {
        with_root(|root| {
            set_state_root(root);
            let warning = Warning::new(NAME_CASE_FOLDED, "folded `Writer` to `writer`")
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
                !dedup::was_seen(root, &key),
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
            let warning = Warning::new(NAME_CASE_FOLDED, "folded `Writer` to `writer`")
                .subject("writer")
                .once_per_change();
            let key = warning.key();
            {
                let _sink = install_command_sink(true);
                warning::warn(warning);
                assert!(!dedup::was_seen(root, &key));
            }
            assert!(
                dedup::was_seen(root, &key),
                "the guard drains held warnings on exit so they are shown once"
            );
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_fresh_per_change_key_starts_at_one() {
        with_root(|root| {
            assert!(dedup::remember(root, "sha256:fresh", "A"));
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
            assert!(!dedup::remember(root, "sha256:fresh", "A"));
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
}
