use crate::policy::Scoped;
use crate::{
    ModelId, RoutingError,
    request::{effort_valid, identifier},
};
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, Profile, SectionError};
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

/// An expiring preference for an exact model in a known task stratum.
/// Selection must still pass through ordinary pool eligibility checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelChoiceOverride {
    pub dimension: String,
    pub risk_tier: String,
    pub task_kind: String,
    pub preferred: ModelId,
    #[serde(with = "provenant_core::rfc3339::option")]
    pub expires: Option<provenant_core::WallTime>,
}

impl<'de> Deserialize<'de> for ModelChoiceOverride {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Instant(#[serde(with = "provenant_core::rfc3339")] provenant_core::WallTime);
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            dimension: String,
            risk_tier: String,
            task_kind: String,
            preferred: ModelId,
            expires: Option<Scoped<Instant>>,
        }
        let input = Input::deserialize(deserializer)?;
        let mut malformed = false;
        let expires = Scoped::take(input.expires, &mut malformed).map(|instant| instant.0);
        Ok(Self {
            dimension: input.dimension,
            risk_tier: input.risk_tier,
            task_kind: input.task_kind,
            preferred: input.preferred,
            expires,
        })
    }
}

impl ModelChoiceOverride {
    pub fn matches(&self, risk_tier: &str, task_kind: &str, now: provenant_core::WallTime) -> bool {
        self.dimension == "model"
            && self.risk_tier == risk_tier
            && self.task_kind == task_kind
            && self.expires.is_some_and(|expires| now < expires)
    }
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
    /// Exact models excluded from every pool while their schedule is active.
    pub model_exclusions: ModelExclusions,
    pub model_choices: Vec<ModelChoiceOverride>,
    pub model_policy: crate::ModelPolicies,
    /// Override the bundled writer advice; omitted fields keep the bundled values.
    pub writer_advisory: Option<WriterAdvisoryOverride>,
    /// Silence the advisory for Opus implementation lanes.
    pub suppress_opus_writer_warning: bool,
    /// Effective setting from the registered routing knob.
    #[serde(skip)]
    pub allow_unknown_partial_headroom: bool,
    /// Source captured with the effective value by the composition edge.
    #[serde(skip)]
    pub partial_headroom_source: Option<provenant_config::KnobValue>,
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

impl RoutingConfig {
    /// The last matching row wins; removing it revokes the preference.
    pub fn preferred_model(
        &self,
        request: &crate::RouteRequest,
        risk_tier: &str,
        task_kind: &str,
        now: provenant_core::WallTime,
    ) -> Option<&ModelId> {
        if matches!(request.choice, crate::RouteChoice::Model(_)) {
            return None;
        }
        self.model_choices
            .iter()
            .rev()
            .find(|row| row.matches(risk_tier, task_kind, now))
            .map(|row| &row.preferred)
    }
}

/// Settlement retries a lane may take; the run store seals at most this many
/// policy successors in one run.
pub const MAX_SETTLEMENT_RETRIES: u8 = 32;
/// Shipped infrastructure-successor allowance.
pub const DEFAULT_SETTLEMENT_RETRIES: u8 = 2;
/// Longest configurable settlement retry wait: one week.
pub const MAX_RETRY_WAIT_SECONDS: u64 = 7 * 24 * 3_600;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BundledRoutingDefaults {
    model_exclusions: ModelExclusions,
    model_choices: Vec<ModelChoiceOverride>,
    model_policy: crate::ModelPolicies,
    writer_advisory: Option<WriterAdvisoryOverride>,
    suppress_opus_writer_warning: bool,
    capacity_cooldown_seconds: u64,
    settlement_retries: u8,
    capacity_retry_seconds: u64,
    capacity_retry_max_seconds: u64,
    retry_delay_ceiling_seconds: u64,
    review: crate::ReviewConfig,
    routes: BTreeMap<String, NamedRoute>,
    pools: BTreeMap<String, Vec<RoutePoolMember>>,
    pacing: PacingSettings,
    pacing_reserve: BTreeMap<String, PacingReserve>,
    pacing_overrides: Vec<PacingOverride>,
    opencode_go: OpenCodeGoSettings,
}

impl Default for RoutingConfig {
    #[allow(clippy::expect_used)] // The bundled asset is validated by tests.
    fn default() -> Self {
        static DEFAULT: std::sync::OnceLock<RoutingConfig> = std::sync::OnceLock::new();
        DEFAULT
            .get_or_init(|| {
                let asset: BundledRoutingDefaults =
                    toml_edit::de::from_str(defaults(Profile::Balanced))
                        .expect("bundled routing defaults are valid");
                Self {
                    model_exclusions: asset.model_exclusions,
                    model_choices: asset.model_choices,
                    model_policy: asset.model_policy,
                    writer_advisory: asset.writer_advisory,
                    suppress_opus_writer_warning: asset.suppress_opus_writer_warning,
                    allow_unknown_partial_headroom: false,
                    partial_headroom_source: None,
                    capacity_cooldown_seconds: asset.capacity_cooldown_seconds,
                    settlement_retries: asset.settlement_retries,
                    capacity_retry_seconds: asset.capacity_retry_seconds,
                    capacity_retry_max_seconds: asset.capacity_retry_max_seconds,
                    retry_delay_ceiling_seconds: asset.retry_delay_ceiling_seconds,
                    review: asset.review,
                    routes: asset.routes,
                    pools: asset.pools,
                    pacing: asset.pacing,
                    pacing_reserve: asset.pacing_reserve,
                    pacing_overrides: asset.pacing_overrides,
                    opencode_go: asset.opencode_go,
                }
            })
            .clone()
    }
}

/// The persisted model exclusions. A row is read on its own: one that cannot
/// be read in full still excludes its model when the model parses (without a
/// schedule, failing closed), and a row whose model does not parse is dropped
/// and reported in `notes`. Neither refuses the configuration.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
pub struct ModelExclusions {
    rows: Vec<ModelExclusion>,
    #[serde(skip)]
    notes: Vec<String>,
}
impl ModelExclusions {
    /// What was repaired or ignored while reading the rows.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }
}
impl std::ops::Deref for ModelExclusions {
    type Target = Vec<ModelExclusion>;
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}
impl std::ops::DerefMut for ModelExclusions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rows
    }
}
impl<'de> Deserialize<'de> for ModelExclusions {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
        let mut read = Self::default();
        for (index, value) in values.into_iter().enumerate() {
            match serde_json::from_value::<ModelExclusion>(value.clone()) {
                Ok(row) if row.schedule.valid() => read.rows.push(row),
                outcome => {
                    let model = value
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|text| ModelId::parse(text).ok());
                    let reason = match outcome {
                        Ok(_) => "its schedule is invalid".to_owned(),
                        Err(error) => error.to_string(),
                    };
                    match model {
                        Some(model) => {
                            read.notes.push(format!(
                                "model_exclusions[{index}]: {reason}; {} stays excluded with no schedule",
                                model.as_str()
                            ));
                            read.rows.push(ModelExclusion {
                                model,
                                schedule: crate::Schedule::default(),
                            });
                        }
                        None => read.notes.push(format!(
                            "model_exclusions[{index}]: {reason}; the row is ignored"
                        )),
                    }
                }
            }
        }
        Ok(read)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelExclusion {
    pub model: ModelId,
    #[serde(default)]
    pub schedule: crate::Schedule,
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
    /// Every name a node may use as its route alias: configured named routes
    /// and pools, plus the catalogue's pools. Dispatch resolves all three, so
    /// a check that a requested route exists must accept all three.
    pub fn route_names(&self, catalogue: &Catalogue) -> std::collections::BTreeSet<String> {
        self.routes
            .keys()
            .chain(self.pools.keys())
            .chain(catalogue.pools.keys())
            .cloned()
            .collect()
    }

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
        for (index, row) in self.model_choices.iter().enumerate() {
            if row.expires.is_none()
                || row.dimension != "model"
                || !identifier(&row.risk_tier)
                || !identifier(&row.task_kind)
            {
                errors.push(SectionError {
                    detail: None,
                    field: format!("model_choices.{index}"),
                    code: "selector",
                });
            }
        }
        if self.model_exclusions.len() > 256 {
            errors.push(SectionError {
                detail: None,
                field: "model_exclusions".into(),
                code: "limit",
            });
        }
        for (index, exclusion) in self.model_exclusions.iter().enumerate() {
            if !exclusion.schedule.valid() {
                errors.push(SectionError {
                    detail: None,
                    field: format!("model_exclusions.{index}"),
                    code: "schedule",
                });
            }
        }
        if let Some(advice) = &self.writer_advisory {
            advice.validate("writer_advisory", &mut errors);
        }
        if self.settlement_retries > MAX_SETTLEMENT_RETRIES {
            errors.push(SectionError {
                detail: None,
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
                    detail: None,
                    field: field.into(),
                    code: "limit",
                });
            }
        }
        if self.routes.len() > 128 {
            errors.push(SectionError {
                detail: None,
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
                    detail: None,
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
                    detail: None,
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
                detail: None,
                field: "pools".into(),
                code: "limit",
            });
        }
        for (name, members) in &self.pools {
            if !identifier(name) || self.routes.contains_key(name) {
                errors.push(SectionError {
                    detail: None,
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
                    detail: None,
                    field: format!("pools.{name}"),
                    code: "limit",
                });
            }
            let mut seen = BTreeSet::new();
            for (index, member) in members.iter().enumerate() {
                let prefix = format!("pools.{name}[{index}]");
                if !seen.insert(&member.model) {
                    errors.push(SectionError {
                        detail: None,
                        field: format!("{prefix}.model"),
                        code: "duplicate",
                    });
                }
                if member.weight > 100 {
                    errors.push(SectionError {
                        detail: None,
                        field: format!("{prefix}.weight"),
                        code: "range",
                    });
                }
                let model = catalogue
                    .as_ref()
                    .and_then(|value| value.model(&member.model));
                if model.is_none() {
                    errors.push(SectionError {
                        detail: None,
                        field: format!("{prefix}.model"),
                        code: "unknown_model",
                    });
                }
                if !provenant_providers::REGISTRY
                    .iter()
                    .any(|provider| provider.adapter.id() == member.model.adapter())
                {
                    errors.push(SectionError {
                        detail: None,
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
                        detail: None,
                        field: format!("{prefix}.efforts"),
                        code: "unsupported_effort",
                    });
                }
                if member.effort.as_deref().is_some_and(|e| !effort_valid(e)) {
                    errors.push(SectionError {
                        detail: None,
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
                    detail: None,
                    field: format!("pacing.{provider}"),
                    code: "unknown_provider",
                });
            }
        }
        for (account, profile) in &self.pacing.accounts {
            if !account_identifier(account) {
                errors.push(SectionError {
                    detail: None,
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
                    detail: None,
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
                    detail: None,
                    field: format!("pacing_overrides.{index}"),
                    code: "selector",
                });
            } else if !override_.schedule.valid() {
                // Reported at the override, so recovery drops the override
                // rather than its schedule, which would make it permanent.
                errors.push(SectionError {
                    detail: None,
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
                    detail: None,
                    field: format!("pacing_reserve.{provider}"),
                    code: "unknown_provider",
                });
            }
            if reserve.five_hour_percent > 100 || reserve.seven_day_percent > 100 {
                errors.push(SectionError {
                    detail: None,
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
            detail: None,
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
                detail: None,
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
            detail: None,
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
            "opencode/opencode-go/deepseek-v4.1-flash",
            "opencode/opencode-go/glm-5.3-flash",
            "opencode/opencode/deepseek-v4.1-flash-free",
            "opencode/opencode/glm-5.3-flash-free",
        ] {
            assert!(
                check_go_allowance(&ModelId::parse(id)?, &config, &catalogue).is_ok(),
                "{id}"
            );
        }
        let model = ModelId::parse("opencode/opencode-go/unlisted")?;
        let error = check_go_allowance(&model, &config, &catalogue).unwrap_err();
        assert!(
            error.contains("opencode-go/unlisted")
                && error.contains("unknown")
                && error.contains("domains.routing.opencode_go.min_monthly_allowance_usd"),
            "{error}"
        );
        config.opencode_go.min_monthly_allowance_usd = 0;
        assert!(check_go_allowance(&model, &config, &catalogue).is_ok());
        config.opencode_go.min_monthly_allowance_usd = 61;
        let error = check_go_allowance(
            &ModelId::parse("opencode/opencode-go/glm-5.3-flash")?,
            &config,
            &catalogue,
        )
        .unwrap_err();
        assert!(error.contains("$60"), "{error}");
        config.opencode_go.min_monthly_allowance_usd = 60;
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
pub const FIELD_METADATA: &[FieldMetadata] = &[
    FieldMetadata::new("review.floors.{floor}.paths", "list"),
    FieldMetadata::enumeration(
        "review.floors.{floor}.tier",
        &["routine", "substantial", "crucial", "terminal"],
    ),
    FieldMetadata::new("review.floors.{floor}.lenses", "list"),
    FieldMetadata::new("review.shapes.{shape}.frame", "bool"),
    FieldMetadata::int("review.shapes.{shape}.verifiers", 0, 32),
    FieldMetadata::enumeration("review.shapes.{shape}.synthesis", &["model", "reduce"]),
    FieldMetadata::int("settlement_retries", 0, MAX_SETTLEMENT_RETRIES as i64),
    FieldMetadata::int("capacity_retry_seconds", 0, MAX_RETRY_WAIT_SECONDS as i64),
    FieldMetadata::int(
        "capacity_retry_max_seconds",
        0,
        MAX_RETRY_WAIT_SECONDS as i64,
    ),
    FieldMetadata::int(
        "retry_delay_ceiling_seconds",
        0,
        MAX_RETRY_WAIT_SECONDS as i64,
    ),
];
pub const SECTION: DomainSection = DomainSection {
    validate: validate_section,
    advisories: crate::policy::fallback_advisories,
    ..DomainSection::new::<RoutingConfig>("routing", defaults)
        .with_boundary_check(crate::policy::boundary)
        .with_fields(FIELD_METADATA)
        .replacing_tables(&["review.lenses.by_paths"])
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
    /// Covers prompt training and retention. Unknown is ineligible for confidential selection.
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
    if floor == 0
        || !provenant_records::AdapterSpec::get(model.adapter())
            .and_then(|spec| spec.allowance_prefix)
            .is_some_and(|prefix| model.model().starts_with(prefix))
    {
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
                detail: None,
                field: format!("{prefix}.label"),
                code: "label",
            });
        }
        if self.warn_models.len() > 256 || self.cheaper_models.len() > 256 {
            errors.push(SectionError {
                detail: None,
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
                detail: None,
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

/// Why a model named without its adapter did not resolve to exactly one row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BareModelError {
    /// Several adapters offer the name; `candidates` are their `ADAPTER/ID` forms.
    #[error("several providers offer this model")]
    Ambiguous { candidates: Vec<String> },
    /// No catalogued model carries the name.
    #[error("no catalogued provider offers this model")]
    Unknown,
}

/// Production catalogues come from the bundled asset. A caller cannot deserialize
/// or change family/effort rows while retaining an existing revision.
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
// The fixture feature provides an isolated catalogue for integration tests. It
// never changes the process-wide bundled catalogue or production behaviour.
#[cfg(feature = "fixture")]
thread_local! {
    static FIXTURE_CATALOGUE: std::cell::RefCell<Option<Catalogue>> = const { std::cell::RefCell::new(None) };
}

/// Restores the calling thread's catalogue after an integration fixture.
#[cfg(feature = "fixture")]
pub struct FixtureFamilyOverride {
    previous: Option<Catalogue>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(feature = "fixture")]
impl Drop for FixtureFamilyOverride {
    fn drop(&mut self) {
        FIXTURE_CATALOGUE.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}

/// Override existing model families only for the current fixture thread.
#[cfg(feature = "fixture")]
pub fn fixture_family_override(
    families: &[(&str, &str)],
) -> Result<FixtureFamilyOverride, RoutingError> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.revision.push_str("-fixture");
    for (model, family) in families {
        let id = ModelId::parse(model)?;
        let entry = catalogue
            .models
            .iter_mut()
            .find(|entry| entry.model == id)
            .ok_or(RoutingError::Catalogue)?;
        entry.family = Some((*family).to_owned());
    }
    let previous = FIXTURE_CATALOGUE.with(|slot| slot.borrow_mut().replace(catalogue));
    Ok(FixtureFamilyOverride {
        previous,
        _thread: std::marker::PhantomData,
    })
}

impl Catalogue {
    /// The bundled catalogue, parsed and validated once per process. Routing
    /// resolves it for every lane, so rebuilding it dominated a multi-lane run.
    pub fn bundled() -> Result<Self, RoutingError> {
        #[cfg(feature = "fixture")]
        if let Some(catalogue) = FIXTURE_CATALOGUE.with(|slot| slot.borrow().clone()) {
            return Ok(catalogue);
        }
        static BUNDLED: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
        if let Some(catalogue) = BUNDLED.get() {
            return Ok(catalogue.clone());
        }
        let catalogue = Self::parse_bundled()?;
        Ok(BUNDLED.get_or_init(|| catalogue).clone())
    }
    fn parse_bundled() -> Result<Self, RoutingError> {
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
            if provenant_records::AdapterSpec::get(id.adapter())
                .is_some_and(|spec| spec.namespaced_models)
            {
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
    /// Resolve a model named without its adapter, such as `claude-sonnet-5-5`.
    /// Exactly one catalogued model must carry that ID. Several offers name
    /// their `ADAPTER/ID` forms, and none is an unknown name.
    pub fn resolve_bare_model(&self, name: &str) -> Result<ModelId, BareModelError> {
        let offered: Vec<&ModelId> = self
            .models
            .iter()
            .map(|row| &row.model)
            .filter(|id| !name.contains('/') && id.model() == name)
            .collect();
        match offered.as_slice() {
            [only] => Ok((*only).clone()),
            [] => Err(BareModelError::Unknown),
            several => Err(BareModelError::Ambiguous {
                candidates: several.iter().map(|id| id.as_str().to_owned()).collect(),
            }),
        }
    }
    /// The single owner of a model's quota pool group. An uncatalogued model
    /// draws on the catalogued pool its family prefix names, so
    /// `agy/gemini-4-pro` follows the gemini windows; an unknown family keeps
    /// every window.
    pub fn quota_pool(&self, id: &ModelId) -> Option<&str> {
        if let Some(row) = self.model(id) {
            return row.quota_pool.as_deref();
        }
        let family = id.model().split('-').next()?;
        self.models
            .iter()
            .filter(|row| row.model.adapter() == id.adapter())
            .find_map(|row| row.quota_pool.as_deref().filter(|pool| *pool == family))
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
    /// The pools a model draws on: the free allowance and its catalogued group.
    pub fn admission_pools(&self, model: &ModelId) -> Vec<String> {
        let mut pools = free_pools(model.adapter(), || self.is_free_model(model));
        pools.extend(self.quota_pool(model).map(str::to_owned));
        pools
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

/// Only OpenCode draws on the keyless free allowance, whatever a pool lists.
pub fn free_pools(adapter: &str, free: impl FnOnce() -> bool) -> Vec<String> {
    provenant_records::AdapterSpec::get(adapter)
        .and_then(|spec| spec.free_pool)
        .filter(|_| free())
        .map(|pool| vec![pool.to_owned()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessContext, Fallback, RouteChoice, RouteRequest, RoutingContext, resolve};
    use provenant_providers::{AvailabilityRecord, REGISTRY};

    #[test]
    fn route_names_include_configured_and_catalogue_pools() -> Result<(), Box<dyn std::error::Error>>
    {
        let catalogue = Catalogue::bundled()?;
        let mut config = RoutingConfig::default();
        config.pools.insert("verifier".into(), Vec::new());
        let names = config.route_names(&catalogue);
        assert!(names.contains("verifier"), "{names:?}");
        assert!(names.contains("junior"), "{names:?}");
        assert!(!names.contains("absent"), "{names:?}");
        Ok(())
    }

    #[test]
    fn writer_advisory_classes_and_pool_priority_are_overridable()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let context = RoutingContext {
            policy_account_hint: None,
            choice_context: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            launch_specs: None,
            readiness: None,
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
            fallback: Fallback::None,
        })
    }

    #[test]
    fn bundled_opus_writer_advice_names_exact_routes() -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let context = RoutingContext {
            policy_account_hint: None,
            choice_context: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            launch_specs: None,
            readiness: None,
            access: &[],
            now: provenant_core::WallTime::from_micros(0)?,
            host: "test-host",
        };
        for model in &catalogue.writer_advisory.warn_models {
            for (pool, members) in &catalogue.pools {
                let mut route = resolve(&request_for(model.as_str())?, &context)?;
                route.effort = Some("high".into());
                route.pool_selection = Some(provenant_records::RouteSelectionReceipt {
                    extra: Default::default(),
                    pool: pool.clone(),
                    candidates: vec![],
                    roll: 0,
                    total_weight: 1,
                    pick: format!("{}/{}", route.adapter, route.model),
                    reason: None,
                    redraws: vec![],
                });
                let advice = config.opus_writer_advisory(&catalogue, &route);
                let cheaper = members.iter().find(|member| {
                    member.weight > 0
                        && member.model != *model
                        && catalogue
                            .writer_advisory
                            .cheaper_models
                            .contains(&member.model)
                });
                match cheaper {
                    Some(cheaper) => {
                        let advice = advice.ok_or("missing writer advisory")?;
                        for part in [
                            "Opus",
                            model.as_str(),
                            pool.as_str(),
                            cheaper.model.as_str(),
                            "suppress_opus_writer_warning",
                        ] {
                            assert!(advice.contains(part), "{advice}");
                        }
                    }
                    None => assert!(advice.is_none(), "unexpected advisory for {pool}"),
                }
            }
        }
        let route = resolve(&request_for("claude/claude-opus-5")?, &context)?;
        assert!(config.opus_writer_advisory(&catalogue, &route).is_none());
        Ok(())
    }

    #[test]
    fn partial_writer_advisory_override_keeps_bundled_classes()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let base = RoutingConfig::default();
        let context = RoutingContext {
            policy_account_hint: None,
            choice_context: None,
            catalogue: &catalogue,
            config: &base,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            launch_specs: None,
            readiness: None,
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
    fn model_policy_fallbacks_are_reported_for_the_effective_route()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = RoutingConfig::default();
        config.model_policy.defaults.result.fallback =
            Some(ModelId::parse("codex/not-catalogued")?);
        assert!(config.validate().is_empty());
        let effective = config.model_policy.effective(
            &ModelId::parse("codex/gpt-6-luna")?,
            None,
            provenant_core::WallTime::from_unix_seconds(1)?,
        );
        assert!(effective.policy.result.fallback.is_none());
        assert_eq!(
            effective.dropped_fallback,
            Some((ModelId::parse("codex/not-catalogued")?, "unknown model"))
        );

        config.model_policy.defaults.result.fallback = Some(ModelId::parse("codex/gpt-6-luna")?);
        config.model_policy.defaults.result.fallback_effort = Some("unsupported".into());
        assert!(config.validate().is_empty());
        let effective = config.model_policy.effective(
            &ModelId::parse("codex/gpt-6-luna")?,
            None,
            provenant_core::WallTime::from_unix_seconds(1)?,
        );
        assert_eq!(
            effective.dropped_fallback,
            Some((ModelId::parse("codex/gpt-6-luna")?, "unsupported effort"))
        );
        Ok(())
    }

    #[test]
    fn bundled_pools_prefer_free_flash_and_exclude_other_opencode_models()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let junior = &catalogue.pools["junior"];
        let mut pairs = 0;
        for members in catalogue.pools.values() {
            for member in members {
                let name = member.model.as_str();
                if name.starts_with("opencode/") {
                    assert!(
                        name.contains("flash"),
                        "unexpected default pool model: {name}"
                    );
                }
            }
        }
        for free in junior
            .iter()
            .filter(|member| catalogue.is_free_model(&member.model))
        {
            let Some(name) = free
                .model
                .as_str()
                .strip_prefix("opencode/opencode/")
                .and_then(|name| name.strip_suffix("-free"))
            else {
                continue;
            };
            let paid = ModelId::parse(&format!("opencode/opencode-go/{name}"))?;
            if let Some(paid) = junior.iter().find(|member| member.model == paid) {
                assert!(free.weight > paid.weight);
                pairs += 1;
            }
        }
        assert!(
            pairs > 0,
            "a free flash model should outweigh its paid counterpart"
        );
        Ok(())
    }

    #[test]
    fn bundled_go_models_resolve_and_only_flash_models_have_provisional_pool_weights()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let model_names: Vec<_> = catalogue
            .models()
            .iter()
            .filter_map(|entry| entry.model.as_str().strip_prefix("opencode/opencode-go/"))
            .collect();
        assert!(!model_names.is_empty());
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
            choice_context: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &availability,
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            launch_specs: None,
            readiness: None,
            access: &access,
            now: provenant_core::WallTime::from_micros(100_000_000)?,
            host: "test-host",
        };
        for name in model_names {
            let model = ModelId::parse(&format!("opencode/opencode-go/{name}"))?;
            let entry = catalogue.model(&model).ok_or("Go model missing")?;
            assert!(!entry.evidence_ref.is_empty());
            let route = resolve(
                &RouteRequest {
                    intent: Default::default(),
                    choice: RouteChoice::Model(model.clone()),
                    effort: None,
                    confidential: false,
                    fallback: Fallback::None,
                },
                &context,
            )?;
            assert_eq!(route.model, format!("opencode-go/{name}"));
            for member in catalogue
                .pools
                .values()
                .flatten()
                .filter(|member| member.model == model)
            {
                assert!(name.contains("flash"));
                assert!(!member.free);
                assert!(member.effort.is_none());
                assert!(member.efforts.is_empty());
            }
        }
        Ok(())
    }
}
