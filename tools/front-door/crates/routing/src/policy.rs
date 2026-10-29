//! Layered model guidance and enforced lane/result admission, evaluated in UTC.
use crate::{ModelId, RoutingError};
use provenant_config::SectionError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workload {
    #[default]
    General,
    ReadHeavy,
    EditHeavy,
    Review,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RouteIntent {
    pub workload: Workload,
    pub writes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMode {
    Auto,
    TaskBoundary,
    Never,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompactionPolicy {
    pub window: Option<u64>,
    pub mode: Option<CompactionMode>,
    pub min_turns: Option<u64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CachePolicy {
    pub read_share_target: Option<f64>,
    pub price_multiplier: Option<f64>,
    pub read_ratio: Option<f64>,
    pub write_ratio: Option<f64>,
    /// Output price divided by uncached input price, for break-even guidance.
    pub output_ratio: Option<f64>,
    pub ttl_seconds: Option<u64>,
    pub min_prefix: Option<u64>,
    pub context_window: Option<u64>,
    pub read_heavy_weight: Option<u8>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanePolicy {
    pub max_context: Option<u64>,
    pub max_turns: Option<u64>,
    pub stop_early_ok: Option<bool>,
    pub max_continuations: Option<u8>,
    pub read_only: Option<bool>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResultPolicy {
    pub check: Option<bool>,
    pub sections: Option<Vec<String>>,
    pub fallback: Option<ModelId>,
    pub fallback_effort: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicy {
    pub compaction: CompactionPolicy,
    pub cache: CachePolicy,
    pub lane: LanePolicy,
    pub result: ResultPolicy,
}

macro_rules! overlay {
    ($base:expr, $patch:expr, $($field:ident),+ $(,)?) => {
        $(if $patch.$field.is_some() { $base.$field.clone_from(&$patch.$field); })+
    };
}
impl ModelPolicy {
    fn overlay(&mut self, patch: &Self) {
        overlay!(self.compaction, patch.compaction, window, mode, min_turns);
        overlay!(
            self.cache,
            patch.cache,
            read_share_target,
            price_multiplier,
            read_ratio,
            write_ratio,
            output_ratio,
            ttl_seconds,
            min_prefix,
            context_window,
            read_heavy_weight
        );
        overlay!(
            self.lane,
            patch.lane,
            max_context,
            max_turns,
            stop_early_ok,
            max_continuations,
            read_only
        );
        overlay!(
            self.result,
            patch.result,
            check,
            sections,
            fallback,
            fallback_effort
        );
    }

    pub fn permits(&self, intent: RouteIntent) -> bool {
        self.lane.read_only != Some(true) || !intent.writes
    }

    pub fn cache_factor_basis_points(&self, intent: RouteIntent) -> u32 {
        if matches!(intent.workload, Workload::ReadHeavy | Workload::Review)
            && self.cache.read_ratio.is_some_and(|r| r <= 0.03)
            && self.cache.context_window.is_some_and(|c| c >= 1_000_000)
        {
            (f64::from(self.cache.read_heavy_weight.unwrap_or(3)) * 10_000.0
                / self.cache.price_multiplier.unwrap_or(1.0))
            .round()
            .clamp(1.0, 100_000.0) as u32
        } else {
            10_000
        }
    }

    /// The issue's N* formula, in uncached-input price units. Unknown prices
    /// remain unknown; a zero read price has no finite compaction payback.
    pub fn break_even_turns(&self, context: u64, summary: u64, rework: u64) -> Option<f64> {
        let r = self.cache.read_ratio?;
        let w = self.cache.write_ratio?;
        let o = self.cache.output_ratio?;
        if r <= 0.0 || context <= summary {
            return None;
        }
        Some(
            (r * context as f64 + (o + w) * summary as f64 + w * rework as f64)
                / (r * (context - summary) as f64),
        )
    }

    pub fn result_valid(&self, text: &str) -> bool {
        self.result.check != Some(true)
            || (!text.trim().is_empty()
                && self.result.sections.as_ref().is_none_or(|sections| {
                    sections
                        .iter()
                        .all(|section| text.lines().any(|line| line.trim() == section))
                }))
    }

    /// This is a launch control only for Claude. Other providers receive
    /// task-boundary advice, never guessed provider environment variables.
    pub fn launch_env(&self, adapter: &str) -> Vec<(String, String)> {
        if adapter == "claude" && self.compaction.mode != Some(CompactionMode::Never) {
            self.compaction
                .window
                .map(|window| vec![("CLAUDE_CODE_AUTO_COMPACT_WINDOW".into(), window.to_string())])
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    pub fn guidance(&self) -> String {
        let mut lines = Vec::new();
        if let Some(turns) = self
            .compaction
            .window
            .and_then(|window| self.break_even_turns(window, 15_000, 0))
        {
            lines.push(format!("Compaction payback estimate: {turns:.1} future turns for a 15k summary and zero rework. Rework raises this estimate; use judgement at the task boundary."));
        }
        if self.compaction.mode == Some(CompactionMode::TaskBoundary) {
            lines.push(format!("Compact deliberately at a task boundary while the cache is warm, after at least {} turns. Prefer a fresh lane with a handoff over repeated compaction. Compaction window guidance: {:?} tokens.", self.compaction.min_turns.unwrap_or(0), self.compaction.window));
        } else if self.compaction.mode == Some(CompactionMode::Never) {
            lines.push("Do not deliberately compact; hand off to a fresh lane when needed.".into());
        }
        if let Some(context) = self.lane.max_context {
            lines.push(format!(
                "Hand off at a task boundary before {context} context tokens."
            ));
        }
        if let Some(turns) = self.lane.max_turns {
            lines.push(format!("Finish or hand off within {turns} turns."));
        }
        if self.lane.stop_early_ok == Some(false) {
            lines.push("Carry the task to completion. If work remains, end with the exact line `Status: incomplete` and explain the next step; otherwise end with `Status: complete`.".into());
        }
        if let Some(sections) = &self.result.sections {
            lines.push(format!(
                "Your final answer must include these exact section lines: {}",
                sections.join(", ")
            ));
        }
        if let Some(ttl) = self.cache.ttl_seconds {
            lines.push(format!("Cache planning TTL: {ttl} seconds; reuse stable prefixes while warm. This is a routing assumption, not a provider TTL control."));
        }
        if let Some(prefix) = self.cache.min_prefix {
            lines.push(format!(
                "Keep at least {prefix} stable prefix tokens when useful."
            ));
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtcWindow {
    pub start_hour: u8,
    pub end_hour: u8,
}
impl UtcWindow {
    fn active(&self, now: i64) -> bool {
        let hour = now.rem_euclid(86_400) / 3600;
        if self.start_hour < self.end_hour {
            hour >= i64::from(self.start_hour) && hour < i64::from(self.end_hour)
        } else {
            hour >= i64::from(self.start_hour) || hour < i64::from(self.end_hour)
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicyRule {
    pub adapter: Option<String>,
    pub model: Option<ModelId>,
    /// Canonical adapter/key or a registry-free account key.
    pub account: Option<String>,
    pub starts_unix_seconds: Option<i64>,
    pub expires_unix_seconds: Option<i64>,
    /// Any matching UTC window activates this rule. Empty means all day.
    pub windows: Vec<UtcWindow>,
    pub policy: ModelPolicy,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicies {
    pub bundled: bool,
    pub defaults: ModelPolicy,
    pub rules: Vec<ModelPolicyRule>,
}
impl Default for ModelPolicies {
    fn default() -> Self {
        Self {
            bundled: true,
            defaults: ModelPolicy::default(),
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EffectiveModelPolicy {
    #[serde(flatten)]
    pub policy: ModelPolicy,
    pub sources: Vec<String>,
}
impl ModelPolicies {
    pub fn effective(
        &self,
        model: &ModelId,
        account: Option<&str>,
        now: i64,
    ) -> EffectiveModelPolicy {
        let account = account.and_then(|account| match account.split_once('/') {
            Some((adapter, key)) if adapter == model.adapter() => Some(key),
            Some(_) => None,
            None => Some(account),
        });
        let mut policy = if self.bundled {
            bundled_policy(model, now)
        } else {
            ModelPolicy::default()
        };
        policy.overlay(&self.defaults);
        let mut sources = vec![
            if self.bundled {
                "bundled + defaults"
            } else {
                "defaults"
            }
            .into(),
        ];
        // Scope outranks array position. Later rules win within one scope.
        let mut matching: Vec<_> = self
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| {
                rule.adapter.as_deref().is_none_or(|a| a == model.adapter())
                    && rule.model.as_ref().is_none_or(|m| m == model)
                    && rule.account.as_deref().is_none_or(|a| {
                        account.is_some_and(|key| {
                            a == key || a == format!("{}/{key}", model.adapter())
                        })
                    })
                    && rule.starts_unix_seconds.is_none_or(|start| now >= start)
                    && rule.expires_unix_seconds.is_none_or(|end| now < end)
                    && (rule.windows.is_empty() || rule.windows.iter().any(|w| w.active(now)))
            })
            .collect();
        matching.sort_by_key(|(index, r)| {
            (
                u8::from(r.adapter.is_some())
                    + 2 * u8::from(r.model.is_some())
                    + 4 * u8::from(r.account.is_some()),
                *index,
            )
        });
        for (index, rule) in matching {
            policy.overlay(&rule.policy);
            sources.push(format!("rules[{index}]"));
        }
        EffectiveModelPolicy { policy, sources }
    }

    pub(crate) fn validate(&self, errors: &mut Vec<SectionError>) {
        if self.rules.len() > 128 {
            errors.push(SectionError {
                field: "model_policy.rules".into(),
                code: "limit",
            });
        }
        let invalid = |policy: &ModelPolicy| {
            [
                policy.cache.read_ratio,
                policy.cache.write_ratio,
                policy.cache.output_ratio,
            ]
            .into_iter()
            .flatten()
            .any(|n| !n.is_finite() || !(0.0..=1000.0).contains(&n))
                || policy
                    .cache
                    .price_multiplier
                    .is_some_and(|n| !n.is_finite() || !(0.01..=1000.0).contains(&n))
                || policy
                    .cache
                    .read_share_target
                    .is_some_and(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
                || [
                    policy.compaction.window,
                    policy.cache.context_window,
                    policy.lane.max_context,
                    policy.lane.max_turns,
                    policy.cache.ttl_seconds,
                ]
                .into_iter()
                .flatten()
                .any(|n| n == 0)
                || policy.lane.max_continuations.is_some_and(|n| n > 8)
                || policy
                    .cache
                    .read_heavy_weight
                    .is_some_and(|n| n == 0 || n > 10)
                || policy.result.sections.as_ref().is_some_and(|s| {
                    s.len() > 16
                        || s.iter().any(|s| {
                            s.trim().is_empty() || s.len() > 256 || s.chars().any(char::is_control)
                        })
                })
                || policy
                    .result
                    .fallback_effort
                    .as_deref()
                    .is_some_and(|e| !crate::request::effort_valid(e))
        };
        if invalid(&self.defaults) {
            errors.push(SectionError {
                field: "model_policy.defaults".into(),
                code: "policy",
            });
        }
        for (i, rule) in self.rules.iter().enumerate() {
            if invalid(&rule.policy)
                || rule.adapter.as_deref().is_some_and(|a| {
                    !crate::request::identifier(a)
                        || rule.model.as_ref().is_some_and(|m| m.adapter() != a)
                })
                || rule.account.as_deref().is_some_and(|a| {
                    a.split('/').count() > 2
                        || a.split('/').any(|part| !crate::request::identifier(part))
                })
                || rule
                    .starts_unix_seconds
                    .zip(rule.expires_unix_seconds)
                    .is_some_and(|(s, e)| s >= e)
                || rule.windows.len() > 16
                || rule
                    .windows
                    .iter()
                    .any(|w| w.start_hour >= 24 || w.end_hour > 24 || w.start_hour == w.end_hour)
            {
                errors.push(SectionError {
                    field: format!("model_policy.rules.{i}"),
                    code: "policy",
                });
            }
        }
    }
}

fn bundled_policy(model: &ModelId, now: i64) -> ModelPolicy {
    let mut policy = ModelPolicy::default();
    policy.cache.read_share_target = Some(0.90);
    if model.adapter() == "claude" {
        policy.compaction = CompactionPolicy {
            window: Some(275_000),
            mode: Some(CompactionMode::TaskBoundary),
            min_turns: Some(14),
        };
        policy.cache.read_ratio = match model.model() {
            "claude-opus-5-5" | "claude-sonnet-5-5" => Some(0.05),
            "claude-haiku-5-5" | "claude-sonnet-5" => Some(0.10),
            _ => None,
        };
        policy.cache.write_ratio = Some(1.25);
        policy.cache.output_ratio = Some(5.0);
    }
    if model.adapter() == "codex" {
        policy.cache.read_ratio = match model.model() {
            "gpt-6.1-sol" | "gpt-6.1-terra" => Some(0.05),
            "gpt-6-luna" => Some(0.10),
            _ => None,
        };
        policy.cache.write_ratio = Some(1.25);
        policy.cache.output_ratio = Some(5.0);
    }
    if model.as_str() == "opencode/opencode-go/deepseek-v4.1-flash" {
        policy.cache.read_ratio = Some(0.02);
        policy.cache.write_ratio = Some(0.0);
        policy.cache.output_ratio = Some(4.0);
        policy.cache.context_window = Some(1_000_000);
        policy.cache.read_heavy_weight = Some(3);
        // Published weekday peak windows in config/telemetry-prices.toml.
        let weekday = (now.div_euclid(86_400) + 3).rem_euclid(7);
        let hour = now.rem_euclid(86_400) / 3600;
        policy.cache.price_multiplier = Some(
            if weekday < 5 && ((1..4).contains(&hour) || (6..10).contains(&hour)) {
                2.0
            } else {
                1.0
            },
        );
        policy.compaction = CompactionPolicy {
            mode: Some(CompactionMode::TaskBoundary),
            window: Some(800_000),
            min_turns: Some(20),
        };
    }
    if model.as_str() == "codex/gpt-6.1-terra" {
        policy.lane.stop_early_ok = Some(false);
        policy.lane.max_continuations = Some(2);
    }
    if model.as_str() == "agy/gemini-3.8-flash"
        || (model.adapter() == "opencode" && model.model().ends_with("-free"))
    {
        policy.lane.read_only = Some(true);
        policy.result.check = Some(true);
        policy.result.sections = Some(vec!["## Findings".into(), "## Evidence".into()]);
        // An exact, catalogued paid route with its recorded effort control.
        policy.result.fallback = ModelId::parse("codex/gpt-6-luna").ok();
        policy.result.fallback_effort = Some("high".into());
    }
    policy
}

/// Unknown input categories stay unknown rather than reporting a false 0%.
/// Input here is the uncached category, separate from cache reads and writes.
pub fn cache_read_share(input: Option<u64>, read: Option<u64>, write: Option<u64>) -> Option<f64> {
    let (input, read, write) = (input?, read?, write?);
    let total = u128::from(input) + u128::from(read) + u128::from(write);
    (total > 0).then(|| read as f64 / total as f64)
}

pub fn validate_intent(policy: &ModelPolicy, intent: RouteIntent) -> Result<(), RoutingError> {
    if policy.permits(intent) {
        Ok(())
    } else {
        Err(RoutingError::ReadOnlyPolicy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The peak window and multiplier in `bundled_policy` are hard-coded; the
    /// price file states them in a comment. Keep the two in step.
    #[test]
    fn deepseek_peak_window_matches_the_price_file() -> Result<(), Box<dyn std::error::Error>> {
        let prices = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../config/telemetry-prices.toml"),
        )?;
        let model = "opencode/opencode-go/deepseek-v4.1-flash";
        let block = prices
            .split("[[models]]")
            .find(|block| block.contains(&format!("model = \"{model}\"")))
            .ok_or("price entry missing")?;
        let comment = block
            .lines()
            .find(|line| line.contains("peak hours"))
            .ok_or("peak comment missing")?;
        let numbers = |text: &str| -> Vec<i64> {
            text.split(|c: char| !c.is_ascii_digit())
                .filter_map(|part| part.parse().ok())
                .collect()
        };
        let (window, rest) = comment
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .ok_or("peak window missing")?;
        let hours = numbers(window.split("UTC").next().unwrap_or_default());
        assert_eq!(hours.len(), 4, "{comment}");
        assert!(window.contains("Monday to Friday"), "{comment}");
        let multiplier = numbers(rest).first().copied().ok_or("multiplier missing")? as f64;
        let model = ModelId::parse(model)?;
        // 1970-01-05 is a Monday; sample every hour of a full week.
        let monday = 4 * 86_400;
        for hour in 0..7 * 24 {
            let at = monday + hour * 3600;
            let weekday = (hour / 24) < 5;
            let hour_of_day = hour % 24;
            let peak = weekday
                && ((hours[0]..hours[1]).contains(&hour_of_day)
                    || (hours[2]..hours[3]).contains(&hour_of_day));
            let expected = if peak { multiplier } else { 1.0 };
            assert_eq!(
                bundled_policy(&model, at).cache.price_multiplier,
                Some(expected),
                "hour {hour}"
            );
        }
        Ok(())
    }

    #[test]
    fn model_account_layers_schedules_and_expiry() -> Result<(), Box<dyn std::error::Error>> {
        let policies: ModelPolicies = toml_edit::de::from_str(
            r#"
            [defaults.cache]
            ttl_seconds = 300
            [[rules]]
            account = "codex/work"
            [rules.policy.compaction]
            window = 280000
            [[rules]]
            model = "codex/gpt-6.1-terra"
            [rules.policy.compaction]
            window = 200000
            [[rules]]
            model = "codex/gpt-6.1-terra"
            account = "codex/work"
            starts_unix_seconds = 3600
            expires_unix_seconds = 10800
            windows = [{ start_hour = 1, end_hour = 2 }]
            [rules.policy.compaction]
            window = 300000
        "#,
        )?;
        let model = ModelId::parse("codex/gpt-6.1-terra")?;
        for (account, time, expected) in [
            (Some("work"), 3599, 280_000),
            (Some("work"), 3600, 300_000),
            (Some("work"), 7200, 280_000),
            (Some("work"), 10800, 280_000),
            (Some("other"), 3600, 200_000),
            (None, 3600, 200_000),
        ] {
            let effective = policies.effective(&model, account, time);
            assert_eq!(effective.policy.compaction.window, Some(expected));
            assert_eq!(effective.policy.cache.ttl_seconds, Some(300));
            assert_eq!(effective.policy.lane.stop_early_ok, Some(false));
        }
        assert!(
            UtcWindow {
                start_hour: 23,
                end_hour: 2
            }
            .active(0)
        );
        assert!(
            !UtcWindow {
                start_hour: 23,
                end_hour: 2
            }
            .active(7200)
        );
        Ok(())
    }

    #[test]
    fn compaction_economics_and_cache_share_preserve_unknowns()
    -> Result<(), Box<dyn std::error::Error>> {
        let model = ModelId::parse("claude/claude-opus-5-5")?;
        let policy = ModelPolicies::default().effective(&model, None, 0).policy;
        // (0.05*275000 + (5+1.25)*15000)/(0.05*(275000-15000)).
        assert_eq!(
            policy.break_even_turns(275_000, 15_000, 0),
            Some(107_500.0 / 13_000.0)
        );
        assert!(policy.break_even_turns(15_000, 15_000, 0).is_none());
        let deepseek = ModelPolicies::default()
            .effective(
                &ModelId::parse("opencode/opencode-go/deepseek-v4.1-flash")?,
                None,
                0,
            )
            .policy;
        let intent = RouteIntent {
            workload: Workload::ReadHeavy,
            writes: false,
        };
        assert_eq!(deepseek.cache_factor_basis_points(intent), 30_000);
        let deepseek_model = ModelId::parse("opencode/opencode-go/deepseek-v4.1-flash")?;
        for (time, expected) in [
            (3600, 15_000),
            (14_400, 30_000),
            (2 * 86_400 + 3600, 30_000),
        ] {
            assert_eq!(
                ModelPolicies::default()
                    .effective(&deepseek_model, None, time)
                    .policy
                    .cache_factor_basis_points(intent),
                expected
            );
        }
        assert_eq!(
            deepseek.break_even_turns(800_000, 15_000, 0),
            Some(76_000.0 / 15_700.0)
        );
        assert_eq!(cache_read_share(Some(10), Some(90), Some(0)), Some(0.9));
        assert_eq!(cache_read_share(Some(10), Some(90), None), None);
        assert_eq!(cache_read_share(Some(0), Some(0), Some(0)), None);
        assert_eq!(
            cache_read_share(Some(u64::MAX), Some(u64::MAX), Some(0)),
            Some(0.5)
        );
        Ok(())
    }

    #[test]
    fn launch_controls_result_sections_and_overrides() -> Result<(), Box<dyn std::error::Error>> {
        let policies = ModelPolicies::default();
        let claude = policies
            .effective(&ModelId::parse("claude/claude-sonnet-5-5")?, None, 0)
            .policy;
        assert_eq!(
            claude.launch_env("claude"),
            vec![("CLAUDE_CODE_AUTO_COMPACT_WINDOW".into(), "275000".into())]
        );
        assert!(claude.launch_env("codex").is_empty());
        let gemini = policies
            .effective(&ModelId::parse("agy/gemini-3.8-flash")?, None, 0)
            .policy;
        assert!(!gemini.result_valid(" \n"));
        assert!(!gemini.result_valid("the words ## Findings and ## Evidence"));
        assert!(gemini.result_valid("## Findings\nClean.\n## Evidence\nChecked the diff."));
        assert!(!gemini.permits(RouteIntent {
            writes: true,
            ..Default::default()
        }));
        let mut disabled = ModelPolicies {
            bundled: false,
            ..Default::default()
        };
        disabled.rules.push(ModelPolicyRule {
            expires_unix_seconds: Some(100),
            policy: gemini,
            ..Default::default()
        });
        assert_eq!(
            disabled
                .effective(&ModelId::parse("agy/gemini-3.8-flash")?, None, 100)
                .policy
                .result
                .check,
            None
        );
        Ok(())
    }

    #[test]
    fn bad_policy_values_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        for text in [
            "[defaults.cache]\nread_ratio=-1",
            "[defaults.cache]\nread_share_target=1.1",
            "[defaults.lane]\nmax_continuations=9",
            "[[rules]]\nwindows=[{start_hour=24,end_hour=1}]",
            "[[rules]]\nstarts_unix_seconds=10\nexpires_unix_seconds=5",
            "[defaults.result]\nsections=['']",
        ] {
            let policy: ModelPolicies = toml_edit::de::from_str(text)?;
            let mut errors = Vec::new();
            policy.validate(&mut errors);
            assert_eq!(errors.len(), 1, "{text}");
        }
        assert!(toml_edit::de::from_str::<ModelPolicies>("[defaults.cache]\nunknown=1").is_err());
        Ok(())
    }
}
