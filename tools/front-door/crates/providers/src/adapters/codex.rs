use super::Mode;
use super::Plan;
use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AdapterError {
    #[error("CF_DISPATCH_CODEX_CONFIG must be clean or inherited")]
    InvalidCodexConfig,
}
use serde::Serialize;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigDecision {
    pub config: String,
    pub args: Vec<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Route {
    pub endpoint_base_url: Option<String>,
    pub endpoint_token_env: Option<String>,
    pub endpoint_profile: Option<String>,
    pub endpoint_wire_api: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inputs {
    pub intent_assurance: bool,
    pub capabilities: bool,
    pub choice: String,
}

pub fn toml<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

pub fn toml_key_safe(name: &str) -> bool {
    name.chars().all(|character| !character.is_control())
}

/// Implements the clean/inherited decision without launching Codex discovery.
pub fn config_decision(
    plan: &Plan,
    inputs: &Inputs,
    discovery_succeeded: bool,
) -> Result<ConfigDecision, AdapterError> {
    if !matches!(inputs.choice.as_str(), "" | "clean" | "inherited") {
        return Err(AdapterError::InvalidCodexConfig);
    }
    let clean = || ConfigDecision {
        config: "clean".into(),
        args: vec!["--ignore-user-config".into(), "--ignore-rules".into()],
        note: None,
    };
    if inputs.intent_assurance || inputs.capabilities || inputs.choice == "clean" {
        return Ok(clean());
    }
    if !discovery_succeeded && plan.mode == Mode::ReadOnly {
        let mut decision = clean();
        decision.note =
            Some("MCP server discovery failed; a non-writer lane cannot inherit blind".into());
        return Ok(decision);
    }
    let mut decision = ConfigDecision {
        config: "inherited".into(),
        args: vec![
            "-c".into(),
            "notify=[]".into(),
            "-c".into(),
            "web_search=\"live\"".into(),
        ],
        note: None,
    };
    if !discovery_succeeded {
        decision.note = Some("MCP server discovery failed; no server approvals set".into());
    }
    if plan.mode == Mode::ReadOnly {
        decision
            .args
            .extend(["-c".into(), "features.hooks=false".into()]);
    }
    Ok(decision)
}

pub fn server_overrides<'a>(
    names: impl IntoIterator<Item = &'a str>,
    writer: bool,
) -> Result<String, serde_json::Error> {
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort_unstable();
    let entries = names
        .into_iter()
        .filter(|name| toml_key_safe(name))
        .map(|name| Ok(format!("{} = {{enabled = {writer}}}", toml(&name)?)))
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    Ok(format!("{{{}}}", entries.join(", ")))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;
    use crate::adapters::codex;

    #[test]
    fn codex_config_fails_closed_for_failed_reader_discovery() {
        let p = plan(Mode::ReadOnly);
        let decision = codex::config_decision(&p, &codex::Inputs::default(), false).unwrap();
        assert_eq!(decision.config, "clean");
        assert_eq!(
            decision.note.as_deref(),
            Some("MCP server discovery failed; a non-writer lane cannot inherit blind")
        );
        assert_eq!(
            codex::config_decision(
                &p,
                &codex::Inputs {
                    choice: "typo".into(),
                    ..Default::default()
                },
                true
            ),
            Err(AdapterError::InvalidCodexConfig)
        );
    }

    #[test]
    fn codex_quotes_unicode_and_sorts_server_overrides() {
        assert_eq!(codex::toml(&"😀").unwrap(), "\"😀\"");
        assert_eq!(
            codex::server_overrides(["z", "dot.name"], false).unwrap(),
            "{\"dot.name\" = {enabled = false}, \"z\" = {enabled = false}}"
        );
        assert!(!codex::toml_key_safe("bad\nname"));
    }
}
