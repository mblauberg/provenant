//! A lane or graph limit with where its value came from and whether the host
//! enforces it. The lane preamble states only what the host will enforce.
pub use crate::Layer;
use crate::{Precedence, Source, Sourced};
use serde::{Deserialize, Serialize};

/// The layers a limit accepts, lowest first. One declaration drives the merge
/// and every explanation (Spec: docs/specs/state.md#every-setting-reports-its-layer).
pub const LIMIT_PRECEDENCE: Precedence = Precedence(&[
    Layer::Default,
    Layer::Host,
    Layer::Project,
    Layer::Tier,
    Layer::Template,
    Layer::Launch,
]);

/// Pair a layer with the value it declares for the shared merge. The origin is
/// absent here because the callers already collapse each layer to one value.
fn sourced<T>(layer: Layer, value: T) -> Sourced<T> {
    Sourced {
        value,
        source: Source {
            layer,
            origin: None,
        },
    }
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

/// Host evidence recorded when a lane crosses its token budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetOverrun {
    #[serde(flatten)]
    pub header: crate::RecordHeader,
    pub tokens: u64,
    pub budget: u64,
}

/// A budget continuation was considered but could not be admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetWrapUpRefused {
    #[serde(flatten)]
    pub header: crate::RecordHeader,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limit {
    pub value: u64,
    pub source: Layer,
    pub strength: LimitStrength,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrun: Option<BudgetOverrun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap_up_refused: Option<BudgetWrapUpRefused>,
    /// The pre-wrap-up target budget retained alongside a bounded continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap_up_target: Option<Box<Limit>>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
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
    pub const ALL: [Self; 3] = [Self::WrapUp, Self::Warn, Self::Stop];
    /// The configuration spellings, in [`Self::ALL`] order. Schema metadata
    /// for every budget-action field uses this one declaration.
    pub const NAMES: [&'static str; Self::ALL.len()] = {
        let mut names = [""; Self::ALL.len()];
        let mut index = 0;
        while index < Self::ALL.len() {
            names[index] = Self::ALL[index].as_str();
            index += 1;
        }
        names
    };

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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetPolicy {
    pub action: BudgetAction,
    pub action_source: Layer,
    pub grace: BudgetGrace,
    pub grace_source: Layer,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            action: BudgetAction::default(),
            action_source: Layer::Default,
            grace: BudgetGrace::default(),
            grace_source: Layer::Default,
            extra: Default::default(),
        }
    }
}

