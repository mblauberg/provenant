//! Encode completely before delivery; never repeat an owner call. A result over
//! the cap is bounded with a warning and an omission marker, or paged once to
//! a published artefact whose pointer keeps the result's original status.
use crate::limits;
pub use crate::limits::MAX_RESULT_BYTES;
use provenant_records::command_result::NextStep;
use provenant_records::warning::{RESULT_TOO_LARGE, Warning};
use provenant_records::{CommandResult, Outcome};
use provenant_workflow::session::CommandPrefix;
use serde::Serialize;
use std::io::{self, Write};
use thiserror::Error;

/// An error cause can carry a 2 KiB command-output tail, expanded by escaping.
/// Human output keeps both ends inside this bound and marks the omitted bytes;
/// compact output summarises it to one row.
const ERROR_CAUSE_BYTES: usize = 4 * 1024;

/// The binary edge owns the warning sink and the standard streams. A library
/// command surface never writes a warning itself: it hands its finished result
/// to the one seam below, which moves the warnings raised while the command ran
/// onto the envelope before it is rendered. Under `--json` the sink holds them,
/// so they travel in `envelope[warnings]` and none reaches stderr; in human
/// mode the sink prints them as they arrive. A second install is ignored so a
/// nested dispatch cannot replace the edge's seam.
///
/// Spec: docs/specs/cli.md#warnings-are-typed-values
/// The edge's standard output and error writers.
pub type Streams = (Box<dyn Write>, Box<dyn Write>);

pub struct CommandSeam {
    /// Drain the warnings raised while the command ran.
    pub take: fn() -> Vec<Warning>,
    /// Record per-change keys once the envelope reached the reader.
    pub remember: fn(&[Warning]),
    /// Return undelivered warnings so the edge's guard shows them on exit.
    pub give_back: fn(Vec<Warning>),
    /// The edge's standard output and error, the only writers a command
    /// surface renders to.
    pub streams: fn() -> Streams,
}

static COMMAND_SEAM: std::sync::OnceLock<CommandSeam> = std::sync::OnceLock::new();

/// Install the binary edge's drain for command warnings. Called once before any
/// command is dispatched.
///
/// Spec: docs/specs/cli.md#warnings-are-typed-values
pub fn install_command_seam(seam: CommandSeam) {
    let _ = COMMAND_SEAM.set(seam);
}

/// The one emit path for a library command surface: move the command's held
/// warnings onto its result before rendering, remember their per-change keys
/// only after the envelope reached the reader, and hand them back to the edge
/// on a write failure so its guard shows them on exit instead of swallowing
/// them. Rendering and overflow policy stay with the existing seam.
///
/// Spec: docs/specs/cli.md#warnings-are-typed-values
pub fn render_command_result<T: Serialize>(
    result: &mut CommandResult<T>,
    mode: RenderMode<'_>,
    publish: impl FnOnce(&CommandResult<T>) -> io::Result<provenant_records::AbsolutePath>,
    policy: OverflowPolicy,
) -> Result<Outcome, RenderError> {
    let seam = COMMAND_SEAM.get();
    result.warnings = seam.map(|seam| (seam.take)()).unwrap_or_default();
    // The one renderer (`render_within`) turns the envelope's typed next steps
    // into human lines for both human and compact modes, so a compact summary
    // cannot drop the line the JSON array carries.
    // Without an installed edge (a library caller or a test) there is no
    // reader, so the rendered bytes go nowhere rather than to process streams.
    let (mut out, mut err): Streams = match seam {
        Some(seam) => (seam.streams)(),
        None => (Box::new(io::sink()), Box::new(io::sink())),
    };
    let rendered = render_or_publish(result, mode, &mut out, &mut err, publish, policy);
    drop((out, err));
    if let Some(seam) = seam {
        if rendered.is_ok() {
            (seam.remember)(&result.warnings);
        } else {
            (seam.give_back)(std::mem::take(&mut result.warnings));
        }
    }
    rendered
}

/// The compact view an owner supplies for the default human output: a headline
/// of counts, then the rows that need action first. Suggested commands are not
/// part of a summary: the owner puts them on the envelope's `next` array and the
/// binary edge renders that array once. The generic truncator is only a fallback
/// for owners without a summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub lines: Vec<String>,
    /// Rows the owner held back; the renderer says so and names `--full`.
    pub omitted: usize,
}
impl Summary {
    /// A headline, then `rows` in the owner's priority order, bounded by the
    /// shared row limits. Rows past the limit are counted, not dropped silently.
    pub fn new(headline: impl Into<String>, rows: Vec<String>) -> Self {
        let omitted = rows.len().saturating_sub(limits::SUMMARY_ROWS);
        let lines = std::iter::once(headline.into())
            .chain(
                rows.iter()
                    .take(limits::SUMMARY_ROWS)
                    .map(|row| bounded_text(row, limits::SUMMARY_ROW_BYTES)),
            )
            .collect();
        Self { lines, omitted }
    }
    /// Text the owner has already bounded; shown as it is.
    pub fn verbatim(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
            omitted: 0,
        }
    }
    /// `3 running, 1 failed` in the order given, skipping zero counts.
    pub fn tally(counts: &[(&str, usize)]) -> String {
        counts
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(name, count)| format!("{count} {name}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The one formatter that turns the envelope's typed next steps into human
/// lines. Every edge calls it with the command prefix appropriate to its caller;
/// JSON never calls it and carries the argv arrays instead. A step's `why` and
/// its rendered `argv` are both derived from the same typed value.
/// Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
pub fn next_steps_text(steps: &[NextStep], prefix: &CommandPrefix) -> String {
    let mut out = String::new();
    for step in steps {
        match prefix.render_argv(&step.argv) {
            Some(command) if !command.is_empty() => {
                let why = bounded_text(&step.why, limits::SUMMARY_ROW_BYTES);
                if why.is_empty() {
                    out.push_str(&format!("next: {command}\n"));
                } else {
                    out.push_str(&format!("next: {why}; {command}\n"));
                }
            }
            _ => out.push_str("next: [this command cannot be shown safely]\n"),
        }
    }
    out
}

#[derive(Clone, Copy)]
pub enum RenderMode<'a> {
    Json,
    /// Complete detail from the owner's own human text.
    Human {
        text: &'a str,
        read_only: bool,
        summary: Option<&'a Summary>,
    },
    /// The owner's summary, or the bounded fallback over its text.
    Compact {
        text: &'a str,
        read_only: bool,
        summary: Option<&'a Summary>,
    },
}

/// How a published overflow is presented. The envelope status must always
/// agree with the process exit code; the policy decides which one changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Page the complete result once, warn on stderr and keep the result's
    /// status. The envelope and the exit code both stay with the result.
    Proceed,
    /// Refuse with a typed `RESULT_TOO_LARGE` Error envelope and exit 3.
    Refuse,
}

