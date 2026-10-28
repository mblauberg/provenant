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
    /// Read the owning section without resolving unrelated domain settings.
    /// Missing configuration selects the compiled defaults; invalid input fails.
    pub fn read(state: &Path) -> std::io::Result<Self> {
        #[derive(Default, Deserialize)]
        struct Settings {
            #[serde(default)]
            gc: GcSettings,
        }
        let read = provenant_os::fs::Directory::open_anchor(state).and_then(|directory| {
            directory.read_bounded(
                "config.toml".as_ref(),
                provenant_records::Limits::CONFIG.max_bytes,
            )
        });
        let settings = match read {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes).map_err(std::io::Error::other)?;
                toml_edit::de::from_str::<Settings>(text)
                    .map_err(std::io::Error::other)?
                    .gc
            }
            Err(error) if error.is_not_found() => Self::default(),
            Err(error) => return Err(std::io::Error::other(error)),
        };
        settings.validate().map_err(std::io::Error::other)?;
        Ok(settings)
    }

    /// The configured floor, or 10% of the state volume with a 50 GiB minimum.
    /// Installation can render before the state directory has been created.
    pub fn free_floor_bytes(&self, state: &Path) -> std::io::Result<u64> {
        if let Some(size) = &self.free_floor {
            return parse_cache_size(size).map_err(std::io::Error::other);
        }
        let mut path = state;
        loop {
            match provenant_os::fs::Directory::open_anchor(path) {
                Ok(volume) => return Ok(default_free_floor(volume.total_bytes()?)),
                Err(error) if error.is_not_found() => {
                    path = path.parent().ok_or_else(|| std::io::Error::other(error))?;
                }
                Err(error) => return Err(std::io::Error::other(error)),
            }
        }
    }

    /// Resolve the floor using an already observed state volume capacity.
    pub fn free_floor_for_total(&self, total: u64) -> std::io::Result<u64> {
        self.free_floor
            .as_deref()
            .map(parse_cache_size)
            .transpose()
            .map(|floor| floor.unwrap_or_else(|| default_free_floor(total)))
            .map_err(std::io::Error::other)
    }

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

fn default_free_floor(total_bytes: u64) -> u64 {
    (total_bytes / 10).max(50 * 1024_u64.pow(3))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectGcSettings {
    pub regenerable: Vec<String>,
    pub keep: Vec<String>,
    pub foreign_worktrees: ForeignWorktrees,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForeignWorktrees {
    #[default]
    Keep,
    MergedClean,
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
    fn foreign_collection_requires_the_explicit_merged_clean_value() {
        let defaults: super::ProjectGcSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.foreign_worktrees, super::ForeignWorktrees::Keep);
        let opted: super::ProjectGcSettings =
            serde_json::from_str(r#"{"foreign_worktrees":"merged-clean"}"#).unwrap();
        assert_eq!(
            opted.foreign_worktrees,
            super::ForeignWorktrees::MergedClean
        );
        assert!(
            serde_json::from_str::<super::ProjectGcSettings>(r#"{"foreign_worktrees":"all"}"#)
                .is_err()
        );
    }

    #[test]
    fn free_floor_defaults_to_ten_percent_with_a_fifty_gib_minimum() {
        let gib = 1024_u64.pow(3);
        assert_eq!(super::default_free_floor(100 * gib), 50 * gib);
        assert_eq!(super::default_free_floor(1000 * gib), 100 * gib);
        let settings = super::GcSettings {
            free_floor: Some("20G".into()),
            ..Default::default()
        };
        assert_eq!(
            settings
                .free_floor_bytes(std::path::Path::new("/missing/state"))
                .unwrap(),
            20 * gib
        );
    }

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
