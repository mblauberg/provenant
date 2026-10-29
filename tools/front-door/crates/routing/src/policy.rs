//! Layered model guidance and enforced lane/result admission, evaluated in UTC.
use crate::{ModelId, RoutingError};
use provenant_config::SectionError;
use serde::{Deserialize, Serialize};
use std::io::Write;

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
#[serde(default)]
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
            lines.push("Carry the task to completion. Keep the schema v2 handoff current and repeat it as the final fenced handoff block. Use status done when finished, stopped_early when work remains, or blocked when a decision or external change is needed.".into());
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

/// Reusable activation schedule for policies, pacing overrides and future
/// time-scoped routing records. Weekdays use Monday-based indexes: Monday is 0.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Schedule {
    /// First active instant, in Unix seconds. Missing means already started.
    pub starts: Option<i64>,
    /// First inactive instant, in Unix seconds. Missing means no expiry.
    pub expires: Option<i64>,
    /// IANA time zone for hour windows. Missing means UTC.
    pub tz: Option<String>,
    /// Hour windows; empty means all hours.
    pub windows: Vec<ScheduleWindow>,
}
/// Most hour windows one schedule may carry.
pub const MAX_SCHEDULE_WINDOWS: usize = 16;
impl Schedule {
    pub fn active(&self, now: i64) -> bool {
        self.starts.is_none_or(|start| now >= start)
            && self.expires.is_none_or(|end| now < end)
            && (self.windows.is_empty()
                || self
                    .windows
                    .iter()
                    .any(|window| window.active(now, self.tz.as_deref())))
    }
    /// Seconds until expiry, zero once expired, `None` without an expiry.
    pub fn remaining(&self, now: i64) -> Option<i64> {
        self.expires.map(|end| end.saturating_sub(now).max(0))
    }
    /// Whether the bounds, windows, weekdays and time zone are well formed.
    pub fn valid(&self) -> bool {
        self.starts
            .zip(self.expires)
            .is_none_or(|(starts, expires)| starts < expires)
            && self
                .tz
                .as_deref()
                .is_none_or(|tz| jiff::tz::TimeZone::get(tz).is_ok())
            && self.windows.len() <= MAX_SCHEDULE_WINDOWS
            && self.windows.iter().all(ScheduleWindow::valid)
    }
    /// The schedule of a record whose scope was malformed: never valid and
    /// never active, so validation reports the whole record.
    pub(crate) fn malformed() -> Self {
        Self {
            starts: Some(1),
            expires: Some(0),
            ..Self::default()
        }
    }
}

