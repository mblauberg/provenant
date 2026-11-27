//! Where a setting's value came from. One vocabulary for every family, so a
//! warning, receipt or explanation never invents a second spelling.
use serde::{Deserialize, Serialize};

/// Where a setting's value came from. Wire spelling is snake_case and equals
/// the current `LimitSource` spelling where they overlap, so limit receipts
/// keep their bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Built-in or bundled value (today: Default, Builtin, Bundled, ShippedDefault).
    Default,
    /// A named preset bundle (today: `KnobSource::Profile`, schema `Layer::Profile`).
    Profile,
    /// Derived from the repository or host (stack, remote, default branch).
    Detected,
    /// The owner's host config file.
    Host,
    /// Host state bound to one project.
    HostProject,
    /// The repository's `.provenant` files.
    Project,
    /// An account-scoped policy.
    Account,
    /// The routing tier selected for the work.
    Tier,
    /// The template a node expands from.
    Template,
    /// The resolved route's policy (today: `RoutePolicy`, sandbox `Route`).
    Route,
    /// Inherited from the launching lane.
    Parent,
    /// A flag, environment variable or launch request.
    Launch,
}

impl Default for Layer {
    /// The compiled default is the natural floor for every family that uses a
    /// shared layer, so records with a `#[derive(Default)]` keep their shape.
    fn default() -> Self {
        Self::Default
    }
}

impl Layer {
    /// Every setting layer in canonical precedence order.
    pub const ALL: [Self; 12] = [
        Self::Default,
        Self::Profile,
        Self::Detected,
        Self::Host,
        Self::HostProject,
        Self::Project,
        Self::Account,
        Self::Tier,
        Self::Template,
        Self::Route,
        Self::Parent,
        Self::Launch,
    ];
}

/// Canonical precedence order shared by all setting families.
pub const CANONICAL_ORDER: [Layer; 12] = Layer::ALL;

/// Whether `layers` appears in canonical order without skipping backwards or
/// repeating a layer. Use in const assertions for each setting family.
pub const fn is_canonical_subsequence(layers: &[Layer]) -> bool {
    let mut layer_index = 0;
    let mut canonical_index = 0;
    while layer_index < layers.len() {
        let mut found = false;
        while canonical_index < CANONICAL_ORDER.len() {
            if CANONICAL_ORDER[canonical_index] as u8 == layers[layer_index] as u8 {
                found = true;
                canonical_index += 1;
                break;
            }
            canonical_index += 1;
        }
        if !found {
            return false;
        }
        layer_index += 1;
    }
    true
}

/// Fail during const evaluation when a family declares a non-canonical order.
pub const fn assert_canonical_subsequence(layers: &[Layer]) {
    if !is_canonical_subsequence(layers) {
        #[allow(clippy::panic)] // const evaluation: this fails the build, never a run.
        panic!("setting precedence must be a canonical layer subsequence");
    }
}

const _: () = {
    assert_canonical_subsequence(crate::ports::PORT_PRECEDENCE.0);
    assert_canonical_subsequence(crate::limit::LIMIT_PRECEDENCE.0);
    assert_canonical_subsequence(crate::diagnostic::PRESSURE_PRECEDENCE.0);
    assert_canonical_subsequence(crate::settlement::policy::SETTLEMENT_PRECEDENCE.0);
    assert_canonical_subsequence(crate::run::LANE_ACCESS_PRECEDENCE.0);
    assert_canonical_subsequence(crate::run::SANDBOX_PRECEDENCE.0);
};

impl Layer {
    /// The person-readable name of this layer. The two layers a shipped
    /// default and a host setting collapse to keep their established prose in
    /// diagnostics; every other layer uses its wire spelling.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Default => "shipped default",
            Self::Host => "host configuration",
            other => other.as_str(),
        }
    }

    /// The wire spelling of this layer.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Profile => "profile",
            Self::Detected => "detected",
            Self::Host => "host",
            Self::HostProject => "host_project",
            Self::Project => "project",
            Self::Account => "account",
            Self::Tier => "tier",
            Self::Template => "template",
            Self::Route => "route",
            Self::Parent => "parent",
            Self::Launch => "launch",
        }
    }
}

