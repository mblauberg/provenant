//! Encode completely before delivery; never clip a result or repeat an owner call.
use crate::limits;
pub use crate::limits::MAX_RESULT_BYTES;
use provenant_records::{CommandResult, Outcome};
use serde::Serialize;
use std::io::{self, Write};
use thiserror::Error;

/// The compact view an owner supplies for the default human output: a headline
/// of counts, then the rows that need action first, and the one typed next
/// command. The generic truncator is only a fallback for owners without one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub lines: Vec<String>,
    pub next: Option<String>,
    /// Rows the owner held back; the renderer says so and names `--full`.
    pub omitted: usize,
}
impl Summary {
    /// A headline, then `rows` in the owner's priority order, bounded by the
    /// shared row limits. Rows past the limit are counted, not dropped silently.
    pub fn new(headline: impl Into<String>, rows: Vec<String>, next: Option<String>) -> Self {
        let omitted = rows.len().saturating_sub(limits::SUMMARY_ROWS);
        let lines = std::iter::once(headline.into())
            .chain(
                rows.iter()
                    .take(limits::SUMMARY_ROWS)
                    .map(|row| bounded_text(row, limits::SUMMARY_ROW_BYTES)),
            )
            .collect();
        Self {
            lines,
            next,
            omitted,
        }
    }
    /// Text the owner has already bounded; shown as it is.
    pub fn verbatim(text: &str, next: Option<String>) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
            next,
            omitted: 0,
        }
    }
    /// Only a next command; the body falls back to the generic truncator.
    pub fn next_only(next: Option<String>) -> Self {
        Self {
            next,
            ..Self::default()
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
    if text.len() <= max {
        return text.into();
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
        tail += cut + 1;
    }
    let tail = tail.min(text.len());
    format!(
        "{}[... {} bytes omitted; use --full for all of it ...]\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
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
    if text.len() <= limits::FALLBACK_BYTES {
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
    let mut out = match summary.filter(|summary| !summary.lines.is_empty()) {
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
    };
    if let Some(next) = summary.and_then(|summary| summary.next.as_deref()) {
        out.push_str(&format!("next: {next}\n"));
    }
    out
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
    match outcome {
        Outcome::Ok => 0,
        Outcome::Failed | Outcome::Refused => 1,
        Outcome::Invalid => 2,
        Outcome::Error => 3,
        Outcome::Unavailable => 4,
        Outcome::Pending => 5,
        Outcome::Waiting => 0,
        Outcome::Timeout => 124,
        Outcome::Cancelled => 130,
    }
}
/// At most cap bytes are retained even if an owner's Serialize floods output.
struct Buffer {
    bytes: Vec<u8>,
    overflow: bool,
}
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RESULT_BYTES.saturating_sub(self.bytes.len()) {
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
    let mut error = result.error.clone();
    if let Some(error) = &mut error {
        error.repair.get_or_insert_with(|| {
            let command = match result.command.as_str() {
                "parse" | "help" => "--all",
                command => command,
            };
            format!("run `provenant help {command}`")
        });
    }
    let result = CommandResult {
        schema_version: result.schema_version,
        command: result.command.clone(),
        status: result.status,
        data: &result.data,
        error,
    };
    let mut buffer = Buffer {
        bytes: Vec::new(),
        overflow: false,
    };
    let encoded = match mode {
        RenderMode::Json => serde_json::to_writer(&mut buffer, &result)
            .map_err(RenderError::Encode)
            .and_then(|()| buffer.write_all(b"\n").map_err(RenderError::Write)),
        RenderMode::Human { .. } | RenderMode::Compact { .. } => {
            let text = match &result.error {
                Some(error) => format!(
                    "provenant: {}: {}\nnext: {}\n",
                    error.code,
                    error.message,
                    error.repair.as_deref().unwrap_or_default()
                ),
                None => match mode {
                    RenderMode::Compact { text, summary, .. } => {
                        compact_text(&result.command, text, summary)
                    }
                    RenderMode::Human { text, summary, .. } => {
                        let mut full = text.to_owned();
                        if let Some(next) = summary.and_then(|summary| summary.next.as_deref()) {
                            if !full.is_empty() && !full.ends_with('\n') {
                                full.push('\n');
                            }
                            full.push_str(&format!("next: {next}\n"));
                        }
                        full
                    }
                    RenderMode::Json => String::new(),
                },
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
/// Characters that could hide or reorder text on a terminal: DEL, C1 controls
/// and the Cf, Zl and Zp categories.
fn is_unsafe(c: char) -> bool {
    matches!(c,
        '\u{7f}'..='\u{9f}' | '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}'
        | '\u{70f}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
        | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
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
            c if c.is_control() || is_unsafe(c) => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
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
    fn compact_uses_the_owner_summary_and_one_typed_next() -> Result<(), Box<dyn std::error::Error>>
    {
        let text = "detail\n".repeat(500);
        let summary = Summary::new(
            "3 lanes: 1 failed, 2 running",
            vec!["aaaa failed".into(), "bbbb running".into()],
            Some("provenant output aaaa".into()),
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
        assert_eq!(
            String::from_utf8(bytes)?,
            "3 lanes: 1 failed, 2 running\naaaa failed\nbbbb running\nnext: provenant output aaaa\n"
        );
        Ok(())
    }
    #[test]
    fn summary_rows_past_the_limit_are_counted() -> Result<(), Box<dyn std::error::Error>> {
        let rows = (0..limits::SUMMARY_ROWS + 5)
            .map(|n| format!("row {n}"))
            .collect();
        let summary = Summary::new("head", rows, None);
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
    fn full_shows_the_owner_text_then_the_typed_next() -> Result<(), Box<dyn std::error::Error>> {
        let summary = Summary::next_only(Some("provenant output aaaa".into()));
        let result = CommandResult::new("lanes", Outcome::Ok, ());
        let mut bytes = Vec::new();
        render(
            &result,
            RenderMode::Human {
                text: "body\n",
                read_only: true,
                summary: Some(&summary),
            },
            &mut bytes,
        )?;
        assert_eq!(
            String::from_utf8(bytes)?,
            "body\nnext: provenant output aaaa\n"
        );
        Ok(())
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
) -> Result<Outcome, RenderError> {
    let first = if !matches!(mode, RenderMode::Json) && result.error.is_some() {
        render(result, mode, stderr)
    } else {
        render(result, mode, writer)
    };
    match first {
        Ok(()) => Ok(result.status),
        Err(RenderError::TooLarge) => {
            let (code, message, artifact) = match publish(result) {
                Ok(path) => (
                    "RESULT_TOO_LARGE",
                    "result exceeds 1 MiB; read the published artifact",
                    Some(path),
                ),
                Err(_) => (
                    "IO",
                    "cannot publish oversized result; the recorded operation must not be repeated",
                    None,
                ),
            };
            #[derive(Serialize)]
            struct Overflow {
                artifact: Option<provenant_records::AbsolutePath>,
            }
            let mut failure =
                CommandResult::new(&result.command, Outcome::Error, Overflow { artifact });
            failure.error = Some(provenant_records::CommandError {
                code: code.into(),
                message: message.into(),
                cause: None,
                repair: None,
            });
            // Human transport includes the pointer using the same typed path
            // record, escaped as JSON. JSON carries it in data.artifact.
            let human = match &failure.data.artifact {
                Some(path) => format!(
                    "provenant: {code}: {message}; {}\n",
                    serde_json::to_string(path).map_err(RenderError::Encode)?
                ),
                None => format!("provenant: {code}: {message}\n"),
            };
            let fallback_mode = match mode {
                RenderMode::Json => RenderMode::Json,
                RenderMode::Human { .. } | RenderMode::Compact { .. } => RenderMode::Human {
                    text: &human,
                    read_only: false,
                    summary: None,
                },
            };
            if matches!(fallback_mode, RenderMode::Json) {
                render(&failure, fallback_mode, writer)?;
            } else {
                let text_result = CommandResult::new(&result.command, Outcome::Error, ());
                render(&text_result, fallback_mode, stderr)?;
            }
            Ok(Outcome::Error)
        }
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod overflow_tests {
    use super::*;
    #[test]
    fn oversize_publishes_once_or_reports_io_without_partial_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = CommandResult::new("dispatch", Outcome::Ok, "x".repeat(MAX_RESULT_BYTES));
        let mut output = Vec::new();
        let mut published = 0;
        let status = render_or_publish(
            &result,
            RenderMode::Json,
            &mut output,
            &mut Vec::new(),
            |_| {
                published += 1;
                provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/result.json"))
                    .map_err(io::Error::other)
            },
        )?;
        assert_eq!(status, Outcome::Error);
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
        )?;
        let value: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(value["error"]["code"], "IO");
        assert!(value["data"]["artifact"].is_null());
        Ok(())
    }
}