/// A record's selector or schedule as written. A value of the wrong type
/// decodes as malformed instead of failing, so validation drops or refuses the
/// whole record. Dropping only the field would widen where or when it applies.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum Scoped<T> {
    Valid(T),
    Malformed(serde::de::IgnoredAny),
}
impl<T> Scoped<T> {
    /// The written value, or `None` after flagging a malformed one.
    pub(crate) fn take(field: Option<Self>, malformed: &mut bool) -> Option<T> {
        match field? {
            Self::Valid(value) => Some(value),
            Self::Malformed(_) => {
                *malformed = true;
                None
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleWindow {
    /// Monday-based weekday indexes, Monday = 0 through Sunday = 6. Empty means every day.
    #[serde(default)]
    pub weekdays: Vec<u8>,
    pub start_hour: u8,
    pub end_hour: u8,
}
impl ScheduleWindow {
    fn valid(&self) -> bool {
        let mut days = self.weekdays.clone();
        days.sort_unstable();
        days.dedup();
        self.start_hour < 24
            && self.end_hour <= 24
            && self.start_hour != self.end_hour
            && days.len() == self.weekdays.len()
            && days.iter().all(|day| *day <= 6)
    }
    fn active(&self, now: i64, tz: Option<&str>) -> bool {
        let zone = tz
            .and_then(|tz| jiff::tz::TimeZone::get(tz).ok())
            .unwrap_or(jiff::tz::TimeZone::UTC);
        let Ok(local) = jiff::Timestamp::from_second(now) else {
            return false;
        };
        let local = local.to_zoned(zone);
        let hour = i64::from(local.hour());
        let mut day = local.date().weekday().to_monday_zero_offset() as u8;
        if self.start_hour >= self.end_hour && hour < i64::from(self.end_hour) {
            day = (day + 6) % 7;
        }
        if !self.weekdays.is_empty() && !self.weekdays.contains(&day) {
            return false;
        }
        if self.start_hour < self.end_hour {
            hour >= i64::from(self.start_hour) && hour < i64::from(self.end_hour)
        } else {
            hour >= i64::from(self.start_hour) || hour < i64::from(self.end_hour)
        }
    }
}

/// Timing keys that predate [`Schedule`], each with the code naming the
/// `schedule` key that replaced it. Records that still carry one are refused.
pub(crate) const RETIRED_SCHEDULE_KEYS: [(&str, &str); 3] = [
    ("starts_unix_seconds", "moved_to_schedule.starts"),
    ("expires_unix_seconds", "moved_to_schedule.expires"),
    ("windows", "moved_to_schedule.windows"),
];
/// Report each retired timing key in the array of records at `path`, so the
/// warning names its replacement instead of dropping the key as unknown, which
/// would leave the record active without its bounds. A tolerant record is
/// reported at the record, so recovery drops only that record. A `strict`
/// record is reported at the key itself, which [`boundary`] refuses.
pub(crate) fn retired_schedule_keys(
    table: &toml_edit::Table,
    path: &[&str],
    strict: impl Fn(&toml_edit::Table) -> bool,
    errors: &mut Vec<SectionError>,
) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let Some(item) = parents
        .iter()
        .try_fold(table as &dyn toml_edit::TableLike, |table, key| {
            table.get(key).and_then(toml_edit::Item::as_table_like)
        })
        .and_then(|table| table.get(last))
    else {
        return;
    };
    let rows: Vec<toml_edit::Table> = match item {
        toml_edit::Item::ArrayOfTables(rows) => rows.iter().cloned().collect(),
        toml_edit::Item::Value(toml_edit::Value::Array(rows)) => rows
            .iter()
            .filter_map(|row| {
                row.as_inline_table()
                    .cloned()
                    .map(toml_edit::InlineTable::into_table)
            })
            .collect(),
        _ => return,
    };
    for (index, row) in rows.iter().enumerate() {
        let record = format!("{}.{index}", path.join("."));
        for (key, code) in RETIRED_SCHEDULE_KEYS {
            if row.contains_key(key) {
                errors.push(SectionError {
                    field: if strict(row) {
                        format!("{record}.{key}")
                    } else {
                        record.clone()
                    },
                    code,
                });
            }
        }
    }
}
/// Whether a policy rule row sets an enforced lane restriction, so a retired
/// key in it refuses the configuration. A row that does not decode counts as
/// restricting.
pub(crate) fn restricting_rule(row: &toml_edit::Table) -> bool {
    toml_edit::de::from_document::<ModelPolicyRule>(toml_edit::DocumentMut::from(row.clone()))
        .map_or(true, |rule| rule.policy.restricts())
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicyRule {
    pub adapter: Option<String>,
    pub model: Option<ModelId>,
    /// Match the provider model's suffix, for open-ended catalogue classes.
    pub model_suffix: Option<String>,
    /// Canonical adapter/key or a registry-free account key.
    pub account: Option<String>,
    pub schedule: Schedule,
    pub policy: ModelPolicy,
}
impl<'de> Deserialize<'de> for ModelPolicyRule {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Retired timing keys decode so validation can name their
        /// replacement; their values are never read.
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Input {
            adapter: Option<Scoped<String>>,
            model: Option<Scoped<ModelId>>,
            model_suffix: Option<Scoped<String>>,
            account: Option<Scoped<String>>,
            schedule: Option<Scoped<Schedule>>,
            policy: ModelPolicy,
            #[serde(rename = "starts_unix_seconds")]
            _starts: Option<serde::de::IgnoredAny>,
            #[serde(rename = "expires_unix_seconds")]
            _expires: Option<serde::de::IgnoredAny>,
            #[serde(rename = "windows")]
            _windows: Option<serde::de::IgnoredAny>,
        }
        let input = Input::deserialize(deserializer)?;
        let mut malformed = false;
        let adapter = Scoped::take(input.adapter, &mut malformed);
        let model = Scoped::take(input.model, &mut malformed);
        let model_suffix = Scoped::take(input.model_suffix, &mut malformed);
        let account = Scoped::take(input.account, &mut malformed);
        let schedule = Scoped::take(input.schedule, &mut malformed).unwrap_or_default();
        Ok(Self {
            adapter,
            model,
            model_suffix,
            account,
            schedule: if malformed {
                Schedule::malformed()
            } else {
                schedule
            },
            policy: input.policy,
        })
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicies {
    pub bundled: bool,
    pub defaults: ModelPolicy,
    pub rules: Vec<ModelPolicyRule>,
}
impl<'de> Deserialize<'de> for ModelPolicies {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Input {
            #[serde(default = "default_bundled")]
            bundled: bool,
            defaults: ModelPolicy,
            rules: Vec<ModelPolicyRule>,
        }
        let input = Input::deserialize(deserializer)?;
        let defaults = input.defaults;
        let rules = input.rules;
        if let Ok(catalogue) = crate::Catalogue::bundled() {
            warn_invalid_fallback(&defaults, "model_policy.defaults", &catalogue);
            for (index, rule) in rules.iter().enumerate() {
                warn_invalid_fallback(
                    &rule.policy,
                    &format!("model_policy.rules.{index}"),
                    &catalogue,
                );
            }
        }
        Ok(Self {
            bundled: input.bundled,
            defaults,
            rules,
        })
    }
}
/// Why a result fallback cannot run on the catalogue, or `None` when it can.
fn fallback_problem(result: &ResultPolicy, catalogue: &crate::Catalogue) -> Option<&'static str> {
    let model = result.fallback.as_ref()?;
    let Some(entry) = catalogue.model(model) else {
        return Some("unknown model");
    };
    result
        .fallback_effort
        .as_ref()
        .is_some_and(|effort| !entry.efforts.contains(effort))
        .then_some("unsupported effort")
}
fn warn_invalid_fallback(policy: &ModelPolicy, field: &str, catalogue: &crate::Catalogue) {
    let Some(model) = policy.result.fallback.as_ref() else {
        return;
    };
    if let Some(reason) = fallback_problem(&policy.result, catalogue) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: domains.routing.{field}.result.fallback: {} ({reason}); effective fallback checked after overlays",
            model.as_str()
        );
    }
}
fn default_bundled() -> bool {
    true
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
    /// The result fallback that the effective policy rejected, and why.
    #[serde(skip)]
    pub dropped_fallback: Option<(ModelId, &'static str)>,
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
        self.apply_rules(&mut policy, &mut sources, model, account, now);
        let mut dropped_fallback = None;
        if let Some(model) = policy.result.fallback.clone() {
            let problem = crate::Catalogue::bundled()
                .map_or(Some("catalogue unavailable"), |catalogue| {
                    fallback_problem(&policy.result, &catalogue)
                });
            if let Some(reason) = problem {
                policy.result.fallback = None;
                policy.result.fallback_effort = None;
                dropped_fallback = Some((model, reason));
            }
        }
        EffectiveModelPolicy {
            policy,
            sources,
            dropped_fallback,
        }
    }

    fn apply_rules(
        &self,
        policy: &mut ModelPolicy,
        sources: &mut Vec<String>,
        model: &ModelId,
        account: Option<&str>,
        now: i64,
    ) {
        // Scope outranks array position. Later rules win within one scope.
        let mut matching: Vec<_> = self
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| {
                rule.scope_valid()
                    && rule.adapter.as_deref().is_none_or(|a| a == model.adapter())
                    && rule.model.as_ref().is_none_or(|m| m == model)
                    && rule
                        .model_suffix
                        .as_deref()
                        .is_none_or(|suffix| model.model().ends_with(suffix))
                    && rule.account.as_deref().is_none_or(|a| {
                        account.is_some_and(|key| {
                            a == key || a == format!("{}/{key}", model.adapter())
                        })
                    })
                    && rule.schedule.active(now)
            })
            .collect();
        matching.sort_by_key(|(index, r)| {
            (
                u8::from(r.adapter.is_some())
                    + 2 * u8::from(r.model.is_some() || r.model_suffix.is_some())
                    + 4 * u8::from(r.account.is_some()),
                *index,
            )
        });
        for (index, rule) in matching {
            policy.overlay(&rule.policy);
            sources.push(format!("rules[{index}]"));
        }
    }