/// A layer plus the exact place, for example
/// `~/.config/provenant/config.toml#lanes.writer_dirty`,
/// `.provenant/ports.json#verify.remote` or `--max-turns`. The default is the
/// compiled default with no named place.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Source {
    /// The layer that set the value.
    pub layer: Layer,
    /// The exact file, key or flag, when the layer has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl Source {
    /// A source at `layer` naming its exact place.
    pub fn at(layer: Layer, origin: impl Into<String>) -> Self {
        Self {
            layer,
            origin: Some(origin.into()),
        }
    }

    /// `LAYER (ORIGIN)`, the prose form diagnostics print.
    pub fn describe(&self) -> String {
        match &self.origin {
            Some(origin) => format!("{} ({origin})", self.layer.description()),
            None => self.layer.description().to_owned(),
        }
    }
}

/// A value with its source. Serialises as
/// `{"value": .., "layer": "<layer>", "origin": ".."}`; the source is
/// flattened beside the value, and an absent origin is omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced<T> {
    /// The value the layer states.
    pub value: T,
    /// Where the value came from.
    #[serde(flatten)]
    pub source: Source,
}

/// One family's precedence, lowest first. Families declare it as a const; the
/// merge and every explanation read it, so order lives in one place per family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Precedence(pub &'static [Layer]);

/// The outcome of merging one family's candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge<T> {
    /// The candidate from the highest listed layer present.
    pub winner: Option<Sourced<T>>,
    /// Listed candidates that lost to the winner, in the order given.
    pub shadowed: Vec<Sourced<T>>,
    /// Candidates whose layer this family does not list.
    pub ignored: Vec<Sourced<T>>,
}

/// The outcome of merging one family's candidates over a built-in default:
/// a value always wins, so callers never assert that one resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved<T> {
    /// The highest declared candidate, or the default when none was given.
    pub winner: Sourced<T>,
    /// Listed candidates that lost to the winner, the default included.
    pub shadowed: Vec<Sourced<T>>,
    /// Candidates whose layer this family does not list.
    pub ignored: Vec<Sourced<T>>,
}

/// Warn for every candidate whose layer a family does not declare, naming its
/// source. Pair this with [`Precedence::merge`] so an ignored value is reported
/// rather than silently dropped.
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
pub fn warn_ignored<T>(ignored: &[Sourced<T>], setting: &str) {
    use crate::warning::{self, SETTING_LAYER_IGNORED, Warning};
    for candidate in ignored {
        warning::warn(
            Warning::new(
                SETTING_LAYER_IGNORED,
                format!(
                    "{setting} value from {} ignored: this family does not declare that layer",
                    candidate.source.layer.description()
                ),
                crate::warning::detected_source(SETTING_LAYER_IGNORED),
            )
            .source(candidate.source.clone())
            .repair(format!("set {setting} from a layer this family declares")),
        );
    }
}

impl Precedence {
    /// The candidate from the highest layer present; ties keep the last given.
    /// A candidate whose layer the family does not list is ignored and
    /// returned in [`Merge::ignored`], so the caller can warn with its source.
    pub fn merge<T>(&self, candidates: impl IntoIterator<Item = Sourced<T>>) -> Merge<T> {
        let mut winner: Option<(usize, Sourced<T>)> = None;
        let mut shadowed = Vec::new();
        let mut ignored = Vec::new();
        for candidate in candidates {
            let Some(rank) = self
                .0
                .iter()
                .position(|layer| *layer == candidate.source.layer)
            else {
                ignored.push(candidate);
                continue;
            };
            match winner.as_ref().map(|(best, _)| *best) {
                Some(best) if best > rank => shadowed.push(candidate),
                // A tie keeps the last candidate; a higher layer displaces the
                // current winner. In both cases the displaced candidate is the
                // one that was the winner, and it keeps its place in shadowed.
                _ => {
                    if let Some((_, previous)) = winner.replace((rank, candidate)) {
                        shadowed.push(previous);
                    }
                }
            }
        }
        Merge {
            winner: winner.map(|(_, candidate)| candidate),
            shadowed,
            ignored,
        }
    }

