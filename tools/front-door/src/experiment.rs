//! Deterministic assignment for opt-in pipeline experiments.
//!
//! Policy is pure. Callers supply the task, the observed outcome rows and an
//! eligibility check for each arm preset, so every skip is testable without
//! storage or clocks.
use provenant_config::experiment::ExperimentConfig;
use serde_json::Value;
use std::{cell::OnceCell, collections::BTreeMap};

const PREFIX: &str = "experiment:";
const CONTROL: &str = "control";
/// Tiers that take part when `tiers` is not configured. Terminal work never does.
const TIERS: [&str; 3] = ["routine", "substantial", "crucial"];
/// Disjoint slices of the seed. The epsilon draw, the control split and the
/// arm index never read the same bits, so they stay independent.
const EPSILON_SLICE: std::ops::Range<usize> = 0..16;
const SPLIT_SLICE: std::ops::Range<usize> = 16..24;
const ARM_SLICE: std::ops::Range<usize> = 24..32;

/// The arm a task runs under. `arm` and `preset` are what executes. Under
/// shadow, `shadow_of` names the candidate that was assigned while control ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Assignment {
    pub experiment: String,
    pub arm: String,
    pub preset: String,
    pub seed: String,
    pub shadow_of: Option<String>,
}

/// The task facts an experiment decision depends on.
pub(crate) struct Task<'a> {
    pub work_item: &'a str,
    pub risk_tier: Option<&'a str>,
    /// The preset the ordinary rules selected. A control must name it.
    pub rule_preset: &'a str,
    pub now: jiff::Timestamp,
}

/// One attributed outcome row whose pattern names an experiment arm. `cost` is
/// `None` when the run's cost could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedRow {
    pub pattern: String,
    pub cost: Option<u64>,
}

/// Experiment rows from the pipeline outcome report, or `None` when the report
/// has no readable rows. Callers read the report once per run.
pub(crate) fn observed_rows(report: &Value) -> Option<Vec<ObservedRow>> {
    let rows = report.pointer("/pipeline/outcomes/rows")?.as_array()?;
    Some(
        rows.iter()
            .filter_map(|row| {
                let pattern = row.pointer("/evidence/pattern")?.as_str()?;
                pattern.starts_with(PREFIX).then(|| ObservedRow {
                    pattern: pattern.to_owned(),
                    cost: row
                        .pointer("/evidence/cost_microdollars")
                        .and_then(Value::as_u64),
                })
            })
            .collect(),
    )
}

/// Observed spend for one experiment, in microdollars, per executed arm.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Spend {
    total: u64,
    arms: BTreeMap<String, ArmSpend>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ArmSpend {
    micro: u64,
    rows: u64,
}

impl Spend {
    /// `None` when any row for this experiment has an unreadable cost, so the
    /// budget and cost ratio checks fail closed.
    fn observe(experiment: &str, rows: &[ObservedRow]) -> Option<Self> {
        let prefix = format!("{PREFIX}{experiment}:");
        let mut spend = Self::default();
        for row in rows.iter().filter(|row| row.pattern.starts_with(&prefix)) {
            let cost = row.cost?;
            let arm = spend
                .arms
                .entry(row.pattern[prefix.len()..].to_owned())
                .or_default();
            arm.micro = arm.micro.checked_add(cost)?;
            arm.rows += 1;
            spend.total = spend.total.checked_add(cost)?;
        }
        Some(spend)
    }

    fn total_usd(&self) -> f64 {
        self.total as f64 / 1_000_000.0
    }

    /// Mean cost per row for `arm`; `None` when the arm has no rows.
    fn mean_usd(&self, arm: &str) -> Option<f64> {
        self.arms
            .get(arm)
            .map(|spend| spend.micro as f64 / spend.rows as f64 / 1_000_000.0)
    }
}

/// What one run's experiments decided. `skipped` says why each experiment
/// did not assign this task. `warnings` name experiments with invalid settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Placement {
    pub assignment: Option<Assignment>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
}

