//! Host config may lower or raise probe execution ceilings within the recorded
//! bounds; the defaults stay conservative.
use crate::Calibration;
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub max_parallel: u64,
    pub wall_budget_seconds: u64,
    /// Timer and open-panel polling overrides, keyed by adapter.
    #[serde(default)]
    pub polling_intervals: BTreeMap<String, PollingIntervals>,
    #[serde(default)]
    pub display: Display,
    #[serde(default)]
    pub calibration: BTreeMap<String, Calibration>,
    /// Disk cap in bytes for each account's quota history (current file plus one rotated).
    #[serde(default = "default_history_max_bytes")]
    pub history_max_bytes: u64,
}
/// Omitted intervals retain the provider default, in seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PollingIntervals {
    pub timer_seconds: Option<u64>,
    pub panel_seconds: Option<u64>,
}
impl Settings {
    /// Apply once before scheduling or reporting, retaining provider probe controls.
    pub fn apply_polling_intervals(&self, policies: &mut [crate::QuotaPolicy]) {
        for policy in policies {
            if let Some(intervals) = self.polling_intervals.get(policy.adapter) {
                if let Some(seconds) = intervals.timer_seconds {
                    policy.timer_seconds = seconds;
                }
                if let Some(seconds) = intervals.panel_seconds {
                    policy.panel_seconds = seconds;
                }
            }
        }
    }
}
fn default_history_max_bytes() -> u64 {
    crate::DEFAULT_HISTORY_MAX_BYTES
}
/// Most accounts a single quota probe tick may run at once.
pub const MAX_PARALLEL: u64 = 16;
/// Longest wall budget a quota probe tick may reserve.
pub const MAX_WALL_BUDGET_SECONDS: u64 = 600;
/// Which window the menu bar leads with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Headline {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "5h")]
    FiveHour,
    #[serde(rename = "weekly")]
    Weekly,
}
/// Menu bar display choices. Hidden accounts and ordering live on each account entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Display {
    pub headline: Headline,
    /// `most_constrained` or an account entry id.
    pub icon: String,
}
pub const MOST_CONSTRAINED: &str = "most_constrained";
impl Default for Display {
    fn default() -> Self {
        Self {
            headline: Headline::Auto,
            icon: MOST_CONSTRAINED.into(),
        }
    }
}
/// Syntax of an account entry id, which the quota domain cannot resolve.
pub fn valid_icon(icon: &str) -> bool {
    icon == MOST_CONSTRAINED
        || (!icon.is_empty()
            && icon.len() <= 128
            && icon
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_parallel: 3,
            wall_budget_seconds: 55,
            polling_intervals: BTreeMap::new(),
            display: Display::default(),
            calibration: BTreeMap::new(),
            history_max_bytes: default_history_max_bytes(),
        }
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors: Vec<SectionError> = [
            (
                "max_parallel",
                (1..=MAX_PARALLEL).contains(&self.max_parallel),
            ),
            (
                "wall_budget_seconds",
                (1..=MAX_WALL_BUDGET_SECONDS).contains(&self.wall_budget_seconds),
            ),
        ]
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(field, _)| SectionError {
            field: field.into(),
            code: "ceiling",
        })
        .collect();
        for (adapter, intervals) in &self.polling_intervals {
            if provenant_records::AdapterSpec::get(adapter).is_none() {
                errors.push(SectionError {
                    field: format!("polling_intervals.{adapter}"),
                    code: "adapter",
                });
            }
            for (field, seconds) in [
                ("timer_seconds", intervals.timer_seconds),
                ("panel_seconds", intervals.panel_seconds),
            ] {
                // The scheduler multiplies by up to four, then uses signed seconds.
                if seconds.is_some_and(|n| n == 0 || n > i64::MAX as u64 / 4) {
                    errors.push(SectionError {
                        field: format!("polling_intervals.{adapter}.{field}"),
                        code: "interval",
                    });
                }
            }
        }
        if !(crate::MIN_HISTORY_MAX_BYTES..=crate::MAX_HISTORY_MAX_BYTES)
            .contains(&self.history_max_bytes)
        {
            errors.push(SectionError {
                field: "history_max_bytes".into(),
                code: "ceiling",
            });
        }
        if !valid_icon(&self.display.icon) {
            errors.push(SectionError {
                field: "display.icon".into(),
                code: "icon",
            });
        }
        for (account, calibration) in &self.calibration {
            if !crate::component(account) || !calibration.valid() {
                errors.push(SectionError {
                    field: format!("calibration.{account}"),
                    code: "calibration",
                });
            }
        }
        errors
    }
}
/// Probe and history bounds are a boundary: configuration may tune them, but
/// values outside the recorded ranges refuse the file.
pub const FIELD_METADATA: &[FieldMetadata] = &[
    FieldMetadata::int("max_parallel", 1, MAX_PARALLEL as i64),
    FieldMetadata::int("wall_budget_seconds", 1, MAX_WALL_BUDGET_SECONDS as i64),
    FieldMetadata::int(
        "history_max_bytes",
        crate::MIN_HISTORY_MAX_BYTES as i64,
        crate::MAX_HISTORY_MAX_BYTES as i64,
    ),
    FieldMetadata::enumeration("display.headline", &["auto", "5h", "weekly"]),
    FieldMetadata::int("polling_intervals.{adapter}.timer_seconds", 1, i64::MAX / 4),
    FieldMetadata::int("polling_intervals.{adapter}.panel_seconds", 1, i64::MAX / 4),
    FieldMetadata::int("calibration.{account}.five_hour", 0, 100),
    FieldMetadata::int("calibration.{account}.seven_day", 0, 100),
    FieldMetadata::int("calibration.{account}.thirty_day", 0, 100),
    FieldMetadata::new("calibration.{account}.expires", "string"),
];
pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "quota",
    |_| "max_parallel = 3\nwall_budget_seconds = 55\nhistory_max_bytes = 4194304\n\n[display]\nheadline = \"auto\"\nicon = \"most_constrained\"\n",
)
.with_boundary_check(|error| error.code == "ceiling")
.with_fields(FIELD_METADATA);

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> Result<Settings, toml_edit::de::Error> {
        toml_edit::de::from_str(text)
    }
    const BASE: &str = "max_parallel = 3\nwall_budget_seconds = 55\n";
    #[test]
    fn polling_defaults_and_adapter_overrides_reach_scheduling() {
        use crate::schedule::{Power, Trigger, interval, next_due};
        let mut policies = crate::source::TEST_POLICIES.to_vec();
        let original = policies.clone();
        let defaults = parse(BASE).unwrap();
        assert!(defaults.polling_intervals.is_empty());
        defaults.apply_polling_intervals(&mut policies);
        for (before, after) in original.iter().zip(&policies) {
            assert_eq!(before.timer_seconds, after.timer_seconds);
            assert_eq!(before.panel_seconds, after.panel_seconds);
        }
        let configured = parse(&format!("{BASE}[polling_intervals.claude]\ntimer_seconds = 17\npanel_seconds = 11\n[polling_intervals.agy]\ntimer_seconds = 2000\n")).unwrap();
        assert!(configured.validate().is_empty());
        configured.apply_polling_intervals(&mut policies);
        assert_eq!(
            interval(&policies, "claude", Trigger::Timer, Power::Ac),
            Some(17)
        );
        assert_eq!(
            interval(&policies, "claude", Trigger::Panel, Power::Battery),
            Some(22)
        );
        assert_eq!(
            interval(&policies, "claude", Trigger::Timer, Power::Low),
            Some(68)
        );
        assert_eq!(
            interval(&policies, "claude", Trigger::Manual, Power::Ac),
            Some(30)
        );
        assert_eq!(
            interval(&policies, "agy", Trigger::Timer, Power::Ac),
            Some(2000)
        );
        assert_eq!(
            interval(&policies, "agy", Trigger::Panel, Power::Ac),
            Some(900)
        );
        assert_eq!(interval(&policies, "agy", Trigger::Timer, Power::Low), None);
        assert_eq!(
            interval(&policies, "opencode", Trigger::Timer, Power::Ac),
            Some(300)
        );
        let mut record = crate::AccountRecord::empty(crate::AccountScope {
            adapter: "claude".into(),
            account_key: "work".into(),
            label: "work".into(),
        });
        record.last_attempt_unix_seconds = Some(100);
        assert_eq!(
            next_due(&policies, &record, Trigger::Timer, Power::Ac).unwrap(),
            Some(117)
        );
        for (before, after) in original.iter().zip(&policies) {
            assert_eq!(before.probe_seconds, after.probe_seconds);
            assert_eq!(before.low_power, after.low_power);
            assert_eq!(before.solo, after.solo);
        }
    }

    #[test]
    fn polling_intervals_reject_unknown_adapters_zero_and_overflow() {
        for text in [
            "[polling_intervals.unknown]\ntimer_seconds=1\n",
            "[polling_intervals.claude]\ntimer_seconds=0\n",
            "[polling_intervals.codex]\npanel_seconds=9223372036854775807\n",
        ] {
            assert!(
                !parse(&format!("{BASE}{text}"))
                    .unwrap()
                    .validate()
                    .is_empty()
            );
        }
        assert!(parse(&format!("{BASE}[polling_intervals.claude]\nextra=1\n")).is_err());
        let max = i64::MAX as u64 / 4;
        assert!(
            parse(&format!(
                "{BASE}[polling_intervals.codex]\ntimer_seconds={max}\n"
            ))
            .unwrap()
            .validate()
            .is_empty()
        );
    }

    #[test]
    fn probe_ceilings_raise_within_bounds_and_defaults_stay_small() {
        let defaults = Settings::default();
        assert_eq!(defaults.max_parallel, 3);
        assert_eq!(defaults.wall_budget_seconds, 55);
        assert!(defaults.validate().is_empty());
        let raised = parse("max_parallel = 8\nwall_budget_seconds = 300\n").unwrap();
        assert_eq!(raised.max_parallel, 8);
        assert_eq!(raised.wall_budget_seconds, 300);
        assert!(raised.validate().is_empty());
        for (text, field) in [
            (
                "max_parallel = 0\nwall_budget_seconds = 55\n",
                "max_parallel",
            ),
            (
                "max_parallel = 3\nwall_budget_seconds = 0\n",
                "wall_budget_seconds",
            ),
            (
                "max_parallel = 17\nwall_budget_seconds = 55\n",
                "max_parallel",
            ),
            (
                "max_parallel = 3\nwall_budget_seconds = 601\n",
                "wall_budget_seconds",
            ),
        ] {
            let invalid = parse(text).unwrap();
            assert!(
                invalid.validate().iter().any(|error| error.field == field),
                "{text}"
            );
        }
    }
    #[test]
    fn display_defaults_and_valid_values() {
        let s = parse(BASE).unwrap();
        assert_eq!(s.display, Display::default());
        assert!(s.validate().is_empty());
        let d = parse(&format!(
            "{BASE}[display]\nheadline = \"5h\"\nicon = \"claude-michael\"\n"
        ))
        .unwrap();
        assert_eq!(d.display.headline, Headline::FiveHour);
        assert!(d.validate().is_empty());
        let w = parse(&format!("{BASE}[display]\nheadline = \"weekly\"\n")).unwrap();
        assert_eq!(w.display.icon, MOST_CONSTRAINED);
        assert_eq!(
            serde_json::to_string(&Headline::FiveHour).unwrap(),
            "\"5h\""
        );
    }
    #[test]
    fn display_rejects_bad_headline_icon_and_unknown_keys() {
        assert!(parse(&format!("{BASE}[display]\nheadline = \"daily\"\n")).is_err());
        assert!(parse(&format!("{BASE}[display]\nextra = 1\n")).is_err());
        for icon in ["", "a b", "x/y", "caf\u{e9}"] {
            let s = parse(&format!("{BASE}[display]\nicon = \"{icon}\"\n")).unwrap();
            assert_eq!(s.validate()[0].code, "icon", "{icon:?}");
        }
        let long = "a".repeat(129);
        let s = parse(&format!("{BASE}[display]\nicon = \"{long}\"\n")).unwrap();
        assert!(!s.validate().is_empty());
    }
    #[test]
    fn history_cap_defaults_and_is_bounded() {
        let s = parse(BASE).unwrap();
        assert_eq!(s.history_max_bytes, crate::DEFAULT_HISTORY_MAX_BYTES);
        assert!(s.validate().is_empty());
        for bad in [
            0,
            crate::MIN_HISTORY_MAX_BYTES - 1,
            crate::MAX_HISTORY_MAX_BYTES + 1,
        ] {
            let s = parse(&format!("{BASE}history_max_bytes = {bad}\n")).unwrap();
            assert_eq!(s.validate()[0].field, "history_max_bytes", "{bad}");
        }
        let s = parse(&format!("{BASE}history_max_bytes = 131072\n")).unwrap();
        assert!(s.validate().is_empty());
    }
}
