//! The limits of one lane, resolved once. Enforcement, the lane preamble and
//! `status` all read the record this builds, so they cannot disagree.
use provenant_confinement::run_config::RunConfig;
use provenant_records::{
    BudgetAction, BudgetGrace, BudgetPolicy, LaneLimits, Limit, LimitSource, LimitStrength,
    run::{HarnessProfile, RequestedRoute, TaskRequest, WorkContext},
};
use std::path::Path;

/// The task setting that carries `retry --extend` raises to the next launch.
pub const EXTENSIONS: &str = "limit_extensions";

/// Host settings that bound one lane.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostCaps {
    /// The provider's cap on the wall clock.
    pub runtime_seconds: Option<u64>,
}

/// Resolve `task`'s limits for a lane on `provider`, once, at preparation.
/// Everything after reads this record.
pub fn lane_limits(
    task: &TaskRequest,
    state_root: &Path,
    project: &str,
    provider: &str,
    caps: HostCaps,
) -> LaneLimits {
    let writer = task.writer.is_some();
    // A provider that reports no usage while it runs cannot be stopped on
    // it: the budget is then reported to the agent, not enforced.
    let live = crate::token_budget::enforces(provider);
    let nonzero = |limit: Limit| (limit.value > 0).then_some(limit);
    let knob = |name: &str, source| {
        task.profile
            .knob_u64(name)
            .and_then(|value| nonzero(Limit::new(value, source, LimitStrength::Default)))
    };
    let config = RunConfig::read(state_root).unwrap_or_default();
    let tier = task
        .context
        .as_ref()
        .and_then(|context| context.tier.as_deref())
        .or(match &task.route {
            RequestedRoute::Named { name, .. } | RequestedRoute::Review { name, .. } => {
                Some(name.as_str())
            }
            _ => None,
        });
    let (token_budget, budget_policy) =
        writer_budget_limits(&config, project, &task.profile, task.context.as_ref(), tier);
    let token_budget = writer.then_some(token_budget).flatten().map(|mut limit| {
        if !live {
            limit.strength = LimitStrength::Advisory;
        }
        limit
    });
    let timeout = task.provider_timeout_seconds;
    let deadline_seconds = match caps.runtime_seconds {
        Some(cap) if cap < timeout => {
            Some(Limit::new(cap, LimitSource::Host, LimitStrength::Ceiling))
        }
        _ => nonzero(Limit::new(
            timeout,
            LimitSource::Launch,
            LimitStrength::Default,
        )),
    };
    let mut limits = LaneLimits {
        token_budget,
        budget_policy: writer.then_some(budget_policy),
        // Wrap-up must spend its remaining allowance in the same session;
        // a context restart here would reset that allowance on another route.
        context_handoff: (writer && !task.profile.extra.contains_key("budget_wrap_up"))
            .then(|| knob("lanes.context_handoff_tokens", LimitSource::Template))
            .flatten(),
        deadline_seconds,
        repair_cycles: knob("lanes.max_repair_cycles", LimitSource::Template),
    };
    // Raises the owner asked for with `retry --extend`; the command refused a
    // ceiling already, and one that arrives here anyway stays as it is.
    if let Some(raises) = task.extra.get(EXTENSIONS).and_then(|v| v.as_object()) {
        for (name, value) in raises {
            if let Some(value) = value.as_u64() {
                let _ = limits.extend(name, value);
            }
        }
    }
    limits
}