    /// Each invalid value is reported at its own field so recovery drops only
    /// that value and its valid siblings, such as `lane.read_only`, survive.
    /// Input whose recovery would lift an enforced lane restriction reports
    /// code `restriction`, a configuration boundary.
    pub(crate) fn validate(&self, errors: &mut Vec<SectionError>) {
        if self.rules.len() > 128 {
            errors.push(SectionError {
                field: "model_policy.rules".into(),
                code: if self.rules.iter().any(|rule| rule.policy.restricts()) {
                    "restriction"
                } else {
                    "limit"
                },
            });
        }
        for (field, code) in self.defaults.invalid_fields() {
            errors.push(SectionError {
                field: format!("model_policy.defaults.{field}"),
                code,
            });
        }
        for (i, rule) in self.rules.iter().enumerate() {
            for (field, code) in rule.policy.invalid_fields() {
                errors.push(SectionError {
                    field: format!("model_policy.rules.{i}.policy.{field}"),
                    code,
                });
            }
            // A malformed selector or schedule drops the whole rule, since a
            // rule without either would apply more widely than written.
            if !rule.scope_valid() {
                errors.push(SectionError {
                    field: format!("model_policy.rules.{i}"),
                    code: if rule.policy.restricts() {
                        "restriction"
                    } else {
                        "policy"
                    },
                });
            }
        }
    }
}