impl crate::Globals {
    /// JSON is an independent transport; --full only affects human output.
    pub fn render_mode<'a>(
        &self,
        text: &'a str,
        read_only: bool,
        summary: Option<&'a Summary>,
    ) -> RenderMode<'a> {
        if self.json {
            RenderMode::Json
        } else if self.full {
            RenderMode::Human {
                text,
                read_only,
                summary,
            }
        } else {
            RenderMode::Compact {
                text,
                read_only,
                summary,
            }
        }
    }
}

/// Clip on a UTF-8 boundary. The suffix makes omitted text explicit.
pub fn bounded_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut end = max.saturating_sub(3).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}
/// Whole text up to `max` bytes; above it, the head and the tail with a marker
/// that names how many bytes were left out and how to read them.
pub fn head_and_tail(text: &str, max: usize) -> String {
    head_and_tail_counted(text, max).0
}

/// `head_and_tail` together with the exact number of *original* bytes omitted.
/// The count is the bytes between head and tail, which is what the in-text
/// marker names; it is not `text.len() - result.len()`, because the result
/// contains the inserted marker.
fn head_and_tail_counted(text: &str, max: usize) -> (String, usize) {
    if text.len() <= max {
        return (text.into(), 0);
    }
    let half = max / 2;
    let mut head = half.min(text.len());
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len().saturating_sub(half);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    // Prefer whole lines at the cut when one is near.
    if let Some(cut) = text[..head].rfind('\n') {
        head = cut + 1;
    }
    if let Some(cut) = text[tail..].find('\n') {
        let snap = tail + cut + 1;
        // Only snap to the next whole line when one follows; otherwise the
        // newline was the final byte and snapping would drop the last line.
        if snap < text.len() {
            tail = snap;
        }
    }
    let tail = tail.min(text.len());
    let omitted = tail - head;
    let bounded = format!(
        "{}[... {} bytes omitted; use --full for all of it ...]\n{}",
        &text[..head],
        omitted,
        &text[tail..]
    );
    (bounded, omitted)
}

/// A bounded presentation for surfaces that warn and proceed on an oversized
/// result: keep both ends of the owner's text and an explicit marker naming the
/// omitted bytes, rather than refusing the whole result. `limit` includes the
/// marker. Returns the clipped text and how many bytes were left out.
pub fn clip_for_output(text: &str, limit: usize) -> (String, usize) {
    const RESERVE: usize = 96;
    let (clipped, omitted) = head_and_tail_counted(text, limit.saturating_sub(RESERVE));
    if clipped.len() <= limit {
        (clipped, omitted)
    } else {
        bounded_head_with_marker(text, limit)
    }
}

