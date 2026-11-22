//! Lane history handling (`lanes.history.*`): rebasing, unrooted tips,
//! conflict markers and default-branch rewrites.
//!
//! Every value is read from the normal layered configuration and carries the
//! layer that set it. The decisions warn and proceed by default except for the
//! default-branch owner gate. A refusal names the setting that would change
//! it, and the setting's source appears beside its value.
use crate::{KnobSource, KnobValue};
use provenant_records::{LimitSource, run::HarnessProfile};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

/// Whether a lane may rebase or merge its own branch.
pub const REBASE: &str = "lanes.history.rebase";
/// What publication does with a tip that descends from no branch the host knows.
pub const UNROOTED_TIP: &str = "lanes.history.unrooted_tip";
/// What a push does with added lines that look like conflict markers.
pub const CONFLICT_MARKERS: &str = "lanes.history.conflict_markers";
/// Whether the owner permits a non-fast-forward rewrite of the default branch.
pub const DEFAULT_BRANCH_REWRITE: &str = "lanes.history.default_branch_rewrite";

/// Whether a lane may rebase or merge its own branch. `allow` publishes a
/// host merge when the lane's history diverged; `refuse` retains the lane's
/// private commits instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rebase {
    #[default]
    Allow,
    Refuse,
}

/// A warning-level outcome for history that is legitimate but unusual.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryEffect {
    /// Publish or push and name the observation.
    #[default]
    Warn,
    /// Proceed without a warning.
    Off,
    /// Stop at a configured refusal that names the setting.
    Refuse,
}

/// Whether the owner permits rewriting the default branch. The default is a
/// refusal: a shared branch needs the owner's own configuration layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DefaultBranchRewrite {
    #[default]
    Refuse,
    Allow,
}

/// A history choice with its stable setting text.
pub trait HistoryValue: Copy + Default {
    fn as_str(self) -> &'static str;
}

impl HistoryValue for Rebase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Refuse => "refuse",
        }
    }
}

impl HistoryValue for HistoryEffect {
    fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Off => "off",
            Self::Refuse => "refuse",
        }
    }
}

impl HistoryValue for DefaultBranchRewrite {
    fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::Allow => "allow",
        }
    }
}

fn rebase(text: &str) -> Option<Rebase> {
    match text {
        "allow" => Some(Rebase::Allow),
        "refuse" => Some(Rebase::Refuse),
        _ => None,
    }
}

fn effect(text: &str) -> Option<HistoryEffect> {
    match text {
        "warn" => Some(HistoryEffect::Warn),
        "off" => Some(HistoryEffect::Off),
        "refuse" => Some(HistoryEffect::Refuse),
        _ => None,
    }
}

fn rewrite(text: &str) -> Option<DefaultBranchRewrite> {
    match text {
        "refuse" => Some(DefaultBranchRewrite::Refuse),
        "allow" => Some(DefaultBranchRewrite::Allow),
        _ => None,
    }
}

/// One effective value and the layer that supplied it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting<T> {
    pub value: T,
    pub source: LimitSource,
}

impl<T: HistoryValue> Setting<T> {
    /// The setting text shared by every warning and refusal: value and source.
    pub fn setting(&self, name: &str) -> String {
        format!(
            "{name} = {} (source: {})",
            self.value.as_str(),
            self.source.as_str()
        )
    }
}

impl<T: HistoryValue> Default for Setting<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            source: LimitSource::Default,
        }
    }
}

/// The effective lane history settings and each value's source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaneHistory {
    pub rebase: Setting<Rebase>,
    pub unrooted_tip: Setting<HistoryEffect>,
    pub conflict_markers: Setting<HistoryEffect>,
    pub default_branch_rewrite: Setting<DefaultBranchRewrite>,
}

fn profile_text<'a>(profile: &'a HarnessProfile, name: &str) -> Option<&'a str> {
    profile.knobs.get(name).and_then(Value::as_str)
}

fn knob_text<'a>(knobs: &'a BTreeMap<String, KnobValue>, name: &str) -> Option<&'a str> {
    knobs.get(name).and_then(|knob| knob.value.as_str())
}

fn knob_limit_source(source: KnobSource) -> LimitSource {
    match source {
        KnobSource::Default => LimitSource::Default,
        KnobSource::Profile | KnobSource::Override => LimitSource::Host,
    }
}