/// Model policy paths whose loss would lift an enforced lane restriction.
const ENFORCED: [&str; 3] = ["max_context", "max_turns", "read_only"];

/// Whether invalid routing input must refuse the configuration: an enforced
/// restriction, or a malformed value at or above one, which recovery would drop.
pub(crate) fn boundary(error: &SectionError) -> bool {
    // A retired key reported at the key, not its record, is in a restricting
    // rule; a record-level report recovers by dropping the record.
    if let Some((key, _)) = RETIRED_SCHEDULE_KEYS
        .iter()
        .find(|(_, code)| *code == error.code)
    {
        return error.field.rsplit('.').next() == Some(*key);
    }
    if error.code == "restriction" {
        return true;
    }
    if error.code == "policy" {
        return false;
    }
    let Some(rest) = error.field.strip_prefix("model_policy") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').filter(|part| !part.is_empty()).collect();
    let policy = match parts.as_slice() {
        [] | ["rules"] | ["rules", _] => return true,
        ["defaults", policy @ ..] => policy,
        ["rules", index, "policy", policy @ ..] if index.parse::<usize>().is_ok() => policy,
        _ => return false,
    };
    match policy {
        [] | ["lane"] => true,
        ["lane", leaf, ..] => ENFORCED.contains(leaf),
        _ => false,
    }
}

impl ModelPolicyRule {
    fn scope_valid(&self) -> bool {
        !(self.adapter.as_deref().is_some_and(|a| {
            !crate::request::identifier(a) || self.model.as_ref().is_some_and(|m| m.adapter() != a)
        }) || self.model_suffix.as_deref().is_some_and(|suffix| {
            suffix.is_empty() || suffix.len() > 128 || suffix.chars().any(char::is_control)
        }) || self.account.as_deref().is_some_and(|a| {
            a.split('/').count() > 2 || a.split('/').any(|part| !crate::request::identifier(part))
        }) || !self.schedule.valid())
    }
}

impl ModelPolicy {
    /// Whether this policy sets an enforced lane restriction.
    fn restricts(&self) -> bool {
        self.lane.read_only == Some(true)
            || self.lane.max_context.is_some()
            || self.lane.max_turns.is_some()
    }