    /// [`Self::merge`] with the built-in default as the floor: the default
    /// sits below every declared layer and wins only when nothing else does.
    pub fn merge_over_default<T>(
        &self,
        default: T,
        candidates: impl IntoIterator<Item = Sourced<T>>,
    ) -> Resolved<T> {
        let default = Sourced {
            value: default,
            source: Source {
                layer: Layer::Default,
                origin: None,
            },
        };
        let Merge {
            winner,
            mut shadowed,
            ignored,
        } = self.merge(candidates);
        let winner = match winner {
            Some(winner) => {
                shadowed.insert(0, default);
                winner
            }
            None => default,
        };
        Resolved {
            winner,
            shadowed,
            ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Layer, Precedence, Source, Sourced};

    fn sourced<'a>(value: &'a str, layer: Layer, origin: &str) -> Sourced<&'a str> {
        Sourced {
            value,
            source: Source {
                layer,
                origin: Some(origin.to_owned()),
            },
        }
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn merge_picks_the_highest_listed_layer_and_reports_the_rest() {
        const FAMILY: Precedence = Precedence(&[
            Layer::Default,
            Layer::Host,
            Layer::HostProject,
            Layer::Project,
            Layer::Launch,
        ]);
        let merge = FAMILY.merge([
            sourced("default", Layer::Default, "builtin"),
            sourced("project", Layer::Project, ".provenant/config.toml"),
            sourced("host", Layer::Host, "~/.config/provenant/config.toml"),
            sourced("account", Layer::Account, "policy.json"),
        ]);
        assert_eq!(
            merge.winner.as_ref().map(|candidate| candidate.value),
            Some("project")
        );
        assert_eq!(
            merge
                .shadowed
                .iter()
                .map(|candidate| candidate.value)
                .collect::<Vec<_>>(),
            ["default", "host"]
        );
        assert_eq!(merge.ignored.len(), 1);
        assert_eq!(merge.ignored[0].source.layer, Layer::Account);
    }

    /// A family with a built-in default always resolves: the default wins only
    /// when no declared layer gives a value, and is shadowed otherwise.
    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn merge_over_default_always_resolves_with_the_default_as_floor() {
        const FAMILY: Precedence = Precedence(&[Layer::Default, Layer::Host, Layer::Project]);
        let alone = FAMILY.merge_over_default("default", []);
        assert_eq!(alone.winner.value, "default");
        assert_eq!(alone.winner.source.layer, Layer::Default);
        assert!(alone.shadowed.is_empty());

        let over =
            FAMILY.merge_over_default("default", [sourced("host", Layer::Host, "config.toml")]);
        assert_eq!(over.winner.value, "host");
        assert_eq!(
            over.shadowed
                .iter()
                .map(|candidate| candidate.value)
                .collect::<Vec<_>>(),
            ["default"]
        );
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn ignored_candidates_are_warned_with_their_source() {
        const FAMILY: Precedence = Precedence(&[Layer::Default, Layer::Host]);
        let merge = FAMILY.merge([sourced("ignored", Layer::Profile, "profile.toml")]);
        let ((), warnings) =
            crate::warning::capture(|| crate::warn_ignored(&merge.ignored, "example"));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_ref(), "SETTING_LAYER_IGNORED");
        assert_eq!(warnings[0].source.layer, Layer::Profile);
        assert!(!warnings[0].next.is_empty());
    }

    #[test]
    fn a_tie_keeps_the_last_candidate_given() {
        const FAMILY: Precedence = Precedence(&[Layer::Host, Layer::Project]);
        let merge = FAMILY.merge([
            sourced("first", Layer::Project, "a"),
            sourced("last", Layer::Project, "b"),
        ]);
        assert_eq!(
            merge.winner.as_ref().map(|candidate| candidate.value),
            Some("last")
        );
        assert_eq!(merge.shadowed.len(), 1);
        assert_eq!(merge.shadowed[0].value, "first");
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn a_sourced_value_round_trips_the_documented_shape() {
        let value = sourced(
            "900",
            Layer::HostProject,
            ".provenant/ports.json#verify.remote",
        );
        let wire = serde_json::to_value(&value).expect("serialise");
        assert_eq!(
            wire,
            serde_json::json!({
                "value": "900",
                "layer": "host_project",
                "origin": ".provenant/ports.json#verify.remote",
            })
        );
        let decoded: Sourced<String> = serde_json::from_value(wire).expect("deserialise");
        assert_eq!(decoded.value, "900");
        assert_eq!(decoded.source.layer, Layer::HostProject);
    }

    #[test]
    fn an_absent_origin_is_omitted_from_the_wire() {
        let wire = serde_json::to_value(Sourced {
            value: 1u8,
            source: Source {
                layer: Layer::Launch,
                origin: None,
            },
        })
        .expect("serialise");
        assert_eq!(wire, serde_json::json!({"value": 1, "layer": "launch"}));
    }
}
