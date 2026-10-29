//! Config can lower execution ceilings, never relax the recorded safety limits.
use crate::Calibration;
use provenant_config::{DomainConfig, DomainSection, SectionError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub max_parallel: u64,
    pub wall_budget_seconds: u64,
    #[serde(default)]
    pub display: Display,
    #[serde(default)]
    pub calibration: BTreeMap<String, Calibration>,
    /// Disk cap in bytes for each account's quota history (current file plus one rotated).
    #[serde(default = "default_history_max_bytes")]
    pub history_max_bytes: u64,
}
fn default_history_max_bytes() -> u64 {
    crate::DEFAULT_HISTORY_MAX_BYTES
}
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
            display: Display::default(),
            calibration: BTreeMap::new(),
            history_max_bytes: default_history_max_bytes(),
        }
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors: Vec<SectionError> = [
            ("max_parallel", (1..=3).contains(&self.max_parallel)),
            (
                "wall_budget_seconds",
                (1..=55).contains(&self.wall_budget_seconds),
            ),
        ]
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(field, _)| SectionError {
            field: field.into(),
            code: "ceiling",
        })
        .collect();
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
pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "quota",
    |_| "max_parallel = 3\nwall_budget_seconds = 55\n\n[display]\nheadline = \"auto\"\nicon = \"most_constrained\"\n",
);

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> Result<Settings, toml_edit::de::Error> {
        toml_edit::de::from_str(text)
    }
    const BASE: &str = "max_parallel = 3\nwall_budget_seconds = 55\n";
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