/// Resolve the budget and each policy field independently. This is shared by
/// admission and `run --explain`, so both show the same winning sources.
pub fn writer_budget_limits(
    config: &RunConfig,
    project: &str,
    profile: &HarnessProfile,
    context: Option<&WorkContext>,
    tier: Option<&str>,
) -> (Option<Limit>, BudgetPolicy) {
    let host = config.writer_token_budget_for(project);
    let (action, action_source) = config.writer_budget_action_for(project);
    let (grace, grace_source) = config.writer_budget_grace_for(project);
    let mut budgets = vec![(host.source, host.value)];
    let mut actions = vec![(action_source, action)];
    let mut graces = vec![(grace_source, grace)];
    if let Some(tier) = tier.and_then(|tier| config.writer_budget_tier_for(tier)) {
        if let Some(value) = tier.token_budget {
            budgets.push((LimitSource::Tier, value));
        }
        if let Some(value) = tier.on_budget {
            actions.push((LimitSource::Tier, value));
        }
        if let Some(value) = tier.budget_grace {
            graces.push((LimitSource::Tier, value));
        }
    }
    let source = |field: &str, knob: &str| -> LimitSource {
        context
            .and_then(|context| context.budget_sources.get(field).copied())
            .or_else(|| {
                profile
                    .extra
                    .get("budget_knob_sources")
                    .and_then(|sources| sources.get(knob))
                    .and_then(|source| serde_json::from_value(source.clone()).ok())
            })
            .unwrap_or(LimitSource::Default)
    };
    let source_budget = source("token_budget", "lanes.token_budget");
    if source_budget != LimitSource::Default
        && let Some(value) = profile.knob_u64("lanes.token_budget")
    {
        budgets.push((source_budget, value));
    }
    let source_action = source("on_budget", "lanes.budget_action");
    if source_action != LimitSource::Default
        && let Some(value) = profile
            .knobs
            .get("lanes.budget_action")
            .cloned()
            .and_then(|value| serde_json::from_value::<BudgetAction>(value).ok())
    {
        actions.push((source_action, value));
    }
    let source_grace = source("budget_grace", "lanes.budget_grace");
    if source_grace != LimitSource::Default
        && let Some(value) = profile
            .knobs
            .get("lanes.budget_grace")
            .cloned()
            .and_then(|value| serde_json::from_value::<BudgetGrace>(value).ok())
    {
        graces.push((source_grace, value));
    }
    let mut budget = Limit::resolve(budgets, LimitStrength::Default);
    let mut policy = BudgetPolicy::resolve(actions, graces);
    if let Some(remaining) = profile
        .extra
        .get("budget_wrap_up")
        .and_then(|wrap| wrap.get("remaining"))
        .and_then(serde_json::Value::as_u64)
    {
        let mut allowance = Limit::new(remaining, LimitSource::Launch, LimitStrength::Default);
        if let Some(target) = budget {
            allowance
                .extra
                .insert("wrap_up_target".into(), serde_json::json!(target));
        }
        budget = Some(allowance);
        policy.action = BudgetAction::Stop;
        policy.action_source = LimitSource::Launch;
    }
    (budget.filter(|budget| budget.value > 0), policy)
}

/// Limits with only a deadline, for tests that need an allowance.
#[cfg(test)]
pub(crate) fn deadline_only(seconds: u64) -> LaneLimits {
    LaneLimits {
        deadline_seconds: (seconds > 0)
            .then(|| Limit::new(seconds, LimitSource::Launch, LimitStrength::Default)),
        ..LaneLimits::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer_task() -> TaskRequest {
        let mut task = crate::tests::input().request.task.clone();
        task.writer = Some(provenant_records::run::WriterRequest {
            extra: Default::default(),
            project: "/p".into(),
            worktree: "/p/lane".into(),
        });
        task
    }

    fn state(config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            format!("schema_version = 1\n{config}"),
        )
        .unwrap();
        dir
    }

    fn found(limit: &Option<Limit>) -> Option<(u64, LimitSource)> {
        limit.as_ref().map(|l| (l.value, l.source))
    }

    /// Acceptance 1: each limit reports the source that won.
    #[test]
    fn each_limit_reports_the_winning_source() {
        let state = state(
            "[domains.run]\nwriter_token_budget = 700\n[domains.run.projects.p]\nwriter_token_budget = 900\n",
        );
        let mut task = writer_task();
        task.profile
            .knobs
            .insert("lanes.context_handoff_tokens".into(), 200_000.into());
        task.provider_timeout_seconds = 3600;
        let caps = HostCaps {
            runtime_seconds: Some(600),
        };
        let limits = lane_limits(&task, state.path(), "p", "claude", caps);
        assert_eq!(
            found(&limits.token_budget),
            Some((900, LimitSource::Project))
        );
        assert_eq!(
            found(&limits.context_handoff),
            Some((200_000, LimitSource::Template))
        );
        assert_eq!(
            found(&limits.deadline_seconds),
            Some((600, LimitSource::Host))
        );
        let host_only = lane_limits(&task, state.path(), "other", "claude", HostCaps::default());
        assert_eq!(
            found(&host_only.token_budget),
            Some((700, LimitSource::Host))
        );
        assert_eq!(
            found(&host_only.deadline_seconds),
            Some((3600, LimitSource::Launch))
        );
        let bare = tempfile::tempdir().unwrap();
        let bare = lane_limits(&task, bare.path(), "p", "claude", HostCaps::default());
        assert_eq!(
            found(&bare.token_budget),
            Some((
                provenant_confinement::run_config::DEFAULT_WRITER_TOKEN_BUDGET,
                LimitSource::Default
            ))
        );
    }

    /// Acceptance 2: the preamble and the capture plan read one record.
    #[test]
    fn the_preamble_and_the_capture_plan_read_the_same_numbers() {
        let state = state("[domains.run]\nwriter_token_budget = 777\n");
        let task = writer_task();
        let limits = lane_limits(&task, state.path(), "p", "claude", HostCaps::default());
        assert_eq!(
            crate::launch::capture_plan(&task, &limits).token_budget,
            777
        );
        assert!(
            limits
                .preamble()
                .contains(&limits.token_budget.as_ref().unwrap().value.to_string())
        );
        assert_eq!(
            crate::launch::capture_plan(&task, &limits).budget_action,
            limits.budget_policy.unwrap().action
        );
        // A provider that reports no live usage is told, not stopped.
        let blind = lane_limits(&task, state.path(), "p", "opencode", HostCaps::default());
        assert_eq!(crate::launch::capture_plan(&task, &blind).token_budget, 0);
        assert!(!blind.preamble().contains("fresh tokens"));
    }

    #[test]
    fn wrap_up_keeps_its_allowance_and_defers_context_restart() {
        let state = state("");
        let mut task = writer_task();
        task.profile
            .knobs
            .insert("lanes.context_handoff_tokens".into(), 1.into());
        assert!(
            lane_limits(&task, state.path(), "p", "claude", HostCaps::default())
                .context_handoff
                .is_some()
        );
        task.profile
            .extra
            .insert("budget_wrap_up".into(), serde_json::json!({"remaining":17}));
        let limits = lane_limits(&task, state.path(), "p", "claude", HostCaps::default());
        assert!(limits.context_handoff.is_none());
        assert_eq!(crate::launch::capture_plan(&task, &limits).token_budget, 17);
        assert_eq!(limits.budget_policy.unwrap().action, BudgetAction::Stop);
    }

    /// Acceptance 3: `retry --extend` raises a default and not a ceiling.
    #[test]
    fn an_extension_raises_a_default_and_never_a_ceiling() {
        let state = state("[domains.run]\nwriter_token_budget = 700\n");
        let mut task = writer_task();
        task.provider_timeout_seconds = 3600;
        task.extra.insert(
            EXTENSIONS.into(),
            serde_json::json!({"token_budget": 5000, "deadline_seconds": 9000}),
        );
        let caps = HostCaps {
            runtime_seconds: Some(600),
        };
        let limits = lane_limits(&task, state.path(), "p", "claude", caps);
        assert_eq!(
            found(&limits.token_budget),
            Some((5000, LimitSource::Launch))
        );
        assert_eq!(
            found(&limits.deadline_seconds),
            Some((600, LimitSource::Host))
        );
        assert_eq!(
            crate::launch::capture_plan(&task, &limits).token_budget,
            limits.token_budget.as_ref().unwrap().value
        );
    }
}