impl BudgetPolicy {
    /// Resolve each policy field from its nearest configured layer. The order
    /// is launch, template, tier, project, host, then the compiled default.
    pub fn resolve(
        actions: impl IntoIterator<Item = (Layer, BudgetAction)>,
        graces: impl IntoIterator<Item = (Layer, BudgetGrace)>,
    ) -> Self {
        let mut policy = Self::default();
        let actions = LIMIT_PRECEDENCE.merge(
            actions
                .into_iter()
                .map(|(layer, value)| sourced(layer, value)),
        );
        crate::warn_ignored(&actions.ignored, "budget action");
        if let Some(winner) = actions.winner {
            policy.action = winner.value;
            policy.action_source = winner.source.layer;
        }
        let graces = LIMIT_PRECEDENCE.merge(
            graces
                .into_iter()
                .map(|(layer, value)| sourced(layer, value)),
        );
        crate::warn_ignored(&graces.ignored, "budget grace");
        if let Some(winner) = graces.winner {
            policy.grace = winner.value;
            policy.grace_source = winner.source.layer;
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
    pub fn new(value: u64, source: Layer, strength: LimitStrength) -> Self {
        Self {
            value,
            source,
            strength,
            overrun: None,
            wrap_up_refused: None,
            wrap_up_target: None,
            extra: Default::default(),
        }
    }

    /// The setting from the nearest source wins; `None` when nothing sets it.
    pub fn resolve(
        layers: impl IntoIterator<Item = (Layer, u64)>,
        strength: LimitStrength,
    ) -> Option<Self> {
        let merge = LIMIT_PRECEDENCE.merge(
            layers
                .into_iter()
                .map(|(layer, value)| sourced(layer, value)),
        );
        crate::warn_ignored(&merge.ignored, "limit");
        merge
            .winner
            .map(|winner| Self::new(winner.value, winner.source.layer, strength))
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
        let mut raised = Self::new(value.max(self.value), Layer::Launch, self.strength);
        raised.wrap_up_target = self.wrap_up_target.clone();
        Ok(raised)
    }
}

/// Seconds without provider output after which a running attempt is reported
/// as stalled. Zero disables the report. This is a read-only projection: it
/// never stops a provider and is never a reason to signal one.
pub const DEFAULT_STALL_AFTER_SECONDS: u64 = 30 * 60;

/// What an owner opts into when an attempt goes silent past its threshold.
/// The default only warns; no action happens without an explicit choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StallPolicy {
    /// Report the stall and nothing more.
    #[default]
    Report,
    /// Warn, then queue a steering turn for this exact attempt.
    Steer,
    /// Warn, then stop with `provider_stalled_retry` so the graph can advance
    /// to the next route in the pool.
    Retry,
    /// Warn, then stop the provider and settle with `provider_stalled`.
    Fail,
}

impl StallPolicy {
    pub const ALL: [Self; 4] = [Self::Report, Self::Steer, Self::Retry, Self::Fail];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Steer => "steer",
            Self::Retry => "retry",
            Self::Fail => "fail",
        }
    }

    /// The default is omitted from persisted limits; every action is explicit.
    pub const fn is_report(&self) -> bool {
        matches!(self, Self::Report)
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
    /// `gh` invocations after which the lane's gh wrapper warns. Always
    /// advisory: it never stops the lane and is not an API request cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_invocations: Option<Limit>,
    /// Seconds without output after which a running attempt reads as stalled.
    /// The limit carries the layer that set it (`host` or the compiled default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_after_seconds: Option<Limit>,
    /// Opt-in response to a stall. The compiled `report` default never acts.
    #[serde(default, skip_serializing_if = "StallPolicy::is_report")]
    pub stall_policy: StallPolicy,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

/// The unit a writer token budget counts, the figure it counted for one
/// attempt and the provider's own totals beside it. `status` and `result`
/// both project this from the same limit and usage record, so the unit named
/// to the chair is the one enforcement used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenBudgetUsage {
    /// Stable unit name: fresh tokens (cache reads excluded).
    pub unit: String,
    /// Plain-language definition of what the unit counts and excludes.
    pub unit_detail: String,
    /// The budget's value in that unit.
    pub limit: u64,
    /// The layer that set the budget.
    pub source: Layer,
    /// Fresh tokens counted for this attempt; absent when the provider
    /// reported no usage, never a guessed zero.
    pub counted: Option<u64>,
    /// The provider's cumulative input including cache reads, for comparison
    /// with `counted`. A different number is expected, not an inconsistency.
    pub reported_input: Option<u64>,
    /// Whether the reported totals cover this attempt or the provider's whole
    /// session. Codex session totals cannot be attributed to one attempt.
    pub basis: String,
    /// Provider-specific caveat, such as turn-boundary enforcement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl LaneLimits {
    /// Project the budget's unit and counted usage for one attempt. Absent
    /// when no writer token budget was set; `provider` adds an adapter's
    /// caveat when it declares one.
    pub fn budget_usage(
        &self,
        usage: &crate::run::UsageRecord,
        provider: Option<&str>,
    ) -> Option<TokenBudgetUsage> {
        let budget = self.token_budget.as_ref()?;
        let spec = provider.and_then(crate::AdapterSpec::get);
        Some(TokenBudgetUsage {
            unit: "fresh_tokens".into(),
            unit_detail:
                "uncached input + cache writes + output and reasoning; cache reads excluded".into(),
            limit: budget.value,
            source: budget.source,
            counted: usage.fresh_tokens(),
            reported_input: usage.input_including_cache(),
            basis: if spec.is_some_and(|spec| spec.usage_scope == crate::UsageAttribution::Session)
                && usage.per_attempt != Some(true)
            {
                "session".into()
            } else {
                "attempt".into()
            },
            notice: spec
                .and_then(|spec| spec.token_budget_notice)
                .map(str::to_owned),
        })
    }

    /// The limits that are set, by name.
    pub fn named(&self) -> impl Iterator<Item = (&'static str, &Limit)> {
        [
            ("token_budget", &self.token_budget),
            ("context_handoff", &self.context_handoff),
            ("deadline_seconds", &self.deadline_seconds),
            ("repair_cycles", &self.repair_cycles),
            ("gh_invocations", &self.gh_invocations),
            ("stall_after_seconds", &self.stall_after_seconds),
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
            "gh_invocations" => &mut self.gh_invocations,
            "stall_after_seconds" => &mut self.stall_after_seconds,
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
                    let policy = self.budget_policy.clone().unwrap_or_default();
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
                "repair_cycles" => {
                    format!("At most {} review and repair rounds run.", limit.value)
                }
                // Advisory limits are never enforced; the lane preamble
                // states the gh budget with the CI scope.
                _ => continue,
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

    /// Every action's configuration spelling is the one serde reads and writes.
    #[test]
    fn budget_action_names_are_its_serde_spellings() {
        for (index, action) in BudgetAction::ALL.iter().copied().enumerate() {
            match action {
                BudgetAction::WrapUp | BudgetAction::Warn | BudgetAction::Stop => {}
            }
            let name = action.as_str();
            assert_eq!(BudgetAction::NAMES[index], name);
            assert_eq!(serde_json::to_value(action).unwrap(), name);
            assert_eq!(
                serde_json::from_value::<BudgetAction>(name.into()).unwrap(),
                action
            );
        }
    }

    #[test]
    fn budget_evidence_fields_round_trip_and_reject_malformed_values() {
        for (field, value, malformed) in [
            (
                "overrun",
                serde_json::json!({"schema_version":1,"tokens":101,"budget":100}),
                serde_json::json!({"schema_version":1,"tokens":"invalid","budget":100}),
            ),
            (
                "wrap_up_refused",
                serde_json::json!({"schema_version":1,"reason":"admission refused"}),
                serde_json::json!({"schema_version":1,"reason":false}),
            ),
            (
                "wrap_up_target",
                serde_json::json!({"value":200,"source":"template","strength":"default"}),
                serde_json::json!({"value":"invalid","source":"template","strength":"default"}),
            ),
        ] {
            let mut wire =
                serde_json::to_value(Limit::new(100, Layer::Launch, LimitStrength::Default))
                    .unwrap();
            wire[field] = value;
            let bytes = serde_json::to_vec(&wire).unwrap();
            let decoded: Limit =
                crate::json::decode(bytes.as_slice(), crate::json::Limits::default()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
            wire[field] = malformed;
            let bytes = serde_json::to_vec(&wire).unwrap();
            assert!(matches!(
                crate::json::decode::<Limit, _>(bytes.as_slice(), crate::json::Limits::default()),
                Err(crate::json::DecodeError::Typed { .. })
            ));
        }
    }

    #[test]
    fn flattened_header_keeps_unknown_fields_from_a_newer_writer() {
        let mut wire = serde_json::json!({"schema_version": 1, "tokens": 10, "budget": 5});
        wire["from_a_newer_writer"] = serde_json::json!(true);
        let bytes = serde_json::to_vec(&wire).unwrap();
        let decoded: BudgetOverrun =
            crate::json::decode(bytes.as_slice(), crate::json::Limits::default()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), wire);
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn every_limit_layer_reports_the_same_winner_and_shadowed_order() {
        let candidates = [
            (Layer::Default, 1),
            (Layer::Host, 2),
            (Layer::Project, 3),
            (Layer::Tier, 4),
            (Layer::Template, 5),
            (Layer::Launch, 6),
        ];
        let merge = LIMIT_PRECEDENCE.merge(
            candidates
                .into_iter()
                .map(|(layer, value)| sourced(layer, value)),
        );
        assert_eq!(merge.winner.as_ref().map(|winner| winner.value), Some(6));
        assert_eq!(
            merge
                .shadowed
                .iter()
                .map(|candidate| candidate.value)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert!(merge.ignored.is_empty());
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn a_profile_candidate_is_ignored_and_warned_by_the_production_resolver() {
        let (limit, warnings) = crate::warning::capture(|| {
            Limit::resolve(
                [(Layer::Profile, 42), (Layer::Host, 7)],
                LimitStrength::Default,
            )
        });
        let limit = limit.expect("a limit resolves");
        assert_eq!((limit.value, limit.source), (7, Layer::Host));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_ref(), "SETTING_LAYER_IGNORED");
        assert_eq!(
            warnings[0].source.as_ref().map(|source| source.layer),
            Some(Layer::Profile)
        );

        let (policy, warnings) = crate::warning::capture(|| {
            BudgetPolicy::resolve(
                [(Layer::Profile, BudgetAction::Warn)],
                [(Layer::Profile, BudgetGrace::default())],
            )
        });
        assert_eq!(policy.action_source, Layer::Default);
        assert_eq!(policy.grace_source, Layer::Default);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn the_nearest_source_wins_and_a_ceiling_cannot_be_raised() {
        let layers = [
            (Layer::Default, 500),
            (Layer::Project, 900),
            (Layer::Host, 700),
            (Layer::Template, 800),
        ];
        let limit = Limit::resolve(layers, LimitStrength::Default).unwrap();
        assert_eq!((limit.value, limit.source), (800, Layer::Template));
        let raised = limit.extend(1000).unwrap();
        assert_eq!((raised.value, raised.source), (1000, Layer::Launch));
        assert_eq!(limit.extend(10).unwrap().value, 800);
        let ceiling = Limit::new(60, Layer::Host, LimitStrength::Ceiling);
        assert_eq!(ceiling.extend(120), Err(CeilingError { value: 60 }));
    }

    #[test]
    fn budget_usage_names_the_unit_beside_the_provider_totals() {
        let limits = LaneLimits {
            token_budget: Some(Limit::new(1_000, Layer::Host, LimitStrength::Default)),
            ..LaneLimits::default()
        };
        let usage = crate::run::UsageRecord {
            tokens: crate::Tokens {
                input: Some(100),
                cache_read: Some(5_000),
                cache_write: Some(20),
                output: Some(30),
                reasoning: Some(10),
                ..crate::Tokens::default()
            },
            ..Default::default()
        };
        let codex = limits.budget_usage(&usage, Some("codex")).unwrap();
        assert_eq!(codex.unit, "fresh_tokens");
        assert_eq!((codex.limit, codex.source), (1_000, Layer::Host));
        // Cache reads are in the provider's figure and out of the budget's.
        assert_eq!(codex.counted, Some(160));
        assert_eq!(codex.reported_input, Some(5_120));
        assert_eq!(codex.basis, "session");
        assert!(codex.notice.is_some());

        let marked = crate::run::UsageRecord {
            per_attempt: Some(true),
            ..usage.clone()
        };
        let marked = limits.budget_usage(&marked, Some("codex")).unwrap();
        assert_eq!(marked.basis, "attempt");

        let absent = limits
            .budget_usage(&crate::run::UsageRecord::default(), Some("codex"))
            .unwrap();
        assert_eq!((absent.counted, absent.reported_input), (None, None));
        assert!(LaneLimits::default().budget_usage(&usage, None).is_none());
    }

    #[test]
    fn the_preamble_states_only_enforced_limits_with_the_resolved_numbers() {
        let limits = LaneLimits {
            token_budget: Some(Limit::new(123, Layer::Host, LimitStrength::Default)),
            deadline_seconds: Some(Limit::new(60, Layer::Host, LimitStrength::Ceiling)),
            repair_cycles: Some(Limit::new(3, Layer::Template, LimitStrength::Advisory)),
            ..LaneLimits::default()
        };
        let preamble = limits.preamble();
        for (_, limit) in limits.named().filter(|(_, limit)| limit.enforced()) {
            assert!(preamble.contains(&limit.value.to_string()), "{preamble}");
        }
        assert!(!preamble.contains("repair"), "{preamble}");
    }
}