    /// Policy-relative paths of invalid values, each with its code.
    fn invalid_fields(&self) -> Vec<(&'static str, &'static str)> {
        let ratio =
            |n: Option<f64>| n.is_some_and(|n| !n.is_finite() || !(0.0..=1000.0).contains(&n));
        let checks = [
            ("cache.read_ratio", ratio(self.cache.read_ratio), "policy"),
            ("cache.write_ratio", ratio(self.cache.write_ratio), "policy"),
            (
                "cache.output_ratio",
                ratio(self.cache.output_ratio),
                "policy",
            ),
            (
                "cache.price_multiplier",
                self.cache
                    .price_multiplier
                    .is_some_and(|n| !n.is_finite() || !(0.01..=1000.0).contains(&n)),
                "policy",
            ),
            (
                "cache.read_share_target",
                self.cache
                    .read_share_target
                    .is_some_and(|n| !n.is_finite() || !(0.0..=1.0).contains(&n)),
                "policy",
            ),
            (
                "compaction.window",
                self.compaction.window == Some(0),
                "policy",
            ),
            (
                "cache.context_window",
                self.cache.context_window == Some(0),
                "policy",
            ),
            (
                "cache.ttl_seconds",
                self.cache.ttl_seconds == Some(0),
                "policy",
            ),
            // A zero cap is invalid; dropping it would leave the lane uncapped.
            (
                "lane.max_context",
                self.lane.max_context == Some(0),
                "restriction",
            ),
            (
                "lane.max_turns",
                self.lane.max_turns == Some(0),
                "restriction",
            ),
            (
                "lane.max_continuations",
                self.lane.max_continuations.is_some_and(|n| n > 8),
                "policy",
            ),
            (
                "cache.read_heavy_weight",
                self.cache
                    .read_heavy_weight
                    .is_some_and(|n| n == 0 || n > 10),
                "policy",
            ),
            (
                "result.sections",
                self.result.sections.as_ref().is_some_and(|s| {
                    s.len() > 16
                        || s.iter().any(|s| {
                            s.trim().is_empty() || s.len() > 256 || s.chars().any(char::is_control)
                        })
                }),
                "policy",
            ),
            (
                "result.fallback_effort",
                self.result
                    .fallback_effort
                    .as_deref()
                    .is_some_and(|e| !crate::request::effort_valid(e)),
                "policy",
            ),
        ];
        checks
            .into_iter()
            .filter(|(_, invalid, _)| *invalid)
            .map(|(field, _, code)| (field, code))
            .collect()
    }
}