/// Places the task in the first experiment, in name order, that accepts it.
/// `observed` reads the outcome rows. It runs at most once per call, and only
/// when a budget or cost ratio needs them.
pub(crate) fn place(
    experiments: &[(&str, Result<ExperimentConfig, String>)],
    task: &Task,
    preset_check: &dyn Fn(&str) -> Result<(), String>,
    arm_check: &dyn Fn(&str) -> Result<(), String>,
    observed: &dyn Fn() -> Option<Vec<ObservedRow>>,
) -> Placement {
    let rows = OnceCell::new();
    let mut placement = Placement::default();
    for (name, parsed) in experiments {
        let checked = parsed
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|config| validate(name, config, preset_check).map(|()| config));
        let config = match checked {
            Ok(config) => config,
            Err(error) => {
                placement
                    .warnings
                    .push(format!("experiment {name} skipped: {error}"));
                continue;
            }
        };
        match assign_one(name, config, task, arm_check, &rows, observed) {
            Ok(assignment) => {
                placement.assignment = Some(assignment);
                return placement;
            }
            Err(reason) => placement.skipped.push(format!("{name}: {reason}")),
        }
    }
    placement
}

/// Checks one experiment's settings. Errors name the field; the caller adds
/// the experiment's name.
fn validate(
    name: &str,
    config: &ExperimentConfig,
    preset_check: &dyn Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if name.is_empty() || name.contains(':') {
        return Err("the experiment name must be non-empty and cannot contain ':'".into());
    }
    if config.control.trim().is_empty() {
        return Err("control must name a preset".into());
    }
    if config.arms.is_empty() {
        return Err("arms must name at least one candidate".into());
    }
    if config.arms.contains_key(CONTROL) {
        return Err("the arm name control is reserved for the control".into());
    }
    for (arm, preset) in &config.arms {
        if arm.trim().is_empty() || arm.contains(':') {
            return Err(format!(
                "arm name {arm:?} must be non-empty and cannot contain ':'"
            ));
        }
        if preset.trim().is_empty() {
            return Err(format!("arm {arm} must name a preset"));
        }
    }
    for preset in std::iter::once(&config.control).chain(config.arms.values()) {
        preset_check(preset).map_err(|error| format!("preset {preset} is unavailable: {error}"))?;
    }
    match config.assign.as_str() {
        "hash" if config.epsilon.is_some() => {
            return Err("epsilon applies only when assign is \"epsilon\"".into());
        }
        "hash" => {}
        "epsilon" => {
            let epsilon = config.epsilon.unwrap_or(0.1);
            if !epsilon.is_finite() || !(0.0..=1.0).contains(&epsilon) {
                return Err("epsilon must be between 0 and 1".into());
            }
        }
        other => {
            return Err(format!(
                "assign must be \"hash\" or \"epsilon\", not {other:?}"
            ));
        }
    }
    if config
        .budget_usd
        .is_some_and(|budget| !budget.is_finite() || budget < 0.0)
    {
        return Err("budget_usd must be finite and non-negative".into());
    }
    if config
        .max_cost_ratio
        .is_some_and(|ratio| !ratio.is_finite() || ratio <= 0.0)
    {
        return Err("max_cost_ratio must be finite and positive".into());
    }
    if let Some(tier) = config
        .tiers
        .iter()
        .find(|tier| !TIERS.contains(&tier.as_str()))
    {
        return Err(format!(
            "tier {tier:?} cannot take part; use routine, substantial or crucial (terminal work is never assigned)"
        ));
    }
    Ok(())
}

