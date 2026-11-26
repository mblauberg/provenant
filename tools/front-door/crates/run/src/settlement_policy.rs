//! Resolve the settlement policy once, at evidence assembly.
//!
//! Host and project `[knobs]` travel in the sealed profile with the layer that
//! set them; the resolved route's model policy supplies the route layer. The
//! result is sealed with the evidence, so `settle` never reads configuration
//! and a re-drive reproduces the same decision. Every layer is reported with
//! its source, and an invalid value warns and keeps the default.

use std::collections::BTreeMap;

use provenant_config::knob_source;
use provenant_records::provenance::{Layer, Source, Sourced, warn_ignored};
use provenant_records::run::HarnessProfile;
use provenant_records::settlement::policy::{
    PolicyValue, RouteSettlement, SETTLEMENT_PRECEDENCE, SettlementKey, SettlementPolicy,
};
use provenant_records::warning::{SETTLEMENT_POLICY_INVALID, Warning, warn};
use serde_json::Value;

/// The configured `[knobs]` entries, each with the layer and origin that set
/// it. Several entries may name one key: the declared precedence decides
/// between them here, not in the reader that collected them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettlementConfig {
    pub knobs: Vec<Sourced<(String, Value)>>,
}

impl SettlementConfig {
    /// Project the sealed dispatch profile onto the layered view. The profile
    /// keeps each knob's winning layer in `knob_sources`, so host and project
    /// values reach this reader with the provenance they were sealed with.
    pub fn from_profile(profile: &HarnessProfile) -> Self {
        Self {
            knobs: profile
                .knobs
                .iter()
                .map(|(name, value)| Sourced {
                    value: (name.clone(), value.clone()),
                    source: Source {
                        layer: knob_source(profile, name),
                        origin: Some(name.clone()),
                    },
                })
                .collect(),
        }
    }

    /// Build a view from explicit layers. Used by the resolver's tests and by
    /// readers that hold config text rather than a sealed profile.
    pub fn from_layers(layers: &[(Layer, BTreeMap<String, Value>)]) -> Self {
        let mut knobs = Vec::new();
        for (layer, values) in layers {
            for (name, value) in values {
                knobs.push(Sourced {
                    value: (name.clone(), value.clone()),
                    source: Source {
                        layer: *layer,
                        origin: Some(name.clone()),
                    },
                });
            }
        }
        Self { knobs }
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

/// Parse and merge one key's candidates. A value outside the key's declared
/// set warns with its layer and origin and is dropped, so a valid lower layer
/// or the default applies; a candidate from an undeclared layer is warned and
/// ignored.
fn resolve_one<T: PolicyValue>(
    key: SettlementKey,
    default: T,
    candidates: Vec<Sourced<Value>>,
) -> Sourced<T> {
    let name = key.knob();
    let mut parsed = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match serde_json::from_value::<T>(candidate.value.clone()).ok() {
            Some(value) => parsed.push(Sourced {
                value,
                source: candidate.source,
            }),
            None => {
                let layer = candidate.source.layer.description();
                warn(
                    Warning::new(
                        SETTLEMENT_POLICY_INVALID,
                        format!(
                            "{name} from {layer} has an unrecognised value; the default applies"
                        ),
                    )
                    .subject(name)
                    .source(candidate.source)
                    .repair(format!("set {name} to one of {}", T::VALUES.join(", "))),
                );
            }
        }
    }
    let resolved = SETTLEMENT_PRECEDENCE.merge_over_default(default, parsed);
    warn_ignored(&resolved.ignored, name);
    resolved.winner
}

fn route_value<T: serde::Serialize>(value: Option<&T>) -> Option<Sourced<Value>> {
    value
        .and_then(|value| serde_json::to_value(value).ok())
        .map(|value| Sourced {
            value,
            source: Source {
                layer: Layer::Route,
                origin: Some("model_policy lane.settlement".into()),
            },
        })
}

fn with_route(
    mut candidates: Vec<Sourced<Value>>,
    route: Option<Sourced<Value>>,
) -> Vec<Sourced<Value>> {
    if let Some(route) = route {
        candidates.push(route);
    }
    candidates
}

/// The `--explain` view of the host and project layers: each key's effective
/// value with its layer and origin, resolved by the same code the holder seals
/// with, and the warnings that resolution gave. The route layer joins at launch,
/// once the route is chosen.
pub fn explain(knobs: &BTreeMap<String, Sourced<Value>>) -> Value {
    let config = SettlementConfig {
        knobs: knobs
            .iter()
            .filter(|(name, _)| {
                SettlementKey::ALL
                    .iter()
                    .any(|key| key.knob() == name.as_str())
            })
            .map(|(name, sourced)| Sourced {
                value: (name.clone(), sourced.value.clone()),
                source: sourced.source.clone(),
            })
            .collect(),
    };
    let (policy, warnings) = resolve(&config, None);
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

/// Resolve one attempt's settlement policy. The only side effect is the typed
/// warnings, returned to the caller so launch, check and explain report the
/// same findings the sealed policy was built from.
pub fn resolve(
    config: &SettlementConfig,
    route_policy: Option<&RouteSettlement>,
) -> (SettlementPolicy, Vec<Warning>) {
    let shipped = SettlementPolicy::shipped();
    let (policy, warnings) = provenant_records::warning::capture(|| SettlementPolicy {
        handoff_missing: resolve_one(
            SettlementKey::HandoffMissing,
            shipped.handoff_missing.value,
            with_route(
                config.candidates(SettlementKey::HandoffMissing),
                route_policy.and_then(|route| route_value(route.handoff_missing.as_ref())),
            ),
        ),
        handoff_malformed: resolve_one(
            SettlementKey::HandoffMalformed,
            shipped.handoff_malformed.value,
            with_route(
                config.candidates(SettlementKey::HandoffMalformed),
                route_policy.and_then(|route| route_value(route.handoff_malformed.as_ref())),
            ),
        ),
        empty_result: resolve_one(
            SettlementKey::EmptyResult,
            shipped.empty_result.value,
            with_route(
                config.candidates(SettlementKey::EmptyResult),
                route_policy.and_then(|route| route_value(route.empty_result.as_ref())),
            ),
        ),
        uncommitted_work: resolve_one(
            SettlementKey::UncommittedWork,
            shipped.uncommitted_work.value,
            with_route(
                config.candidates(SettlementKey::UncommittedWork),
                route_policy.and_then(|route| route_value(route.uncommitted_work.as_ref())),
            ),
        ),
        stray_work: resolve_one(
            SettlementKey::StrayWork,
            shipped.stray_work.value,
            with_route(
                config.candidates(SettlementKey::StrayWork),
                route_policy.and_then(|route| route_value(route.stray_work.as_ref())),
            ),
        ),
        no_work: resolve_one(
            SettlementKey::NoWork,
            shipped.no_work.value,
            with_route(
                config.candidates(SettlementKey::NoWork),
                route_policy.and_then(|route| route_value(route.no_work.as_ref())),
            ),
        ),
    });
    (policy, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::settlement::policy::{
        EmptyResultAction, HandoffAction, NoWork, StrayAction,
    };

    const KNOBS: [&str; 6] = [
        SettlementKey::HandoffMissing.knob(),
        SettlementKey::HandoffMalformed.knob(),
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
                origin: Some(key.into()),
            },
        }]
    }

