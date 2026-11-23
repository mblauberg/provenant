//! The binary edge's output seam. One sink per command collects typed warnings,
//! prints them as they arrive in human mode, or carries them in the JSON
//! envelope. Emission and the dedup store live here so no library crate writes
//! to either standard stream.
//!
//! Spec: docs/specs/cli.md#warnings-are-typed-values
#![allow(clippy::disallowed_methods)] // The only module allowed to write the standard streams.

use crate::overflow;
use provenant_cli::{OverflowPolicy, RenderError, RenderMode, Summary};
use provenant_records::warning::{self, Dedup, Sink, Warning};
use provenant_records::{CommandResult, Outcome};
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

impl Sink for CommandSink {
    /// A warning arrives here from `records::warning::warn`. Collapse repeats by
    /// key; skip per-change keys already remembered in the state root; then
    /// either print to stderr as it arrives or keep it for the envelope.
    fn accept(&self, warning: Warning) {
        let key = warning.key();
        let mut state = self.state.lock().expect("output sink lock");
        if !state.seen.insert(key.clone()) {
            return;
        }
        if warning.dedup == Dedup::PerChange {
            let root = STATE_ROOT.read().expect("state root lock").clone();
            if let Some(root) = root
                && !dedup::remember(&root, &key, &warning.code)
            {
                return;
            }
        }
        if state.json {
            state.warnings.push(warning);
        } else {
            let mut err = io::stderr().lock();
            let _ = writeln!(err, "warning: {}", warning.message);
            if let Some(repair) = warning.repair.as_deref() {
                let _ = writeln!(err, "  repair: {repair}");
            }
        }
    }
}

/// Install the command's sink. The guard restores the previous sink on drop,
/// so nested dispatch in tests cannot leak one command's warnings into another.
pub(crate) fn install_command_sink(json: bool) -> warning::SinkGuard {
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
    warning::install(sink)
}

/// Name the state root used by per-change dedup. Called as soon as roots are
/// resolved, before config and root warnings are raised.
pub(crate) fn set_state_root(root: &Path) {
    if let Ok(mut slot) = STATE_ROOT.write() {
        *slot = Some(root.to_owned());
    }
}

/// Take the warnings collected for the current command's envelope.
fn take_warnings() -> Vec<Warning> {
    let sink = COMMAND_SINK.read().expect("output sink lock").clone();
    sink.map(|sink| {
        let mut state = sink.state.lock().expect("output sink lock");
        state.seen.clear();
        std::mem::take(&mut state.warnings)
    })
    .unwrap_or_default()
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
            warnings,
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
        Ok(status) => provenant_cli::exit_status(&status),
        Err(_) => {
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
    let warnings = take_warnings();
    let rendered = if globals.json {
        let data = serde_json::to_value(&result.data).unwrap_or(Value::Null);
        let json_result = CommandResult {
            schema_version: result.schema_version,
            command: result.command.clone(),
            status: result.status,
            data,
            error: result.error.clone(),
            warnings,
        };
        render_unpublished(
            &json_result,
            globals,
            human,
            read_only,
            summary,
            provenant_cli::MAX_RESULT_BYTES,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
    } else {
        render_unpublished(
            result,
            globals,
            human,
            read_only,
            summary,
            provenant_cli::MAX_RESULT_BYTES,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
    };
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
            let marker = json!({
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

    /// Record `key` and report whether it is new. Existing rows have their
    /// count and last-seen time advanced; stale rows are pruned on every write.
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
        let fresh = tx
            .execute(
                "INSERT INTO seen (key, code, first_seen, last_seen, count) VALUES (?1, ?2, ?3, ?3, 1) ON CONFLICT(key) DO NOTHING",
                rusqlite::params![key, code, now],
            )
            .map(|rows| rows == 1)
            .unwrap_or(true);
        let _ = tx.execute(
            "UPDATE seen SET last_seen=?1, count=count+1 WHERE key=?2",
            rusqlite::params![now, key],
        );
        let _ = tx.execute(
            "DELETE FROM seen WHERE last_seen < ?1",
            rusqlite::params![cutoff],
        );
        let _ = tx.commit();
        fresh
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
mod tests {
    use super::*;
    use provenant_records::warning::{WarningCode, capture};
    use provenant_sqlite::{ensure_schema, open_connection};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_root(f: impl FnOnce(&Path)) {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let dir = tempfile::tempdir().expect("tempdir");
        f(dir.path());
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn json_mode_carries_warnings_in_the_envelope_and_not_on_stderr() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let _sink = install_command_sink(true);
        warning::warn(Warning::new(WarningCode("TEST"), "a typed warning"));
        let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
        let mut globals = provenant_cli::Globals::default();
        globals.json = true;
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
        let _lock = TEST_LOCK.lock().expect("test lock");
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
}
