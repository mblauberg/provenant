//! The configurable settlement policy, sealed with the evidence it decides.
//!
//! Slice 8 declares the shape and the shipped values. Slice 9 resolves the
//! layers and applies the actions; `settle` reads only this sealed value.
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::provenance::{Layer, Precedence, Source, Sourced};

/// Settlement knobs, lowest layer first. Host beats the default, project beats
/// host and a route's model policy beats both.
pub const SETTLEMENT_PRECEDENCE: Precedence =
    Precedence(&[Layer::Default, Layer::Host, Layer::Project, Layer::Route]);

/// A settlement value type. Each key has its own, so a value a key does not
/// allow cannot be represented, and the spellings below are the one list the
/// routing catalog, the knob resolver and the generated reference read.
pub trait PolicyValue:
    Copy + Eq + std::fmt::Debug + Serialize + DeserializeOwned + 'static
{
    /// The accepted spellings, in declaration order.
    const VALUES: &'static [&'static str];
    fn as_str(self) -> &'static str;
}

macro_rules! policy_value {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant,)+
        }
        impl PolicyValue for $name {
            const VALUES: &'static [&'static str] = &[$($text),+];
            fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }
    };
}

policy_value!(
    /// What a missing or malformed declaration does to a passing outcome.
    DeclarationAction { Review => "review", Fail => "fail", Warn => "warn", Off => "off" }
);
policy_value!(
    /// What an empty final result does to a writer's passing outcome. The
    /// reason always names it, so there is no `off`.
    EmptyResultAction { Review => "review", Fail => "fail", Warn => "warn" }
);
policy_value!(
    /// What edits left uncommitted on the worktree do to a success.
    UncommittedAction { Hold => "hold", Review => "review", Warn => "warn", Off => "off" }
);
policy_value!(
    /// What an unaccounted move in the primary checkout does to a success.
    StrayAction { Review => "review", Warn => "warn", Off => "off" }
);
policy_value!(
    /// Whether an unchanged, known head may settle as no-work.
    NoWork { Detect => "detect", Off => "off" }
);

/// The settlement policy keys. A recorded policy effect names its key, not a
/// signal: an empty result and a missing declaration are different knobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementKey {
    DeclarationMissing,
    DeclarationMalformed,
    EmptyResult,
    UncommittedWork,
    StrayWork,
    NoWork,
}

impl SettlementKey {
    /// Every key, in the order a receipt lists them.
    pub const ALL: [Self; 6] = [
        Self::DeclarationMissing,
        Self::DeclarationMalformed,
        Self::EmptyResult,
        Self::UncommittedWork,
        Self::StrayWork,
        Self::NoWork,
    ];

    /// The `[knobs]` name.
    pub const fn knob(self) -> &'static str {
        match self {
            Self::DeclarationMissing => "lanes.settlement.declaration_missing",
            Self::DeclarationMalformed => "lanes.settlement.declaration_malformed",
            Self::EmptyResult => "lanes.settlement.empty_result",
            Self::UncommittedWork => "lanes.settlement.uncommitted_work",
            Self::StrayWork => "lanes.settlement.stray_work",
            Self::NoWork => "lanes.settlement.no_work",
        }
    }

    /// The field name in `SettlementPolicy` and `RouteSettlement`.
    pub fn field(self) -> &'static str {
        self.knob().trim_start_matches("lanes.settlement.")
    }

    /// The values this key allows.
    pub fn values(self) -> &'static [&'static str] {
        match self {
            Self::DeclarationMissing | Self::DeclarationMalformed => DeclarationAction::VALUES,
            Self::EmptyResult => EmptyResultAction::VALUES,
            Self::UncommittedWork => UncommittedAction::VALUES,
            Self::StrayWork => StrayAction::VALUES,
            Self::NoWork => NoWork::VALUES,
        }
    }
}

/// The effective policy for one attempt. Every field carries its layer so a
/// re-drive and an audit reproduce the same decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementPolicy {
    pub declaration_missing: Sourced<DeclarationAction>,
    pub declaration_malformed: Sourced<DeclarationAction>,
    pub empty_result: Sourced<EmptyResultAction>,
    pub uncommitted_work: Sourced<UncommittedAction>,
    pub stray_work: Sourced<StrayAction>,
    pub no_work: Sourced<NoWork>,
}

impl Default for SettlementPolicy {
    fn default() -> Self {
        Self::shipped()
    }
}

