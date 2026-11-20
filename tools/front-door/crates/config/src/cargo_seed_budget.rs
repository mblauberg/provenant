//! Host Cargo target seed budget, shared by lane preparation and `seed-target`.
//!
//! One knob gives every step of a seed its own bounded wait. The compiled
//! default is deliberately generous: a multi-gigabyte target with tens of
//! thousands of files cannot be cloned inside a two-second wait on a loaded
//! host. A configured value names its source so a fallback can say where the
//! budget came from.
use std::path::Path;

pub const KNOB: &str = "lanes.cargo_seed_budget";
pub const DEFAULT: &str = "5m";
/// A seed is best effort; an hour is the longest a single step may wait.
pub const MAX_SECONDS: u64 = 3600;

/// The `[knobs]` slice read independently of the aggregate config. Knobs are
/// written either as a quoted dotted key (`"lanes.cargo_seed_budget"`) or as a
/// nested table (`[knobs.lanes] cargo_seed_budget = ...`), so both are read.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    lanes: Option<Lanes>,
    #[serde(rename = "lanes.cargo_seed_budget")]
    dotted: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Lanes {
    cargo_seed_budget: Option<String>,
}

impl Settings {
    fn value(&self) -> Option<&str> {
        self.dotted.as_deref().or_else(|| {
            self.lanes
                .as_ref()
                .and_then(|lanes| lanes.cargo_seed_budget.as_deref())
        })
    }
}

/// Resolved budget and the layer that supplied it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedBudgetSetting {
    pub micros: u64,
    pub setting: String,
    pub source: provenant_records::LimitSource,
}

impl SeedBudgetSetting {
    /// The compiled default, for callers that need a budget without a state dir.
    pub fn compiled_default() -> Self {
        Self {
            micros: 300 * 1_000_000,
            setting: DEFAULT.to_owned(),
            source: provenant_records::LimitSource::Default,
        }
    }
}

/// Read the effective seed budget from the host state. An unreadable or
/// malformed file is not a reason to refuse a seed: the compiled default is
/// used and reported as such.
pub fn resolve(state: &Path) -> std::io::Result<SeedBudgetSetting> {
    let settings = crate::loader::read_settings(state, "knobs", |settings: &Settings| {
        settings
            .value()
            .map(|value| parse_seconds(value).map(|_| ()))
            .unwrap_or(Ok(()))
            .map_err(|_| "invalid_value")
    })?;
    let (setting, source) = match settings.value() {
        Some(value) => (value.to_owned(), provenant_records::LimitSource::Host),
        None => (DEFAULT.to_owned(), provenant_records::LimitSource::Default),
    };
    let seconds = parse_seconds(&setting).map_err(std::io::Error::other)?;
    Ok(SeedBudgetSetting {
        micros: seconds.saturating_mul(1_000_000),
        setting,
        source,
    })
}

/// Whether a value is a syntactically valid, nonzero seed budget.
pub fn is_valid_setting(value: &str) -> bool {
    parse_seconds(value).is_ok()
}

/// Whole seconds from a duration such as `30s`, `5m` or `1h`.
pub fn parse_seconds(value: &str) -> Result<u64, &'static str> {
    let seconds = crate::parse_gc_idle_duration(value)?;
    if seconds == 0 || seconds > MAX_SECONDS {
        return Err("invalid_seed_budget");
    }
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_accepts_durations_and_rejects_bounds() {
        assert_eq!(parse_seconds("30s"), Ok(30));
        assert_eq!(parse_seconds("5m"), Ok(300));
        assert_eq!(parse_seconds("1h"), Ok(3600));
        for value in ["0", "0s", "3601s", "1d", "-1s", "5M", "x", ""] {
            assert!(!is_valid_setting(value), "{value}");
        }
    }

    #[test]
    fn effective_budget_names_its_source() {
        let state = tempfile::tempdir().unwrap();
        let default = resolve(state.path()).unwrap();
        assert_eq!(default.setting, DEFAULT);
        assert_eq!(default.micros, 300 * 1_000_000);
        assert_eq!(default.source, provenant_records::LimitSource::Default);

        // The form the host actually writes: a quoted dotted key under [knobs].
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version = 1\n[knobs]\n\"lanes.cargo_seed_budget\" = '45s'\n\"lanes.cargo_target\" = 'private'\n",
        )
        .unwrap();
        let host = resolve(state.path()).unwrap();
        assert_eq!(host.setting, "45s");
        assert_eq!(host.micros, 45 * 1_000_000);
        assert_eq!(host.source, provenant_records::LimitSource::Host);

        // A nested table form is accepted too.
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version = 1\n[knobs.lanes]\ncargo_seed_budget = '2m'\n",
        )
        .unwrap();
        let nested = resolve(state.path()).unwrap();
        assert_eq!(nested.setting, "2m");
        assert_eq!(nested.source, provenant_records::LimitSource::Host);
    }
}
