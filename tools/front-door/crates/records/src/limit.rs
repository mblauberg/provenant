//! A lane or graph limit with where its value came from and whether the host
//! enforces it. The lane preamble states only what the host will enforce.
use serde::{Deserialize, Serialize};

/// Where a limit's value was set. The nearest setting to the work wins:
/// `Launch`, then `Template`, then `Tier`, then `Project`, then `Host`, then `Default`
/// (see [`Limit::resolve`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitSource {
    /// The built-in default.
    Default,
    /// The host configuration.
    Host,
    /// The project configuration.
    Project,
    /// The template the node expands from.
    Template,
    /// The risk tier selected for this lane.
    Tier,
    /// The launch request, set by the chair or the owner.
    Launch,
}

/// What reaching the limit does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitStrength {
    /// The host stops or refuses the work at the value, and `retry --extend`
    /// may raise it.
    Default,
    /// The host stops or refuses the work at the value, and nothing raises it.
    Ceiling,
    /// The host reports the limit to the agent and does not act on it.
    Advisory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limit {
    pub value: u64,
    pub source: LimitSource,
    pub strength: LimitStrength,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

impl LimitSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Host => "host",
            Self::Project => "project",
            Self::Template => "template",
            Self::Tier => "tier",
            Self::Launch => "launch",
        }
    }

    /// Higher wins.
    fn precedence(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::Host => 1,
            Self::Project => 2,
            Self::Tier => 3,
            Self::Template => 4,
            Self::Launch => 5,
        }
    }
}

/// What the host does when a writer passes its token budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    /// Continue once through the existing resume path to let the writer wrap up.
    #[default]
    WrapUp,
    /// Report the crossed budget without stopping the lane.
    Warn,
    /// Stop the lane as soon as the supervisor can enforce the limit.
    Stop,
}

impl BudgetAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WrapUp => "wrap_up",
            Self::Warn => "warn",
            Self::Stop => "stop",
        }
    }
}

/// A bounded fractional grace above a token budget, stored in millionths so
/// policy records retain equality. It serialises as decimal text ("0.1"),
/// because sealed records carry no floats; input also accepts a JSON number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BudgetGrace(u64);

impl BudgetGrace {
    pub const SCALE: u64 = 1_000_000;
    pub const DEFAULT: Self = Self(100_000);

    /// Construct a finite nonnegative grace fraction representable in millionths.
    pub fn from_fraction(value: f64) -> Option<Self> {
        let scaled = value * Self::SCALE as f64;
        if !value.is_finite() || value < 0.0 || !scaled.is_finite() || scaled > u64::MAX as f64 {
            return None;
        }
        Some(Self(scaled.round() as u64))
    }

    /// Return the fraction represented by this fixed-point value.
    pub const fn as_fraction(self) -> f64 {
        self.0 as f64 / Self::SCALE as f64
    }

    /// Return the integer millionths used by this fixed-point value.
    pub const fn millionths(self) -> u64 {
        self.0
    }
}

impl Default for BudgetGrace {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for BudgetGrace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whole = self.0 / Self::SCALE;
        let part = self.0 % Self::SCALE;
        if part == 0 {
            return write!(formatter, "{whole}");
        }
        let digits = format!("{part:06}");
        write!(formatter, "{whole}.{}", digits.trim_end_matches('0'))
    }
}

impl Serialize for BudgetGrace {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BudgetGrace {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Text {
            Number(f64),
            Decimal(String),
        }
        let value = match Text::deserialize(deserializer)? {
            Text::Number(value) => Some(value),
            Text::Decimal(text) => text.trim().parse::<f64>().ok(),
        };
        value.and_then(Self::from_fraction).ok_or_else(|| {
            serde::de::Error::custom("budget grace must be a finite nonnegative fraction")
        })
    }
}

/// The resolved soft-budget behaviour and the winning source for each field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetPolicy {
    pub action: BudgetAction,
    pub action_source: LimitSource,
    pub grace: BudgetGrace,
    pub grace_source: LimitSource,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            action: BudgetAction::default(),
            action_source: LimitSource::Default,
            grace: BudgetGrace::default(),
            grace_source: LimitSource::Default,
        }
    }
}

impl BudgetPolicy {
    /// Resolve each policy field from its nearest configured layer. The order
    /// is launch, template, tier, project, host, then the compiled default.
    pub fn resolve(
        actions: impl IntoIterator<Item = (LimitSource, BudgetAction)>,
        graces: impl IntoIterator<Item = (LimitSource, BudgetGrace)>,
    ) -> Self {
        let mut policy = Self::default();
        if let Some((source, action)) = actions
            .into_iter()
            .max_by_key(|(source, _)| source.precedence())
        {
            policy.action = action;
            policy.action_source = source;
        }
        if let Some((source, grace)) = graces
            .into_iter()
            .max_by_key(|(source, _)| source.precedence())
        {
            policy.grace = grace;
            policy.grace_source = source;
        }
        policy
    }
}

/// A ceiling cannot be raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CeilingError {
    pub value: u64,
}

impl Limit {
    pub fn new(value: u64, source: LimitSource, strength: LimitStrength) -> Self {
        Self {
            value,
            source,
            strength,
            extra: Default::default(),
        }
    }

    /// The setting from the nearest source wins; `None` when nothing sets it.
    pub fn resolve(
        layers: impl IntoIterator<Item = (LimitSource, u64)>,
        strength: LimitStrength,
    ) -> Option<Self> {
        layers
            .into_iter()
            .max_by_key(|(source, _)| source.precedence())
            .map(|(source, value)| Self::new(value, source, strength))
    }

