use super::Mode;
use super::Plan;
use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AdapterError {
    #[error("CF_DISPATCH_CODEX_CONFIG must be clean or inherited")]
    InvalidCodexConfig,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigDecision {
    pub config: String,
    pub args: Vec<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inputs {
    pub intent_assurance: bool,
    pub capabilities: bool,
    pub choice: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn codex_config_fails_closed_for_failed_reader_discovery() {
        let p = plan(Mode::ReadOnly);
        let decision = config_decision(&p, &Inputs::default(), false).unwrap();
        assert_eq!(decision.config, "clean");
        assert_eq!(
            decision.note.as_deref(),
            Some("MCP server discovery failed; a non-writer lane cannot inherit blind")
        );
        assert_eq!(
            config_decision(
                &p,
                &Inputs {
                    choice: "typo".into(),
                    ..Default::default()
                },
                true
            ),
            Err(AdapterError::InvalidCodexConfig)
        );
    }

    #[test]
    fn codex_inherits_only_when_safe_and_respects_assurance_and_capability_lanes() {
        let reader = plan(Mode::ReadOnly);
        let inherited = config_decision(&reader, &Inputs::default(), true).unwrap();
        assert_eq!(inherited.config, "inherited");
        assert!(inherited.args.contains(&"features.hooks=false".into()));

        let mut writer = plan(Mode::WorktreeWrite);
        let writer_policy = config_decision(&writer, &Inputs::default(), true).unwrap();
        assert_eq!(writer_policy.config, "inherited");
        assert!(!writer_policy.args.contains(&"features.hooks=false".into()));

        let assurance = config_decision(
            &writer,
            &Inputs {
                intent_assurance: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(assurance.config, "clean");
        writer.mode = Mode::ReadOnly;
        let capability = config_decision(
            &writer,
            &Inputs {
                capabilities: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(capability.config, "clean");
    }
}
