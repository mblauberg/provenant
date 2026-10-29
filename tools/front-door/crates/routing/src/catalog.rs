use crate::policy::Scoped;
use crate::{
    ModelId, RoutingError,
    request::{effort_valid, identifier},
};
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PacingPolicy {
    Pace,
    Spend,
    Max,
    Off,
}

/// Provider defaults remain directly under `[pacing]`; account profiles live
/// under `[pacing.accounts.<account>]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PacingSettings {
    pub accounts: BTreeMap<String, PacingProfile>,
    pub free: BTreeMap<String, PacingProfile>,
    #[serde(flatten)]
    pub providers: BTreeMap<String, PacingPolicy>,
}
impl std::ops::Deref for PacingSettings {
    type Target = BTreeMap<String, PacingPolicy>;
    fn deref(&self) -> &Self::Target {
        &self.providers
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacingWindow {
    FiveHour,
    SevenDay,
    Monthly,
    Rolling,
    Daily,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterDeadline {
    #[default]
    Standby,
    Continue,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PacingProfile {
    pub policy: Option<PacingPolicy>,
    pub windows: Option<Vec<PacingWindow>>,
    pub reserve: BTreeMap<PacingWindow, u8>,
    pub deadline: Option<PacingDeadline>,
    pub after_deadline: AfterDeadline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PacingOverride {
    pub account: Option<String>,
    pub provider: Option<String>,
    pub policy: PacingPolicy,
    #[serde(default)]
    pub schedule: crate::Schedule,
}
impl<'de> Deserialize<'de> for PacingOverride {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Retired timing keys decode so validation can name their
        /// replacement; their values are never read.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            account: Option<Scoped<String>>,
            provider: Option<Scoped<String>>,
            policy: PacingPolicy,
            #[serde(default)]
            schedule: Option<Scoped<crate::Schedule>>,
            #[serde(rename = "starts_unix_seconds")]
            _starts: Option<serde::de::IgnoredAny>,
            #[serde(rename = "expires_unix_seconds")]
            _expires: Option<serde::de::IgnoredAny>,
            #[serde(rename = "windows")]
            _windows: Option<serde::de::IgnoredAny>,
        }
        let input = Input::deserialize(deserializer)?;
        let mut malformed = false;
        let account = Scoped::take(input.account, &mut malformed);
        let provider = Scoped::take(input.provider, &mut malformed);
        let schedule = Scoped::take(input.schedule, &mut malformed).unwrap_or_default();
        Ok(Self {
            account,
            provider,
            policy: input.policy,
            schedule: if malformed {
                crate::Schedule::malformed()
            } else {
                schedule
            },
        })
    }
}

/// Fixed deadlines use host-local time. Reset references use fresh quota facts.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PacingDeadline {
    ResetOf(ResetDeadline),
    Fixed(FixedDeadline),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetDeadline {
    pub reset_of: String,
    #[serde(default = "default_deadline_window")]
    pub window: PacingWindow,
}
fn default_deadline_window() -> PacingWindow {
    PacingWindow::SevenDay
}

impl<'de> Deserialize<'de> for PacingDeadline {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = PacingDeadline;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "a weekly, monthly or absolute deadline string, or { reset_of, window? }",
                )
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                FixedDeadline::try_from(value.to_owned())
                    .map(PacingDeadline::Fixed)
                    .map_err(E::custom)
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Self::Value, M::Error> {
                ResetDeadline::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(PacingDeadline::ResetOf)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum FixedDeadline {
    Weekly { weekday: u8, hour: i8, minute: i8 },
    Monthly { day: i8 },
    Absolute(jiff::civil::DateTime),
}
const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
impl TryFrom<String> for FixedDeadline {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let parts: Vec<_> = value.split_whitespace().collect();
        match parts.as_slice() {
            [day, time] if WEEKDAYS.contains(day) => {
                if let Some((hour, minute)) = time.split_once(':')
                    && hour.len() == 2
                    && minute.len() == 2
                    && let (Ok(hour), Ok(minute)) = (hour.parse::<i8>(), minute.parse::<i8>())
                    && (0..24).contains(&hour)
                    && (0..60).contains(&minute)
                {
                    return Ok(Self::Weekly {
                        weekday: WEEKDAYS.iter().position(|d| d == day).unwrap_or(0) as u8,
                        hour,
                        minute,
                    });
                }
            }
            ["day", day] => {
                if let Ok(day) = day.parse::<i8>()
                    && (1..=31).contains(&day)
                {
                    return Ok(Self::Monthly { day });
                }
            }
            [_] => {
                if let Ok(datetime) = value.parse::<jiff::civil::DateTime>() {
                    return Ok(Self::Absolute(datetime));
                }
            }
            _ => {}
        }
        Err(
            "deadline must be 'thu 00:00', 'day 25', a local ISO datetime, or { reset_of = 'ACCOUNT', window = 'seven_day' }",
        )
    }
}
impl From<FixedDeadline> for String {
    fn from(value: FixedDeadline) -> Self {
        match value {
            FixedDeadline::Weekly {
                weekday,
                hour,
                minute,
            } => format!("{} {hour:02}:{minute:02}", WEEKDAYS[usize::from(weekday)]),
            FixedDeadline::Monthly { day } => format!("day {day}"),
            FixedDeadline::Absolute(datetime) => datetime.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
/// Routing settings. Library callers should resolve account selectors to
/// `adapter/key` before routing. Bare account keys remain supported for
/// registry-free callers and can match accounts across adapters, including
/// profile, reset reference and override selectors.
pub struct RoutingConfig {
    pub model_policy: crate::ModelPolicies,
    /// Override the bundled writer advice; omitted fields keep the bundled values.
    pub writer_advisory: Option<WriterAdvisoryOverride>,
    /// Silence the advisory for Opus implementation lanes.
    pub suppress_opus_writer_warning: bool,
    pub capacity_cooldown_seconds: u64,
    /// Further attempts after a capacity or quota settlement, on the same
    /// session where it resumes or a fresh draw where it cannot. Zero retries
    /// none; at most [`MAX_SETTLEMENT_RETRIES`].
    pub settlement_retries: u8,
    /// Wait before a capacity retry, multiplied by the retry's number.
    pub capacity_retry_seconds: u64,
    /// Cap on one capacity retry's wait, jitter included.
    pub capacity_retry_max_seconds: u64,
    /// Ceiling on any settlement retry's sealed wait, whatever its cause.
    pub retry_delay_ceiling_seconds: u64,
    pub review: crate::ReviewConfig,
    pub routes: BTreeMap<String, NamedRoute>,
    pub pools: BTreeMap<String, Vec<RoutePoolMember>>,
    pub pacing: PacingSettings,
    pub pacing_reserve: BTreeMap<String, PacingReserve>,
    pub pacing_overrides: Vec<PacingOverride>,
    pub opencode_go: OpenCodeGoSettings,
}

/// Settlement retries a lane may take; the run store seals at most this many
/// policy successors in one run.
pub const MAX_SETTLEMENT_RETRIES: u8 = 32;
/// Longest configurable settlement retry wait: one week.
pub const MAX_RETRY_WAIT_SECONDS: u64 = 7 * 24 * 3_600;

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            model_policy: crate::ModelPolicies::default(),
            writer_advisory: None,
            suppress_opus_writer_warning: false,
            capacity_cooldown_seconds: 600,
            settlement_retries: 2,
            capacity_retry_seconds: 30,
            capacity_retry_max_seconds: 600,
            retry_delay_ceiling_seconds: 3_600,
            review: crate::ReviewConfig::default(),
            routes: BTreeMap::new(),
            pools: BTreeMap::new(),
            pacing: PacingSettings::default(),
            pacing_reserve: BTreeMap::new(),
            pacing_overrides: Vec::new(),
            opencode_go: OpenCodeGoSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenCodeGoSettings {
    pub min_monthly_allowance_usd: u64,
}
impl Default for OpenCodeGoSettings {
    fn default() -> Self {
        Self {
            min_monthly_allowance_usd: 60,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PacingReserve {
    pub five_hour_percent: u8,
    pub seven_day_percent: u8,
}
impl Default for PacingReserve {
    fn default() -> Self {
        Self {
            five_hour_percent: 15,
            seven_day_percent: 10,
        }
    }
}
impl RoutingConfig {
    /// Dispatch warning for each configured pool member whose efforts no
    /// longer appear in the catalogue. The efforts are dropped at resolution.
    pub fn pool_effort_warnings(&self, catalogue: &Catalogue) -> Vec<String> {
        self.pools
            .iter()
            .flat_map(|(name, members)| {
                members.iter().enumerate().filter_map(move |(index, member)| {
                    let (_, dropped) = member.reconciled(catalogue.model(&member.model));
                    (!dropped.is_empty()).then(|| {
                        format!(
                            "warning: domains.routing.pools.{name}[{index}].efforts: {} does not list {}; dropped",
                            member.model.as_str(),
                            dropped.join(", ")
                        )
                    })
                })
            })
            .collect()
    }

    /// As above, including the origin of each configured value for diagnostics.
    pub fn pool_effort_warnings_with_sources(
        &self,
        catalogue: &Catalogue,
        mut source: impl FnMut(&str) -> String,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for (name, members) in &self.pools {
            for (index, member) in members.iter().enumerate() {
                let (_, dropped) = member.reconciled(catalogue.model(&member.model));
                let member_path = format!("domains.routing.pools.{name}.{index}");
                for effort in dropped {
                    let mut value_sources = Vec::new();
                    if member.effort.as_deref() == Some(effort.as_str()) {
                        value_sources.push(format!(
                            "effort source: {}",
                            source(&format!("{member_path}.effort"))
                        ));
                    }
                    if member.efforts.iter().any(|value| value == &effort) {
                        value_sources.push(format!(
                            "efforts source: {}",
                            source(&format!("{member_path}.efforts"))
                        ));
                    }
                    warnings.push(format!(
                        "warning: domains.routing.pools.{name}[{index}].efforts: pool {name} effort {effort} for model {} is unsupported and will be dropped (pool source: {}; model source: {}; {})",
                        member.model.as_str(),
                        source(&format!("domains.routing.pools.{name}")),
                        source(&format!("{member_path}.model")),
                        value_sources.join("; "),
                    ));
                }
            }
        }
        warnings
    }
    pub fn model_policy_fallback_warnings(&self, catalogue: &Catalogue) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut inspect = |policy: &crate::ModelPolicy, field: &str| {
            if let Some(model) = policy.result.fallback.as_ref() {
                let entry = catalogue.model(model);
                let reason = if entry.is_none() {
                    Some("unknown model")
                } else if policy
                    .result
                    .fallback_effort
                    .as_ref()
                    .is_some_and(|effort| {
                        !entry.is_some_and(|entry| entry.efforts.contains(effort))
                    })
                {
                    Some("unsupported effort")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    warnings.push(format!(
                        "warning: domains.routing.{field}.result.fallback: {} ({reason}); dropped",
                        model.as_str()
                    ));
                }
            }
        };
        inspect(&self.model_policy.defaults, "model_policy.defaults");
        for (index, rule) in self.model_policy.rules.iter().enumerate() {
            inspect(&rule.policy, &format!("model_policy.rules.{index}"));
        }
        warnings
    }
    /// Suggest a cheaper route in the applied tier. Exact models and custom
    /// pools use the model's highest bundled implementation tier.
    pub fn opus_writer_advisory(
        &self,
        catalogue: &Catalogue,
        route: &crate::ResolvedRoute,
    ) -> Option<String> {
        let advice = match &self.writer_advisory {
            Some(user) => Cow::Owned(user.merged(&catalogue.writer_advisory)),
            None => Cow::Borrowed(&catalogue.writer_advisory),
        };
        let applied = format!("{}/{}", route.adapter, route.model);
        if self.suppress_opus_writer_warning
            || !advice.warn_models.iter().any(|id| id.as_str() == applied)
        {
            return None;
        }
        let pool = route
            .pool_selection
            .as_ref()
            .filter(|receipt| catalogue.has_pool(&receipt.pool))
            .map(|receipt| receipt.pool.as_str())
            .or_else(|| {
                advice
                    .pool_priority
                    .iter()
                    .map(String::as_str)
                    .find(|name| {
                        catalogue.pools.get(*name).is_some_and(|members| {
                            members.iter().any(|m| m.model.as_str() == applied)
                        })
                    })
            })?;
        let members = self.pools.get(pool).or_else(|| catalogue.pools.get(pool))?;
        let cheaper = members
            .iter()
            .chain(catalogue.pools.get(pool).into_iter().flatten())
            .find(|member| {
                member.weight > 0
                    && member.model.as_str() != applied
                    && advice.cheaper_models.contains(&member.model)
            })?;
        let effort = cheaper.effort.as_deref().or_else(|| {
            route
                .effort
                .as_deref()
                .filter(|effort| cheaper.efforts.iter().any(|e| e == effort))
        });
        let writer = advice
            .label
            .as_ref()
            .map(|label| format!("{label} writer"))
            .unwrap_or_else(|| "Writer".into());
        Some(format!(
            "advisory: {writer} route {applied}; consider cheaper {pool} route {}{} (silence with domains.routing.suppress_opus_writer_warning = true)",
            cheaper.model.as_str(),
            effort
                .map(|effort| format!(" effort={effort}"))
                .unwrap_or_default(),
        ))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedRoute {
    pub model: ModelId,
    pub effort: Option<String>,
}
impl DomainConfig for RoutingConfig {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        self.model_policy.validate(&mut errors);
        if let Some(advice) = &self.writer_advisory {
            advice.validate("writer_advisory", &mut errors);
        }
        if self.settlement_retries > MAX_SETTLEMENT_RETRIES {
            errors.push(SectionError {
                field: "settlement_retries".into(),
                code: "limit",
            });
        }
        // Waits are bounded only so a typo in seconds cannot park a lane for
        // months; within a week every value is the operator's choice.
        for (field, seconds) in [
            ("capacity_retry_seconds", self.capacity_retry_seconds),
            (
                "capacity_retry_max_seconds",
                self.capacity_retry_max_seconds,
            ),
            (
                "retry_delay_ceiling_seconds",
                self.retry_delay_ceiling_seconds,
            ),
        ] {
            if seconds > MAX_RETRY_WAIT_SECONDS {
                errors.push(SectionError {
                    field: field.into(),
                    code: "limit",
                });
            }
        }
        if self.routes.len() > 128 {
            errors.push(SectionError {
                field: "routes".into(),
                code: "limit",
            });
        }
        let catalogue = Catalogue::bundled().ok();
        self.review.validate(self, catalogue.as_ref(), &mut errors);
        for (name, route) in &self.routes {
            if catalogue
                .as_ref()
                .and_then(|value| value.model(&route.model))
                .is_none()
            {
                errors.push(SectionError {
                    field: format!("routes.{name}.model"),
                    code: "unknown_model",
                });
            }
            let collides_with_pool = catalogue.as_ref().is_some_and(|value| value.has_pool(name));
            if !identifier(name)
                || collides_with_pool
                || route.effort.as_deref().is_some_and(|e| !effort_valid(e))
            {
                errors.push(SectionError {
                    field: "routes".into(),
                    code: if collides_with_pool {
                        "collision"
                    } else {
                        "selector"
                    },
                });
            }
        }
        if self.pools.len() > 32 {
            errors.push(SectionError {
                field: "pools".into(),
                code: "limit",
            });
        }
        for (name, members) in &self.pools {
            if !identifier(name) || self.routes.contains_key(name) {
                errors.push(SectionError {
                    field: format!("pools.{name}"),
                    code: if self.routes.contains_key(name) {
                        "collision"
                    } else {
                        "selector"
                    },
                });
            }
            if members.len() > 16 {
                errors.push(SectionError {
                    field: format!("pools.{name}"),
                    code: "limit",
                });
            }
            let mut seen = BTreeSet::new();
            for (index, member) in members.iter().enumerate() {
                let prefix = format!("pools.{name}[{index}]");
                if !seen.insert(&member.model) {
                    errors.push(SectionError {
                        field: format!("{prefix}.model"),
                        code: "duplicate",
                    });
                }
                if member.weight > 100 {
                    errors.push(SectionError {
                        field: format!("{prefix}.weight"),
                        code: "range",
                    });
                }
                let model = catalogue
                    .as_ref()
                    .and_then(|value| value.model(&member.model));
                if model.is_none() {
                    errors.push(SectionError {
                        field: format!("{prefix}.model"),
                        code: "unknown_model",
                    });
                }
                if !provenant_providers::REGISTRY
                    .iter()
                    .any(|provider| provider.adapter.id() == member.model.adapter())
                {
                    errors.push(SectionError {
                        field: format!("{prefix}.model"),
                        code: "unknown_provider",
                    });
                }
                // An effort the catalogue does not list is not an error here: it
                // is dropped at resolution (see `RoutePoolMember::reconciled`).
                let mut seen_efforts = BTreeSet::new();
                if member.efforts.len() > 16
                    || member
                        .efforts
                        .iter()
                        .any(|effort| !effort_valid(effort) || !seen_efforts.insert(effort))
                {
                    errors.push(SectionError {
                        field: format!("{prefix}.efforts"),
                        code: "unsupported_effort",
                    });
                }
                if member.effort.as_deref().is_some_and(|e| !effort_valid(e)) {
                    errors.push(SectionError {
                        field: format!("{prefix}.effort"),
                        code: "unsupported_effort",
                    });
                }
            }
        }
        for provider in self.pacing.keys() {
            if !provenant_providers::REGISTRY
                .iter()
                .any(|registration| registration.adapter.id() == provider)
            {
                errors.push(SectionError {
                    field: format!("pacing.{provider}"),
                    code: "unknown_provider",
                });
            }
        }
        for (account, profile) in &self.pacing.accounts {
            if !account_identifier(account) {
                errors.push(SectionError {
                    field: format!("pacing.accounts.{account}"),
                    code: "selector",
                });
            }
            validate_profile(&format!("pacing.accounts.{account}"), profile, &mut errors);
        }
        for (provider, profile) in &self.pacing.free {
            validate_profile(&format!("pacing.free.{provider}"), profile, &mut errors);
            if !provenant_providers::REGISTRY
                .iter()
                .any(|r| r.adapter.id() == provider)
            {
                errors.push(SectionError {
                    field: format!("pacing.free.{provider}"),
                    code: "unknown_provider",
                });
            }
        }
        for (index, override_) in self.pacing_overrides.iter().enumerate() {
            if override_
                .account
                .as_deref()
                .is_some_and(|a| !account_identifier(a))
                || override_.account.is_some() && override_.provider.is_some()
                || override_.provider.as_deref().is_some_and(|p| {
                    !provenant_providers::REGISTRY
                        .iter()
                        .any(|r| r.adapter.id() == p)
                })
            {
                errors.push(SectionError {
                    field: format!("pacing_overrides.{index}"),
                    code: "selector",
                });
            } else if !override_.schedule.valid() {
                // Reported at the override, so recovery drops the override
                // rather than its schedule, which would make it permanent.
                errors.push(SectionError {
                    field: format!("pacing_overrides.{index}"),
                    code: "schedule",
                });
            }
        }
        for (provider, reserve) in &self.pacing_reserve {
            if !provenant_providers::REGISTRY
                .iter()
                .any(|r| r.adapter.id() == provider)
            {
                errors.push(SectionError {
                    field: format!("pacing_reserve.{provider}"),
                    code: "unknown_provider",
                });
            }
            if reserve.five_hour_percent > 100 || reserve.seven_day_percent > 100 {
                errors.push(SectionError {
                    field: format!("pacing_reserve.{provider}"),
                    code: "range",
                });
            }
        }
        errors
    }
}
fn account_identifier(value: &str) -> bool {
    value.split_once('/').map_or_else(
        || identifier(value),
        |(adapter, key)| identifier(adapter) && identifier(key),
    )
}

fn validate_profile(field: &str, profile: &PacingProfile, errors: &mut Vec<SectionError>) {
    if profile.reserve.values().any(|percent| *percent > 100) {
        errors.push(SectionError {
            field: format!("{field}.reserve"),
            code: "range",
        });
    }
    if profile
        .deadline
        .as_ref()
        .is_some_and(|deadline| matches!(deadline, PacingDeadline::ResetOf(reset) if !account_identifier(&reset.reset_of)))
    {
        errors.push(SectionError {
            field: format!("{field}.deadline"),
            code: "selector",
        });
    }
    if profile
        .windows
        .as_ref()
        .is_some_and(|windows| windows.iter().collect::<BTreeSet<_>>().len() != windows.len())
    {
        errors.push(SectionError {
            field: format!("{field}.windows"),
            code: "duplicate",
        });
    }
}

#[cfg(test)]
mod go_allowance_tests {
    use super::*;

    #[test]
    fn go_allowance_floor_handles_limits_unknown_and_zen() -> Result<(), Box<dyn std::error::Error>>
    {
        let catalogue = Catalogue::bundled()?;
        let mut config: RoutingConfig = toml_edit::de::from_str("[opencode_go]\n")?;
        assert_eq!(config.opencode_go.min_monthly_allowance_usd, 60);
        for id in [
            "opencode/opencode-go/beam-free",
            "opencode/opencode-go/glm-5.3-flash",
            "opencode/opencode-go/deepseek-v4.1-flash",
            "opencode/opencode/glm-5.3-flash-free",
        ] {
            assert!(
                check_go_allowance(&ModelId::parse(id)?, &config, &catalogue).is_ok(),
                "{id}"
            );
        }
        for (id, expected) in [
            ("opencode/opencode-go/minimax-m3.1-flash", "$20"),
            ("opencode/opencode-go/unlisted", "unknown"),
        ] {
            let model = ModelId::parse(id)?;
            let error = check_go_allowance(&model, &config, &catalogue).unwrap_err();
            assert!(
                error.contains(id)
                    && error.contains(expected)
                    && error.contains("domains.routing.opencode_go.min_monthly_allowance_usd"),
                "{error}"
            );
            config.opencode_go.min_monthly_allowance_usd = 0;
            assert!(check_go_allowance(&model, &config, &catalogue).is_ok());
            config.opencode_go.min_monthly_allowance_usd = 60;
        }
        config.opencode_go.min_monthly_allowance_usd = 61;
        assert!(
            check_go_allowance(
                &ModelId::parse("opencode/opencode-go/glm-5.3-flash")?,
                &config,
                &catalogue
            )
            .is_err()
        );
        Ok(())
    }
}

#[cfg(test)]
mod pacing_profile_tests {
    use super::*;

    #[test]
    fn provider_and_account_pacing_share_a_table() {
        let config: RoutingConfig = toml_edit::de::from_str(
            "[pacing]\nclaude = 'pace'\n[pacing.accounts.pro]\npolicy = 'pace'\nwindows = ['five_hour']\nreserve = { five_hour = 15 }\ndeadline = 'thu 00:00'\n[pacing.free.opencode]\npolicy = 'max'\n",
        ).unwrap();
        assert_eq!(config.pacing.get("claude"), Some(&PacingPolicy::Pace));
        assert_eq!(
            config.pacing.accounts["pro"].reserve[&PacingWindow::FiveHour],
            15
        );
        assert_eq!(
            config.pacing.free["opencode"].policy,
            Some(PacingPolicy::Max)
        );
        assert!(config.validate().is_empty());
    }

    #[test]
    fn deadline_forms_and_unknown_keys_are_validated_together() {
        for deadline in [
            "'thu 00:00'",
            "'day 25'",
            "'2026-11-02T09:00'",
            "{reset_of='anese'}",
            "{reset_of='anese',window='monthly'}",
        ] {
            let profile: PacingProfile =
                toml_edit::de::from_str(&format!("deadline={deadline}")).unwrap();
            assert!(profile.deadline.is_some());
        }
        let profile: PacingProfile =
            toml_edit::de::from_str("windows=['daily', 'five_hour']").unwrap();
        assert_eq!(
            profile.windows,
            Some(vec![PacingWindow::Daily, PacingWindow::FiveHour])
        );
        assert!(toml_edit::de::from_str::<PacingProfile>("windows=['hourly']").is_err());
        for invalid in [
            "deadline='thu 25:00'",
            "deadline='day 32'",
            "deadline={reset_of='anese',when='weekly'}",
            "policy='unlimited'",
            "after_deadline='stop'",
            "window=['five_hour']",
        ] {
            assert!(
                toml_edit::de::from_str::<PacingProfile>(invalid).is_err(),
                "{invalid}"
            );
        }
        let error =
            toml_edit::de::from_str::<PacingProfile>("deadline={reset_of='anese',when='weekly'}")
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("reset_of") && error.contains("window"),
            "{error}"
        );
    }

    #[test]
    fn invalid_profile_is_rejected() {
        let mut config = RoutingConfig::default();
        config.pacing.accounts.insert(
            "pro".into(),
            PacingProfile {
                deadline: Some(PacingDeadline::ResetOf(ResetDeadline {
                    reset_of: "bad account".into(),
                    window: PacingWindow::SevenDay,
                })),
                reserve: BTreeMap::from([(PacingWindow::FiveHour, 101)]),
                ..PacingProfile::default()
            },
        );
        assert_eq!(config.validate().len(), 2);
    }
}
fn defaults(_: Profile) -> &'static str {
    include_str!("../assets/review-defaults.toml")
}
/// Enforced model policy restrictions, including a retired schedule key in a
/// restricting rule, are a boundary; other routing input recovers.
pub const SECTION: DomainSection = DomainSection {
    validate: validate_section,
    ..DomainSection::new::<RoutingConfig>("routing", defaults)
        .with_boundary_check(crate::policy::boundary)
};
/// Retired timing keys are named first; serde never reads their values.
fn validate_section(table: &toml_edit::Table) -> Vec<SectionError> {
    let mut errors = Vec::new();
    crate::policy::retired_schedule_keys(table, &["pacing_overrides"], |_| false, &mut errors);
    crate::policy::retired_schedule_keys(
        table,
        &["model_policy", "rules"],
        crate::policy::restricting_rule,
        &mut errors,
    );
    errors.extend((DomainSection::new::<RoutingConfig>("routing", defaults).validate)(table));
    errors
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    #[serde(default)]
    pub free: bool,
    pub model: ModelId,
    /// Only exact controls with recording evidence. No guessed effort default.
    pub efforts: Vec<String>,
    pub evidence_ref: String,
    pub family: Option<String>,
    /// Unknown is treated as training for confidential selection.
    #[serde(default)]
    pub trains_on_prompts: Option<bool>,
    #[serde(default)]
    pub monthly_allowance_usd: Option<MonthlyAllowance>,
    /// The quota pool group this model draws on, for adapters whose windows
    /// are grouped (`group.window`). Unset applies every window.
    #[serde(default)]
    pub quota_pool: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MonthlyAllowance {
    Dollars(u64),
    Unlimited(UnlimitedAllowance),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnlimitedAllowance {
    Unlimited,
}

/// Apply the Go plan floor to every dispatch candidate, including unknown models.
pub fn check_go_allowance(
    model: &ModelId,
    config: &RoutingConfig,
    catalogue: &Catalogue,
) -> Result<(), String> {
    let floor = config.opencode_go.min_monthly_allowance_usd;
    if floor == 0 || model.adapter() != "opencode" || !model.model().starts_with("opencode-go/") {
        return Ok(());
    }
    let allowance = catalogue
        .model(model)
        .and_then(|entry| entry.monthly_allowance_usd.as_ref());
    let description = match allowance {
        Some(MonthlyAllowance::Unlimited(_)) => return Ok(()),
        Some(MonthlyAllowance::Dollars(value)) if *value >= floor => return Ok(()),
        Some(MonthlyAllowance::Dollars(value)) => format!("${value}"),
        None => "unknown".to_owned(),
    };
    Err(format!(
        "{}: Go monthly allowance {description}; domains.routing.opencode_go.min_monthly_allowance_usd is {floor}; lower [domains.routing.opencode_go] min_monthly_allowance_usd or choose a model with a documented allowance of at least ${floor} (0 disables the floor)",
        model.as_str()
    ))
}
/// Pool candidate; list order is used for weight-zero fallback selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePoolMember {
    pub model: ModelId,
    #[serde(default = "default_weight")]
    pub weight: u8,
    pub effort: Option<String>,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub free: bool,
}
impl RoutePoolMember {
    /// The single place a pool member is reconciled with the catalogue: efforts
    /// the model does not list are dropped, and a default effort that is no
    /// longer allowed falls back to none. Returns the member and the dropped
    /// efforts (the default is reported once, as `effort`).
    pub(crate) fn reconciled(&self, entry: Option<&ModelEntry>) -> (Self, Vec<String>) {
        let mut member = self.clone();
        let Some(entry) = entry else {
            return (member, Vec::new());
        };
        let (kept, mut dropped): (Vec<_>, Vec<_>) = self
            .efforts
            .iter()
            .cloned()
            .partition(|effort| entry.efforts.contains(effort));
        member.efforts = kept;
        if let Some(effort) = member.effort.take() {
            if member.efforts.contains(&effort) {
                member.effort = Some(effort);
            } else if !dropped.contains(&effort) {
                dropped.push(effort);
            }
        }
        (member, dropped)
    }
}
fn default_weight() -> u8 {
    1
}
/// Catalogue classes used only for cost advice, separate from answering-family evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterAdvisory {
    /// Display label for the warning class; without one the advice says generic writer.
    #[serde(default)]
    pub label: Option<String>,
    pub warn_models: Vec<ModelId>,
    pub cheaper_models: Vec<ModelId>,
    pub pool_priority: Vec<String>,
}
impl WriterAdvisory {
    fn validate(&self, prefix: &str, errors: &mut Vec<SectionError>) {
        if self.label.as_deref().is_some_and(|label| {
            label.is_empty() || label.len() > 128 || label.chars().any(char::is_control)
        }) {
            errors.push(SectionError {
                field: format!("{prefix}.label"),
                code: "label",
            });
        }
        if self.warn_models.len() > 256 || self.cheaper_models.len() > 256 {
            errors.push(SectionError {
                field: prefix.into(),
                code: "limit",
            });
        }
        let mut seen = BTreeSet::new();
        if self.pool_priority.len() > 32
            || self
                .pool_priority
                .iter()
                .any(|pool| !identifier(pool) || !seen.insert(pool))
        {
            errors.push(SectionError {
                field: format!("{prefix}.pool_priority"),
                code: "selector",
            });
        }
    }
}

/// A `[writer_advisory]` table. Each field the user omits keeps its bundled value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterAdvisoryOverride {
    pub label: Option<String>,
    pub warn_models: Option<Vec<ModelId>>,
    pub cheaper_models: Option<Vec<ModelId>>,
    pub pool_priority: Option<Vec<String>>,
}
impl WriterAdvisoryOverride {
    fn merged(&self, bundled: &WriterAdvisory) -> WriterAdvisory {
        WriterAdvisory {
            label: self.label.clone().or_else(|| bundled.label.clone()),
            warn_models: self
                .warn_models
                .clone()
                .unwrap_or_else(|| bundled.warn_models.clone()),
            cheaper_models: self
                .cheaper_models
                .clone()
                .unwrap_or_else(|| bundled.cheaper_models.clone()),
            pool_priority: self
                .pool_priority
                .clone()
                .unwrap_or_else(|| bundled.pool_priority.clone()),
        }
    }
    /// Checks only what the user wrote. Omitted fields are empty here, which is valid.
    fn validate(&self, prefix: &str, errors: &mut Vec<SectionError>) {
        WriterAdvisory {
            label: self.label.clone(),
            warn_models: self.warn_models.clone().unwrap_or_default(),
            cheaper_models: self.cheaper_models.clone().unwrap_or_default(),
            pool_priority: self.pool_priority.clone().unwrap_or_default(),
        }
        .validate(prefix, errors);
    }
}

/// Only the bundled asset constructs a catalogue. A caller cannot deserialize or
/// change family/effort rows while retaining an existing revision.
#[derive(Debug, Clone, Serialize)]
pub struct Catalogue {
    pub(crate) schema_version: u64,
    pub(crate) revision: String,
    pub(crate) models: Vec<ModelEntry>,
    pub(crate) pools: BTreeMap<String, Vec<RoutePoolMember>>,
    pub(crate) pacing: BTreeMap<String, PacingPolicy>,
    pub(crate) writer_advisory: WriterAdvisory,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueAsset {
    schema_version: u64,
    revision: String,
    models: Vec<ModelEntry>,
    #[serde(default)]
    pools: BTreeMap<String, Vec<RoutePoolMember>>,
    #[serde(default)]
    pacing: BTreeMap<String, PacingPolicy>,
}
impl Catalogue {
    pub fn bundled() -> Result<Self, RoutingError> {
        let asset: CatalogueAsset = toml_edit::de::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/catalogue.toml"
        )))
        .map_err(|_| RoutingError::Catalogue)?;
        let value = Self {
            schema_version: asset.schema_version,
            revision: asset.revision,
            models: asset.models,
            pools: asset.pools,
            pacing: asset.pacing,
            writer_advisory: toml_edit::de::from_str(include_str!(
                "../assets/writer-advisory.toml"
            ))
            .map_err(|_| RoutingError::Catalogue)?,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    /// Similar recorded model IDs in the same adapter and OpenCode namespace.
    /// Results are ordered by edit distance and exclude unrelated names.
    pub fn nearest_models(&self, model: &ModelId) -> Vec<String> {
        fn distance(a: &str, b: &str) -> usize {
            let b: Vec<char> = b.chars().collect();
            let mut row: Vec<usize> = (0..=b.len()).collect();
            for (i, ca) in a.chars().enumerate() {
                let mut diagonal = row[0];
                row[0] = i + 1;
                for (j, cb) in b.iter().enumerate() {
                    let above = row[j + 1];
                    row[j + 1] = (diagonal + usize::from(ca != *cb))
                        .min(row[j] + 1)
                        .min(above + 1);
                    diagonal = above;
                }
            }
            row[b.len()]
        }
        // Every OpenCode path component before the model name is its namespace.
        fn parts(id: &ModelId) -> (&str, &str) {
            if id.adapter() == "opencode" {
                id.model().rsplit_once('/').unwrap_or(("", id.model()))
            } else {
                ("", id.model())
            }
        }
        let (namespace, name) = parts(model);
        let mut matches: Vec<_> = self
            .models
            .iter()
            .filter(|entry| entry.model.adapter() == model.adapter())
            .filter_map(|entry| {
                let (entry_namespace, entry_name) = parts(&entry.model);
                if entry_namespace != namespace {
                    return None;
                }
                let edits = distance(name, entry_name);
                // Allow small typos, while limiting suggestions for long names.
                let cutoff = (name.chars().count().max(entry_name.chars().count()) / 3).clamp(1, 3);
                (edits <= cutoff).then(|| (edits, entry.model.as_str().to_owned()))
            })
            .collect();
        matches.sort();
        matches.into_iter().take(3).map(|(_, id)| id).collect()
    }

    pub fn models(&self) -> &[ModelEntry] {
        &self.models
    }
    pub fn validate(&self) -> Result<(), RoutingError> {
        let mut advice_errors = Vec::new();
        self.writer_advisory
            .validate("writer_advisory", &mut advice_errors);
        if !advice_errors.is_empty()
            || self
                .writer_advisory
                .warn_models
                .iter()
                .chain(&self.writer_advisory.cheaper_models)
                .any(|id| self.model(id).is_none())
            || self
                .writer_advisory
                .pool_priority
                .iter()
                .any(|pool| !self.has_pool(pool))
        {
            return Err(RoutingError::Catalogue);
        }
        let mut seen = BTreeSet::new();
        if self.schema_version != 1 || !identifier(&self.revision) || self.models.len() > 256 {
            return Err(RoutingError::Catalogue);
        }
        for row in &self.models {
            if !seen.insert(&row.model)
                || row.evidence_ref.is_empty()
                || row.evidence_ref.len() > 512
                || row.efforts.len() > 16
                || row.efforts.iter().any(|e| !effort_valid(e))
                || row.family.as_deref().is_some_and(|f| !identifier(f))
                || row.quota_pool.as_deref().is_some_and(|pool| {
                    !identifier(pool)
                        || !provenant_records::AdapterSpec::get(row.model.adapter())
                            .is_some_and(|spec| spec.grouped_quota_pools)
                })
            {
                return Err(RoutingError::Catalogue);
            }
        }
        if self.pools.len() > 32 {
            return Err(RoutingError::Catalogue);
        }
        for (name, members) in &self.pools {
            if !identifier(name) || members.is_empty() || members.len() > 16 {
                return Err(RoutingError::Catalogue);
            }
            let mut pool_models = BTreeSet::new();
            for member in members {
                let Some(model) = self.model(&member.model) else {
                    return Err(RoutingError::Catalogue);
                };
                if !pool_models.insert(&member.model)
                    || member.weight > 100
                    || member
                        .efforts
                        .iter()
                        .any(|effort| !model.efforts.iter().any(|supported| supported == effort))
                    || member.effort.as_deref().is_some_and(|effort| {
                        !member.efforts.iter().any(|allowed| allowed == effort)
                    })
                {
                    return Err(RoutingError::Catalogue);
                }
            }
        }
        Ok(())
    }
    pub fn model(&self, id: &ModelId) -> Option<&ModelEntry> {
        self.models.iter().find(|row| row.model == *id)
    }
    /// The single owner of a model's quota pool group.
    pub fn quota_pool(&self, id: &ModelId) -> Option<&str> {
        self.model(id)?.quota_pool.as_deref()
    }
    pub fn has_pool(&self, name: &str) -> bool {
        self.pools.contains_key(name)
    }
    /// Use a model default only when its applicable sources agree.
    pub fn model_default_effort(&self, config: &RoutingConfig, model: &ModelId) -> Option<String> {
        let configured: std::collections::BTreeSet<_> = config
            .routes
            .values()
            .filter(|route| &route.model == model)
            .filter_map(|route| route.effort.as_deref())
            .chain(
                config
                    .pools
                    .values()
                    .flatten()
                    .filter(|member| &member.model == model)
                    .filter_map(|member| member.effort.as_deref()),
            )
            .collect();
        if !configured.is_empty() {
            return (configured.len() == 1)
                .then(|| configured.first().map(|s| (*s).to_owned()))
                .flatten();
        }
        let bundled: std::collections::BTreeSet<_> = self
            .pools
            .iter()
            .filter(|(name, _)| !config.pools.contains_key(*name))
            .flat_map(|(_, members)| members)
            .filter(|member| &member.model == model)
            .filter_map(|member| member.effort.as_deref())
            .collect();
        (bundled.len() == 1)
            .then(|| bundled.first().map(|s| (*s).to_owned()))
            .flatten()
    }
    /// Billing classification belongs to the model, independently of pools.
    pub fn is_free_model(&self, id: &ModelId) -> bool {
        self.model(id).is_some_and(|model| model.free)
            || self
                .pools
                .values()
                .flatten()
                .any(|member| member.free && member.model == *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessContext, Fallback, RouteChoice, RouteRequest, RoutingContext, resolve};
    use provenant_providers::{AvailabilityRecord, REGISTRY};
    use provenant_records::{RouteEligibility, RouteReadiness};

    #[test]
    fn writer_advisory_classes_and_pool_priority_are_overridable()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let context = RoutingContext {
            policy_account_hint: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            access: &[],
            now: provenant_core::WallTime::from_micros(0)?,
            host: "test-host",
        };
        let request = |id| -> Result<RouteRequest, RoutingError> {
            Ok(RouteRequest {
                intent: Default::default(),
                choice: RouteChoice::Model(ModelId::parse(id)?),
                effort: None,
                confidential: false,
                allow_free: false,
                fallback: Fallback::None,
            })
        };
        let mut route = resolve(&request("claude/claude-opus-5-5")?, &context)?;
        let default = config
            .opus_writer_advisory(&catalogue, &route)
            .ok_or("default advisory missing")?;
        assert!(default.starts_with("advisory: Opus writer route"));
        assert!(
            default.contains("principal route codex/gpt-6.1-sol effort=high"),
            "{default}"
        );
        let changed: RoutingConfig = toml_edit::de::from_str(
            r#"
            [writer_advisory]
            warn_models = ["claude/claude-opus-5-5", "codex/gpt-6.1-sol"]
            cheaper_models = ["claude/claude-sonnet-5-5", "codex/gpt-6.1-sol"]
            pool_priority = ["design", "senior", "principal"]
        "#,
        )?;
        assert!(changed.validate().is_empty());
        let advisory = changed
            .opus_writer_advisory(&catalogue, &route)
            .ok_or("override missing")?;
        assert!(
            advisory.contains("design route claude/claude-sonnet-5-5"),
            "{advisory}"
        );
        route.adapter = "codex".into();
        route.model = "gpt-6.1-sol".into();
        assert!(config.opus_writer_advisory(&catalogue, &route).is_none());
        let changed_advice = changed
            .opus_writer_advisory(&catalogue, &route)
            .ok_or("class override missing")?;
        // The omitted label inherits the bundled "Opus" class label.
        assert!(changed_advice.starts_with("advisory: Opus writer route codex/gpt-6.1-sol"));
        assert!(
            changed_advice.contains("cheaper senior route claude/claude-sonnet-5-5"),
            "must not recommend the applied model: {changed_advice}"
        );
        // Familiar spelling on an uncatalogued ID supplies no class evidence.
        route.model = "unknown-opus".into();
        assert!(config.opus_writer_advisory(&catalogue, &route).is_none());
        Ok(())
    }

    fn request_for(id: &str) -> Result<RouteRequest, RoutingError> {
        Ok(RouteRequest {
            intent: Default::default(),
            choice: RouteChoice::Model(ModelId::parse(id)?),
            effort: None,
            confidential: false,
            allow_free: false,
            fallback: Fallback::None,
        })
    }

    #[test]
    fn bundled_opus_writer_advice_names_exact_routes() -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let context = RoutingContext {
            policy_account_hint: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            access: &[],
            now: provenant_core::WallTime::from_micros(0)?,
            host: "test-host",
        };
        // Each receipt names the pool that selected the route; `None` uses pool priority.
        let rows: [(&str, Option<&str>, Option<&str>); 8] = [
            (
                "claude/claude-opus-5-5",
                None,
                Some(
                    "advisory: Opus writer route claude/claude-opus-5-5; consider cheaper principal route codex/gpt-6.1-sol effort=high (silence with domains.routing.suppress_opus_writer_warning = true)",
                ),
            ),
            (
                "claude/claude-opus-5-5",
                Some("senior"),
                Some(
                    "advisory: Opus writer route claude/claude-opus-5-5; consider cheaper senior route codex/gpt-6.1-sol effort=medium (silence with domains.routing.suppress_opus_writer_warning = true)",
                ),
            ),
            (
                "claude/claude-opus-5-5",
                Some("junior"),
                Some(
                    "advisory: Opus writer route claude/claude-opus-5-5; consider cheaper junior route codex/gpt-6.1-terra effort=low (silence with domains.routing.suppress_opus_writer_warning = true)",
                ),
            ),
            (
                "claude/claude-opus-5-5",
                Some("design"),
                Some(
                    "advisory: Opus writer route claude/claude-opus-5-5; consider cheaper design route claude/claude-sonnet-5-5 effort=high (silence with domains.routing.suppress_opus_writer_warning = true)",
                ),
            ),
            (
                "claude/claude-opus-5-5",
                Some("writing"),
                Some(
                    "advisory: Opus writer route claude/claude-opus-5-5; consider cheaper writing route agy/claude-sonnet-4-6 (silence with domains.routing.suppress_opus_writer_warning = true)",
                ),
            ),
            (
                "claude/claude-opus-5-5",
                Some("utility"),
                Some(
                    "advisory: Opus writer route claude/claude-opus-5-5; consider cheaper utility route codex/gpt-6-luna effort=low (silence with domains.routing.suppress_opus_writer_warning = true)",
                ),
            ),
            (
                "agy/claude-opus-4-6-thinking",
                None,
                Some(
                    "advisory: Opus writer route agy/claude-opus-4-6-thinking; consider cheaper senior route codex/gpt-6.1-sol effort=medium (silence with domains.routing.suppress_opus_writer_warning = true)",
                ),
            ),
            // Only exact catalogued IDs carry Opus class evidence.
            ("claude/claude-opus-5", None, None),
        ];
        for (model, pool, expected) in rows {
            let mut route = resolve(&request_for(model)?, &context)?;
            route.effort = Some("high".into());
            route.pool_selection = pool.map(|pool| provenant_records::RouteSelectionReceipt {
                extra: Default::default(),
                pool: pool.into(),
                candidates: vec![],
                roll: 0,
                total_weight: 1,
                pick: format!("{}/{}", route.adapter, route.model),
                reason: None,
                redraws: vec![],
            });
            let advisory = config.opus_writer_advisory(&catalogue, &route);
            assert_eq!(advisory.as_deref(), expected, "{model} via {pool:?}");
        }
        Ok(())
    }

    #[test]
    fn partial_writer_advisory_override_keeps_bundled_classes()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let base = RoutingConfig::default();
        let context = RoutingContext {
            policy_account_hint: None,
            catalogue: &catalogue,
            config: &base,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            access: &[],
            now: provenant_core::WallTime::from_micros(0)?,
            host: "test-host",
        };
        let mut route = resolve(&request_for("claude/claude-opus-5-5")?, &context)?;
        route.effort = Some("high".into());
        let advice = |toml: &str| -> Result<Option<String>, Box<dyn std::error::Error>> {
            let config: RoutingConfig = toml_edit::de::from_str(toml)?;
            assert!(config.validate().is_empty(), "{toml}");
            Ok(config.opus_writer_advisory(&catalogue, &route))
        };
        let label_only = advice("[writer_advisory]\nlabel = \"Principal\"\n")?.ok_or("label")?;
        assert!(
            label_only.starts_with("advisory: Principal writer route claude/claude-opus-5-5; consider cheaper principal route codex/gpt-6.1-sol"),
            "{label_only}"
        );
        let pool_only =
            advice("[writer_advisory]\npool_priority = [\"design\"]\n")?.ok_or("pool")?;
        assert!(
            pool_only.contains("Opus writer route claude/claude-opus-5-5; consider cheaper design route claude/claude-sonnet-5-5 effort=high"),
            "{pool_only}"
        );
        let emptied = advice("[writer_advisory]\nwarn_models = []\n")?;
        assert!(emptied.is_none(), "an empty warn list silences the class");
        Ok(())
    }

    #[test]
    fn writer_advisory_rejects_invalid_pool_order() -> Result<(), Box<dyn std::error::Error>> {
        for pools in [r#"["design", "design"]"#, r#"["bad pool"]"#] {
            let config: RoutingConfig = toml_edit::de::from_str(&format!(
                "[writer_advisory]\nwarn_models=[]\ncheaper_models=[]\npool_priority={pools}\n"
            ))?;
            assert!(
                config
                    .validate()
                    .iter()
                    .any(|e| e.field == "writer_advisory.pool_priority")
            );
        }
        Ok(())
    }

    #[test]
    fn model_policy_fallbacks_are_warned_and_reconciled() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = RoutingConfig::default();
        config.model_policy.defaults.result.fallback =
            Some(ModelId::parse("codex/not-catalogued")?);
        assert!(config.validate().is_empty());
        assert!(
            config
                .model_policy_fallback_warnings(&Catalogue::bundled()?)
                .len()
                == 1
        );
        let effective =
            config
                .model_policy
                .effective(&ModelId::parse("codex/gpt-6-luna")?, None, 1);
        assert!(effective.policy.result.fallback.is_none());

        config.model_policy.defaults.result.fallback = Some(ModelId::parse("codex/gpt-6-luna")?);
        config.model_policy.defaults.result.fallback_effort = Some("unsupported".into());
        assert!(config.validate().is_empty());
        assert!(
            config
                .model_policy_fallback_warnings(&Catalogue::bundled()?)
                .len()
                == 1
        );
        config.model_policy.rules.push(crate::ModelPolicyRule {
            schedule: crate::Schedule {
                expires: Some(1),
                ..Default::default()
            },
            policy: crate::ModelPolicy {
                result: crate::ResultPolicy {
                    fallback: Some(ModelId::parse("codex/not-catalogued")?),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(config.validate().is_empty());
        assert_eq!(
            config
                .model_policy_fallback_warnings(&Catalogue::bundled()?)
                .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn bundled_pools_prefer_free_flash_and_exclude_other_opencode_models()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let junior = &catalogue.pools["junior"];
        for name in ["deepseek-v4.1-flash", "glm-5.3-flash"] {
            let free = ModelId::parse(&format!("opencode/opencode/{name}-free"))?;
            let paid = ModelId::parse(&format!("opencode/opencode-go/{name}"))?;
            let free_member = junior
                .iter()
                .find(|member| member.model == free)
                .ok_or("free flash missing")?;
            let paid_member = junior
                .iter()
                .find(|member| member.model == paid)
                .ok_or("Go flash missing")?;
            assert!(catalogue.is_free_model(&free));
            assert_eq!(
                catalogue
                    .model(&free)
                    .ok_or("free model missing")?
                    .trains_on_prompts,
                Some(false)
            );
            assert_eq!(free_member.weight, 50);
            assert!(free_member.weight > paid_member.weight);
            assert!(free_member.efforts.is_empty());
        }
        for members in catalogue.pools.values() {
            for member in members {
                let name = member.model.as_str();
                if name.starts_with("opencode/") {
                    assert!(
                        [
                            "opencode/opencode/deepseek-v4.1-flash-free",
                            "opencode/opencode/glm-5.3-flash-free",
                            "opencode/opencode-go/deepseek-v4.1-flash",
                            "opencode/opencode-go/glm-5.3-flash"
                        ]
                        .contains(&name),
                        "unexpected default pool model: {name}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn bundled_go_models_resolve_and_only_flash_models_have_provisional_pool_weights()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let model_names = ["deepseek-v4.1-flash", "glm-5.3-flash", "mimo-v2.6-flash"];
        let availability = [AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/opencode".into(),
            version: "fixture".into(),
            fixture_ref: "test".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("go".into()),
            observed_models: model_names
                .iter()
                .map(|name| format!("opencode-go/{name}"))
                .collect(),
            observed_unix_seconds: 80,
            expires_unix_seconds: 120,
            last_update: None,
        }];
        let access = [AccessContext {
            adapter: "opencode",
            disclosure_ref: "policy",
            confinement_ref: "grant",
        }];
        let context = RoutingContext {
            policy_account_hint: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &availability,
            quota: &[],
            capacity_cooldowns: &[],
            access: &access,
            now: provenant_core::WallTime::from_micros(100_000_000)?,
            host: "test-host",
        };
        for name in model_names {
            let model = ModelId::parse(&format!("opencode/opencode-go/{name}"))?;
            let entry = catalogue.model(&model).ok_or("Go model missing")?;
            assert_eq!(entry.trains_on_prompts, Some(false));
            assert!(entry.efforts.is_empty());
            assert!(entry.evidence_ref.contains("opencode-go-models.md"));
            let route = resolve(
                &RouteRequest {
                    intent: Default::default(),
                    choice: RouteChoice::Model(model.clone()),
                    effort: None,
                    confidential: true,
                    allow_free: false,
                    fallback: Fallback::None,
                },
                &context,
            )?;
            assert_eq!(route.model, format!("opencode-go/{name}"));
            assert_eq!(route.eligibility, RouteEligibility::Eligible);
            assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
            let memberships: Vec<_> = catalogue
                .pools
                .iter()
                .filter_map(|(pool, members)| {
                    members
                        .iter()
                        .find(|member| member.model == model)
                        .map(|member| (pool.as_str(), member))
                })
                .collect();
            let pools: &[(&str, u8)] = match name {
                "deepseek-v4.1-flash" => &[("junior", 2), ("senior", 1)],
                "glm-5.3-flash" => &[("junior", 1), ("senior", 1)],
                _ => &[],
            };
            if !pools.is_empty() {
                assert_eq!(
                    memberships
                        .iter()
                        .map(|(pool, member)| (*pool, member.weight))
                        .collect::<Vec<_>>(),
                    pools
                );
                for (_, member) in memberships {
                    assert!(!member.free);
                    assert!(member.effort.is_none());
                    assert!(member.efforts.is_empty());
                }
            } else {
                assert!(memberships.is_empty(), "{name} must await a trial");
            }
        }
        assert_eq!(catalogue.pacing["opencode_paid"], PacingPolicy::Pace);
        assert_eq!(catalogue.pacing["opencode_free"], PacingPolicy::Max);
        Ok(())
    }
}
