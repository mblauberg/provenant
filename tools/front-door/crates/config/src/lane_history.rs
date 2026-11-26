//! Lane history handling (`lanes.history.*`): rebasing, unrooted tips,
//! conflict markers and default-branch rewrites.
//!
//! Every value is read from the normal layered configuration and carries the
//! layer that set it. The decisions warn and proceed by default except for the
//! default-branch owner gate. A refusal names the setting that would change
//! it, and the setting's source appears beside its value. No setting here is
//! strict: an unknown value warns with the layer that supplied it and keeps the
//! documented default, so a typo never silently chooses a policy.
use provenant_records::warning::{CONFIG_MALFORMED, Warning, warn};
use provenant_records::{Layer, Sourced, run::HarnessProfile};
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

/// The warning for an explicit value outside a setting's values. It names the
/// setting, the value and the layer that supplied it, so a typo is repairable,
/// and the documented default that stands in for it.
fn unknown_value(name: &str, text: &str, source: Layer, default: &str) -> String {
    format!(
        "{name} = {text} (source: {}) is not a value of this setting; using {default} (source: default)",
        source.as_str()
    )
}

/// One effective value: its parsed text, the layer that supplied it, and the
/// documented default when an explicit value is unknown. A setting is never
/// strict: an unknown value warns with its source and keeps the default, so a
/// typo neither refuses the file nor silently chooses the permissive reading
/// under the setting's own name.
fn resolve_setting<T: HistoryValue>(
    name: &str,
    text: Option<&str>,
    source: Layer,
    parse: impl Fn(&str) -> Option<T>,
) -> Setting<T> {
    match text {
        Some(text) => match parse(text) {
            Some(value) => Setting { value, source },
            None => {
                warn(
                    Warning::new(
                        CONFIG_MALFORMED,
                        unknown_value(name, text, source, T::default().as_str()),
                        provenant_records::warning::detected_source(CONFIG_MALFORMED),
                    )
                    .subject(name.to_owned())
                    .repair(format!("set {name} to a declared value"))
                    .once_per_change(),
                );
                Setting {
                    value: T::default(),
                    source: Layer::Default,
                }
            }
        },
        None => Setting {
            value: T::default(),
            source,
        },
    }
}

/// One effective value and the layer that supplied it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting<T> {
    pub value: T,
    pub source: Layer,
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
            source: Layer::Default,
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

fn knob_text<'a>(knobs: &'a BTreeMap<String, Sourced<Value>>, name: &str) -> Option<&'a str> {
    knobs.get(name).and_then(|knob| knob.value.as_str())
}

fn knob_limit_source(source: Layer) -> Layer {
    match source {
        // A preset and a host `[knobs]` entry both count as host configuration.
        Layer::Profile | Layer::Host => Layer::Host,
        other => other,
    }
}

impl LaneHistory {
    /// Resolve from a sealed launch profile. A missing value is the registry
    /// default; an explicit value outside the setting warns with its source and
    /// keeps the default.
    pub fn resolve(profile: &HarnessProfile) -> Self {
        Self {
            rebase: resolve_setting(
                REBASE,
                profile_text(profile, REBASE),
                crate::knob_source(profile, REBASE),
                rebase,
            ),
            unrooted_tip: resolve_setting(
                UNROOTED_TIP,
                profile_text(profile, UNROOTED_TIP),
                crate::knob_source(profile, UNROOTED_TIP),
                effect,
            ),
            conflict_markers: resolve_setting(
                CONFLICT_MARKERS,
                profile_text(profile, CONFLICT_MARKERS),
                crate::knob_source(profile, CONFLICT_MARKERS),
                effect,
            ),
            default_branch_rewrite: resolve_setting(
                DEFAULT_BRANCH_REWRITE,
                profile_text(profile, DEFAULT_BRANCH_REWRITE),
                crate::knob_source(profile, DEFAULT_BRANCH_REWRITE),
                rewrite,
            ),
        }
    }

