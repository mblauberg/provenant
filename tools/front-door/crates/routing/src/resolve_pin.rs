use super::*;

/// Scope a fallback may use when a model becomes unavailable, from
/// `routing.pin`. `tier` keeps the hand-off inside the stopped tier, `family`
/// keeps it with the stopped model's answering family, and `exact` admits no
/// fallback at all. The key is declared once in `config/harness-knobs.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutePin {
    Tier,
    #[default]
    Family,
    Exact,
}

impl RoutePin {
    /// The declared key, for diagnostics that must name the setting.
    pub const KEY: &'static str = "routing.pin";

    /// `exact` is the only pin that refuses: no hand-off may leave the stopped
    /// route, so the stop fails naming `routing.pin` instead of falling back.
    pub const fn refuses(self) -> bool {
        matches!(self, Self::Exact)
    }

    /// Whether a depletion hand-off stays inside this pin's scope. An `exact`
    /// pin admits nothing; a tier pin keeps the named tier; a family pin keeps
    /// the stopped model's answering family (an uncatalogued family is not
    /// treated as equal). Host-native hand-offs have no family or tier to
    /// compare, so they stay outside both scopes.
    pub fn admits(self, catalogue: &Catalogue, stopped: &Stopped, handoff: &Handoff) -> bool {
        match self {
            Self::Exact => false,
            Self::Tier => match &handoff.target {
                Target::HostNative { .. } => false,
                // An absent tier means the same model on another account, so
                // it is still the stopped tier.
                Target::Route { .. } => handoff
                    .tier
                    .as_deref()
                    .is_none_or(|tier| Some(tier) == stopped.tier.as_deref()),
            },
            Self::Family => match &handoff.target {
                Target::Route { model, .. } => catalogue
                    .model(model)
                    .and_then(|entry| entry.family.clone())
                    .is_some_and(|family| Some(family) == stopped.family),
                Target::HostNative { .. } => false,
            },
        }
    }
}

impl std::fmt::Display for RoutePin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Tier => "tier",
            Self::Family => "family",
            Self::Exact => "exact",
        };
        formatter.write_str(text)
    }
}

impl std::str::FromStr for RoutePin {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "tier" => Ok(Self::Tier),
            "family" => Ok(Self::Family),
            "exact" => Ok(Self::Exact),
            other => Err(format!(
                "invalid `routing.pin` value `{other}`; expected tier, family or exact"
            )),
        }
    }
}

/// The `fallback{from,to,cause}` fact recorded when a model-unavailable stop
/// hands work to another route. `from` and `to` are stable route spellings and
/// `cause` is the sealed stop cause, so a reader can tell which route moved
/// and why without reading the ladder's prose.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FallbackFact {
    pub from: String,
    pub to: String,
    pub cause: String,
}

impl FallbackFact {
    /// Record the route that stopped and the route the ladder chose.
    pub fn of(stopped: &Stopped, handoff: &Handoff, cause: &str) -> Self {
        let to = match &handoff.target {
            Target::Route { model, account } => account.as_deref().map_or_else(
                || model.as_str().to_owned(),
                |account| format!("{} ({account})", model.as_str()),
            ),
            Target::HostNative { name } => format!("host-native/{name}"),
        };
        Self {
            from: format!("{}/{}", stopped.model.adapter(), stopped.model.model()),
            to,
            cause: cause.to_owned(),
        }
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    fn catalogue() -> Catalogue {
        Catalogue::bundled().unwrap()
    }

    fn stopped(family: &str) -> Stopped {
        Stopped {
            model: ModelId::parse("claude/claude-sonnet-5-5").unwrap(),
            account: Some("spent".into()),
            effort: None,
            tier: Some("junior".into()),
            family: Some(family.to_owned()),
        }
    }

    fn handoff(model: &str, tier: Option<&str>) -> Handoff {
        Handoff {
            rung: Rung::SameModelOtherAccount,
            target: Target::Route {
                model: ModelId::parse(model).unwrap(),
                account: Some("spare".into()),
            },
            effort: None,
            tier: tier.map(str::to_owned),
        }
    }

    #[test]
    fn routing_pin_parses_and_defaults_to_family() {
        assert_eq!(RoutePin::default(), RoutePin::Family);
        assert_eq!("tier".parse::<RoutePin>().unwrap(), RoutePin::Tier);
        assert_eq!("family".parse::<RoutePin>().unwrap(), RoutePin::Family);
        assert_eq!("exact".parse::<RoutePin>().unwrap(), RoutePin::Exact);
        assert!("other".parse::<RoutePin>().is_err());
        assert_eq!(RoutePin::Exact.to_string(), "exact");
        assert_eq!(RoutePin::KEY, "routing.pin");
    }

    #[test]
    fn pin_scope_limits_model_unavailable_fallback() {
        let catalogue = catalogue();
        let stopped = stopped("anthropic");
        let same_family = handoff("claude/claude-sonnet-5-5", Some("junior"));
        let other_family_same_tier = handoff("codex/gpt-6-luna", Some("junior"));
        let other_family_other_tier = handoff("codex/gpt-6-luna", Some("senior"));

        // The default keeps the answering family: another account of the same
        // model is in scope, a different family is not, at any tier.
        assert!(RoutePin::Family.admits(&catalogue, &stopped, &same_family));
        assert!(!RoutePin::Family.admits(&catalogue, &stopped, &other_family_same_tier));
        assert!(!RoutePin::Family.admits(&catalogue, &stopped, &other_family_other_tier));

        // A tier pin keeps any family in the stopped tier, but not another tier.
        assert!(RoutePin::Tier.admits(&catalogue, &stopped, &same_family));
        assert!(RoutePin::Tier.admits(&catalogue, &stopped, &other_family_same_tier));
        assert!(!RoutePin::Tier.admits(&catalogue, &stopped, &other_family_other_tier));

        // A host-native hand-off has no family or tier to compare.
        let native = Handoff {
            rung: Rung::HostNative,
            target: Target::HostNative { name: "sub".into() },
            effort: None,
            tier: None,
        };
        assert!(!RoutePin::Family.admits(&catalogue, &stopped, &native));
        assert!(!RoutePin::Tier.admits(&catalogue, &stopped, &native));
    }

    #[test]
    fn exact_pin_admits_no_fallback() {
        let catalogue = catalogue();
        let stopped = stopped("anthropic");
        assert!(RoutePin::Exact.refuses());
        assert!(!RoutePin::Exact.admits(
            &catalogue,
            &stopped,
            &handoff("claude/claude-sonnet-5-5", Some("junior"))
        ));
        assert!(!RoutePin::Tier.refuses());
        assert!(!RoutePin::Family.refuses());
    }

    #[test]
    fn fallback_fact_names_from_to_and_cause() {
        let stopped = stopped("family-a");
        let handoff = handoff("claude/claude-sonnet-5-5", Some("junior"));
        let fact = FallbackFact::of(&stopped, &handoff, "model_unavailable");
        assert_eq!(
            fact,
            FallbackFact {
                from: "claude/claude-sonnet-5-5".into(),
                to: "claude/claude-sonnet-5-5 (spare)".into(),
                cause: "model_unavailable".into(),
            }
        );
    }
}
