//! A lane or graph limit with where its value came from and whether the host
//! enforces it. The lane preamble states only what the host will enforce.
use serde::{Deserialize, Serialize};

/// Where a limit's value was set. The nearest setting to the work wins:
/// `Launch`, then `Template`, then `Project`, then `Host`, then `Default`
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
            Self::Launch => "launch",
        }
    }

    /// Higher wins.
    fn precedence(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::Host => 1,
            Self::Project => 2,
            Self::Template => 3,
            Self::Launch => 4,
        }
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
    /// Context tokens at which a writer is handed off to a fresh attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_handoff: Option<Limit>,
    /// Wall-clock seconds before the lane is stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_seconds: Option<Limit>,
    /// Continuations after a stop, shared across the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuations: Option<Limit>,
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
            ("continuations", &self.continuations),
            ("repair_cycles", &self.repair_cycles),
        ]
        .into_iter()
        .filter_map(|(name, limit)| limit.as_ref().map(|limit| (name, limit)))
    }

    /// The preamble block: one line per enforced limit, empty when none.
    pub fn preamble(&self) -> String {
        let mut text = String::new();
        for (name, limit) in self.named().filter(|(_, limit)| limit.enforced()) {
            let line = match name {
                "token_budget" => format!(
                    "The host stops this lane after {} fresh tokens (cache reads do not count) and commits uncommitted work.",
                    limit.value
                ),
                "context_handoff" => format!(
                    "The host stops this lane for a fresh attempt when one call's context passes {} tokens.",
                    limit.value
                ),
                "deadline_seconds" => {
                    format!("The host stops this lane after {} seconds.", limit.value)
                }
                "continuations" => format!("At most {} continuations follow a stop.", limit.value),
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
        assert!(preamble.contains("after 123 fresh tokens"), "{preamble}");
        assert!(preamble.contains("after 60 seconds"), "{preamble}");
        assert!(!preamble.contains("repair"), "{preamble}");
    }
}