#[cfg(test)]
mod budget_invariants {
    use super::*;

    #[test]
    fn every_budget_field_resolves_independently_and_zero_remains_an_override() {
        let config: RunConfig = toml_edit::de::from_str(
            r#"
writer_token_budget = 100
writer_budget_action = "stop"
writer_budget_grace = 0.01
[projects.p]
writer_token_budget = 200
writer_budget_action = "warn"
writer_budget_grace = 0.02
[writer_budget_tiers.junior]
token_budget = 300
on_budget = "stop"
budget_grace = 0.03
"#,
        )
        .unwrap();
        let mut profile = HarnessProfile::default();
        let mut context = WorkContext::default();
        let (budget, policy) =
            writer_budget_limits(&config, "p", &profile, Some(&context), Some("junior"));
        assert_eq!(budget.unwrap().source, LimitSource::Tier);
        assert_eq!(
            (policy.action_source, policy.grace_source),
            (LimitSource::Tier, LimitSource::Tier)
        );
        for source in [LimitSource::Template, LimitSource::Launch] {
            context.budget_sources = ["token_budget", "on_budget", "budget_grace"]
                .into_iter()
                .map(|field| (field.into(), source))
                .collect();
            profile
                .knobs
                .insert("lanes.token_budget".into(), 400.into());
            profile
                .knobs
                .insert("lanes.budget_action".into(), serde_json::json!("wrap_up"));
            profile
                .knobs
                .insert("lanes.budget_grace".into(), serde_json::json!(0.04));
            let (budget, policy) =
                writer_budget_limits(&config, "p", &profile, Some(&context), Some("junior"));
            assert_eq!(
                (
                    budget.unwrap().source,
                    policy.action_source,
                    policy.grace_source
                ),
                (source, source, source)
            );
        }
        profile.knobs.insert("lanes.token_budget".into(), 0.into());
        assert!(
            writer_budget_limits(&config, "p", &profile, Some(&context), Some("junior"))
                .0
                .is_none()
        );
        profile
            .extra
            .insert("budget_wrap_up".into(), serde_json::json!({"remaining":17}));
        let (budget, policy) =
            writer_budget_limits(&config, "p", &profile, Some(&context), Some("junior"));
        assert_eq!(budget.unwrap().value, 17);
        assert_eq!(policy.action, BudgetAction::Stop);
    }
}
