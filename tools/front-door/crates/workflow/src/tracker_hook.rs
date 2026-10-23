//! Claude Code tracker hook policy: detect raw `gh` writes and suggest the
//! repository's declared tracker command.

use provenant_records::checked_json;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::{io, path::Path};
use thiserror::Error;

/// Repository lookup is the input seam for this hook; tests can supply a
/// declaration without changing process-global paths.
pub trait TrackerHookFileSystem {
    fn read_maintaining(&self, repo: &Path) -> io::Result<String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OsTrackerHookFileSystem;

impl TrackerHookFileSystem for OsTrackerHookFileSystem {
    fn read_maintaining(&self, repo: &Path) -> io::Result<String> {
        let raw = std::fs::read(repo.join("MAINTAINING.md"))?;
        let content = String::from_utf8(raw)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(content.replace("\r\n", "\n").replace('\r', "\n"))
    }
}

#[derive(Debug, Error)]
pub enum TrackerHookError {
    #[error("invalid hook event JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    NumberOutOfRange(#[from] checked_json::NumberOutOfRange),
    #[error("hook event field {field} must be {expected}")]
    InvalidField {
        field: &'static str,
        expected: &'static str,
    },
    #[error("invalid repository tracker declaration: {0}")]
    InvalidDeclaration(String),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookSpecificOutput {
    pub hook_event_name: &'static str,
    pub permission_decision: &'static str,
    pub permission_decision_reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HookDecision {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: HookSpecificOutput,
}

impl HookDecision {
    pub fn wire_json(&self) -> Result<String, serde_json::Error> {
        let compact = serde_json::to_string(self)?;
        Ok(python_default_spacing(&ensure_ascii_json(&compact)))
    }
}

fn ensure_ascii_json(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii() {
            escaped.push(character);
            continue;
        }
        let codepoint = u32::from(character);
        if codepoint <= 0xffff {
            escaped.push_str(&format!("\\u{codepoint:04x}"));
        } else {
            let shifted = codepoint - 0x1_0000;
            let high = 0xd800 + (shifted >> 10);
            let low = 0xdc00 + (shifted & 0x3ff);
            escaped.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
        }
    }
    escaped
}

fn python_default_spacing(text: &str) -> String {
    let mut spaced = String::with_capacity(text.len() + 8);
    let mut in_string = false;
    let mut escaped = false;
    for character in text.chars() {
        spaced.push(character);
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
        } else if matches!(character, ',' | ':') {
            spaced.push(' ');
        }
    }
    spaced
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LexState {
    Plain,
    SingleQuoted,
    DoubleQuoted,
}

fn punctuation(c: char) -> bool {
    matches!(c, ';' | '&' | '|')
}

/// POSIX tokenisation matching the source's `shlex` punctuation mode. It
/// handles shell quoting and escapes but deliberately does not execute shell
/// syntax or expand variables.
fn shell_tokens(command: &str) -> Result<Vec<String>, ()> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut state = LexState::Plain;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match state {
            LexState::SingleQuoted => {
                if c == '\'' {
                    state = LexState::Plain;
                } else {
                    word.push(c);
                }
            }
            LexState::DoubleQuoted => match c {
                '"' => state = LexState::Plain,
                '\\' => match chars.peek().copied() {
                    Some(next @ ('$' | '`' | '"' | '\\')) => {
                        chars.next();
                        word.push(next);
                    }
                    Some('\n') => {
                        chars.next();
                    }
                    Some(next) => {
                        word.push('\\');
                        word.push(next);
                        chars.next();
                    }
                    None => return Err(()),
                },
                _ => word.push(c),
            },
            LexState::Plain => match c {
                '\'' => {
                    started = true;
                    state = LexState::SingleQuoted;
                }
                '"' => {
                    started = true;
                    state = LexState::DoubleQuoted;
                }
                '\\' => match chars.next() {
                    Some('\n') => {}
                    Some(next) => {
                        started = true;
                        word.push(next);
                    }
                    None => return Err(()),
                },
                '#' => {
                    if started {
                        tokens.push(std::mem::take(&mut word));
                        started = false;
                    }
                    for rest in chars.by_ref() {
                        if rest == '\n' {
                            break;
                        }
                    }
                }
                ' ' | '\t' | '\r' | '\n' => {
                    if started {
                        tokens.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                c if punctuation(c) => {
                    if started {
                        tokens.push(std::mem::take(&mut word));
                        started = false;
                    }
                    let mut token = String::from(c);
                    while chars.peek().is_some_and(|next| punctuation(*next)) {
                        if let Some(next) = chars.next() {
                            token.push(next);
                        }
                    }
                    tokens.push(token);
                }
                _ => {
                    started = true;
                    word.push(c);
                }
            },
        }
    }
    if state != LexState::Plain {
        return Err(());
    }
    if started {
        tokens.push(word);
    }
    Ok(tokens)
}

fn separator(token: &str) -> bool {
    matches!(token, ";" | "&&" | "||" | "|" | "&")
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

pub fn raw_tracker_write(command: &str) -> bool {
    let Ok(tokens) = shell_tokens(command) else {
        return false;
    };
    let mut segment = Vec::<String>::new();
    for token in tokens.into_iter().chain(std::iter::once(";".to_owned())) {
        if separator(&token) {
            let mut parts = segment.clone();
            segment.clear();
            if parts.first().is_some_and(|part| part == "env") {
                parts.remove(0);
            }
            while parts.first().is_some_and(|part| is_assignment(part)) {
                parts.remove(0);
            }
            if parts.first().is_none_or(|part| part != "gh") {
                continue;
            }
            parts.remove(0);
            while parts.len() > 1
                && matches!(
                    parts.first().map(String::as_str),
                    Some("-R" | "--repo" | "--hostname")
                )
            {
                parts.drain(..2);
            }
            if parts.first().is_some_and(|part| part == "issue")
                && parts
                    .get(1)
                    .is_some_and(|verb| matches!(verb.as_str(), "create" | "edit" | "close"))
            {
                return true;
            }
            if parts.first().is_some_and(|part| part == "project")
                && parts.get(1).is_some_and(|verb| {
                    matches!(
                        verb.as_str(),
                        "item-add" | "item-archive" | "item-create" | "item-edit" | "item-delete"
                    )
                })
            {
                return true;
            }
        } else {
            segment.push(token);
        }
    }
    false
}

pub fn tracker_command(
    repo: &Path,
    fs: &impl TrackerHookFileSystem,
) -> Result<Option<String>, TrackerHookError> {
    let content = match fs.read_maintaining(repo) {
        Ok(content) => content,
        Err(error) if error.kind() != io::ErrorKind::InvalidData => return Ok(None),
        Err(error) => return Err(TrackerHookError::InvalidDeclaration(error.to_string())),
    };
    let Some((_, section)) = content.split_once("### Tracker\n") else {
        return Ok(None);
    };
    let section = section.split("\n### ").next().unwrap_or_default();
    let pattern = Regex::new(r"(?m)^- Command: `([^`]+)`\s*$")
        .map_err(|error| TrackerHookError::InvalidDeclaration(error.to_string()))?;
    let Some(command) = pattern
        .captures(section)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str())
    else {
        return Ok(None);
    };
    Ok((command != "none").then(|| command.to_owned()))
}

/// Apply the hook policy to one decoded Claude event. Missing command text is
/// treated as an empty command, as in the Python hook.
pub fn evaluate_event(
    event: &Value,
    repo: &Path,
    fs: &impl TrackerHookFileSystem,
) -> Result<Option<HookDecision>, TrackerHookError> {
    let Some(event) = event.as_object() else {
        return Err(TrackerHookError::InvalidField {
            field: "event",
            expected: "an object",
        });
    };
    if event.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return Ok(None);
    }
    let tool_input = event.get("tool_input");
    let command = match tool_input {
        None => "",
        Some(Value::Object(input)) => match input.get("command") {
            None => "",
            Some(Value::String(command)) => command,
            Some(_) => {
                return Err(TrackerHookError::InvalidField {
                    field: "tool_input.command",
                    expected: "a string",
                });
            }
        },
        Some(_) => {
            return Err(TrackerHookError::InvalidField {
                field: "tool_input",
                expected: "an object",
            });
        }
    };
    if !raw_tracker_write(command) {
        return Ok(None);
    }
    let Some(declared) = tracker_command(repo, fs)? else {
        return Ok(None);
    };
    Ok(Some(HookDecision {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PreToolUse",
            permission_decision: "deny",
            permission_decision_reason: format!("Use the repository tracker command: {declared}"),
        },
    }))
}