// Invalid embedded data is a build defect, never a recoverable user configuration.
#[allow(clippy::expect_used)]
fn bundled_policy(model: &ModelId, now: i64) -> ModelPolicy {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Bundled {
        defaults: ModelPolicy,
        rules: Vec<ModelPolicyRule>,
    }
    static BUNDLED: std::sync::LazyLock<ModelPolicies> = std::sync::LazyLock::new(|| {
        let asset: Bundled = toml_edit::de::from_str(include_str!("../assets/model-policy.toml"))
            .expect("valid bundled model policy");
        let policies = ModelPolicies {
            bundled: false,
            defaults: asset.defaults,
            rules: asset.rules,
        };
        let mut errors = Vec::new();
        policies.validate(&mut errors);
        assert!(
            errors.is_empty(),
            "invalid bundled model policy: {errors:?}"
        );
        policies
    });
    let mut policy = BUNDLED.defaults.clone();
    BUNDLED.apply_rules(&mut policy, &mut Vec::new(), model, None, now);
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

    #[test]
    fn malformed_suffix_selectors_do_not_broaden_rules() -> Result<(), Box<dyn std::error::Error>> {
        for suffix in [
            serde_json::json!(""),
            serde_json::json!("\n"),
            serde_json::json!("x".repeat(129)),
            serde_json::json!(7),
        ] {
            let policies: ModelPolicies = serde_json::from_value(serde_json::json!({"rules": [{
                "adapter": "opencode", "model_suffix": suffix,
                "policy": {"cache": {"read_ratio": 0.5}}
            }]}))?;
            let mut errors = Vec::new();
            policies.validate(&mut errors);
            assert!(errors.iter().any(|e| e.field == "model_policy.rules.0"));
            assert_eq!(
                policies
                    .effective(&ModelId::parse("opencode/unknown-free")?, None, 0)
                    .policy
                    .cache
                    .read_ratio,
                None
            );
        }
        Ok(())
    }

    #[test]
    fn bundled_policy_matches_pre_data_golden() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Deserialize)]
        struct Case {
            model: ModelId,
            at: i64,
            policy: ModelPolicy,
        }
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../assets/model-policy-golden.json"))?;
        for case in cases {
            assert_eq!(
                serde_json::to_value(
                    ModelPolicies::default()
                        .effective(&case.model, None, case.at)
                        .policy
                )?,
                serde_json::to_value(case.policy)?,
                "{} at {}",
                case.model.as_str(),
                case.at
            );
        }
        Ok(())
    }

    #[test]
    fn bundled_fields_and_peak_schedule_can_be_overridden_per_account()
    -> Result<(), Box<dyn std::error::Error>> {
        let policies: ModelPolicies = toml_edit::de::from_str(
            r#"
            [defaults.cache]
            read_share_target = 0.8
            [[rules]]
            adapter = "claude"
            account = "work"
            [rules.policy.compaction]
            window = 400000
            [[rules]]
            model = "opencode/opencode-go/deepseek-v4.1-flash"
            account = "work"
            [rules.schedule]
            windows = [{ start_hour = 2, end_hour = 3 }]
            [rules.policy.cache]
            read_ratio = 0.04
            price_multiplier = 3.0
            [[rules]]
            model = "codex/gpt-6.1-terra"
            account = "work"
            [rules.policy.lane]
            stop_early_ok = true
            max_continuations = 0
            [[rules]]
            adapter = "opencode"
            model_suffix = "-free"
            account = "work"
            [rules.policy.lane]
            read_only = false
        "#,
        )?;
        let claude = policies
            .effective(&ModelId::parse("claude/claude-opus-5-5")?, Some("work"), 0)
            .policy;
        assert_eq!(claude.compaction.window, Some(400000));
        assert_eq!(claude.cache.read_share_target, Some(0.8));
        let deepseek = ModelId::parse("opencode/opencode-go/deepseek-v4.1-flash")?;
        for (account, hour, expected) in [
            ("work", 1, 2.0),
            ("work", 2, 3.0),
            ("work", 3, 2.0),
            ("other", 2, 2.0),
        ] {
            let policy = policies
                .effective(&deepseek, Some(account), 4 * 86400 + hour * 3600)
                .policy;
            assert_eq!(policy.cache.price_multiplier, Some(expected));
            assert_eq!(
                policy.cache.read_ratio,
                Some(if account == "work" && hour == 2 {
                    0.04
                } else {
                    0.02
                })
            );
        }
        let terra = policies
            .effective(
                &ModelId::parse("codex/gpt-6.1-terra")?,
                Some("codex/work"),
                0,
            )
            .policy;
        assert_eq!(terra.lane.stop_early_ok, Some(true));
        assert_eq!(terra.lane.max_continuations, Some(0));
        let free = policies
            .effective(&ModelId::parse("opencode/unknown-free")?, Some("work"), 0)
            .policy;
        assert_eq!(free.lane.read_only, Some(false));
        Ok(())
    }

    #[test]
    fn bundled_deepseek_schedule_preserves_every_hour_of_the_week()
    -> Result<(), Box<dyn std::error::Error>> {
        let hours = [1, 4, 6, 10];
        let multiplier = 2.0;
        let model = "opencode/opencode-go/deepseek-v4.1-flash";
        let model = ModelId::parse(model)?;
        // 1970-01-05 is a Monday; sample every hour of a full week. // timebomb: allow evaluated at an injected instant
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
            [rules.schedule]
            starts = 3600
            expires = 10800
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
            ScheduleWindow {
                weekdays: vec![],
                start_hour: 23,
                end_hour: 2
            }
            .active(0, None)
        );
        assert!(
            !ScheduleWindow {
                weekdays: vec![],
                start_hour: 23,
                end_hour: 2
            }
            .active(7200, None)
        );
        let weekdays = ScheduleWindow {
            weekdays: vec![3], // Thursday, ISO Monday = 0.
            start_hour: 1,
            end_hour: 4,
        };
        assert!(weekdays.active(3600, None));
        assert!(!weekdays.active(86_400 + 3600, None));
        assert!(
            !Schedule {
                starts: Some(10),
                expires: Some(20),
                tz: None,
                windows: vec![]
            }
            .active(9)
        );
        assert!(
            Schedule {
                starts: Some(10),
                expires: Some(20),
                tz: None,
                windows: vec![]
            }
            .active(10)
        );
        Ok(())
    }

    #[test]
    fn fallback_effort_is_validated_after_policy_layers_are_overlaid()
    -> Result<(), Box<dyn std::error::Error>> {
        let policies: ModelPolicies = toml_edit::de::from_str(
            "[defaults.result]\nfallback='codex/gpt-6-luna'\nfallback_effort='max'\n[[rules]]\naccount='work'\n[rules.policy.result]\nfallback_effort='high'\n",
        )?;
        assert_eq!(
            policies.defaults.result.fallback_effort.as_deref(),
            Some("max")
        );
        let effective = policies.effective(
            &ModelId::parse("codex/gpt-6-luna")?,
            Some("work"),
            jiff::Timestamp::now().as_second(),
        );
        assert_eq!(
            effective
                .policy
                .result
                .fallback
                .as_ref()
                .map(ModelId::as_str),
            Some("codex/gpt-6-luna")
        );
        assert_eq!(
            effective.policy.result.fallback_effort.as_deref(),
            Some("high")
        );
        assert!(effective.dropped_fallback.is_none());
        let other = policies.effective(
            &ModelId::parse("codex/gpt-6-luna")?,
            Some("other"),
            jiff::Timestamp::now().as_second(),
        );
        assert!(other.policy.result.fallback.is_none());
        assert_eq!(
            other.dropped_fallback,
            Some((ModelId::parse("codex/gpt-6-luna")?, "unsupported effort"))
        );
        Ok(())
    }

    #[test]
    fn shared_schedule_evaluator_handles_weekday_and_local_windows()
    -> Result<(), Box<dyn std::error::Error>> {
        let model = ModelId::parse("codex/gpt-6.1-terra")?;
        let starts = "2026-10-29T23:00:00Z" // timebomb: allow evaluated at an injected instant
            .parse::<jiff::Timestamp>()?
            .as_second();
        let canonical: ModelPolicies = toml_edit::de::from_str(&format!(
            "[[rules]]\nmodel='codex/gpt-6.1-terra'\n[rules.schedule]\nstarts={starts}\nwindows=[{{weekdays=[3],start_hour=23,end_hour=2}}]\n[rules.policy.lane]\nmax_turns=17\n"
        ))?;
        assert_eq!(
            canonical
                .effective(&model, None, starts)
                .policy
                .lane
                .max_turns,
            Some(17)
        );
        let sunday_monday = Schedule {
            windows: vec![ScheduleWindow {
                weekdays: vec![6],
                start_hour: 23,
                end_hour: 2,
            }],
            ..Default::default()
        };
        assert!(sunday_monday.active(3 * 86_400 + 23 * 3600));
        assert!(sunday_monday.active(4 * 86_400 + 3600));
        let policies = ModelPolicies {
            bundled: false,
            rules: vec![ModelPolicyRule {
                schedule: sunday_monday,
                policy: ModelPolicy {
                    lane: LanePolicy {
                        max_turns: Some(17),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        for time in [3 * 86_400 + 23 * 3600, 4 * 86_400 + 3600] {
            assert_eq!(
                policies.effective(&model, None, time).policy.lane.max_turns,
                Some(17)
            );
        }
        let dst: Schedule = toml_edit::de::from_str(
            "tz='America/New_York'\nwindows=[{weekdays=[6],start_hour=1,end_hour=4}]",
        )?;
        assert!(
            dst.active(
                "2026-03-08T06:30:00Z" // timebomb: allow evaluated at an injected instant
                    .parse::<jiff::Timestamp>()?
                    .as_second()
            )
        );
        assert!(
            dst.active(
                "2026-03-08T07:30:00Z" // timebomb: allow evaluated at an injected instant
                    .parse::<jiff::Timestamp>()?
                    .as_second()
            )
        );
        assert!(dst.valid());
        Ok(())
    }

    #[test]
    fn schedule_validation_refuses_malformed_bounds_windows_and_zones()
    -> Result<(), Box<dyn std::error::Error>> {
        let window = |weekdays: Vec<u8>, start_hour, end_hour| ScheduleWindow {
            weekdays,
            start_hour,
            end_hour,
        };
        let valid = Schedule {
            starts: Some(1),
            expires: Some(2),
            tz: Some("Australia/Sydney".into()),
            windows: vec![window(vec![0, 4], 9, 17), window(vec![], 23, 2)],
        };
        assert!(valid.valid());
        assert_eq!(valid.remaining(0), Some(2));
        assert_eq!(valid.remaining(5), Some(0));
        assert_eq!(Schedule::default().remaining(5), None);
        for invalid in [
            Schedule {
                starts: Some(2),
                expires: Some(2),
                ..Default::default()
            },
            Schedule {
                tz: Some("Mars/Olympus_Mons".into()),
                ..Default::default()
            },
            Schedule {
                windows: vec![window(vec![], 9, 17); MAX_SCHEDULE_WINDOWS + 1],
                ..Default::default()
            },
            Schedule {
                windows: vec![window(vec![7], 9, 17)],
                ..Default::default()
            },
            Schedule {
                windows: vec![window(vec![1, 1], 9, 17)],
                ..Default::default()
            },
            Schedule {
                windows: vec![window(vec![], 9, 9)],
                ..Default::default()
            },
            Schedule {
                windows: vec![window(vec![], 24, 2)],
                ..Default::default()
            },
        ] {
            assert!(!invalid.valid(), "{invalid:?}");
        }
        // A rule with a malformed schedule is dropped at load, not applied
        // more widely than written.
        let policies: ModelPolicies = toml_edit::de::from_str(
            "[[rules]]\nmodel='codex/gpt-6.1-terra'\n[rules.schedule]\ntz='Mars/Olympus_Mons'\n[rules.policy.cache]\nttl_seconds=60\n",
        )?;
        let mut errors = Vec::new();
        policies.validate(&mut errors);
        assert_eq!(
            errors,
            [SectionError {
                field: "model_policy.rules.0".into(),
                code: "policy",
            }]
        );
        Ok(())
    }

    #[test]
    fn retired_schedule_keys_drop_their_record_unless_it_restricts()
    -> Result<(), Box<dyn std::error::Error>> {
        let table: toml_edit::DocumentMut = "pacing_overrides=[{provider='codex',policy='pace',expires_unix_seconds=9}]\n[[model_policy.rules]]\nstarts_unix_seconds=1\nwindows=[]\n[[model_policy.rules]]\nexpires_unix_seconds=1\n[model_policy.rules.policy.lane]\nread_only=true\n".parse()?;
        let errors = (crate::SECTION.validate)(table.as_table());
        let named: Vec<_> = errors
            .iter()
            .filter(|error| error.code.starts_with("moved_to_schedule."))
            .map(|error| {
                (
                    error.field.as_str(),
                    error.code,
                    (crate::SECTION.boundary)(error),
                )
            })
            .collect();
        assert_eq!(
            named,
            [
                ("pacing_overrides.0", "moved_to_schedule.expires", false),
                ("model_policy.rules.0", "moved_to_schedule.starts", false),
                ("model_policy.rules.0", "moved_to_schedule.windows", false),
                (
                    "model_policy.rules.1.expires_unix_seconds",
                    "moved_to_schedule.expires",
                    true
                ),
            ]
        );
        // Serde never reads a retired value into the schedule.
        let rule: ModelPolicyRule = toml_edit::de::from_str("expires_unix_seconds=1\n")?;
        assert_eq!(rule.schedule, Schedule::default());
        Ok(())
    }

    #[test]
    fn schedule_expiry_is_evaluated_at_the_supplied_instant()
    -> Result<(), Box<dyn std::error::Error>> {
        let model = ModelId::parse("codex/gpt-6.1-terra")?;
        let policies: ModelPolicies = toml_edit::de::from_str(
            "[[rules]]\nmodel='codex/gpt-6.1-terra'\nschedule={expires=100}\n[rules.policy.lane]\nmax_turns=17\n",
        )?;

        assert_eq!(
            policies.effective(&model, None, 99).policy.lane.max_turns,
            Some(17)
        );
        assert_eq!(
            policies.effective(&model, None, 100).policy.lane.max_turns,
            None
        );
        assert_eq!(
            policies.effective(&model, None, 101).policy.lane.max_turns,
            None
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
            schedule: Schedule {
                expires: Some(100),
                ..Default::default()
            },
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
            "[[rules]]\n[rules.schedule]\nwindows=[{start_hour=24,end_hour=1}]",
            "[[rules]]\nschedule={ starts=10, expires=5 }",
            "[defaults.result]\nsections=['']",
        ] {
            let policy: ModelPolicies = toml_edit::de::from_str(text)?;
            let mut errors = Vec::new();
            policy.validate(&mut errors);
            assert_eq!(errors.len(), 1, "{text}");
        }
        assert!(toml_edit::de::from_str::<ModelPolicies>("[defaults.cache]\nunknown=1").is_err());
        let drifted: ModelPolicies = toml_edit::de::from_str(
            "[[rules]]\n[rules.policy.result]\nfallback='codex/not-catalogued'\n",
        )?;
        assert_eq!(
            drifted.rules[0]
                .policy
                .result
                .fallback
                .as_ref()
                .map(ModelId::as_str),
            Some("codex/not-catalogued")
        );
        assert!(
            drifted
                .effective(&ModelId::parse("codex/gpt-6-luna")?, None, 0)
                .policy
                .result
                .fallback
                .is_none()
        );
        Ok(())
    }
}