/// A head-only clip for the case the two-ended view cannot fit the limit. The
/// marker names the omitted bytes and the count is exactly the number of
/// original bytes no longer present, so a tiny limit never under-reports or
/// silently drops the omission marker.
fn bounded_head_with_marker(text: &str, limit: usize) -> (String, usize) {
    if text.len() <= limit {
        return (text.into(), 0);
    }
    const PREFIX: &str = "...[";
    const SUFFIX: &str = " bytes omitted; use --full for all of it]";
    // Reserve for the widest possible count so the marker always fits when the
    // limit can hold it; the real count needs the same or fewer digits.
    let fixed = PREFIX.len() + SUFFIX.len() + text.len().to_string().len();
    if limit <= fixed {
        // No room for a marker: keep the bounded head and report exactly how
        // many original bytes its ellipsis replaced.
        let clipped = bounded_text(text, limit);
        let shown = clipped.len().saturating_sub(3);
        return (clipped, text.len().saturating_sub(shown));
    }
    let mut end = limit - fixed;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = text.len() - end;
    let marker = format!("{PREFIX}{omitted}{SUFFIX}");
    (format!("{}{marker}", &text[..end]), omitted)
}
/// A bounded copy of an error for a capped result view: it keeps the typed
/// code, repair and diagnostic while shortening the free-form text so a large
/// failure is still reported without reintroducing the overflow.
pub fn bounded_error(
    error: &provenant_records::CommandError,
    max: usize,
) -> provenant_records::CommandError {
    let mut bounded = error.clone();
    bounded.message = head_and_tail(&error.message, max);
    bounded.cause = error
        .cause
        .as_deref()
        .map(|cause| head_and_tail(cause, max));
    bounded
}
/// The generic fallback for an owner with no summary: small text is shown
/// whole; larger text keeps its first rows and says how much was left out.
fn fallback_text(command: &str, text: &str) -> String {
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return format!("{command}: done\n");
    }
    if text.len() <= limits::FALLBACK_BYTES
        && lines.len() <= limits::FALLBACK_LINES
        && lines
            .iter()
            .all(|line| line.len() <= limits::FALLBACK_ROW_BYTES)
    {
        let mut whole = text.to_owned();
        if !whole.ends_with('\n') {
            whole.push('\n');
        }
        return whole;
    }
    let mut out = String::new();
    let mut clipped = 0;
    for line in lines.iter().take(limits::FALLBACK_LINES) {
        if line.len() > limits::FALLBACK_ROW_BYTES {
            clipped += 1;
        }
        out.push_str(&bounded_text(line, limits::FALLBACK_ROW_BYTES));
        out.push('\n');
    }
    let shown = lines.len().min(limits::FALLBACK_LINES);
    out.push_str(&format!(
        "({} of {} lines shown, {clipped} clipped to {} bytes; {} lines omitted; use --full for all detail)\n",
        shown,
        lines.len(),
        limits::FALLBACK_ROW_BYTES,
        lines.len() - shown,
    ));
    out
}
fn compact_text(command: &str, text: &str, summary: Option<&Summary>) -> String {
    match summary.filter(|summary| !summary.lines.is_empty()) {
        Some(summary) => {
            let mut body = summary.lines.join("\n");
            body.push('\n');
            if summary.omitted > 0 {
                body.push_str(&format!(
                    "({} more not shown; use --full for all detail)\n",
                    summary.omitted
                ));
            }
            body
        }
        None => fallback_text(command, text),
    }
}
#[derive(Debug, Error)]
pub enum RenderError {
    #[error("result exceeds 1 MiB; read the owner-published artifact or request a smaller page")]
    TooLarge,
    #[error("cannot encode command result")]
    Encode(#[source] serde_json::Error),
    #[error("cannot write command result")]
    Write(#[source] io::Error),
}
pub fn exit_status(outcome: &Outcome) -> u8 {
    outcome.exit_code()
}
/// At most cap bytes are retained even if an owner's Serialize floods output.
struct Buffer {
    bytes: Vec<u8>,
    overflow: bool,
    limit: usize,
}
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.overflow = true;
            return Err(io::Error::other("result limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn render<T: Serialize>(
    result: &CommandResult<T>,
    mode: RenderMode<'_>,
    writer: &mut impl Write,
) -> Result<(), RenderError> {
    render_within(result, mode, writer, MAX_RESULT_BYTES)
}
/// `render` with the result bound a caller chooses; the default is
/// `MAX_RESULT_BYTES`, and a lower bound lets a test reach the overflow path
/// without building a megabyte of real data.
pub fn render_within<T: Serialize>(
    result: &CommandResult<T>,
    mode: RenderMode<'_>,
    writer: &mut impl Write,
    limit: usize,
) -> Result<(), RenderError> {
    let error = result.error.clone();
    let result = CommandResult {
        schema_version: result.schema_version,
        command: result.command.clone(),
        status: result.status,
        data: &result.data,
        error,
        warnings: result.warnings.clone(),
        next: result.next.clone(),
    };
    let mut buffer = Buffer {
        bytes: Vec::new(),
        overflow: false,
        limit,
    };
    let encoded = match mode {
        RenderMode::Json => serde_json::to_writer(&mut buffer, &result)
            .map_err(RenderError::Encode)
            .and_then(|()| buffer.write_all(b"\n").map_err(RenderError::Write)),
        RenderMode::Human { .. } | RenderMode::Compact { .. } => {
            let text = match &result.error {
                Some(error) => {
                    let mut text = format!("provenant: {}: {}\n", error.code, error.message);
                    // A cause is extra evidence, not a second copy of the
                    // message. Owners that set both to the same detail must
                    // not make the operator read it twice.
                    if let Some(cause) = error.cause.as_deref()
                        && !cause.is_empty()
                        && cause != error.message
                    {
                        match mode {
                            RenderMode::Human { .. } => {
                                // A cause can be a 2 KiB command tail that expands
                                // when terminal controls are escaped. Escape first,
                                // then keep both ends inside the bound with the
                                // shared omitted-bytes marker: the failure is at the
                                // end, so never clip it away.
                                let escaped =
                                    provenant_records::diagnostic::escape_display_text(cause);
                                let bounded = head_and_tail(&escaped, ERROR_CAUSE_BYTES);
                                for line in bounded.lines() {
                                    text.push_str("  ");
                                    text.push_str(line);
                                    text.push('\n');
                                }
                            }
                            RenderMode::Compact { .. } => {
                                // Compact stays summarised: one bounded line
                                // and a pointer to --full, never a tail dump.
                                let safe = provenant_records::diagnostic::safe_multiline_text(
                                    cause,
                                    ERROR_CAUSE_BYTES,
                                );
                                let first = safe
                                    .lines()
                                    .find(|line| !line.trim().is_empty())
                                    .unwrap_or("");
                                text.push_str("  ");
                                text.push_str(&bounded_text(first, limits::FALLBACK_ROW_BYTES));
                                if safe.lines().count() > 1
                                    || first.len() > limits::FALLBACK_ROW_BYTES
                                {
                                    text.push_str(" (more with --full)");
                                }
                                text.push('\n');
                            }
                            RenderMode::Json => {}
                        }
                    }
                    // The repair is a typed field of the error envelope, not an
                    // owner suggestion: JSON carries it at `error.repair` and
                    // this renders that same field once for a human reader.
                    text.push_str(&format!(
                        "next: {}\n",
                        error.repair.as_deref().unwrap_or_default()
                    ));
                    text
                }
                None => {
                    let body = match mode {
                        RenderMode::Compact { text, summary, .. } => {
                            compact_text(&result.command, text, summary)
                        }
                        RenderMode::Human { text, .. } => text.to_owned(),
                        RenderMode::Json => String::new(),
                    };
                    // The envelope's typed next steps become human lines here,
                    // after the compact summary, so a summarised view never
                    // drops the command the JSON array still carries. The one
                    // formatter derives both the why and the command from the
                    // same typed value.
                    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
                    let next = match mode {
                        RenderMode::Json => String::new(),
                        RenderMode::Human { .. } | RenderMode::Compact { .. } => {
                            next_steps_text(&result.next, &CommandPrefix::default())
                        }
                    };
                    format!("{body}{next}")
                }
            };
            buffer
                .write_all(text.as_bytes())
                .map_err(RenderError::Write)
        }
    };
    if buffer.overflow {
        return Err(RenderError::TooLarge);
    }
    encoded?;
    match writer
        .write_all(&buffer.bytes)
        .and_then(|()| writer.flush())
    {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == io::ErrorKind::BrokenPipe
                && matches!(
                    mode,
                    RenderMode::Human {
                        read_only: true,
                        ..
                    } | RenderMode::Compact {
                        read_only: true,
                        ..
                    }
                )
                && result.status == Outcome::Ok =>
        {
            Ok(())
        }
        Err(error) => Err(RenderError::Write(error)),
    }
}
/// Quote text outside the conservative bare set `[A-Za-z0-9._:/@+%,-]`.
fn quoted(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@+%,-".contains(c))
    {
        return text.to_owned();
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if provenant_records::diagnostic::unsafe_display_character(c) => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
/// Terse human text for a result: one line per record, never pretty JSON.
///
/// An array, or an `items` array one object deep, yields one `key=value` line
/// per element, keys sorted; remaining scalars share a header line. Nulls and
/// empty arrays are dropped from the header, and null fields from records.
/// Strings outside a conservative bare set are quoted, with control, bidi and
/// other format characters escaped as `\u{..}`; a nested value is its compact
/// JSON as one quoted token. Every line ends in LF.
pub fn terse(value: &serde_json::Value) -> String {
    use serde_json::Value;
    fn scalar(value: &Value) -> String {
        match value {
            Value::String(text) => quoted(text),
            Value::Array(_) | Value::Object(_) => quoted(&value.to_string()),
            other => other.to_string(),
        }
    }
    fn record(value: &Value) -> String {
        match value {
            Value::Object(map) => map
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| format!("{k}={}", scalar(v)))
                .collect::<Vec<_>>()
                .join(" "),
            other => scalar(other),
        }
    }
    fn fields(
        map: &serde_json::Map<String, Value>,
        depth: u8,
        head: &mut Vec<String>,
        rows: &mut Vec<String>,
    ) {
        for (key, value) in map {
            match value {
                Value::Null => {}
                Value::Array(items) if items.is_empty() => {}
                Value::Array(items) if items.iter().all(Value::is_object) => {
                    let prefix = if key == "items" {
                        String::new()
                    } else {
                        format!("{key}: ")
                    };
                    rows.extend(items.iter().map(|item| format!("{prefix}{}", record(item))));
                }
                Value::Object(inner)
                    if depth == 0 && inner.values().any(|v| matches!(v, Value::Array(_))) =>
                {
                    fields(inner, 1, head, rows);
                }
                other => head.push(format!("{key}={}", scalar(other))),
            }
        }
    }
    let (mut head, mut rows) = (Vec::new(), Vec::new());
    match value {
        Value::Array(items) => rows.extend(items.iter().map(record)),
        Value::Object(map) => fields(map, 0, &mut head, &mut rows),
        Value::Null => {}
        other => head.push(scalar(other)),
    }
    let mut lines = Vec::new();
    if !head.is_empty() {
        lines.push(head.join(" "));
    }
    lines.extend(rows);
    lines.into_iter().map(|line| line + "\n").collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_uses_the_owner_summary_without_rendering_next()
    -> Result<(), Box<dyn std::error::Error>> {
        let text = "detail\n".repeat(500);
        let summary = Summary::new(
            "3 lanes: 1 failed, 2 running",
            vec!["aaaa failed".into(), "bbbb running".into()],
        );
        let result = CommandResult::new("lanes", Outcome::Ok, ());
        let mut bytes = Vec::new();
        render(
            &result,
            RenderMode::Compact {
                text: &text,
                read_only: true,
                summary: Some(&summary),
            },
            &mut bytes,
        )?;
        // Suggested commands are rendered by the binary edge from the envelope's
        // next array, never by the summary renderer.
        assert_eq!(
            String::from_utf8(bytes)?,
            "3 lanes: 1 failed, 2 running\naaaa failed\nbbbb running\n"
        );
        Ok(())
    }
    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn compact_renders_the_envelope_next_after_the_summary()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = Summary::new("wait mode: all", vec!["peer pending".into()]);
        let result =
            CommandResult::new("wait", Outcome::Waiting, ()).with_next(vec![NextStep::from_args(
                &["provenant", "wait", "peer"],
                "peer still running at timeout",
            )]);
        let mut bytes = Vec::new();
        render(
            &result,
            RenderMode::Compact {
                text: "peer pending\n",
                read_only: true,
                summary: Some(&summary),
            },
            &mut bytes,
        )?;
        // The summary renderer owns only the headline and rows. The one edge
        // appends the typed next line, so a compact view keeps what JSON carries.
        assert_eq!(
            String::from_utf8(bytes)?,
            "wait mode: all\npeer pending\nnext: peer still running at timeout; provenant wait peer\n"
        );
        Ok(())
    }
    #[test]
    fn summary_rows_past_the_limit_are_counted() -> Result<(), Box<dyn std::error::Error>> {
        let rows = (0..limits::SUMMARY_ROWS + 5)
            .map(|n| format!("row {n}"))
            .collect();
        let summary = Summary::new("head", rows);
        assert_eq!(summary.omitted, 5);
        let result = CommandResult::new("lanes", Outcome::Ok, ());
        let mut bytes = Vec::new();
        render(
            &result,
            RenderMode::Compact {
                text: "",
                read_only: true,
                summary: Some(&summary),
            },
            &mut bytes,
        )?;
        let text = String::from_utf8(bytes)?;
        assert!(text.contains("5 more not shown"), "{text}");
        assert!(text.contains("--full"), "{text}");
        assert!(!text.contains("next:"), "{text}");
        Ok(())
    }
    #[test]
    fn fallback_says_how_much_it_omitted_and_never_invents_a_next()
    -> Result<(), Box<dyn std::error::Error>> {
        let text = (0..100)
            .map(|n| format!("row {n} {}\n", "界".repeat(1000)))
            .collect::<String>();
        let result = CommandResult::new("route list", Outcome::Ok, ());
        let mut bytes = Vec::new();
        render(
            &result,
            RenderMode::Compact {
                text: &text,
                read_only: true,
                summary: None,
            },
            &mut bytes,
        )?;
        let compact = String::from_utf8(bytes)?;
        assert!(compact.contains("row 0"));
        assert!(compact.contains("--full"));
        assert!(compact.contains("of 100 lines"), "{compact}");
        assert!(!compact.contains("next:"), "{compact}");
        let mut full = Vec::new();
        render(
            &result,
            RenderMode::Human {
                text: &text,
                read_only: true,
                summary: None,
            },
            &mut full,
        )?;
        assert_eq!(full, text.as_bytes());
        Ok(())
    }
    #[test]
    fn head_and_tail_keeps_both_ends_and_names_the_omission() {
        let text = format!("{}{}", "a".repeat(100), "z".repeat(100));
        let bounded = head_and_tail(&text, 60);
        assert!(bounded.starts_with("aaa"));
        assert!(bounded.ends_with("zzz"));
        assert!(bounded.contains("bytes omitted; use --full"), "{bounded}");
        assert_eq!(head_and_tail("short", 60), "short");
    }
    #[test]
    fn head_and_tail_keeps_a_last_line_whose_newline_ends_the_text() {
        // The tail boundary lands inside a line whose trailing newline is the
        // final byte. Snapping past it would leave an empty tail and lose that
        // line — the very end where a failure usually sits.
        let text = format!("{}{}", "a".repeat(100), "LAST-LINE\n");
        let bounded = head_and_tail(&text, 40);
        assert!(bounded.ends_with("LAST-LINE\n"), "{bounded}");
        assert!(bounded.contains("bytes omitted"), "{bounded}");
    }
    #[test]
    fn clip_for_output_reports_the_exact_bytes_the_in_text_marker_names() {
        let text = format!("{}{}", "a".repeat(4096), "z".repeat(4096));
        let (clipped, omitted) = clip_for_output(&text, 512);
        assert!(clipped.len() <= 512, "{} exceeds the cap", clipped.len());
        let named = clipped
            .split("[... ")
            .nth(1)
            .and_then(|rest| rest.split(" bytes omitted;").next())
            .and_then(|count| count.parse::<usize>().ok())
            .expect("the bounded view carries an in-text omission marker");
        assert_eq!(
            omitted, named,
            "the caller's count must match the marker, not text.len() - clipped.len()"
        );
        assert!(omitted > 0 && omitted < text.len());
    }
    #[test]
    fn full_shows_the_owner_text_without_rendering_next() -> Result<(), Box<dyn std::error::Error>>
    {
        let result = CommandResult::new("lanes", Outcome::Ok, ());
        let mut bytes = Vec::new();
        render(
            &result,
            RenderMode::Human {
                text: "body\n",
                read_only: true,
                summary: None,
            },
            &mut bytes,
        )?;
        assert_eq!(String::from_utf8(bytes)?, "body\n");
        Ok(())
    }
    #[test]
    // Spec: docs/specs/cli.md#results
    fn error_cause_is_indented_after_the_message_and_never_repeats_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let cause = "npm ERR! Cannot find package 'zod'\nnext line of output";
        let mut result = CommandResult::new("worktree doctor", Outcome::Error, ());
        result.error = Some(provenant_records::CommandError::new(
            "WORKTREE",
            "doctor: worktree state could not be read (exit 1)",
            Some(cause),
            "run `provenant worktree doctor`",
        ));
        let human_mode = || RenderMode::Human {
            text: "",
            read_only: false,
            summary: None,
        };
        let mut human = Vec::new();
        render(&result, human_mode(), &mut human)?;
        let text = String::from_utf8(human)?;
        assert!(text.starts_with("provenant: WORKTREE: doctor:"));
        assert!(
            text.contains("  npm ERR! Cannot find package 'zod'\n"),
            "{text}"
        );
        assert!(text.contains("  next line of output\n"), "{text}");
        // The cause sits between the message and the one typed next command.
        let message_at = text.find("doctor:").ok_or("message")?;
        let cause_at = text.find("  npm ERR!").ok_or("cause")?;
        let next_at = text.find("next:").ok_or("next")?;
        assert!(message_at < cause_at && cause_at < next_at, "{text}");

        let mut json = Vec::new();
        render(&result, RenderMode::Json, &mut json)?;
        let value: serde_json::Value = serde_json::from_slice(&json)?;
        assert_eq!(value["error"]["cause"].as_str(), Some(cause));

        // Owners that set cause to the message must not show the same detail
        // twice in human output.
        let message = result
            .error
            .as_ref()
            .ok_or("missing error")?
            .message
            .clone();
        result.error.as_mut().ok_or("missing error")?.cause = Some(message.clone());
        let mut human = Vec::new();
        render(&result, human_mode(), &mut human)?;
        let text = String::from_utf8(human)?;
        assert_eq!(text.matches(&message).count(), 1, "{text}");
        Ok(())
    }

    #[test]
    // Spec: docs/specs/cli.md#results
    fn compact_error_cause_is_one_bounded_row_and_human_keeps_every_line()
    -> Result<(), Box<dyn std::error::Error>> {
        let cause = format!("npm ERR! {}\nsecond line\nthird line", "z".repeat(300));
        let mut result = CommandResult::new("worktree doctor", Outcome::Error, ());
        result.error = Some(provenant_records::CommandError::new(
            "WORKTREE",
            "doctor: worktree state could not be read",
            Some(cause.as_str()),
            "run `provenant worktree doctor`",
        ));
        let mut compact = Vec::new();
        render(
            &result,
            RenderMode::Compact {
                text: "",
                read_only: false,
                summary: None,
            },
            &mut compact,
        )?;
        let compact = String::from_utf8(compact)?;
        assert_eq!(
            compact
                .lines()
                .filter(|line| line.starts_with("  "))
                .count(),
            1,
            "{compact}"
        );
        assert!(compact.contains("--full"), "{compact}");
        assert!(!compact.contains("second line"), "{compact}");
        assert!(!compact.contains("third line"), "{compact}");
        assert!(
            compact
                .lines()
                .all(|line| line.len() <= limits::FALLBACK_ROW_BYTES + 24),
            "{compact}"
        );

        let mut human = Vec::new();
        render(
            &result,
            RenderMode::Human {
                text: "",
                read_only: false,
                summary: None,
            },
            &mut human,
        )?;
        let human = String::from_utf8(human)?;
        assert!(
            human.contains("second line") && human.contains("third line"),
            "{human}"
        );
        Ok(())
    }
    #[test]
    // Spec: docs/specs/cli.md#results
    fn long_human_cause_keeps_its_end_and_marks_the_omitted_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        // A bounded command tail grows well past the cap once terminal controls
        // are escaped. The renderer must keep the end, where the failure is,
        // and say how much it left out instead of silently clipping the tail.
        let end = "FATAL: final failure line";
        let cause = format!(
            "beginning-of-log {}\n{end}",
            "\u{202e}".repeat(700) // Cf override: 3 raw bytes, 8 escaped chars
        );
        assert!(cause.len() <= 2 * ERROR_CAUSE_BYTES, "{cause}");
        let mut result = CommandResult::new("worktree doctor", Outcome::Error, ());
        result.error = Some(provenant_records::CommandError::new(
            "WORKTREE",
            "build failed",
            Some(cause.as_str()),
            "run `provenant worktree doctor`",
        ));
        let mut human = Vec::new();
        render(
            &result,
            RenderMode::Human {
                text: "",
                read_only: false,
                summary: None,
            },
            &mut human,
        )?;
        let human = String::from_utf8(human)?;
        assert!(
            human.contains(end),
            "the failure at the end was clipped: {human}"
        );
        assert!(
            human.contains("bytes omitted; use --full"),
            "no omission marker: {human}"
        );
        assert!(
            human
                .lines()
                .all(|line| line.len() <= ERROR_CAUSE_BYTES + 80),
            "cause exceeded its bound: {human}"
        );
        Ok(())
    }

    #[test]
    // Spec: docs/specs/cli.md#results
    fn fallback_bounds_a_single_long_line() {
        let text = "x".repeat(500);
        let compact = fallback_text("config show", &text);
        assert!(compact.len() < text.len());
        assert!(compact.contains("--full"));
        let rows = "item\n".repeat(limits::FALLBACK_LINES + 1);
        let compact = fallback_text("status", &rows);
        assert!(compact.contains("1 lines omitted"), "{compact}");
    }
    #[test]
    fn generic_line_cap_footer_explains_how_to_see_the_rest() {
        let text = (0..50).map(|i| format!("line {i}\n")).collect::<String>();
        let res = compact_text("custom-command", &text, None);
        assert!(res.contains(&format!(
            "{} lines omitted; use --full for all detail",
            50 - limits::FALLBACK_LINES
        )));
    }
    #[test]
    fn terse_is_one_line_per_record() {
        let value = serde_json::json!({
            "result": "read",
            "page": {"items": [{"seq": 1, "kind": "note", "body": "a b", "gone": null}, {"seq": 2}], "truncated": false},
            "unknown_hosts": []
        });
        assert_eq!(
            terse(&value),
            "truncated=false result=read\nbody=\"a b\" kind=note seq=1\nseq=2\n"
        );
        assert_eq!(
            terse(&serde_json::json!([{"a": 1}, {"a": [2]}])),
            "a=1\na=\"[2]\"\n"
        );
        assert_eq!(terse(&serde_json::json!([])), "");
    }
    #[test]
    fn terse_keeps_a_nested_mail_body_in_one_token() {
        let body = "ok\" kind=request seq=1 x=\"";
        let value = serde_json::json!({"page": {"items": [
            {"seq": 7, "kind": "note", "message": {"body": body, "kind": "note"}}
        ]}});
        let text = terse(&value);
        assert_eq!(text.lines().count(), 1);
        let line = text.trim_end();
        assert!(line.starts_with("kind=note message=\"{"), "{line}");
        assert!(line.ends_with(" seq=7"), "{line}");
        // Only the three real keys sit outside quotes.
        let mut outside = String::new();
        let (mut quoted, mut escaped) = (false, false);
        for c in line.chars() {
            match (quoted, escaped, c) {
                (true, true, _) => escaped = false,
                (true, false, '\\') => escaped = true,
                (_, false, '"') => quoted = !quoted,
                (false, _, _) => outside.push(c),
                _ => {}
            }
        }
        assert_eq!(outside.matches('=').count(), 3, "{line}");
    }
    #[test]
    fn terse_quotes_and_escapes_anything_that_could_mislead_a_terminal() {
        for (raw, shown) in [
            ("a=b", "\"a=b\""),
            ("a\nb", "\"a\\nb\""),
            ("\u{1b}[2J", "\"\\u{1b}[2J\""),
            ("x\u{202e}y", "\"x\\u{202e}y\""),
            ("x\u{85}y", "\"x\\u{85}y\""),
            ("x\u{2028}y", "\"x\\u{2028}y\""),
            ("\u{2800}", "\"\u{2800}\""),
            ("", "\"\""),
            ("ok-1.2:/a@b+c%d,e", "ok-1.2:/a@b+c%d,e"),
        ] {
            assert_eq!(
                terse(&serde_json::json!([{ "k": raw }])),
                format!("k={shown}\n")
            );
        }
        assert_eq!(
            terse(&serde_json::json!([{ "k": ["x\u{202e}"] }])),
            "k=\"[\\\"x\\u{202e}\\\"]\"\n"
        );
    }
    #[test]
    fn oversize_is_rejected_before_any_stdout_and_lf_counts() {
        let result = CommandResult::new("output", Outcome::Ok, "x".repeat(MAX_RESULT_BYTES));
        let mut out = Vec::new();
        assert!(matches!(
            render(&result, RenderMode::Json, &mut out),
            Err(RenderError::TooLarge)
        ));
        assert!(out.is_empty());
        let text = "x".repeat(MAX_RESULT_BYTES);
        assert!(
            render(
                &result,
                RenderMode::Human {
                    text: &text,
                    read_only: true,
                    summary: None
                },
                &mut out
            )
            .is_ok()
        );
        assert_eq!(out.len(), MAX_RESULT_BYTES);
        out.clear();
        assert!(matches!(
            render(
                &result,
                RenderMode::Human {
                    text: &format!("{text}\n"),
                    read_only: true,
                    summary: None
                },
                &mut out
            ),
            Err(RenderError::TooLarge)
        ));
        assert!(out.is_empty());
    }
    #[test]
    fn all_outcomes_and_partial_input_required_data_keep_their_contract()
    -> Result<(), Box<dyn std::error::Error>> {
        for (outcome, code) in [
            (Outcome::Ok, 0),
            (Outcome::Failed, 1),
            (Outcome::Refused, 1),
            (Outcome::Invalid, 2),
            (Outcome::Error, 3),
            (Outcome::Unavailable, 4),
            (Outcome::Pending, 5),
            (Outcome::Waiting, 0),
            (Outcome::Timeout, 124),
            (Outcome::Cancelled, 130),
        ] {
            assert_eq!(exit_status(&outcome), code);
        }
        #[derive(Serialize)]
        struct Data {
            partial: bool,
            state: &'static str,
        }
        let result = CommandResult::new(
            "lanes",
            Outcome::Ok,
            Data {
                partial: true,
                state: "input_required",
            },
        );
        let mut bytes = Vec::new();
        render(&result, RenderMode::Json, &mut bytes)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["data"]["partial"], true);
        assert_eq!(value["status"], "ok");
        Ok(())
    }
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn broken_pipe_only_succeeds_for_human_read_only_results() {
        let result = CommandResult::new("lanes", Outcome::Ok, ());
        assert!(
            render(
                &result,
                RenderMode::Human {
                    text: "lanes",
                    read_only: true,
                    summary: None
                },
                &mut Broken
            )
            .is_ok()
        );
        assert!(matches!(
            render(&result, RenderMode::Json, &mut Broken),
            Err(RenderError::Write(_))
        ));
        assert!(matches!(
            render(
                &result,
                RenderMode::Human {
                    text: "accepted",
                    read_only: false,
                    summary: None
                },
                &mut Broken
            ),
            Err(RenderError::Write(_))
        ));
    }
}