pub fn evaluate_json(
    raw: &str,
    repo: &Path,
    fs: &impl TrackerHookFileSystem,
) -> Result<Option<HookDecision>, TrackerHookError> {
    let event: Value = match checked_json::from_str(raw) {
        Ok(event) => event,
        Err(checked_json::DecodeError::Json(error)) => return Err(error.into()),
        Err(checked_json::DecodeError::NumberOutOfRange(error)) => return Err(error.into()),
    };
    evaluate_event(&event, repo, fs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn oracle(cases: Value) -> Option<Value> {
        crate::test_support::oracle_script("oracle_session.py", "tracker_hook", &cases)
    }

    #[test]
    fn detects_only_raw_tracker_writes_after_shell_quoting_and_separators() {
        let positive = [
            "gh issue create -t bug",
            "gh issue edit 12 --title fixed",
            "gh issue close 12",
            "gh project item-add 2 --url x",
            "gh project item-archive 2 --id x",
            "gh project item-create 2 --title x",
            "gh project item-edit --id x",
            "gh project item-delete 2 --id x",
            "cd repo && gh -R owner/repo issue create -t bug",
            "env FOO=bar gh --repo owner/repo project item-edit --id 3",
            "echo ok; gh issue create",
            "gh issue create 2>&1",
            "gh issue create # ignored",
            "gh issue create#note",
            "gh issue create#note; echo ok",
        ];
        let negative = [
            "gh issue list",
            "gh issue view 12",
            "gh project item-list 2",
            "pnpm issue create",
            "echo 'gh issue create'",
            "git status",
            "gh issue createx",
            "gh issue 'create",
            "echo ok# harmless; gh issue close",
        ];
        for command in positive {
            assert!(raw_tracker_write(command), "{command}");
        }
        for command in negative {
            assert!(!raw_tracker_write(command), "{command}");
        }
        if let Some(expected) =
            oracle(json!([{"op":"commands","positive":positive,"negative":negative}]))
        {
            assert_eq!(
                expected[0]["positive"],
                json!(positive.map(raw_tracker_write))
            );
            assert_eq!(
                expected[0]["negative"],
                json!(negative.map(raw_tracker_write))
            );
        }
    }

    #[test]
    fn declared_command_controls_the_exact_hook_response() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(
            root.join("MAINTAINING.md"),
            "### Tracker\n- Choice: `github-issues`\n- Command: `pnpm issue`\n\n### Scope and stories\n",
        )
        .unwrap();
        let fs = OsTrackerHookFileSystem;
        let event = json!({"tool_name":"Bash","tool_input":{"command":"gh issue create -t bug"}});
        let actual = evaluate_event(&event, &root, &fs).unwrap().unwrap();
        if let Some(expected) = oracle(json!([{"op":"evaluate","repo":root,"event":event}])) {
            assert_eq!(json!(actual), expected[0]);
        }
        if let Some(expected) = oracle(json!([{
            "op":"evaluate_wire",
            "repo":root,
            "event":event
        }])) {
            assert_eq!(format!("{}\n", actual.wire_json().unwrap()), expected[0]);
        }
        std::fs::write(
            root.join("MAINTAINING.md"),
            "### Tracker\n- Command: `pnpm issue café 🦀`\n",
        )
        .unwrap();
        let unicode_actual = evaluate_event(&event, &root, &fs).unwrap().unwrap();
        let unicode_wire = unicode_actual.wire_json().unwrap();
        assert!(unicode_wire.contains("\\u00e9"));
        assert!(unicode_wire.contains("\\ud83e\\udd80"));
        if let Some(expected) = oracle(json!([{
            "op":"evaluate_wire",
            "repo":root,
            "event":event
        }])) {
            assert_eq!(format!("{unicode_wire}\n"), expected[0].as_str().unwrap());
        }
        std::fs::write(
            root.join("MAINTAINING.md"),
            "### Tracker\n- Command: `none`\n",
        )
        .unwrap();
        assert!(evaluate_event(&event, &root, &fs).unwrap().is_none());
        assert!(evaluate_event(&json!({"tool_name":"Read"}), &root, &fs)
            .unwrap()
            .is_none());
    }
}
