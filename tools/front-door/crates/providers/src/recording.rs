//! Shared recording transforms, also exercised by the provider library tests.
use serde_json::Value;
use std::error::Error;
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub const SHELL_PROOF_COMMAND: &str = r#"test -z "${CLAUDE_CODE_OAUTH_TOKEN+x}" && test -z "${CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR+x}" && printf provenant-shell-proof"#;

/// Replace session ids, local paths, emails and token-like strings.
pub struct Redactor {
    pub literals: Vec<(String, String)>,
}
impl Redactor {
    pub fn text(&self, value: &str) -> String {
        // Apply the capture redactor to every value, including executable paths.
        let mut out = match provenant_confinement::redact::known_shapes(value.as_bytes()) {
            Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| "[REDACTED]".into()),
            Err(_) => "[REDACTED]".into(),
        };
        for (from, to) in &self.literals {
            out = out.replace(from.as_str(), to);
        }
        out.split_inclusive(|c: char| c.is_whitespace() || "\"'(),;<>[]{}".contains(c))
            .map(|piece| {
                let token = piece
                    .trim_end_matches(|c: char| c.is_whitespace() || "\"'(),;<>[]{}".contains(c));
                let tail = &piece[token.len()..];
                let class = if !token.contains('/')
                    && token
                        .split_once('@')
                        .is_some_and(|(_, domain)| domain.contains('.'))
                {
                    Some("FIXTURE_EMAIL")
                } else if (token.len() == 36 && token.chars().filter(|c| *c == '-').count() == 4)
                    || token.split_once('_').is_some_and(|(prefix, id)| {
                        prefix.chars().all(|c| c.is_ascii_lowercase())
                            && id.len() >= 16
                            && id.chars().all(|c| c.is_ascii_alphanumeric())
                    })
                {
                    Some("FIXTURE_IDENTIFIER")
                } else if token.len() >= 32
                    && !token.starts_with(['/', '-'])
                    && token.chars().filter(char::is_ascii_digit).count() >= 4
                    && token
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_+/=.".contains(c))
                {
                    Some("FIXTURE_TOKEN")
                } else {
                    None
                };
                match class {
                    Some(class) => format!("{class}{tail}"),
                    None => piece.to_owned(),
                }
            })
            .collect()
    }
    pub fn value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.text(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.value(item)),
            Value::Object(map) => {
                let entries = std::mem::take(map);
                for (key, mut item) in entries {
                    self.value(&mut item);
                    map.insert(self.text(&key), item);
                }
            }
            _ => {}
        }
    }
    pub fn stream(&self, bytes: &[u8]) -> Result<String> {
        let mut out = String::new();
        for line in String::from_utf8(bytes.to_vec())?
            .lines()
            .filter(|line| !line.is_empty())
        {
            let mut value: Value = serde_json::from_str(line)?;
            self.value(&mut value);
            out.push_str(&serde_json::to_string(&value)?);
            out.push('\n');
        }
        Ok(out)
    }
}

pub fn successful_bash(stream: &[u8]) -> bool {
    let mut requests = std::collections::BTreeSet::new();
    for event in stream
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
    {
        let Some(content) = event
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for item in content {
            match (
                event.get("type").and_then(Value::as_str),
                item.get("type").and_then(Value::as_str),
            ) {
                (Some("assistant"), Some("tool_use"))
                    if item.get("name").and_then(Value::as_str) == Some("Bash")
                        && item
                            .get("input")
                            .and_then(|input| input.get("command"))
                            .and_then(Value::as_str)
                            .is_some_and(|command| command == SHELL_PROOF_COMMAND) =>
                {
                    if let Some(id) = item.get("id").and_then(Value::as_str) {
                        requests.insert(id.to_owned());
                    }
                }
                (Some("user"), Some("tool_result"))
                    if item.get("is_error").and_then(Value::as_bool) != Some(true)
                        && item
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| requests.contains(id))
                        && item
                            .get("content")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.trim() == "provenant-shell-proof") =>
                {
                    return true;
                }
                _ => {}
            }
        }
    }
    false
}

/// Codex streams carry no answering model: only its private rollout under the
/// recording's `CODEX_HOME` can establish one. Other adapters report it in-stream.
pub fn answering_model(
    adapter: &str,
    session: Option<&str>,
    stream_model: Option<&str>,
    codex_home: &Path,
) -> Option<String> {
    if adapter == "codex" {
        session
            .and_then(|thread| crate::codex_rollout::observe(codex_home, thread).ok())
            .map(|turn| turn.model)
    } else {
        stream_model.map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recording_redacts_known_tokens_in_argv_zero() {
        let redactor = Redactor { literals: vec![] };
        let argv = ["/tools/ghp_abcdefghijklmnopqrst", "--help"];
        let recorded: Vec<_> = argv.iter().map(|arg| redactor.text(arg)).collect();
        assert_eq!(recorded, ["/tools/[REDACTED]", "--help"]);
    }
    #[test]
    fn codex_answering_model_comes_from_the_private_rollout() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        let day = home.join("sessions/2026/10/24");
        std::fs::create_dir_all(&day).unwrap();
        let thread = "01234567-89ab-cdef-0123-456789abcdef";
        std::fs::write(
            day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
            format!(
                "{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":thread}}),
                json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
            ),
        )
        .unwrap();
        let observed = answering_model("codex", Some(thread), None, home);
        let unbound = answering_model("codex", None, Some("argv-model"), home);
        let claude = answering_model("claude", None, Some("claude-sonnet-5"), home);
        assert_eq!(observed.as_deref(), Some("gpt-6-luna"));
        assert_eq!(unbound, None);
        assert_eq!(claude.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn bash_proof_needs_a_matching_successful_tool_result() {
        let request = json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":SHELL_PROOF_COMMAND}}]}}).to_string();
        let unchecked = json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"printf provenant-shell-proof"}}]}}).to_string();
        let result = |id: &str, error: bool, output: &str| {
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"is_error":error,"content":output}]}}).to_string()
        };
        assert!(!successful_bash(request.as_bytes()));
        assert!(!successful_bash(
            format!(
                "{unchecked}\n{}",
                result("b", false, "provenant-shell-proof")
            )
            .as_bytes()
        ));
        for (id, error, output) in [
            ("other", false, "provenant-shell-proof"),
            ("b", true, "provenant-shell-proof"),
            ("b", false, "permission denied"),
        ] {
            assert!(!successful_bash(
                format!("{request}\n{}", result(id, error, output)).as_bytes()
            ));
        }
        assert!(successful_bash(
            format!("{request}\n{}", result("b", false, "provenant-shell-proof")).as_bytes()
        ));
    }
}
