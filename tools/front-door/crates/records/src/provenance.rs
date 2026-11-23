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
/// `.provenant/ports.json#verify.remote` or `--max-turns`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Source {
    /// The layer that set the value.
    pub layer: Layer,
    /// The exact file, key or flag, when the layer has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
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
                Some(best) if best == rank => {
                    if let Some((_, previous)) = winner.replace((rank, candidate)) {
                        shadowed.push(previous);
                    }
                }
                Some(_) => shadowed.push(candidate),
                None => winner = Some((rank, candidate)),
            }
        }
        Merge {
            winner: winner.map(|(_, candidate)| candidate),
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