    /// Resolve from effective host configuration, as the pre-push hook reads
    /// it. Presets and `[knobs]` both count as the host layer.
    pub fn from_knobs(knobs: &BTreeMap<String, Sourced<Value>>) -> Self {
        let source = |name: &str| {
            knobs
                .get(name)
                .map_or(Layer::Default, |knob| knob_limit_source(knob.source.layer))
        };
        Self {
            rebase: resolve_setting(REBASE, knob_text(knobs, REBASE), source(REBASE), rebase),
            unrooted_tip: resolve_setting(
                UNROOTED_TIP,
                knob_text(knobs, UNROOTED_TIP),
                source(UNROOTED_TIP),
                effect,
            ),
            conflict_markers: resolve_setting(
                CONFLICT_MARKERS,
                knob_text(knobs, CONFLICT_MARKERS),
                source(CONFLICT_MARKERS),
                effect,
            ),
            default_branch_rewrite: resolve_setting(
                DEFAULT_BRANCH_REWRITE,
                knob_text(knobs, DEFAULT_BRANCH_REWRITE),
                source(DEFAULT_BRANCH_REWRITE),
                rewrite,
            ),
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
    use provenant_records::Source;

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
        assert_eq!(history.unrooted_tip.source, Layer::Default);
    }

    #[test]
    fn every_value_round_trips_with_its_source() {
        for (text, expected) in [("allow", Rebase::Allow), ("refuse", Rebase::Refuse)] {
            let mut profile = HarnessProfile::default();
            profile.knobs.insert(REBASE.into(), Value::from(text));
            profile.knob_sources.insert(REBASE.into(), Layer::Launch);
            let history = LaneHistory::resolve(&profile);
            assert_eq!(history.rebase.value, expected);
            assert_eq!(history.rebase.source, Layer::Launch);
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
            Sourced {
                value: Value::from("refuse"),
                source: Source {
                    layer: Layer::Host,
                    origin: None,
                },
            },
        )]);
        let history = LaneHistory::from_knobs(&knobs);
        assert_eq!(history.conflict_markers.value, HistoryEffect::Refuse);
        assert_eq!(history.conflict_markers.source, Layer::Host);
    }

    #[test]
    fn an_unknown_value_warns_with_its_source_and_takes_the_documented_default() {
        // A typo must not silently choose a policy. `allow` publishes a host
        // merge, so an unknown value falls back to the documented default and
        // says which layer supplied the text and that the default now stands.
        let mut profile = HarnessProfile::default();
        profile.knobs.insert(REBASE.into(), Value::from("permit"));
        profile.knob_sources.insert(REBASE.into(), Layer::Host);
        let history = LaneHistory::resolve(&profile);
        assert_eq!(history.rebase.value, Rebase::Allow);
        assert_eq!(history.rebase.source, Layer::Default);
        assert_eq!(
            unknown_value(REBASE, "permit", Layer::Host, Rebase::Allow.as_str()),
            "lanes.history.rebase = permit (source: host) is not a value of this setting; using allow (source: default)"
        );

        // The restrictive default is no proof of safe fallback: the same text
        // on the owner gate takes its documented refusal and names the layer.
        let mut profile = HarnessProfile::default();
        profile
            .knobs
            .insert(DEFAULT_BRANCH_REWRITE.into(), Value::from("perhaps"));
        let history = LaneHistory::resolve(&profile);
        assert_eq!(
            history.default_branch_rewrite.value,
            DefaultBranchRewrite::Refuse
        );
        assert_eq!(history.default_branch_rewrite.source, Layer::Default);

        // Host configuration reads resolve the same way: an unknown value
        // reports the default layer, never the layer that typed it.
        let knobs = BTreeMap::from([(
            REBASE.to_owned(),
            Sourced {
                value: Value::from("alloww"),
                source: Source {
                    layer: Layer::Host,
                    origin: None,
                },
            },
        )]);
        let history = LaneHistory::from_knobs(&knobs);
        assert_eq!(history.rebase.value, Rebase::Allow);
        assert_eq!(history.rebase.source, Layer::Default);
    }
}
