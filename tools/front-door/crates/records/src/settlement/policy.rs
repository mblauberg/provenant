//! The configurable settlement policy, sealed with the evidence it decides.
//!
//! Slice 8 declares the shape and the shipped values. Slice 9 resolves the
//! layers and applies the actions; `settle` reads only this sealed value.
use serde::{Deserialize, Serialize};

use crate::provenance::{Layer, Precedence, Source, Sourced};
use crate::run::SettlementSignal;

/// Settlement knobs, lowest layer first. Host beats the default, project beats
/// host and a route's model policy beats both.
pub const SETTLEMENT_PRECEDENCE: Precedence =
    Precedence(&[Layer::Default, Layer::Host, Layer::Project, Layer::Route]);

/// What a recorded signal does to the outcome. The signal itself is always
/// recorded; only the decision and the warning change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalAction {
    Hold,
    Review,
    Fail,
    Warn,
    Off,
}

/// Whether an unchanged, known head may settle as no-work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoWork {
    Detect,
    Off,
}

/// The values each settlement key allows, declared once. Routing's catalog,
/// the knob resolver and the generated reference read these, so a value one of
/// them accepts is a value every one of them accepts.
pub const HANDOFF_MISSING_VALUES: &[&str] = &["review", "fail", "warn", "off"];
pub const HANDOFF_MALFORMED_VALUES: &[&str] = &["review", "fail", "warn", "off"];
pub const EMPTY_RESULT_VALUES: &[&str] = &["review", "fail", "warn"];
pub const UNCOMMITTED_WORK_VALUES: &[&str] = &["hold", "review", "warn", "off"];
pub const STRAY_WORK_VALUES: &[&str] = &["review", "warn", "off"];
pub const NO_WORK_VALUES: &[&str] = &["detect", "off"];

/// The effective policy for one attempt. Every field carries its layer so a
/// re-drive and an audit reproduce the same decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementPolicy {
    pub handoff_missing: Sourced<SignalAction>,
    pub handoff_malformed: Sourced<SignalAction>,
    pub empty_result: Sourced<SignalAction>,
    pub uncommitted_work: Sourced<SignalAction>,
    pub stray_work: Sourced<SignalAction>,
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
        let action = |value| Sourced {
            value,
            source: Source {
                layer: Layer::Default,
                origin: None,
            },
        };
        Self {
            handoff_missing: action(SignalAction::Review),
            handoff_malformed: action(SignalAction::Review),
            empty_result: action(SignalAction::Review),
            uncommitted_work: action(SignalAction::Hold),
            stray_work: action(SignalAction::Review),
            no_work: Sourced {
                value: NoWork::Detect,
                source: Source {
                    layer: Layer::Default,
                    origin: None,
                },
            },
        }
    }
}

/// The route layer of settlement policy. Routing declares these under
/// `model_policy.defaults.lane.settlement` and `rules.policy.lane.settlement`;
/// the holder reads the resolved value from the sealed route. Every field is
/// optional, so an absent field leaves the lower layers in place.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "snake_case")]
pub struct RouteSettlement {
    pub handoff_missing: Option<SignalAction>,
    pub handoff_malformed: Option<SignalAction>,
    pub empty_result: Option<SignalAction>,
    pub uncommitted_work: Option<SignalAction>,
    pub stray_work: Option<SignalAction>,
    pub no_work: Option<NoWork>,
}

/// A non-default action that changed an outcome or a class, with the source
/// that chose it. Built only inside the classifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedPolicy {
    pub signal: SettlementSignal,
    pub action: SignalAction,
    pub source: Source,
}
