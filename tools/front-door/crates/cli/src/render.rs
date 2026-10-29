//! Encode completely before delivery; never clip a result or repeat an owner call.
use provenant_records::{CommandResult, Outcome};
use serde::Serialize;
use std::io::{self, Write};
use thiserror::Error;
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy)]
pub enum RenderMode<'a> {
    Json,
    Human { text: &'a str, read_only: bool },
    Compact { text: &'a str, read_only: bool },
}
impl crate::Globals {
    /// JSON is an independent transport; --full only affects human output.
    pub fn render_mode<'a>(&self, text: &'a str, read_only: bool) -> RenderMode<'a> {
        if self.json {
            RenderMode::Json
        } else if self.full {
            RenderMode::Human { text, read_only }
        } else {
            RenderMode::Compact { text, read_only }
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

/// Shared human envelope: at most eight rows, 2 KiB including the next step.
/// Owners retain their complete data and human text for --json and --full.
pub fn compact_text(command: &str, text: &str) -> String {
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let next = ["next: ", "keep waiting: ", "more: "]
        .into_iter()
        .find_map(|prefix| {
            lines.iter().rev().find_map(|line| {
                line.trim()
                    .strip_prefix(prefix)
                    .filter(|next| next.len() <= 384)
            })
        });
    let mut out = String::new();
    let row_bytes = if text.len() <= 1536 { 1536 } else { 200 };
    for line in lines.iter().take(6) {
        if !line.trim().starts_with("next: ") {
            out.push_str(&bounded_text(line, row_bytes));
            out.push('\n');
        }
    }
    if lines.len() > 6 || lines.iter().any(|line| line.len() > row_bytes) {
        out.push_str(&format!(
            "({} lines; use --full for all detail)\n",
            lines.len()
        ));
    }
    if out.is_empty() {
        out.push_str(&format!("{command}: done\n"));
    }
    out.push_str("next: ");
    if let Some(next) = next {
        out.push_str(next);
    } else {
        out.push_str("provenant help ");
        out.push_str(if command == "parse" { "--all" } else { command });
    }
    out.push('\n');
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
        RenderMode::Human { text, .. } | RenderMode::Compact { text, .. } => {
            let error_text = result.error.as_ref().map(|error| {
                format!(
                    "provenant: {}: {}\nnext: {}\n",
                    error.code,
                    error.message,
                    error.repair.as_deref().unwrap_or_default()
                )
            });
            let text = error_text.as_deref().unwrap_or(text);
            let compacted;
            let text = if matches!(mode, RenderMode::Compact { .. }) {
                compacted = compact_text(&result.command, text);
                compacted.as_str()
            } else {
                text
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
    fn compact_run_preserves_the_wait_command_after_omitted_rows() {
        let text = format!(
            "{}keep waiting: provenant lanes --id 11111111 --wait 4m\nmore: provenant output 11111111\n",
            "detail\n".repeat(100)
        );
        let compact = compact_text("run", &text);
        assert!(
            compact.ends_with("next: provenant lanes --id 11111111 --wait 4m\n"),
            "{compact}"
        );
        assert!(compact.len() <= 2048);
    }
    #[test]
    fn compact_results_are_bounded_and_always_give_a_next_command()
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
            },
            &mut bytes,
        )?;
        assert!(bytes.len() <= 2048, "{} bytes", bytes.len());
        let compact = String::from_utf8(bytes)?;
        assert!(compact.contains("row 0"));
        assert!(compact.contains("--full"));
        assert!(compact.contains("next: provenant help route list"));
        let mut full = Vec::new();
        render(
            &result,
            RenderMode::Human {
                text: &text,
                read_only: true,
            },
            &mut full,
        )?;
        assert_eq!(full, text.as_bytes());
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
                    read_only: true
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
                    read_only: true
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
                    read_only: true
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
                    read_only: false
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