fn assign_one(
    name: &str,
    config: &ExperimentConfig,
    task: &Task,
    arm_check: &dyn Fn(&str) -> Result<(), String>,
    rows: &OnceCell<Option<Vec<ObservedRow>>>,
    observed: &dyn Fn() -> Option<Vec<ObservedRow>>,
) -> Result<Assignment, String> {
    if config.control != task.rule_preset {
        return Err(format!(
            "control {} is not the preset the rules select ({})",
            config.control, task.rule_preset
        ));
    }
    if task.work_item.trim().is_empty() {
        return Err("the task has no work item".into());
    }
    // A task without a tier is never assigned: it cannot be shown to be safe.
    let tier = task.risk_tier.ok_or("the task has no risk tier")?;
    let tiers: Vec<&str> = if config.tiers.is_empty() {
        TIERS.to_vec()
    } else {
        config.tiers.iter().map(String::as_str).collect()
    };
    if !tiers.contains(&tier) {
        return Err(format!("risk tier {tier} is not in the experiment's tiers"));
    }
    if config
        .expires
        .is_some_and(|expires| task.now >= expires.as_timestamp())
    {
        return Err("the experiment has expired".into());
    }
    let spend = if config.budget_usd.is_some() || config.max_cost_ratio.is_some() {
        let spend = rows
            .get_or_init(observed)
            .as_deref()
            .and_then(|rows| Spend::observe(name, rows))
            .ok_or("observed spend is unreadable")?;
        Some(spend)
    } else {
        None
    };
    if let (Some(budget), Some(spend)) = (config.budget_usd, &spend)
        && spend.total_usd() >= budget
    {
        return Err("the budget is spent".into());
    }
    let seed = hash(&[name, task.work_item]);
    let (mut arm, mut preset) = draw(config, &seed)?;
    if arm != CONTROL
        && let Some(spend) = &spend
        && over_ratio(config.max_cost_ratio, spend, &arm)
    {
        (arm, preset) = (CONTROL.into(), config.control.clone());
    }
    if arm != CONTROL {
        arm_check(&preset).map_err(|error| format!("arm {arm} is not eligible: {error}"))?;
    }
    let shadow_of = (config.shadow && arm != CONTROL).then(|| arm.clone());
    if config.shadow {
        (arm, preset) = (CONTROL.into(), config.control.clone());
    }
    Ok(Assignment {
        experiment: name.to_owned(),
        arm,
        preset,
        seed,
        shadow_of,
    })
}

/// A candidate stops being assigned once its mean cost per row exceeds the
/// ratio times the control's. Without control rows there is no baseline, so
/// candidates wait for control evidence.
fn over_ratio(ratio: Option<f64>, spend: &Spend, arm: &str) -> bool {
    let Some(ratio) = ratio else {
        return false;
    };
    match spend.mean_usd(CONTROL) {
        None => true,
        Some(control) => spend.mean_usd(arm).unwrap_or(0.0) > ratio * control,
    }
}

/// The arm and preset a seed selects. Under `hash` the control holds one of
/// `arms + 1` equal shares. Under `epsilon` candidates take `epsilon` of tasks.
fn draw(config: &ExperimentConfig, seed: &str) -> Result<(String, String), String> {
    let control = || (CONTROL.to_owned(), config.control.clone());
    if config.assign == "epsilon" {
        let epsilon = config.epsilon.unwrap_or(0.1);
        let sample =
            u64::from_str_radix(&seed[EPSILON_SLICE], 16).map_err(|_| "invalid assignment seed")?;
        return Ok(if (sample as f64 / u64::MAX as f64) < epsilon {
            choose_candidate(config, seed)
        } else {
            control()
        });
    }
    let split =
        usize::from_str_radix(&seed[SPLIT_SLICE], 16).map_err(|_| "invalid assignment seed")?;
    Ok(if split % (config.arms.len() + 1) == 0 {
        control()
    } else {
        choose_candidate(config, seed)
    })
}

