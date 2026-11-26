//! Resolve the settlement policy once, at evidence assembly.
//!
//! Host and project `[knobs]` travel in the sealed profile with the layer that
//! set them; launch seals the matched model policy's `lanes.settlement.<key>`
//! values on the applied route as the route layer. The result is sealed with
//! the evidence, so `settle` never reads configuration and a re-drive
//! reproduces the same decision. Every layer is reported with the file or
//! model-policy entry that set it, and an invalid value warns and keeps the
//! default.

use std::collections::BTreeMap;
use std::path::Path;

use provenant_config::knob_source;
use provenant_records::provenance::{Layer, Source, Sourced, warn_ignored};
use provenant_records::run::HarnessProfile;
use provenant_records::settlement::policy::{
    RouteLayerValue, SETTLEMENT_PRECEDENCE, SettlementKey, SettlementPolicy,
};
use provenant_records::warning::{SETTLEMENT_POLICY_INVALID, Warning, warn};
use serde_json::Value;

/// The configured settlement entries, each with the layer and origin that set
/// it. Several entries may name one key: the declared precedence decides
/// between them here, not in the reader that collected them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettlementConfig {
    pub knobs: Vec<Sourced<(String, Value)>>,
}

/// The config file and key a knob layer is read from, as `<file>#<key>`. The
/// project file is relative to the project root.
fn knob_file(layer: Layer, state: &Path, name: &str) -> Option<String> {
    match layer {
        Layer::Host => Some(format!("{}#{name}", state.join("config.toml").display())),
        Layer::Project => Some(format!(".provenant/config.toml#{name}")),
        _ => None,
    }
}

fn is_settlement_knob(name: &str) -> bool {
    SettlementKey::ALL.iter().any(|key| key.knob() == name)
}

impl SettlementConfig {
    /// Project the sealed dispatch profile onto the layered view. The profile
    /// keeps each knob's winning layer in `knob_sources`, so host and project
    /// values reach this reader with the layer they were sealed with; the
    /// origin is that layer's config file under `state`.
    pub fn from_profile(profile: &HarnessProfile, state: &Path) -> Self {
        Self {
            knobs: profile
                .knobs
                .iter()
                .filter(|(name, _)| is_settlement_knob(name))
                .map(|(name, value)| {
                    let layer = knob_source(profile, name);
                    Sourced {
                        value: (name.clone(), value.clone()),
                        source: Source {
                            layer,
                            origin: knob_file(layer, state, name),
                        },
                    }
                })
                .collect(),
        }
    }

    /// The same view from a resolved config's effective knobs, for readers
    /// that hold configuration rather than a sealed profile.
    pub fn from_knobs(knobs: &BTreeMap<String, Sourced<Value>>, state: &Path) -> Self {
        Self {
            knobs: knobs
                .iter()
                .filter(|(name, _)| is_settlement_knob(name))
                .map(|(name, sourced)| Sourced {
                    value: (name.clone(), sourced.value.clone()),
                    source: Source {
                        layer: sourced.source.layer,
                        origin: sourced
                            .source
                            .origin
                            .clone()
                            .or_else(|| knob_file(sourced.source.layer, state, name)),
                    },
                })
                .collect(),
        }
    }

    /// Add the route layer launch sealed on the applied route. Each entry
    /// already names the model-policy rule or default that set it.
    pub fn with_route(mut self, route: &[RouteLayerValue]) -> Self {
        self.knobs.extend(route.iter().cloned());
        self
    }

    fn candidates(&self, key: SettlementKey) -> Vec<Sourced<Value>> {
        self.knobs
            .iter()
            .filter(|candidate| candidate.value.0 == key.knob())
            .map(|candidate| Sourced {
                value: candidate.value.1.clone(),
                source: candidate.source.clone(),
            })
            .collect()
    }
}

/// Merge one key's candidates over its shipped value. A value outside the
/// key's declared set warns with its layer and origin and is dropped, so a
/// valid lower layer or the default applies; a candidate from an undeclared
/// layer is warned and ignored.
fn resolve_key(
    key: SettlementKey,
    default: Value,
    candidates: Vec<Sourced<Value>>,
) -> Sourced<Value> {
    let name = key.knob();
    let mut valid = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate
            .value
            .as_str()
            .is_some_and(|text| key.values().contains(&text))
        {
            valid.push(candidate);
            continue;
        }
        let layer = candidate.source.layer.description();
        warn(
            Warning::new(
                SETTLEMENT_POLICY_INVALID,
                format!("{name} from {layer} has an unrecognised value; the default applies"),
                candidate.source,
            )
            .subject(name)
            .repair(format!("set {name} to one of {}", key.values().join(", "))),
        );
    }
    let resolved = SETTLEMENT_PRECEDENCE.merge_over_default(default, valid);
    warn_ignored(&resolved.ignored, name);
    resolved.winner
}

