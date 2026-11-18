//! Host-wide Cargo cache budget, shared by launch admission and collection.
use std::path::Path;

pub const KNOB: &str = "lanes.cargo_cache_max";
pub const DEFAULT: &str = "10%";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CargoCacheBudget {
    pub bytes: u64,
    pub setting: String,
    pub source: provenant_records::LimitSource,
}

/// Resolve the effective lane budget using the volume that contains the cache.
pub fn resolve(
    profile: &provenant_records::run::HarnessProfile,
    cache_path: &Path,
) -> std::io::Result<CargoCacheBudget> {
    let value = profile.knobs.get(KNOB);
    let setting = match value {
        None => DEFAULT.to_owned(),
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| std::io::Error::other("invalid_cache_size"))?
            .to_string(),
        Some(_) => return Err(std::io::Error::other("invalid_cache_size")),
    };
    let total = volume_bytes(cache_path)?;
    let bytes = parse(&setting, total).map_err(std::io::Error::other)?;
    Ok(CargoCacheBudget {
        bytes,
        setting,
        source: crate::knob_source(profile, KNOB),
    })
}

/// Whether a size or whole-volume percentage is syntactically valid.
pub fn is_valid_setting(value: &str) -> bool {
    parse(value, 1).is_ok()
}

/// Resolve a byte size or integer percentage of the volume.
pub fn parse(value: &str, volume_bytes: u64) -> Result<u64, &'static str> {
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent
            .parse::<u64>()
            .map_err(|_| "invalid_cache_percent")?;
        if percent > 100 {
            return Err("invalid_cache_percent");
        }
        return Ok(((u128::from(volume_bytes) * u128::from(percent)) / 100) as u64);
    }
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse().map_err(|_| "invalid_cache_size");
    }
    crate::parse_cache_size(value)
}

fn volume_bytes(path: &Path) -> std::io::Result<u64> {
    let mut current = path;
    loop {
        match provenant_os::fs::Directory::open_anchor(current) {
            Ok(volume) => return volume.total_bytes().map_err(std::io::Error::other),
            Err(error) if error.is_not_found() => {
                current = current
                    .parent()
                    .ok_or_else(|| std::io::Error::other(error))?;
            }
            Err(error) => return Err(std::io::Error::other(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_accepts_bytes_and_whole_volume_percentages() {
        assert_eq!(parse("8G", 100), Ok(8 * 1024_u64.pow(3)));
        assert_eq!(parse("100", 800), Ok(100));
        assert_eq!(parse("25%", 800), Ok(200));
        assert_eq!(parse("0%", 800), Ok(0));
        for value in ["101%", "1.5%", "%", "8GB", ""] {
            assert!(!is_valid_setting(value), "{value}");
        }
    }

    #[test]
    fn effective_budget_exposes_default_and_host_sources() {
        let directory = tempfile::tempdir().unwrap();
        let volume = provenant_os::fs::Directory::open_anchor(directory.path())
            .unwrap()
            .total_bytes()
            .unwrap();
        let default = resolve(
            &provenant_records::run::HarnessProfile::default(),
            directory.path(),
        )
        .unwrap();
        assert_eq!(default.bytes, volume / 10);
        assert_eq!(default.setting, DEFAULT);
        assert_eq!(default.source, provenant_records::LimitSource::Default);

        let host = provenant_records::run::HarnessProfile {
            knobs: std::collections::BTreeMap::from([(
                KNOB.into(),
                serde_json::Value::String("1073741824".into()),
            )]),
            knob_sources: std::collections::BTreeMap::from([(
                KNOB.into(),
                provenant_records::LimitSource::Host,
            )]),
            ..Default::default()
        };
        let budget = resolve(&host, directory.path()).unwrap();
        assert_eq!(budget.bytes, 1_073_741_824);
        assert_eq!(budget.setting, "1073741824");
        assert_eq!(budget.source, provenant_records::LimitSource::Host);

        let numeric = provenant_records::run::HarnessProfile {
            knobs: std::collections::BTreeMap::from([(
                KNOB.into(),
                serde_json::Value::from(1_073_741_824_u64),
            )]),
            knob_sources: std::collections::BTreeMap::from([(
                KNOB.into(),
                provenant_records::LimitSource::Host,
            )]),
            ..Default::default()
        };
        let budget = resolve(&numeric, directory.path()).unwrap();
        assert_eq!(budget.bytes, 1_073_741_824);
        assert_eq!(budget.setting, "1073741824");
    }
}