impl SettlementPolicy {
    /// Today's behaviour, declared at the default layer. Keeping these values
    /// in one place makes the shipped policy the parity oracle for resolution.
    pub fn shipped() -> Self {
        fn shipped<T>(value: T) -> Sourced<T> {
            Sourced {
                value,
                source: Source {
                    layer: Layer::Default,
                    origin: None,
                },
            }
        }
        Self {
            declaration_missing: shipped(DeclarationAction::Review),
            declaration_malformed: shipped(DeclarationAction::Review),
            empty_result: shipped(EmptyResultAction::Review),
            uncommitted_work: shipped(UncommittedAction::Hold),
            stray_work: shipped(StrayAction::Review),
            no_work: shipped(NoWork::Detect),
        }
    }
}

/// The route layer of settlement policy. Routing declares these under
/// `model_policy.defaults.lanes.settlement.<key>` and a rule's
/// `policy.lanes.settlement.<key>`, the same `lanes.settlement.<key>` spelling
/// every layer uses. Launch seals the matched values on the applied route and
/// the holder resolves from that record. Every field is optional, so an absent
/// field leaves the lower layers in place.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "snake_case")]
pub struct RouteSettlement {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration_missing: Option<DeclarationAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration_malformed: Option<DeclarationAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_result: Option<EmptyResultAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncommitted_work: Option<UncommittedAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stray_work: Option<StrayAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_work: Option<NoWork>,
}

impl RouteSettlement {
    /// Each key this layer sets, with its configured value, in key order.
    pub fn entries(&self) -> Vec<(SettlementKey, serde_json::Value)> {
        let value = serde_json::to_value(self).unwrap_or_default();
        SettlementKey::ALL
            .into_iter()
            .filter_map(|key| {
                value
                    .get(key.field())
                    .filter(|value| !value.is_null())
                    .map(|value| (key, value.clone()))
            })
            .collect()
    }

    /// Overlay every key `patch` sets, keeping the keys it leaves unset.
    pub fn overlay(&mut self, patch: &Self) {
        macro_rules! keys {
            ($($field:ident),+) => {
                $(if patch.$field.is_some() { self.$field = patch.$field; })+
            };
        }
        keys!(
            declaration_missing,
            declaration_malformed,
            empty_result,
            uncommitted_work,
            stray_work,
            no_work
        );
    }
}

/// One route-layer settlement value as launch seals it on the applied route:
/// the `lanes.settlement.<key>` name and value, at the route layer, with the
/// model-policy entry that set it as the origin.
pub type RouteLayerValue = Sourced<(String, serde_json::Value)>;

/// A non-default value that changed an outcome or a class, with the key and
/// the source that chose it. Built only inside the classifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedPolicy {
    pub key: SettlementKey,
    /// The configured spelling, one of `key.values()`.
    pub action: String,
    pub source: Source,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trips<T: PolicyValue>() {
        for text in T::VALUES {
            let value: T = serde_json::from_value(serde_json::Value::from(*text))
                .unwrap_or_else(|error| panic!("{text}: {error}"));
            assert_eq!(value.as_str(), *text);
            assert_eq!(serde_json::to_value(value).unwrap(), *text);
        }
    }

    #[test]
    fn every_declared_spelling_is_the_serde_spelling() {
        round_trips::<DeclarationAction>();
        round_trips::<EmptyResultAction>();
        round_trips::<UncommittedAction>();
        round_trips::<StrayAction>();
        round_trips::<NoWork>();
    }

    #[test]
    fn a_route_overlay_keeps_the_keys_the_patch_leaves_unset() {
        let mut base = RouteSettlement {
            stray_work: Some(StrayAction::Warn),
            ..Default::default()
        };
        base.overlay(&RouteSettlement {
            no_work: Some(NoWork::Off),
            ..Default::default()
        });
        assert_eq!(
            base.entries(),
            vec![
                (SettlementKey::StrayWork, serde_json::Value::from("warn")),
                (SettlementKey::NoWork, serde_json::Value::from("off")),
            ]
        );
    }

    #[test]
    fn every_key_names_a_policy_field_and_allows_its_shipped_value() {
        let shipped = serde_json::to_value(SettlementPolicy::shipped()).unwrap();
        for key in SettlementKey::ALL {
            let value = shipped[key.field()]["value"].as_str().unwrap();
            assert!(key.values().contains(&value), "{key:?}={value}");
        }
    }
}
