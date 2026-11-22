//! Host config may lower or raise probe execution ceilings within the recorded
//! bounds; the defaults stay conservative.
use crate::Calibration;
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
/// Every field defaults, because the state-keyed boundary reader decodes the
/// user's own table alone: a host that set one freshness key wrote no other
/// field, and a missing field must not discard the ones it did write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub max_parallel: u64,
    pub wall_budget_seconds: u64,
    /// Independent CLI refresh deadline, in seconds.
    #[serde(default = "default_refresh_budget_seconds")]
    pub refresh_budget_seconds: u64,
    /// Minimum independent refresh budget remaining before the CLI may start.
    #[serde(default = "default_refresh_min_budget_seconds")]
    pub refresh_min_budget_seconds: u64,
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
    /// Advance warning window; zero disables advance warnings.
    #[serde(default = "default_login_warning_seconds")]
    pub login_expiry_warning_seconds: u64,
    /// The single owner of expiry and grace rules for quota evidence.
    #[serde(default)]
    pub freshness: FreshnessSettings,
}
/// Every freshness and grace rule, in seconds. Runtime consumers read these
/// through [`crate::FreshnessPolicy`], which also carries their source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FreshnessSettings {
    /// Maximum age of a quota snapshot when no per-adapter timer applies.
    pub snapshot_seconds: u64,
    /// How long a positive login verification suppresses an ambient sign-in detection.
    pub login_verification_seconds: u64,
    /// Maximum age of a snapshot used for an automatic chair switch.
    pub chair_seconds: u64,
    /// Grace after an availability observation expires, for decisions already in flight.
    pub availability_grace_seconds: u64,
    /// Refusal lifetime when the provider names no reset and the adapter has no override.
    pub refusal_seconds: u64,
    /// How long a live free-pool refusal may go without a cheap recheck.
    pub free_pool_recheck_seconds: u64,
    /// Shortest expiry synthesised for a readiness observation.
    pub readiness_min_seconds: u64,
}
impl Default for FreshnessSettings {
    fn default() -> Self {
        Self {
            snapshot_seconds: DEFAULT_SNAPSHOT_SECONDS,
            login_verification_seconds: DEFAULT_LOGIN_VERIFICATION_SECONDS,
            chair_seconds: DEFAULT_CHAIR_SECONDS,
            availability_grace_seconds: DEFAULT_AVAILABILITY_GRACE_SECONDS,
            refusal_seconds: DEFAULT_REFUSAL_SECONDS,
            free_pool_recheck_seconds: DEFAULT_FREE_POOL_RECHECK_SECONDS,
            readiness_min_seconds: DEFAULT_READINESS_MIN_SECONDS,
        }
    }
}
pub const DEFAULT_SNAPSHOT_SECONDS: u64 = 900;
pub const DEFAULT_LOGIN_VERIFICATION_SECONDS: u64 = 900;
pub const DEFAULT_CHAIR_SECONDS: u64 = 900;
pub const DEFAULT_AVAILABILITY_GRACE_SECONDS: u64 = 60;
pub const DEFAULT_REFUSAL_SECONDS: u64 = 3600;
pub const DEFAULT_FREE_POOL_RECHECK_SECONDS: u64 = 600;
pub const DEFAULT_READINESS_MIN_SECONDS: u64 = 1;
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
pub const DEFAULT_LOGIN_WARNING_SECONDS: u64 = 10 * 24 * 60 * 60;
fn default_login_warning_seconds() -> u64 {
    DEFAULT_LOGIN_WARNING_SECONDS
}
pub const DEFAULT_REFRESH_BUDGET_SECONDS: u64 = 120;
fn default_refresh_budget_seconds() -> u64 {
    DEFAULT_REFRESH_BUDGET_SECONDS
}
pub const DEFAULT_REFRESH_MIN_BUDGET_SECONDS: u64 = 60;
fn default_refresh_min_budget_seconds() -> u64 {
    DEFAULT_REFRESH_MIN_BUDGET_SECONDS
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
pub const MAX_DISPLAY_ICON_BYTES: usize = 128;
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
            && icon.len() <= MAX_DISPLAY_ICON_BYTES
            && icon
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
}
pub const DEFAULT_WALL_BUDGET_SECONDS: u64 = 55;

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_parallel: 3,
            wall_budget_seconds: DEFAULT_WALL_BUDGET_SECONDS,
            refresh_budget_seconds: default_refresh_budget_seconds(),
            refresh_min_budget_seconds: default_refresh_min_budget_seconds(),
            polling_intervals: BTreeMap::new(),
            display: Display::default(),
            calibration: BTreeMap::new(),
            history_max_bytes: default_history_max_bytes(),
            login_expiry_warning_seconds: default_login_warning_seconds(),
            freshness: FreshnessSettings::default(),
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
            (
                "refresh_budget_seconds",
                (1..=MAX_WALL_BUDGET_SECONDS).contains(&self.refresh_budget_seconds),
            ),
            (
                "refresh_min_budget_seconds",
                (1..=MAX_WALL_BUDGET_SECONDS).contains(&self.refresh_min_budget_seconds)
                    && self.refresh_min_budget_seconds <= self.refresh_budget_seconds,
            ),
        ]
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(field, _)| SectionError {
            detail: None,
            field: field.into(),
            code: "ceiling",
        })
        .collect();
        for (adapter, intervals) in &self.polling_intervals {
            if !crate::component(adapter) {
                errors.push(SectionError {
                    detail: None,
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
                        detail: None,
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
                detail: None,
                field: "history_max_bytes".into(),
                code: "ceiling",
            });
        }
        if !valid_icon(&self.display.icon) {
            errors.push(SectionError {
                detail: None,
                field: "display.icon".into(),
                code: "icon",
            });
        }
        for (account, calibration) in &self.calibration {
            if !crate::component(account) || !calibration.valid() {
                errors.push(SectionError {
                    detail: None,
                    field: format!("calibration.{account}"),
                    code: "calibration",
                });
            }
        }
        for (field, seconds) in [
            (
                "freshness.snapshot_seconds",
                self.freshness.snapshot_seconds,
            ),
            (
                "freshness.login_verification_seconds",
                self.freshness.login_verification_seconds,
            ),
            ("freshness.chair_seconds", self.freshness.chair_seconds),
            (
                "freshness.availability_grace_seconds",
                self.freshness.availability_grace_seconds,
            ),
            ("freshness.refusal_seconds", self.freshness.refusal_seconds),
            (
                "freshness.free_pool_recheck_seconds",
                self.freshness.free_pool_recheck_seconds,
            ),
            (
                "freshness.readiness_min_seconds",
                self.freshness.readiness_min_seconds,
            ),
        ] {
            if seconds == 0 || seconds > i64::MAX as u64 {
                errors.push(SectionError {
                    detail: None,
                    field: field.into(),
                    code: "interval",
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
    FieldMetadata::int("refresh_budget_seconds", 1, MAX_WALL_BUDGET_SECONDS as i64),
    FieldMetadata::int(
        "refresh_min_budget_seconds",
        1,
        MAX_WALL_BUDGET_SECONDS as i64,
    ),
    FieldMetadata::int("login_expiry_warning_seconds", 0, i64::MAX),
    FieldMetadata::int(
        "history_max_bytes",
        crate::MIN_HISTORY_MAX_BYTES as i64,
        crate::MAX_HISTORY_MAX_BYTES as i64,
    ),
    FieldMetadata::enumeration("display.headline", &["auto", "5h", "weekly"]),
    FieldMetadata::new("display.icon", "string"),
    FieldMetadata::int("polling_intervals.{adapter}.timer_seconds", 1, i64::MAX / 4),
    FieldMetadata::int("polling_intervals.{adapter}.panel_seconds", 1, i64::MAX / 4),
    FieldMetadata::int("calibration.{account}.five_hour", 0, 100),
    FieldMetadata::int("calibration.{account}.seven_day", 0, 100),
    FieldMetadata::int("calibration.{account}.thirty_day", 0, 100),
    FieldMetadata::new("calibration.{account}.expires", "string"),
    FieldMetadata::int("freshness.snapshot_seconds", 1, i64::MAX),
    FieldMetadata::int("freshness.login_verification_seconds", 1, i64::MAX),
    FieldMetadata::int("freshness.chair_seconds", 1, i64::MAX),
    FieldMetadata::int("freshness.availability_grace_seconds", 1, i64::MAX),
    FieldMetadata::int("freshness.refusal_seconds", 1, i64::MAX),
    FieldMetadata::int("freshness.free_pool_recheck_seconds", 1, i64::MAX),
    FieldMetadata::int("freshness.readiness_min_seconds", 1, i64::MAX),
];
pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "quota",
    |_| "max_parallel = 3\nwall_budget_seconds = 55\nrefresh_budget_seconds = 120\nrefresh_min_budget_seconds = 60\nhistory_max_bytes = 4194304\nlogin_expiry_warning_seconds = 864000\n\n[display]\nheadline = \"auto\"\nicon = \"most_constrained\"\n\n[freshness]\nsnapshot_seconds = 900\nlogin_verification_seconds = 900\nchair_seconds = 900\navailability_grace_seconds = 60\nrefusal_seconds = 3600\nfree_pool_recheck_seconds = 600\nreadiness_min_seconds = 1\n",
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
    // The boundary reader hands serde the user's table, not the aggregate's
    // merged one: one configured freshness key must still apply.
    #[test]
    fn a_partial_section_reads_the_configured_keys_beside_the_defaults() {
        let configured = parse("[freshness]\nreadiness_min_seconds = 500\nrefusal_seconds = 60\n")
            .expect("a partial quota section reads");
        let defaults = Settings::default();
        assert_eq!(configured.max_parallel, defaults.max_parallel);
        assert_eq!(configured.wall_budget_seconds, defaults.wall_budget_seconds);
        assert_eq!(
            configured.freshness.readiness_min_seconds, 500,
            "the configured floor"
        );
        assert_eq!(configured.freshness.refusal_seconds, 60);
        assert_eq!(
            configured.freshness.snapshot_seconds, defaults.freshness.snapshot_seconds,
            "an unset freshness key keeps its compiled default"
        );
        assert!(configured.validate().is_empty());
    }
    #[test]
    fn login_warning_defaults_and_overrides() {
        let defaults = parse(BASE).unwrap();
        assert_eq!(defaults.login_expiry_warning_seconds, 864_000);
        assert_eq!(defaults.refresh_budget_seconds, 120);
        assert_eq!(defaults.refresh_min_budget_seconds, 60);
        for seconds in [0, 900, 86_400] {
            let settings =
                parse(&format!("{BASE}login_expiry_warning_seconds = {seconds}\n")).unwrap();
            assert_eq!(settings.login_expiry_warning_seconds, seconds);
            assert!(settings.validate().is_empty());
        }
    }
    #[test]
    fn refresh_budgets_have_safe_defaults_and_bounded_overrides() {
        let defaults = parse(BASE).unwrap();
        assert_eq!(defaults.refresh_budget_seconds, 120);
        assert_eq!(defaults.refresh_min_budget_seconds, 60);
        for (budget, minimum) in [(60, 1), (120, 60), (600, 600)] {
            let settings = parse(&format!(
                "{BASE}refresh_budget_seconds = {budget}\nrefresh_min_budget_seconds = {minimum}\n"
            ))
            .unwrap();
            assert!(settings.validate().is_empty());
        }
        for (budget, minimum) in [(0, 1), (60, 0), (60, 61), (601, 1)] {
            let settings = parse(&format!(
                "{BASE}refresh_budget_seconds = {budget}\nrefresh_min_budget_seconds = {minimum}\n"
            ))
            .unwrap();
            assert!(!settings.validate().is_empty());
        }
    }
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
        let claude = policies
            .iter()
            .find(|policy| policy.adapter == "claude")
            .unwrap();
        let agy = policies
            .iter()
            .find(|policy| policy.adapter == "agy")
            .unwrap();
        let opencode = policies
            .iter()
            .find(|policy| policy.adapter == "opencode")
            .unwrap();
        assert_eq!(
            interval(&policies, "claude", Trigger::Timer, Power::Ac),
            Some(17)
        );
        assert_eq!(
            interval(&policies, "claude", Trigger::Panel, Power::Battery),
            Some(claude.panel_seconds * 2)
        );
        assert_eq!(
            interval(&policies, "claude", Trigger::Timer, Power::Low),
            Some(claude.timer_seconds * 4)
        );
        assert_eq!(
            interval(&policies, "agy", Trigger::Timer, Power::Ac),
            Some(2000)
        );
        assert_eq!(
            interval(&policies, "agy", Trigger::Panel, Power::Ac),
            Some(agy.panel_seconds)
        );
        assert_eq!(interval(&policies, "agy", Trigger::Timer, Power::Low), None);
        assert_eq!(
            interval(&policies, "opencode", Trigger::Timer, Power::Ac),
            Some(opencode.timer_seconds)
        );
        let mut record = crate::AccountRecord::empty(crate::AccountScope {
            extra: Default::default(),
            adapter: "claude".into(),
            account_key: "work".into(),
            label: "work".into(),
        });
        record.last_attempt_unix_seconds = Some(100);
        assert_eq!(
            next_due(
                &policies,
                &record,
                Trigger::Timer,
                Power::Ac,
                &crate::DEFAULT_FRESHNESS
            )
            .unwrap(),
            Some(100 + interval(&policies, "claude", Trigger::Timer, Power::Ac).unwrap() as i64)
        );
        assert_eq!(
            interval(&policies, "claude", Trigger::Manual, Power::Ac),
            interval(&original, "claude", Trigger::Manual, Power::Ac)
        );
        for (before, after) in original.iter().zip(&policies) {
            assert_eq!(before.probe_seconds, after.probe_seconds);
            assert_eq!(before.low_power, after.low_power);
            assert_eq!(before.solo, after.solo);
        }
    }

    #[test]
    fn polling_intervals_accept_registered_policy_and_reject_invalid_values() {
        let configured = parse(&format!(
            "{BASE}[polling_intervals.fixture_provider]\ntimer_seconds=17\n"
        ))
        .unwrap();
        assert!(configured.validate().is_empty());
        let mut policies = [crate::QuotaPolicy {
            adapter: "fixture_provider",
            timer_seconds: 300,
            panel_seconds: 120,
            probe_seconds: 10,
            low_power: true,
            solo: false,
        }];
        configured.apply_polling_intervals(&mut policies);
        assert_eq!(
            crate::schedule::interval(
                &policies,
                "fixture_provider",
                crate::schedule::Trigger::Timer,
                crate::schedule::Power::Ac,
            ),
            Some(17)
        );
        for text in [
            "[polling_intervals.'../invalid']\ntimer_seconds=1\n",
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
        assert!(defaults.max_parallel <= MAX_PARALLEL);
        assert!(defaults.wall_budget_seconds <= MAX_WALL_BUDGET_SECONDS);
        assert!(defaults.validate().is_empty());
        let raised = parse("max_parallel = 8\nwall_budget_seconds = 300\n").unwrap();
        assert_eq!(raised.max_parallel, 8);
        assert_eq!(raised.wall_budget_seconds, 300);
        assert!(raised.validate().is_empty());
        for (text, field) in [
            (
                "max_parallel = 0\nwall_budget_seconds = 55\n".to_owned(),
                "max_parallel",
            ),
            (
                "max_parallel = 3\nwall_budget_seconds = 0\n".to_owned(),
                "wall_budget_seconds",
            ),
            (
                format!(
                    "max_parallel = {}\nwall_budget_seconds = 55\n",
                    MAX_PARALLEL + 1
                ),
                "max_parallel",
            ),
            (
                format!(
                    "max_parallel = 3\nwall_budget_seconds = {}\n",
                    MAX_WALL_BUDGET_SECONDS + 1
                ),
                "wall_budget_seconds",
            ),
        ] {
            let invalid = parse(&text).unwrap();
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
            "{BASE}[display]\nheadline = \"5h\"\nicon = \"account-one\"\n"
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
        let long = "a".repeat(MAX_DISPLAY_ICON_BYTES + 1);
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
