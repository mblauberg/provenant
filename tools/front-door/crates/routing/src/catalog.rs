use crate::{
    ModelId, RoutingError,
    request::{effort_valid, identifier},
};
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacingOverride {
    pub account: Option<String>,
    pub provider: Option<String>,
    pub policy: PacingPolicy,
    pub expires_unix_seconds: i64,
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
    /// Silence the advisory for Opus implementation lanes.
    pub suppress_opus_writer_warning: bool,
    pub capacity_cooldown_seconds: u64,
    pub review: crate::ReviewConfig,
    pub routes: BTreeMap<String, NamedRoute>,
    pub pools: BTreeMap<String, Vec<RoutePoolMember>>,
    pub pacing: PacingSettings,
    pub pacing_reserve: BTreeMap<String, PacingReserve>,
    pub pacing_overrides: Vec<PacingOverride>,
    pub opencode_go: OpenCodeGoSettings,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            model_policy: crate::ModelPolicies::default(),
            suppress_opus_writer_warning: false,
            capacity_cooldown_seconds: 600,
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
    /// Suggest a cheaper route in the applied tier. Exact models and custom
    /// pools use the model's highest bundled implementation tier.
    pub fn opus_writer_advisory(
        &self,
        catalogue: &Catalogue,
        route: &crate::ResolvedRoute,
    ) -> Option<String> {
        if self.suppress_opus_writer_warning || !route.model.contains("opus") {
            return None;
        }
        let applied = format!("{}/{}", route.adapter, route.model);
        let pool = route
            .pool_selection
            .as_ref()
            .filter(|receipt| catalogue.has_pool(&receipt.pool))
            .map(|receipt| receipt.pool.as_str())
            .or_else(|| {
                [
                    "principal",
                    "senior",
                    "junior",
                    "design",
                    "writing",
                    "utility",
                ]
                .into_iter()
                .find(|name| {
                    catalogue
                        .pools
                        .get(*name)
                        .is_some_and(|members| members.iter().any(|m| m.model.as_str() == applied))
                })
            })?;
        let members = self.pools.get(pool).or_else(|| catalogue.pools.get(pool))?;
        let cheaper = members
            .iter()
            .chain(catalogue.pools.get(pool).into_iter().flatten())
            .find(|member| {
                member.weight > 0
                    && (member.model.model().contains("sonnet")
                        || member.model.adapter() == "codex")
            })?;
        let effort = cheaper.effort.as_deref().or_else(|| {
            route
                .effort
                .as_deref()
                .filter(|effort| cheaper.efforts.iter().any(|e| e == effort))
        });
        Some(format!(
            "advisory: Opus writer route {applied}; consider cheaper {pool} route {}{} (silence with domains.routing.suppress_opus_writer_warning = true)",
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
                let mut seen_efforts = BTreeSet::new();
                if member.efforts.len() > 16
                    || member.efforts.iter().any(|effort| {
                        !effort_valid(effort)
                            || !seen_efforts.insert(effort)
                            || model.is_some_and(|entry| {
                                !entry.efforts.iter().any(|supported| supported == effort)
                            })
                    })
                {
                    errors.push(SectionError {
                        field: format!("{prefix}.efforts"),
                        code: "unsupported_effort",
                    });
                }
                if member.effort.as_deref().is_some_and(|effort| {
                    !effort_valid(effort) || !member.efforts.iter().any(|allowed| allowed == effort)
                }) {
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
                || override_.expires_unix_seconds <= 0
            {
                errors.push(SectionError {
                    field: format!("pacing_overrides[{index}]"),
                    code: "selector",
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
            ("opencode/opencode-go/claude-haiku-5-5", "$15"),
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
pub const SECTION: DomainSection = DomainSection::new::<RoutingConfig>("routing", defaults);

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
fn default_weight() -> u8 {
    1
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
