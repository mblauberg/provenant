//! Shared recording transforms, also exercised by the provider library tests.
use serde_json::Value;
use std::error::Error;
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub const SHELL_PROOF_COMMAND: &str = r#"test -z "${CLAUDE_CODE_OAUTH_TOKEN+x}" && test -z "${CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR+x}" && printf provenant-shell-proof"#;

/// Apply the configured egress policy to fixture exports.
pub struct Redactor {
    projector: provenant_confinement::publication::Projector,
}
impl Redactor {
    pub fn new(token: Option<&crate::claude::OauthToken>) -> Result<Self> {
        let mut secrets: Vec<_> = std::env::vars_os()
            .filter(|(key, value)| {
                key.to_str()
                    .is_some_and(provenant_confinement::publication::secret_field)
                    && !value.is_empty()
            })
            .map(|(_, value)| value.as_encoded_bytes().to_vec())
            .collect();
        if let Some(token) = token {
            secrets.push(token.access.as_bytes().to_vec());
        }
        Ok(Self {
            projector: provenant_confinement::publication::Projector::new(secrets)?,
        })
    }

    pub fn text(&self, value: &str) -> Result<String> {
        let projection = self.projector.project(
            provenant_confinement::publication::Destination::FixtureExport,
            value.as_bytes(),
        )?;
        Ok(String::from_utf8(provenant_redact::egress_shapes(
            projection.bytes(),
        )?)?)
    }

    pub fn value(&self, value: &mut Value) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let projection = self.projector.json(
            provenant_confinement::publication::Destination::FixtureExport,
            &bytes,
        )?;
        *value = serde_json::from_slice(projection.bytes())?;
        Self::shape_values(value, false)?;
        Ok(())
    }

    fn shape_values(value: &mut Value, metadata: bool) -> Result<()> {
        match value {
            Value::String(text) if !metadata => {
                *text = String::from_utf8(provenant_redact::egress_shapes(text.as_bytes())?)?
            }
            Value::Array(items) => {
                for item in items {
                    Self::shape_values(item, metadata)?;
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    Self::shape_values(
                        item,
                        provenant_confinement::publication::metadata_field(key),
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn stream(&self, bytes: &[u8]) -> Result<String> {
        let values: Vec<Value> = std::str::from_utf8(bytes)?
            .lines()
            .filter(|line| !line.is_empty())
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        let mut values = Value::Array(values);
        self.value(&mut values)?;
        let mut out = String::new();
        for value in values
            .as_array()
            .ok_or("fixture projection is not an array")?
        {
            out.push_str(&serde_json::to_string(value)?);
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
    if provenant_records::AdapterSpec::get(adapter).is_some_and(|spec| {
        spec.capabilities.answering_model.value == provenant_records::AnsweringModelSource::Rollout
    }) {
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
    fn recording_preserves_metadata_and_masks_credential_prose() {
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let redactor = Redactor {
            projector: provenant_confinement::publication::Projector::new([token
                .as_bytes()
                .to_vec()])
            .unwrap(),
        };
        let mut value = json!({"model":token,"account_key":token,"message":token,"source":{"token":"fixture-secret-value","name":[{"message":token}]},"paths":["ordinary",token,[token,{"token":"fixture-nested-secret"}]]});
        redactor.value(&mut value).unwrap();
        assert_eq!(value["model"], token);
        assert_eq!(value["account_key"], token);
        assert_eq!(value["message"], "[REDACTED]");
        assert_eq!(value["source"]["token"], "[REDACTED]");
        assert_eq!(value["source"]["name"][0]["message"], "[REDACTED]");
        assert_eq!(value["paths"][0], "ordinary");
        assert_eq!(value["paths"][1], token);
        assert_eq!(value["paths"][2][0], token);
        assert_eq!(value["paths"][2][1]["token"], "[REDACTED]");
    }

    #[test]
    fn recording_redacts_known_tokens_in_argv_zero() {
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
        let redactor = Redactor {
            projector: provenant_confinement::publication::Projector::new([
                b"ghp_abcdefghijklmnopqrst".to_vec(),
            ])
            .unwrap(),
        };
        let argv = ["/tools/ghp_abcdefghijklmnopqrst", "--help"];
        let recorded: Vec<_> = argv.iter().map(|arg| redactor.text(arg).unwrap()).collect();
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