fn choose_candidate(config: &ExperimentConfig, seed: &str) -> (String, String) {
    let index = usize::from_str_radix(&seed[ARM_SLICE], 16).unwrap_or_default() % config.arms.len();
    config
        .arms
        .iter()
        .nth(index)
        .map(|(arm, preset)| (arm.clone(), preset.clone()))
        .unwrap_or_else(|| (CONTROL.into(), config.control.clone()))
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
    use super::{ObservedRow, Placement, Spend, Task, observed_rows, place, validate};
    use provenant_config::experiment::ExperimentConfig;
    use serde_json::json;
    use std::collections::BTreeMap;

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
    fn now() -> jiff::Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }
    fn known(_: &str) -> Result<(), String> {
        Ok(())
    }
    fn arm_ok(_: &str) -> Result<(), String> {
        Ok(())
    }
    fn row(pattern: &str, cost: Option<u64>) -> ObservedRow {
        ObservedRow {
            pattern: pattern.into(),
            cost,
        }
    }
    fn placed(
        config: ExperimentConfig,
        work_item: &str,
        tier: Option<&str>,
        rows: Vec<ObservedRow>,
    ) -> Placement {
        place(
            &[("x", Ok(config))],
            &Task {
                work_item,
                risk_tier: tier,
                rule_preset: "single",
                now: now(),
            },
            &known,
            &arm_ok,
            &|| Some(rows.clone()),
        )
    }
    /// Epsilon 1.0 sends every task to the candidate, so tests can name the arm.
    fn always_candidate() -> ExperimentConfig {
        ExperimentConfig {
            assign: "epsilon".into(),
            epsilon: Some(1.0),
            budget_usd: None,
            ..config()
        }
    }

    #[test]
    fn assignment_is_deterministic_for_a_work_item() {
        let a = placed(config(), "task-1", Some("routine"), vec![]);
        let b = placed(config(), "task-1", Some("routine"), vec![]);
        assert_eq!(a, b);
        let arm = a.assignment.unwrap().arm;
        assert!(matches!(arm.as_str(), "control" | "candidate"));
    }

    #[test]
    fn epsilon_draw_is_reproducible_and_tracks_epsilon() {
        let mut cfg = config();
        cfg.assign = "epsilon".into();
        cfg.epsilon = Some(0.2);
        cfg.budget_usd = None;
        cfg.arms.insert("alt".into(), "chain".into());
        let items: Vec<String> = (0..2000).map(|n| format!("task-{n}")).collect();
        let arms = |cfg: &ExperimentConfig| -> Vec<String> {
            items
                .iter()
                .map(|item| {
                    placed(cfg.clone(), item, Some("routine"), vec![])
                        .assignment
                        .unwrap()
                        .arm
                })
                .collect()
        };
        let first = arms(&cfg);
        assert_eq!(first, arms(&cfg));
        let candidates = first.iter().filter(|arm| *arm != "control").count();
        assert!((300..500).contains(&candidates), "{candidates} candidates");
        assert!(first.iter().any(|arm| arm == "candidate"));
        assert!(first.iter().any(|arm| arm == "alt"));
    }

    #[test]
    fn epsilon_bounds_select_all_or_no_candidates() {
        let mut none = always_candidate();
        none.epsilon = Some(0.0);
        assert_eq!(
            placed(none, "task-1", Some("routine"), vec![])
                .assignment
                .unwrap()
                .arm,
            "control"
        );
        assert_eq!(
            placed(always_candidate(), "task-1", Some("routine"), vec![])
                .assignment
                .unwrap()
                .arm,
            "candidate"
        );
    }

    #[test]
    fn budget_expiry_and_terminal_tier_stop_assignment() {
        let spent = vec![row("experiment:x:control", Some(1_000_000))];
        let budget = placed(config(), "t", Some("routine"), spent);
        assert!(budget.assignment.is_none());
        assert!(budget.skipped[0].contains("the budget is spent"));

        let terminal = placed(config(), "t", Some("terminal"), vec![]);
        assert!(terminal.assignment.is_none());
        assert!(terminal.skipped[0].contains("not in the experiment's tiers"));

        let mut expired = config();
        expired.expires = provenant_core::WallTime::parse_rfc3339("2025-12-31T00:00:00Z").ok();
        let expired = placed(expired, "t", Some("routine"), vec![]);
        assert!(expired.assignment.is_none());
        assert!(expired.skipped[0].contains("expired"));
    }

    #[test]
    fn missing_or_unlisted_risk_tier_is_ineligible() {
        let missing = placed(config(), "t", None, vec![]);
        assert!(missing.assignment.is_none());
        assert!(missing.skipped[0].contains("no risk tier"));

        let mut unlisted = config();
        unlisted.tiers = vec![];
        let unknown = placed(unlisted, "t", Some("bogus"), vec![]);
        assert!(unknown.assignment.is_none());
        assert!(unknown.skipped[0].contains("bogus"));
    }

    #[test]
    fn tiers_filter_limits_which_work_is_assigned() {
        let mut cfg = always_candidate();
        cfg.tiers = vec!["substantial".into()];
        let routine = placed(cfg.clone(), "t", Some("routine"), vec![]);
        assert!(routine.assignment.is_none());
        let substantial = placed(cfg, "t", Some("substantial"), vec![]);
        assert_eq!(substantial.assignment.unwrap().arm, "candidate");
    }

    #[test]
    fn shadow_assignment_executes_control_and_records_the_candidate() {
        let mut shadow = config();
        shadow.shadow = true;
        let selected = (0..100)
            .find_map(|n| {
                placed(shadow.clone(), &format!("t{n}"), Some("routine"), vec![])
                    .assignment
                    .filter(|assignment| assignment.shadow_of.is_some())
            })
            .unwrap();
        assert_eq!(selected.arm, "control");
        assert_eq!(selected.preset, "single");
        assert_eq!(selected.shadow_of.as_deref(), Some("candidate"));
    }

    #[test]
    fn candidate_over_cost_ratio_falls_back_to_control() {
        let mut cfg = always_candidate();
        cfg.max_cost_ratio = Some(3.0);
        let over = vec![
            row("experiment:x:control", Some(1_000_000)),
            row("experiment:x:control", Some(1_000_000)),
            row("experiment:x:candidate", Some(5_000_000)),
        ];
        let blocked = placed(cfg.clone(), "t", Some("routine"), over);
        assert_eq!(blocked.assignment.unwrap().arm, "control");

        let within = vec![
            row("experiment:x:control", Some(1_000_000)),
            row("experiment:x:candidate", Some(2_000_000)),
        ];
        let allowed = placed(cfg, "t", Some("routine"), within);
        assert_eq!(allowed.assignment.unwrap().arm, "candidate");
    }

    #[test]
    fn candidate_waits_for_control_evidence_under_a_cost_ratio() {
        let mut cfg = always_candidate();
        cfg.max_cost_ratio = Some(3.0);
        let waiting = placed(cfg, "t", Some("routine"), vec![]);
        assert_eq!(waiting.assignment.unwrap().arm, "control");
    }

    #[test]
    fn unreadable_cost_fails_closed() {
        let unreadable = vec![
            row("experiment:x:control", Some(1_000_000)),
            row("experiment:x:control", None),
        ];
        let budget = placed(config(), "t", Some("routine"), unreadable.clone());
        assert!(budget.assignment.is_none());
        assert!(budget.skipped[0].contains("unreadable"));

        let mut ratio = always_candidate();
        ratio.max_cost_ratio = Some(3.0);
        let ratio = placed(ratio, "t", Some("routine"), unreadable);
        assert!(ratio.assignment.is_none());
        assert!(ratio.skipped[0].contains("unreadable"));

        let missing = place(
            &[("x", Ok(config()))],
            &Task {
                work_item: "t",
                risk_tier: Some("routine"),
                rule_preset: "single",
                now: now(),
            },
            &known,
            &arm_ok,
            &|| None,
        );
        assert!(missing.assignment.is_none());
        assert!(missing.skipped[0].contains("unreadable"));
    }

    #[test]
    fn report_without_outcome_rows_is_unreadable() {
        assert!(observed_rows(&json!({})).is_none());
        let rows = observed_rows(&json!({"pipeline": {"outcomes": {"rows": [
            {"evidence": {"pattern": "experiment:x:control", "cost_microdollars": 2_000_000}},
            {"evidence": {"pattern": "experiment:y:control"}},
            {"evidence": {"pattern": "single"}}
        ]}}}))
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(Spend::observe("y", &rows).is_none());
        assert_eq!(Spend::observe("x", &rows).unwrap().total_usd(), 2.0);
    }

    #[test]
    fn control_must_be_the_rule_selected_preset() {
        let placement = place(
            &[("x", Ok(config()))],
            &Task {
                work_item: "t",
                risk_tier: Some("routine"),
                rule_preset: "chain",
                now: now(),
            },
            &known,
            &arm_ok,
            &|| Some(vec![]),
        );
        assert!(placement.assignment.is_none());
        assert!(placement.skipped[0].contains("is not the preset the rules select"));
    }

    #[test]
    fn ineligible_arm_falls_back_and_records_the_skip() {
        let placement = place(
            &[("x", Ok(always_candidate()))],
            &Task {
                work_item: "t",
                risk_tier: Some("routine"),
                rule_preset: "single",
                now: now(),
            },
            &known,
            &|preset| Err(format!("{preset} has no write scope")),
            &|| Some(vec![]),
        );
        assert!(placement.assignment.is_none());
        assert!(placement.skipped[0].contains("arm candidate is not eligible"));
        assert!(placement.skipped[0].contains("chain has no write scope"));
    }

    #[test]
    fn invalid_experiment_warns_and_does_not_block_the_next() {
        let placement = place(
            &[
                ("a-broken", Err("unknown field `expire`".into())),
                ("b-valid", Ok(always_candidate())),
            ],
            &Task {
                work_item: "t",
                risk_tier: Some("routine"),
                rule_preset: "single",
                now: now(),
            },
            &known,
            &arm_ok,
            &|| Some(vec![]),
        );
        assert_eq!(placement.warnings.len(), 1);
        assert!(placement.warnings[0].contains("a-broken"));
        let assignment = placement.assignment.unwrap();
        assert_eq!(assignment.experiment, "b-valid");
        assert_eq!(assignment.arm, "candidate");
    }

    #[test]
    fn malformed_experiments_are_rejected_with_the_field_named() {
        let check = |cfg: ExperimentConfig| validate("x", &cfg, &known).unwrap_err();
        let expires = serde_json::from_value::<ExperimentConfig>(serde_json::json!({
            "control": "single", "arms": {"candidate": "chain"}, "expires": "next year"
        }))
        .unwrap_err();
        assert!(expires.to_string().contains("RFC 3339"), "{expires}");

        let mut reserved = config();
        reserved.arms.insert("control".into(), "chain".into());
        assert!(check(reserved).contains("reserved"));

        let mut negative = config();
        negative.budget_usd = Some(-1.0);
        assert!(check(negative).contains("budget_usd"));

        let mut unknown = config();
        unknown.assign = "random".into();
        assert!(check(unknown).contains("assign must be"));

        let mut hashed = config();
        hashed.epsilon = Some(0.1);
        assert!(check(hashed).contains("epsilon applies only"));

        let mut terminal = config();
        terminal.tiers = vec!["terminal".into()];
        assert!(check(terminal).contains("terminal work is never assigned"));

        let mut ratio = config();
        ratio.max_cost_ratio = Some(0.0);
        assert!(check(ratio).contains("max_cost_ratio"));
    }

    #[test]
    fn unavailable_presets_are_named_by_experiment_check() {
        let missing = |preset: &str| -> Result<(), String> {
            if preset == "chain" {
                Err("unknown preset: chain".into())
            } else {
                Ok(())
            }
        };
        let error = validate("review", &config(), &missing).unwrap_err();
        assert_eq!(error, "preset chain is unavailable: unknown preset: chain");
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
        let parsed = config.pipelines.parsed();
        assert_eq!(parsed[0].0, "trial");
        assert_eq!(parsed[0].1.as_ref().unwrap().control, "single");
    }
}