    #[test]
    fn shipped_is_the_parity_oracle_when_nothing_is_configured() {
        let (policy, warnings) = resolve(&SettlementConfig::default(), None);
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
        let (policy, warnings) = resolve(&config, None);
        assert!(warnings.is_empty());
        assert_eq!(policy.handoff_missing.value, HandoffAction::Warn);
        assert_eq!(policy.handoff_missing.source.layer, Layer::Project);
    }

    #[test]
    fn a_route_layer_beats_a_project_layer() {
        let config = SettlementConfig {
            knobs: value_map(Layer::Project, KNOBS[4], json!("warn")),
        };
        let route = RouteSettlement {
            stray_work: Some(StrayAction::Off),
            ..Default::default()
        };
        let (policy, warnings) = resolve(&config, Some(&route));
        assert!(warnings.is_empty());
        assert_eq!(policy.stray_work.value, StrayAction::Off);
        assert_eq!(policy.stray_work.source.layer, Layer::Route);
    }

    #[test]
    fn an_invalid_value_warns_and_keeps_the_default() {
        let config = SettlementConfig {
            knobs: value_map(Layer::Host, KNOBS[2], json!("other")),
        };
        let (policy, warnings) = resolve(&config, None);
        assert_eq!(policy.empty_result.value, EmptyResultAction::Review);
        assert_eq!(policy.empty_result.source.layer, Layer::Default);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_ref(), SETTLEMENT_POLICY_INVALID.0);
        assert_eq!(
            warnings[0].source.as_ref().map(|source| source.layer),
            Some(Layer::Host)
        );
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
        let (policy, warnings) = resolve(&config, None);
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
        let (policy, warnings) = resolve(&config, None);
        assert_eq!(policy.no_work.value, NoWork::Detect);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_ref(), "SETTING_LAYER_IGNORED");
    }

    #[test]
    fn every_declared_key_has_a_resolved_source() {
        let (policy, warnings) = resolve(&SettlementConfig::default(), None);
        assert!(warnings.is_empty());
        for sourced in [
            &policy.handoff_missing,
            &policy.handoff_malformed,
            &policy.empty_result,
            &policy.uncommitted_work,
            &policy.stray_work,
        ] {
            assert_eq!(sourced.source.layer, Layer::Default);
        }
        assert_eq!(policy.no_work.source.layer, Layer::Default);
    }
}