impl LaneHistory {
    /// Resolve from a sealed launch profile. A missing or unknown value is the
    /// registry default and its source is still reported.
    pub fn resolve(profile: &HarnessProfile) -> Self {
        Self {
            rebase: Setting {
                value: profile_text(profile, REBASE)
                    .and_then(rebase)
                    .unwrap_or_default(),
                source: crate::knob_source(profile, REBASE),
            },
            unrooted_tip: Setting {
                value: profile_text(profile, UNROOTED_TIP)
                    .and_then(effect)
                    .unwrap_or_default(),
                source: crate::knob_source(profile, UNROOTED_TIP),
            },
            conflict_markers: Setting {
                value: profile_text(profile, CONFLICT_MARKERS)
                    .and_then(effect)
                    .unwrap_or_default(),
                source: crate::knob_source(profile, CONFLICT_MARKERS),
            },
            default_branch_rewrite: Setting {
                value: profile_text(profile, DEFAULT_BRANCH_REWRITE)
                    .and_then(rewrite)
                    .unwrap_or_default(),
                source: crate::knob_source(profile, DEFAULT_BRANCH_REWRITE),
            },
        }
    }

    /// Resolve from effective host configuration, as the pre-push hook reads
    /// it. Presets and `[knobs]` both count as the host layer.
    pub fn from_knobs(knobs: &BTreeMap<String, KnobValue>) -> Self {
        let source = |name: &str| {
            knobs
                .get(name)
                .map_or(LimitSource::Default, |knob| knob_limit_source(knob.source))
        };
        Self {
            rebase: Setting {
                value: knob_text(knobs, REBASE)
                    .and_then(rebase)
                    .unwrap_or_default(),
                source: source(REBASE),
            },
            unrooted_tip: Setting {
                value: knob_text(knobs, UNROOTED_TIP)
                    .and_then(effect)
                    .unwrap_or_default(),
                source: source(UNROOTED_TIP),
            },
            conflict_markers: Setting {
                value: knob_text(knobs, CONFLICT_MARKERS)
                    .and_then(effect)
                    .unwrap_or_default(),
                source: source(CONFLICT_MARKERS),
            },
            default_branch_rewrite: Setting {
                value: knob_text(knobs, DEFAULT_BRANCH_REWRITE)
                    .and_then(rewrite)
                    .unwrap_or_default(),
                source: source(DEFAULT_BRANCH_REWRITE),
            },
        }
    }
}

impl fmt::Display for Rebase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for HistoryEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for DefaultBranchRewrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_allow_rebasing_and_warn_about_unusual_history() {
        let history = LaneHistory::default();
        assert_eq!(history.rebase.value, Rebase::Allow);
        assert_eq!(history.unrooted_tip.value, HistoryEffect::Warn);
        assert_eq!(history.conflict_markers.value, HistoryEffect::Warn);
        assert_eq!(
            history.default_branch_rewrite.value,
            DefaultBranchRewrite::Refuse
        );
        assert_eq!(history.unrooted_tip.source, LimitSource::Default);
    }

    #[test]
    fn every_value_round_trips_with_its_source() {
        for (text, expected) in [("allow", Rebase::Allow), ("refuse", Rebase::Refuse)] {
            let mut profile = HarnessProfile::default();
            profile.knobs.insert(REBASE.into(), Value::from(text));
            profile
                .knob_sources
                .insert(REBASE.into(), LimitSource::Launch);
            let history = LaneHistory::resolve(&profile);
            assert_eq!(history.rebase.value, expected);
            assert_eq!(history.rebase.source, LimitSource::Launch);
            assert!(history.rebase.setting(REBASE).contains("source: launch"));
        }
        for (text, expected) in [
            ("warn", HistoryEffect::Warn),
            ("off", HistoryEffect::Off),
            ("refuse", HistoryEffect::Refuse),
        ] {
            let mut profile = HarnessProfile::default();
            profile.knobs.insert(UNROOTED_TIP.into(), Value::from(text));
            assert_eq!(LaneHistory::resolve(&profile).unrooted_tip.value, expected);
        }
        for (text, expected) in [
            ("refuse", DefaultBranchRewrite::Refuse),
            ("allow", DefaultBranchRewrite::Allow),
        ] {
            let mut profile = HarnessProfile::default();
            profile
                .knobs
                .insert(DEFAULT_BRANCH_REWRITE.into(), Value::from(text));
            assert_eq!(
                LaneHistory::resolve(&profile).default_branch_rewrite.value,
                expected
            );
        }
    }

    #[test]
    fn host_knobs_report_the_host_layer() {
        let knobs = BTreeMap::from([(
            CONFLICT_MARKERS.to_owned(),
            KnobValue {
                value: Value::from("refuse"),
                source: KnobSource::Override,
            },
        )]);
        let history = LaneHistory::from_knobs(&knobs);
        assert_eq!(history.conflict_markers.value, HistoryEffect::Refuse);
        assert_eq!(history.conflict_markers.source, LimitSource::Host);
    }

    #[test]
    fn an_unknown_value_takes_the_default_not_a_permissive_reading() {
        let mut profile = HarnessProfile::default();
        profile
            .knobs
            .insert(DEFAULT_BRANCH_REWRITE.into(), Value::from("perhaps"));
        assert_eq!(
            LaneHistory::resolve(&profile).default_branch_rewrite.value,
            DefaultBranchRewrite::Refuse
        );
    }
}
