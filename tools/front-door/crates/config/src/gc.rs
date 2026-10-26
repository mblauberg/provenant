//! Lane cache collection settings shared by the CLI and scheduled agent.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GcSettings {
    pub schedule: bool,
    pub idle_days: u64,
    pub free_floor: Option<String>,
    pub alert_below: Option<String>,
    pub projects: BTreeMap<String, ProjectGcSettings>,
}

impl Default for GcSettings {
    fn default() -> Self {
        Self {
            schedule: true,
            idle_days: 7,
            free_floor: None,
            alert_below: None,
            projects: BTreeMap::new(),
        }
    }
}

impl GcSettings {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3650).contains(&self.idle_days) {
            return Err("idle_days_range");
        }
        if let Some(size) = &self.free_floor {
            parse_cache_size(size)?;
        }
        if let Some(size) = &self.alert_below {
            parse_cache_size(size)?;
        }
        for project in self.projects.values() {
            project.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectGcSettings {
    pub regenerable: Vec<String>,
    pub keep: Vec<String>,
}

impl ProjectGcSettings {
    fn validate(&self) -> Result<(), &'static str> {
        for name in &self.regenerable {
            if !safe_relative(name)
                || Path::new(name).components().count() != 1
                || name == ".git"
                || name == ".worktrees"
            {
                return Err("regenerable_directory_name");
            }
        }
        if self.keep.iter().any(|path| !safe_relative(path)) {
            return Err("gc_keep_relative_path");
        }
        Ok(())
    }
}

fn safe_relative(text: &str) -> bool {
    !text.is_empty()
        && !text.contains('\0')
        && Path::new(text)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Whole binary sizes; an unsuffixed number is GiB, as is `G` or `GiB`.
pub fn parse_cache_size(text: &str) -> Result<u64, &'static str> {
    let split = text
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "b" => 1,
        "k" | "kib" => 1024,
        "m" | "mib" => 1024_u64.pow(2),
        "" | "g" | "gib" => 1024_u64.pow(3),
        "t" | "tib" => 1024_u64.pow(4),
        _ => return Err("invalid_cache_size"),
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(multiplier))
        .ok_or("invalid_cache_size")
}

#[cfg(test)]
mod tests {
    #[test]
    fn cache_sizes_accept_binary_suffixes_and_plain_gib_and_refuse_overflow() {
        for size in ["20", "20G", "20GiB", "20gib"] {
            assert_eq!(super::parse_cache_size(size), Ok(20 * 1024_u64.pow(3)));
        }
        assert_eq!(super::parse_cache_size("512M"), Ok(512 * 1024_u64.pow(2)));
        for size in ["-1", "1.5G", "20GB", "", "G", "18446744073709551615G"] {
            assert!(super::parse_cache_size(size).is_err());
        }
    }
}