    /// The host stops or refuses at this limit.
    pub fn enforced(&self) -> bool {
        matches!(
            self.strength,
            LimitStrength::Default | LimitStrength::Ceiling
        )
    }

    /// Raise the limit to `value` for a launch. A ceiling refuses; a value
    /// below the current one leaves it unchanged.
    pub fn extend(&self, value: u64) -> Result<Self, CeilingError> {
        if self.strength == LimitStrength::Ceiling {
            return Err(CeilingError { value: self.value });
        }
        Ok(Self::new(
            value.max(self.value),
            LimitSource::Launch,
            self.strength,
        ))
    }
}

/// Every limit of one lane, resolved once at admission. The enforcement, the
/// lane preamble and `status` all read this one record, so what the agent is
/// told is what the host enforces. A limit that is off or does not apply is
/// `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneLimits {
    /// Fresh tokens a writer may spend before the host stops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<Limit>,
    /// Resolved action and grace for the writer token budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_policy: Option<BudgetPolicy>,
    /// Context tokens at which a writer is handed off to a fresh attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_handoff: Option<Limit>,
    /// Wall-clock seconds before the lane is stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_seconds: Option<Limit>,
    /// Review and repair rounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_cycles: Option<Limit>,
}

impl LaneLimits {
    /// The limits that are set, by name.
    pub fn named(&self) -> impl Iterator<Item = (&'static str, &Limit)> {
        [
            ("token_budget", &self.token_budget),
            ("context_handoff", &self.context_handoff),
            ("deadline_seconds", &self.deadline_seconds),
            ("repair_cycles", &self.repair_cycles),
        ]
        .into_iter()
        .filter_map(|(name, limit)| limit.as_ref().map(|limit| (name, limit)))
    }

    /// Raise the named limit (see [`Limit::extend`]). `None` when no limit
    /// of that name is set or the name is unknown.
    pub fn extend(&mut self, name: &str, value: u64) -> Option<Result<(), CeilingError>> {
        let limit = match name {
            "token_budget" => &mut self.token_budget,
            "context_handoff" => &mut self.context_handoff,
            "deadline_seconds" => &mut self.deadline_seconds,
            "repair_cycles" => &mut self.repair_cycles,
            _ => return None,
        };
        let current = limit.as_ref()?;
        Some(current.extend(value).map(|raised| *limit = Some(raised)))
    }

    /// The preamble block: one line per enforced limit, empty when none.
    pub fn preamble(&self) -> String {
        let mut text = String::new();
        for (name, limit) in self.named().filter(|(_, limit)| limit.enforced()) {
            let line = match name {
                "token_budget" => {
                    let policy = self.budget_policy.unwrap_or_default();
                    let action = match policy.action {
                        BudgetAction::WrapUp => format!(
                            "resumes this session once to commit, push and report, with {}% grace",
                            policy.grace.as_fraction() * 100.0
                        ),
                        BudgetAction::Warn => "warns and lets this lane continue".into(),
                        BudgetAction::Stop => {
                            "stops dirty work and snapshots remaining edits".into()
                        }
                    };
                    format!(
                        "After {} fresh tokens (source: {}; cache reads do not count), the host {action} (action source: {}; grace source: {}). Natural completion and a clean pushed worktree keep the lane's own outcome.",
                        limit.value,
                        limit.source.as_str(),
                        policy.action_source.as_str(),
                        policy.grace_source.as_str()
                    )
                }
                "context_handoff" => format!(
                    "The host stops this lane for a fresh attempt when one call's context passes {} tokens.",
                    limit.value
                ),
                "deadline_seconds" => {
                    format!("The host stops this lane after {} seconds.", limit.value)
                }
                _ => format!("At most {} review and repair rounds run.", limit.value),
            };
            text.push_str(&line);
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_source_wins_and_a_ceiling_cannot_be_raised() {
        let layers = [
            (LimitSource::Default, 500),
            (LimitSource::Project, 900),
            (LimitSource::Host, 700),
            (LimitSource::Template, 800),
        ];
        let limit = Limit::resolve(layers, LimitStrength::Default).unwrap();
        assert_eq!((limit.value, limit.source), (800, LimitSource::Template));
        let raised = limit.extend(1000).unwrap();
        assert_eq!((raised.value, raised.source), (1000, LimitSource::Launch));
        assert_eq!(limit.extend(10).unwrap().value, 800);
        let ceiling = Limit::new(60, LimitSource::Host, LimitStrength::Ceiling);
        assert_eq!(ceiling.extend(120), Err(CeilingError { value: 60 }));
    }

    #[test]
    fn the_preamble_states_only_enforced_limits_with_the_resolved_numbers() {
        let limits = LaneLimits {
            token_budget: Some(Limit::new(123, LimitSource::Host, LimitStrength::Default)),
            deadline_seconds: Some(Limit::new(60, LimitSource::Host, LimitStrength::Ceiling)),
            repair_cycles: Some(Limit::new(
                3,
                LimitSource::Template,
                LimitStrength::Advisory,
            )),
            ..LaneLimits::default()
        };
        let preamble = limits.preamble();
        for (_, limit) in limits.named().filter(|(_, limit)| limit.enforced()) {
            assert!(preamble.contains(&limit.value.to_string()), "{preamble}");
        }
        assert!(!preamble.contains("repair"), "{preamble}");
    }
}