/// The `--explain` view: each key's effective value with its layer and
/// origin, resolved by the same code the holder seals with, and the warnings
/// that resolution gave. Pass the config with its route layer to show the
/// value a launch on that route seals.
pub fn explain(config: &SettlementConfig) -> Value {
    let (policy, warnings) = resolve(config);
    let resolved = serde_json::to_value(&policy).unwrap_or_default();
    let knobs: Vec<Value> = SettlementKey::ALL
        .iter()
        .map(|key| {
            let mut entry = resolved.get(key.field()).cloned().unwrap_or_default();
            if let Value::Object(map) = &mut entry {
                map.insert("key".into(), Value::from(key.knob()));
                map.insert("allowed".into(), Value::from(key.values().to_vec()));
            }
            entry
        })
        .collect();
    serde_json::json!({
        "precedence": SETTLEMENT_PRECEDENCE.0.iter().map(|layer| layer.as_str()).collect::<Vec<_>>(),
        "knobs": knobs,
        "warnings": warnings,
    })
}

/// Resolve one attempt's settlement policy, one key at a time over the same
/// table. The only side effect is the typed warnings, returned to the caller
/// so launch, check and explain report the findings the sealed policy was
/// built from.
pub fn resolve(config: &SettlementConfig) -> (SettlementPolicy, Vec<Warning>) {
    let shipped = SettlementPolicy::shipped();
    let defaults = serde_json::to_value(&shipped).unwrap_or_default();
    let (fields, warnings) = provenant_records::warning::capture(|| {
        SettlementKey::ALL
            .into_iter()
            .map(|key| {
                let default = defaults[key.field()]["value"].clone();
                let winner = resolve_key(key, default, config.candidates(key));
                (
                    key.field().to_owned(),
                    serde_json::to_value(winner).unwrap_or_default(),
                )
            })
            .collect::<serde_json::Map<String, Value>>()
    });
    // Every winner is one of its key's declared spellings or the shipped
    // value, so the typed policy always decodes; the shipped policy is the
    // floor if a declaration ever drifted from its type.
    let policy = serde_json::from_value(Value::Object(fields)).unwrap_or(shipped);
    (policy, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::settlement::policy::{
        DeclarationAction, EmptyResultAction, NoWork, StrayAction,
    };

    const KNOBS: [&str; 6] = [
        SettlementKey::DeclarationMissing.knob(),
        SettlementKey::DeclarationMalformed.knob(),
        SettlementKey::EmptyResult.knob(),
        SettlementKey::UncommittedWork.knob(),
        SettlementKey::StrayWork.knob(),
        SettlementKey::NoWork.knob(),
    ];
    use serde_json::json;

    fn value_map(layer: Layer, key: &str, value: Value) -> Vec<Sourced<(String, Value)>> {
        vec![Sourced {
            value: (key.into(), value),
            source: Source {
                layer,
                origin: Some("config.toml".into()),
            },
        }]
    }

    #[test]
    fn shipped_is_the_parity_oracle_when_nothing_is_configured() {
        let (policy, warnings) = resolve(&SettlementConfig::default());
        assert!(warnings.is_empty());
        assert_eq!(policy, SettlementPolicy::shipped());
    }

    #[test]
    fn a_project_layer_beats_a_host_layer() {
        let config = SettlementConfig {
            knobs: value_map(Layer::Host, KNOBS[0], json!("fail"))
                .into_iter()
                .chain(value_map(Layer::Project, KNOBS[0], json!("warn")))
                .collect(),
        };
        let (policy, warnings) = resolve(&config);
        assert!(warnings.is_empty());
        assert_eq!(policy.declaration_missing.value, DeclarationAction::Warn);
        assert_eq!(policy.declaration_missing.source.layer, Layer::Project);
    }

    #[test]
    fn a_route_layer_beats_a_project_layer() {
        let config = SettlementConfig {
            knobs: value_map(Layer::Project, KNOBS[4], json!("warn")),
        };
        let route = vec![Sourced {
            value: (KNOBS[4].into(), json!("off")),
            source: Source {
                layer: Layer::Route,
                origin: Some("model_policy.rules[1].policy.lanes.settlement.stray_work".into()),
            },
        }];
        let (policy, warnings) = resolve(&config.with_route(&route));
        assert!(warnings.is_empty());
        assert_eq!(policy.stray_work.value, StrayAction::Off);
        assert_eq!(policy.stray_work.source, route[0].source);
    }

    #[test]
    fn a_sealed_profile_names_the_config_file_that_set_each_knob() {
        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOBS[3].into(), json!("warn"));
        profile.knob_sources.insert(KNOBS[3].into(), Layer::Host);
        profile.knobs.insert("lanes.access".into(), json!("host"));
        let config = SettlementConfig::from_profile(&profile, Path::new("/state"));
        assert_eq!(config.knobs.len(), 1, "only settlement knobs are read");
        let (policy, _) = resolve(&config);
        assert_eq!(
            policy.uncommitted_work.source.origin.as_deref(),
            Some(
                format!(
                    "{}#{}",
                    Path::new("/state").join("config.toml").display(),
                    KNOBS[3]
                )
                .as_str()
            )
        );
    }

    #[test]
    fn explain_reports_the_route_value_a_launch_seals() {
        let route = vec![Sourced {
            value: (KNOBS[5].into(), json!("off")),
            source: Source {
                layer: Layer::Route,
                origin: Some("model_policy.defaults.lanes.settlement.no_work".into()),
            },
        }];
        let config = SettlementConfig::default().with_route(&route);
        let explained = explain(&config);
        let (sealed, _) = resolve(&config);
        let row = explained["knobs"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["key"] == KNOBS[5]))
            .cloned()
            .unwrap_or_default();
        assert_eq!(row["value"], "off");
        assert_eq!(row["layer"], "route");
        assert_eq!(sealed.no_work.value, NoWork::Off);
    }

    #[test]
    fn an_invalid_value_warns_and_keeps_the_default() {
        let config = SettlementConfig {
            knobs: value_map(Layer::Host, KNOBS[2], json!("other")),
        };
        let (policy, warnings) = resolve(&config);
        assert_eq!(policy.empty_result.value, EmptyResultAction::Review);
        assert_eq!(policy.empty_result.source.layer, Layer::Default);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_ref(), SETTLEMENT_POLICY_INVALID.0);
        assert_eq!(warnings[0].source.layer, Layer::Host);
        assert!(warnings[0].message.contains(KNOBS[2]));
    }

    #[test]
    fn a_value_outside_its_keys_declared_set_warns_and_keeps_the_default() {
        // `hold` is an uncommitted-work value but not an empty-result one;
        // `fail` is not a stray-work value. Each key admits only its own.
        let config = SettlementConfig {
            knobs: value_map(Layer::Host, KNOBS[2], json!("hold"))
                .into_iter()
                .chain(value_map(Layer::Project, KNOBS[4], json!("fail")))
                .collect(),
        };
        let (policy, warnings) = resolve(&config);
        assert_eq!(
            policy.empty_result,
            SettlementPolicy::shipped().empty_result
        );
        assert_eq!(policy.stray_work, SettlementPolicy::shipped().stray_work);
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .all(|warning| warning.code.as_ref() == SETTLEMENT_POLICY_INVALID.0)
        );
    }

    #[test]
    fn an_undeclared_layer_is_warned_and_ignored() {
        let config = SettlementConfig {
            knobs: value_map(Layer::Launch, KNOBS[5], json!("off")),
        };
        let (policy, warnings) = resolve(&config);
        assert_eq!(policy.no_work.value, NoWork::Detect);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_ref(), "SETTING_LAYER_IGNORED");
    }

    #[test]
    fn every_declared_key_has_a_resolved_source() {
        let (policy, warnings) = resolve(&SettlementConfig::default());
        assert!(warnings.is_empty());
        // Each key now resolves to its own value type, so assert on the
        // homogeneous `Layer` projection rather than the heterogeneous `Sourced<T>`.
        for layer in [
            policy.declaration_missing.source.layer,
            policy.declaration_malformed.source.layer,
            policy.empty_result.source.layer,
            policy.uncommitted_work.source.layer,
            policy.stray_work.source.layer,
            policy.no_work.source.layer,
        ] {
            assert_eq!(layer, Layer::Default);
        }
    }
}
