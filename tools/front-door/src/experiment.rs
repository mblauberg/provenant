//! Deterministic assignment for opt-in pipeline experiments.
use provenant_config::experiment::ExperimentConfig;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Assignment {
    pub experiment: String,
    pub arm: String,
    pub preset: String,
    pub seed: String,
    pub shadow: bool,
}

/// Returns no assignment when policy is ineligible, expired, or over budget.
/// The caller supplies spend from retained graph telemetry, keeping policy
/// evaluation independent from storage and clocks.
pub(crate) fn assign(
    name: &str,
    config: &ExperimentConfig,
    work_item: &str,
    risk_tier: Option<&str>,
    spent_usd: f64,
    now: jiff::Timestamp,
) -> Result<Option<Assignment>, String> {
    if name.is_empty() || name.contains(':') {
        return Err("experiment ID must be non-empty and cannot contain ':'".into());
    }
    if work_item.trim().is_empty() || risk_tier == Some("terminal") {
        return Ok(None);
    }
    if !config.tiers.is_empty()
        && !risk_tier.is_some_and(|tier| config.tiers.iter().any(|v| v == tier))
    {
        return Ok(None);
    }
    if let Some(budget) = config.budget_usd {
        if !budget.is_finite() || budget < 0.0 || !spent_usd.is_finite() || spent_usd < 0.0 {
            return Err("experiment budget must be finite and non-negative".into());
        }
        if spent_usd >= budget {
            return Ok(None);
        }
    }
    if let Some(expires) = &config.expires {
        let expires = expires
            .parse::<jiff::Timestamp>()
            .map_err(|_| "experiment expiry must be RFC3339")?;
        if now >= expires {
            return Ok(None);
        }
    }
    if config.arms.is_empty() || config.arms.contains_key("control") {
        return Err(
            "experiment arms must be non-empty and cannot use the reserved name control".into(),
        );
    }
    if config.control.trim().is_empty()
        || config
            .arms
            .iter()
            .any(|(arm, preset)| arm.trim().is_empty() || preset.trim().is_empty())
    {
        return Err("experiment control and arm preset names must be non-empty".into());
    }
    if config
        .max_cost_ratio
        .is_some_and(|ratio| !ratio.is_finite() || ratio <= 0.0)
    {
        return Err("experiment max_cost_ratio must be finite and positive".into());
    }
    let seed = hash(&[name, work_item]);
    let (arm, preset) = match config.assign.as_str() {
        "hash" => choose(config, &seed),
        "epsilon" => {
            let epsilon = config.epsilon.unwrap_or(0.1);
            if !epsilon.is_finite() || !(0.0..=1.0).contains(&epsilon) {
                return Err("experiment epsilon must be between 0 and 1".into());
            }
            let sample =
                u64::from_str_radix(&seed[..16], 16).map_err(|_| "invalid assignment seed")?;
            if (sample as f64 / u64::MAX as f64) >= epsilon {
                ("control".to_owned(), config.control.clone())
            } else {
                choose_candidate(config, &seed)
            }
        }
        _ => return Err("experiment assign must be hash or epsilon".into()),
    };
    Ok(Some(Assignment {
        experiment: name.to_owned(),
        arm,
        preset: if config.shadow {
            config.control.clone()
        } else {
            preset
        },
        seed,
        shadow: config.shadow,
    }))
}

fn choose(config: &ExperimentConfig, seed: &str) -> (String, String) {
    let count = config.arms.len() + 1;
    let index = usize::from_str_radix(&seed[16..24], 16).unwrap_or_default() % count;
    if index == 0 {
        return ("control".into(), config.control.clone());
    }
    choose_candidate(config, seed)
}

fn choose_candidate(config: &ExperimentConfig, seed: &str) -> (String, String) {
    let index = usize::from_str_radix(&seed[..8], 16).unwrap_or_default() % config.arms.len();
    config
        .arms
        .iter()
        .nth(index)
        .map(|(name, preset)| (name.clone(), preset.clone()))
        .unwrap_or_else(|| ("control".into(), config.control.clone()))
}

fn hash(parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    provenant_records::CanonicalV1::file_digest(&bytes).to_string()[7..].to_owned()
}

#[cfg(test)]
#[allow(
    dead_code,
    unused_imports,
    reason = "fixture drivers include this module without libtest"
)]
mod tests {
    #[allow(unused_imports)]
    use super::assign;
    use provenant_config::experiment::ExperimentConfig;
    use std::collections::BTreeMap;

    #[allow(dead_code)]
    fn config() -> ExperimentConfig {
        ExperimentConfig {
            control: "single".into(),
            arms: BTreeMap::from([("candidate".into(), "chain".into())]),
            assign: "hash".into(),
            epsilon: None,
            shadow: false,
            tiers: vec!["routine".into()],
            budget_usd: Some(1.0),
            max_cost_ratio: None,
            expires: None,
        }
    }
    #[allow(dead_code)]
    fn now() -> jiff::Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn assignment_is_deterministic_for_a_work_item() {
        let a = assign("x", &config(), "task-1", Some("routine"), 0.0, now()).unwrap();
        let b = assign("x", &config(), "task-1", Some("routine"), 0.0, now()).unwrap();
        assert_eq!(a, b);
        assert!(matches!(a.unwrap().arm.as_str(), "control" | "candidate"));
    }
    #[test]
    fn budget_expiry_and_terminal_tier_stop_assignment() {
        assert!(
            assign("x", &config(), "t", Some("routine"), 1.0, now())
                .unwrap()
                .is_none()
        );
        assert!(
            assign("x", &config(), "t", Some("terminal"), 0.0, now())
                .unwrap()
                .is_none()
        );
        let mut expired = config();
        expired.expires = Some("2025-12-31T00:00:00Z".into());
        assert!(
            assign("x", &expired, "t", Some("routine"), 0.0, now())
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn shadow_assignment_executes_control() {
        let mut shadow = config();
        shadow.shadow = true;
        let selected = (0..100)
            .find_map(|n| {
                assign("x", &shadow, &format!("t{n}"), Some("routine"), 0.0, now())
                    .unwrap()
                    .filter(|assignment| assignment.arm == "candidate")
            })
            .unwrap();
        assert_eq!(selected.preset, "single");
        assert!(selected.shadow);
    }

    #[test]
    fn project_config_parses_with_unrelated_fabric_settings() {
        let config: provenant_config::experiment::FabricExperiments = toml_edit::de::from_str(
            r#"
agent_label = "host"
client_label = "host"
home_hosts = {}
peers = {}
limits = {}
[pipelines.experiment.trial]
control = "single"
arms = { candidate = "chain" }
"#,
        )
        .unwrap();
        assert_eq!(config.pipelines.experiment["trial"].control, "single");
    }
}