/// Overflow publication is an I/O seam supplied by the composition root. A
/// committed result is passed once; this function never calls its owner again.
pub fn render_or_publish<T: Serialize>(
    result: &CommandResult<T>,
    mode: RenderMode<'_>,
    writer: &mut impl Write,
    stderr: &mut impl Write,
    publish: impl FnOnce(&CommandResult<T>) -> io::Result<provenant_records::AbsolutePath>,
    policy: OverflowPolicy,
) -> Result<Outcome, RenderError> {
    let launch_receipt = result.error.is_some()
        && matches!(
            result.command.as_str(),
            "run" | "dispatch" | "resume" | "submit"
        )
        && match mode {
            RenderMode::Human { text, .. } | RenderMode::Compact { text, .. } => !text.is_empty(),
            RenderMode::Json => false,
        };
    let first = if launch_receipt {
        // Failed launches still own a receipt. Keep it on stdout and the
        // diagnostic on stderr, just as JSON keeps both in its envelope.
        let receipt = CommandResult {
            schema_version: result.schema_version,
            command: result.command.clone(),
            status: result.status,
            data: &result.data,
            error: None,
            warnings: Vec::new(),
            next: Vec::new(),
        };
        (|| {
            let mut receipt_bytes = Vec::new();
            let mut diagnostic_bytes = Vec::new();
            render(&receipt, mode, &mut receipt_bytes)?;
            render(result, mode, &mut diagnostic_bytes)?;
            if receipt_bytes.len().saturating_add(diagnostic_bytes.len()) > MAX_RESULT_BYTES {
                return Err(RenderError::TooLarge);
            }
            // Reuse the renderer's flush and broken-pipe policy after the
            // complete two-stream result has passed the size checks.
            render(&receipt, mode, writer)?;
            render(result, mode, stderr)
        })()
    } else if !matches!(mode, RenderMode::Json) && result.error.is_some() {
        render(result, mode, stderr)
    } else {
        render(result, mode, writer)
    };
    match first {
        Ok(()) => Ok(result.status),
        Err(RenderError::TooLarge) => {
            #[derive(Serialize)]
            struct Overflow {
                artifact: Option<provenant_records::AbsolutePath>,
            }
            let published = publish(result);
            match policy {
                OverflowPolicy::Refuse => {
                    let (code, message, artifact, repair) = match &published {
                        Ok(path) => (
                            "RESULT_TOO_LARGE",
                            "result exceeds 1 MiB; read the published artifact",
                            Some(path.clone()),
                            "Read the published artifact named in data.artifact for the full result; request a smaller page.",
                        ),
                        Err(_) => (
                            "IO",
                            "cannot publish oversized result; the recorded operation must not be repeated",
                            None,
                            "Inspect the artifact publication error, correct the output path or permissions, then rerun the command.",
                        ),
                    };
                    let mut failure =
                        CommandResult::new(&result.command, Outcome::Error, Overflow { artifact });
                    failure.warnings = result.warnings.clone();
                    failure.error = Some(provenant_records::CommandError::new(
                        code, message, None, repair,
                    ));
                    // Human transport includes the pointer using the same typed
                    // path record, escaped as JSON; JSON carries it in data.
                    let human = match &failure.data.artifact {
                        Some(path) => format!(
                            "provenant: {code}: {message}; {}\n",
                            serde_json::to_string(path).map_err(RenderError::Encode)?
                        ),
                        None => format!("provenant: {code}: {message}\n"),
                    };
                    let fallback_mode = match mode {
                        RenderMode::Json => RenderMode::Json,
                        RenderMode::Human { .. } | RenderMode::Compact { .. } => {
                            RenderMode::Human {
                                text: &human,
                                read_only: false,
                                summary: None,
                            }
                        }
                    };
                    if matches!(fallback_mode, RenderMode::Json) {
                        render(&failure, fallback_mode, writer)?;
                    } else {
                        let text_result = CommandResult::new(&result.command, Outcome::Error, ());
                        render(&text_result, fallback_mode, stderr)?;
                    }
                    // A published overflow is handed back so a Refuse caller
                    // exits 3, matching the Error envelope; a failed publication
                    // is already an Error envelope and exits 3 either way.
                    if published.is_ok() {
                        Err(RenderError::TooLarge)
                    } else {
                        Ok(Outcome::Error)
                    }
                }
                OverflowPolicy::Proceed => match published {
                    Ok(path) => {
                        // The complete result is paged once. The pointer keeps
                        // the result's ORIGINAL status so the envelope status
                        // and the exit code agree: a published overflow is not
                        // a command failure.
                        let path_text =
                            serde_json::to_string(&path).map_err(RenderError::Encode)?;
                        // Human transport names the pointer in the same typed
                        // path record; JSON carries it in data.artifact.
                        let human =
                            format!("provenant: result exceeds 1 MiB; paged to {path_text}\n");
                        let fallback_mode = match mode {
                            RenderMode::Json => RenderMode::Json,
                            RenderMode::Human { .. } | RenderMode::Compact { .. } => {
                                RenderMode::Human {
                                    text: &human,
                                    read_only: false,
                                    summary: None,
                                }
                            }
                        };
                        // The paged envelope keeps every warning the reader
                        // already has and the one the page itself raises. Under
                        // --json the cap notice is typed in that envelope, never
                        // written to stderr; human mode routes it through warn.
                        let mut paged = CommandResult::new(
                            &result.command,
                            result.status,
                            Overflow {
                                artifact: Some(path),
                            },
                        );
                        paged.warnings = result.warnings.clone();
                        let notice = Warning::new(
                            RESULT_TOO_LARGE,
                            format!(
                                "[RESULT_TOO_LARGE] {} exceeds 1 MiB (source: COMMAND_RESULT_MAX_BYTES); paged to the owner-published artefact; proceeding",
                                result.command
                            ),
                        )
                        .repair(
                            "Read the published artifact named in data.artifact for the full result; request a smaller page.",
                        );
                        if matches!(mode, RenderMode::Json) {
                            paged.warnings.push(notice);
                        } else {
                            provenant_records::warning::warn(notice);
                        }
                        render(&paged, fallback_mode, writer)?;
                        Ok(result.status)
                    }
                    Err(_) => {
                        // A failed publication is a real IO failure: the
                        // recorded operation must not be repeated and the
                        // command exits 3 with its Error envelope.
                        let mut failure = CommandResult::new(
                            &result.command,
                            Outcome::Error,
                            Overflow { artifact: None },
                        );
                        failure.error = Some(provenant_records::CommandError::new(
                            "IO",
                            "cannot publish oversized result; the recorded operation must not be repeated",
                            None,
                            "Inspect the artifact publication error, correct the output path or permissions, then rerun the command.",
                        ));
                        let human = "provenant: IO: cannot publish oversized result; the recorded operation must not be repeated\n";
                        let fallback_mode = match mode {
                            RenderMode::Json => RenderMode::Json,
                            RenderMode::Human { .. } | RenderMode::Compact { .. } => {
                                RenderMode::Human {
                                    text: human,
                                    read_only: false,
                                    summary: None,
                                }
                            }
                        };
                        if matches!(fallback_mode, RenderMode::Json) {
                            render(&failure, fallback_mode, writer)?;
                        } else {
                            render(&failure, fallback_mode, stderr)?;
                        }
                        Ok(Outcome::Error)
                    }
                },
            }
        }
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod overflow_tests {
    use super::*;

    #[test]
    fn refused_launch_keeps_its_human_receipt_and_error_on_separate_streams()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut result = CommandResult::new("run", Outcome::Refused, ());
        result.error = Some(provenant_records::CommandError::new(
            "REFUSED",
            "both lanes refused",
            None,
            "Resolve both lane rejection reasons, then rerun `provenant run`.",
        ));
        let receipt = "run run-id\nworktree /fixture/lane (branch)\nfirst rejected: disk floor\nsecond rejected: preparation refused\n";
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let status = render_or_publish(
            &result,
            RenderMode::Human {
                text: receipt,
                read_only: false,
                summary: None,
            },
            &mut output,
            &mut errors,
            |_| unreachable!("bounded receipt does not need publication"),
            OverflowPolicy::Refuse,
        )?;
        assert_eq!(status, Outcome::Refused);
        assert_eq!(String::from_utf8(output)?, receipt);
        assert!(String::from_utf8(errors)?.contains("REFUSED: both lanes refused"));

        // Encode the diagnostic before either stream escapes. An oversized
        // error must publish the complete result without a partial receipt.
        result.error.as_mut().ok_or("missing diagnostic")?.message = "x".repeat(MAX_RESULT_BYTES);
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut published = 0;
        let outcome = render_or_publish(
            &result,
            RenderMode::Human {
                text: receipt,
                read_only: false,
                summary: None,
            },
            &mut output,
            &mut errors,
            |_| {
                published += 1;
                provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/result.json"))
                    .map_err(io::Error::other)
            },
            OverflowPolicy::Refuse,
        );
        assert!(matches!(outcome, Err(RenderError::TooLarge)), "{outcome:?}");
        assert_eq!(published, 1);
        assert!(output.is_empty());
        assert!(String::from_utf8(errors)?.contains("RESULT_TOO_LARGE"));
        Ok(())
    }

    #[test]
    fn oversize_publishes_once_or_reports_io_without_partial_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = CommandResult::new("dispatch", Outcome::Ok, "x".repeat(MAX_RESULT_BYTES));
        let mut output = Vec::new();
        let mut published = 0;
        let outcome = render_or_publish(
            &result,
            RenderMode::Json,
            &mut output,
            &mut Vec::new(),
            |_| {
                published += 1;
                provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/result.json"))
                    .map_err(io::Error::other)
            },
            OverflowPolicy::Refuse,
        );
        assert!(matches!(outcome, Err(RenderError::TooLarge)), "{outcome:?}");
        assert_eq!(published, 1);
        let value: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(value["error"]["code"], "RESULT_TOO_LARGE");
        assert!(value["data"]["artifact"].is_object());
        output.clear();
        render_or_publish(
            &result,
            RenderMode::Json,
            &mut output,
            &mut Vec::new(),
            |_| Err(io::ErrorKind::StorageFull.into()),
            OverflowPolicy::Refuse,
        )?;
        let value: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(value["error"]["code"], "IO");
        assert!(value["data"]["artifact"].is_null());
        Ok(())
    }

    #[test]
    fn published_overflow_proceeds_with_the_envelope_status()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = CommandResult::new("dispatch", Outcome::Ok, "x".repeat(MAX_RESULT_BYTES));
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let outcome = render_or_publish(
            &result,
            RenderMode::Json,
            &mut output,
            &mut errors,
            |_| {
                provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/result.json"))
                    .map_err(io::Error::other)
            },
            OverflowPolicy::Proceed,
        );
        // The paged envelope keeps `ok`; the caller exits with this same status
        // so a script's exit code can never disagree with its envelope.
        assert!(matches!(outcome, Ok(Outcome::Ok)), "{outcome:?}");
        let value: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(value["status"], "ok");
        assert!(value["error"].is_null());
        assert!(value["data"]["artifact"].is_object());
        assert!(
            value["warnings"]
                .as_array()
                .ok_or("warnings")?
                .iter()
                .any(|warning| warning["code"] == "RESULT_TOO_LARGE"),
            "{value}"
        );
        let stderr = String::from_utf8(errors)?;
        assert!(stderr.is_empty(), "{stderr}");
        Ok(())
    }
}
