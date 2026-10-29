//! Lane cache collection settings shared by the CLI and scheduled agent.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

/// Host-wide build limits, read independently by admission guardians.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaneBuildSettings {
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub max_concurrent_builds: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub cargo_build_jobs: Option<u64>,
    /// `CARGO_INCREMENTAL` for lanes; off unless set.
    pub cargo_incremental: Option<bool>,
    /// `CARGO_PROFILE_DEV_DEBUG` for lanes; `line-tables-only` unless set.
    pub cargo_debug: Option<String>,
    /// Cap on the allocated bytes of all lane Cargo profile directories, idle
    /// and busy. Collection removes idle ones, least recently used first; when
    /// busy directories alone exceed the cap it removes every eligible idle
    /// directory and stays over.
    pub build_dir_max_bytes: Option<u64>,
    /// Seconds after its newest write during which a profile directory is
    /// never removed, even with its Cargo locks free: test and run binaries
    /// execute after Cargo drops its locks, and Cargo may be opening its lock
    /// files. 15 minutes unless set.
    pub build_dir_grace_seconds: Option<u64>,
}
/// Cargo's accepted `debug` levels, including the legacy numeric and boolean forms.
const CARGO_DEBUG_LEVELS: [&str; 10] = [
    "none",
    "line-directives-only",
    "line-tables-only",
    "limited",
    "full",
    "0",
    "1",
    "2",
    "true",
    "false",
];
/// Lane build output cap when `build_dir_max_bytes` is unset.
pub const DEFAULT_BUILD_DIR_MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;
/// Recency grace for lane build output when `build_dir_grace_seconds` is unset.
pub const DEFAULT_BUILD_DIR_GRACE_SECONDS: u64 = 15 * 60;
/// The asset's -1 sentinel selects the host-dependent default.
pub fn deserialize_auto<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    use serde::de::Error;
    let value = Option::<i64>::deserialize(deserializer)?;
    match value {
        None | Some(-1) => Ok(None),
        Some(value) => u64::try_from(value).map(Some).map_err(D::Error::custom),
    }
}
impl LaneBuildSettings {
    pub fn read(state: &Path) -> std::io::Result<Self> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Root {
            domains: Domains,
        }
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Domains {
            fabric: Fabric,
        }
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Fabric {
            lanes: LaneBuildSettings,
        }
        let read = provenant_os::fs::Directory::open_anchor(state).and_then(|directory| {
            directory.read_bounded(
                "config.toml".as_ref(),
                provenant_records::Limits::CONFIG.max_bytes,
            )
        });
        let settings = match read {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                let document = text
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                toml_edit::de::from_document::<Root>(document)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?
                    .domains
                    .fabric
                    .lanes
            }
            Err(error) if error.is_not_found() => Self::default(),
            Err(error) => return Err(std::io::Error::other(error)),
        };
        if !settings.invalid_keys().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid lane build settings",
            ));
        }
        Ok(settings)
    }
    /// Invalid host build setting names, shared with the Fabric reader.
    pub fn invalid_keys(&self) -> Vec<&'static str> {
        let mut invalid = Vec::new();
        if self
            .max_concurrent_builds
            .is_some_and(|n| !(1..=64).contains(&n))
        {
            invalid.push("max_concurrent_builds");
        }
        if self
            .cargo_build_jobs
            .is_some_and(|n| !(1..=1024).contains(&n))
        {
            invalid.push("cargo_build_jobs");
        }
        if self
            .cargo_debug
            .as_deref()
            .is_some_and(|level| !CARGO_DEBUG_LEVELS.contains(&level))
        {
            invalid.push("cargo_debug");
        }
        invalid
    }
    /// Incremental compilation is off by default: its sessions grow without
    /// bound in a lane's target and are rarely reused.
    pub fn incremental(&self) -> bool {
        self.cargo_incremental.unwrap_or(false)
    }
    /// Line tables keep backtraces useful at a fraction of full debug info.
    pub fn debug_level(&self) -> &str {
        self.cargo_debug.as_deref().unwrap_or("line-tables-only")
    }
    pub fn build_dir_cap(&self) -> u64 {
        self.build_dir_max_bytes
            .unwrap_or(DEFAULT_BUILD_DIR_MAX_BYTES)
    }
    pub fn build_dir_grace_seconds(&self) -> u64 {
        self.build_dir_grace_seconds
            .unwrap_or(DEFAULT_BUILD_DIR_GRACE_SECONDS)
    }
    pub fn build_limit(&self, total_bytes: u64) -> u64 {
        self.max_concurrent_builds
            .unwrap_or(if total_bytes <= 16 * 1024_u64.pow(3) {
                1
            } else {
                2
            })
    }
    pub fn jobs(&self) -> u64 {
        self.cargo_build_jobs.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1) as u64)
        })
    }
}

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
    fn observed_default_floor_matches_dispatch_floor() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let settings = super::GcSettings::read(temp.path())?;
        let total = provenant_os::fs::Directory::open_anchor(temp.path())?.total_bytes()?;
        assert_eq!(
            settings.free_floor_for_total(total)?,
            settings.free_floor_bytes(temp.path())?
        );
        Ok(())
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

#[cfg(test)]
mod build_tests {
    use super::LaneBuildSettings;
    #[test]
    fn automatic_and_invalid_build_values() {
        let automatic: LaneBuildSettings =
            toml_edit::de::from_str("max_concurrent_builds = -1\ncargo_build_jobs = -1").unwrap();
        assert_eq!(automatic, LaneBuildSettings::default());
        let invalid: LaneBuildSettings =
            toml_edit::de::from_str("max_concurrent_builds = 0\ncargo_build_jobs = 1025").unwrap();
        assert_eq!(
            invalid.invalid_keys(),
            ["max_concurrent_builds", "cargo_build_jobs"]
        );
        assert!(toml_edit::de::from_str::<LaneBuildSettings>("cargo_build_jobs = -2").is_err());
    }
    #[test]
    fn defaults_and_overrides() {
        let settings = LaneBuildSettings::default();
        let boundary = 16 * 1024_u64.pow(3);
        assert_eq!(settings.build_limit(boundary), 1);
        assert_eq!(settings.build_limit(boundary + 1), 2);
        assert!(settings.jobs() >= 1);
        let settings = LaneBuildSettings {
            max_concurrent_builds: Some(3),
            cargo_build_jobs: Some(7),
            ..LaneBuildSettings::default()
        };
        assert_eq!(settings.build_limit(boundary), 3);
        assert_eq!(settings.jobs(), 7);
    }
    #[test]
    fn cargo_profile_defaults_and_overrides() {
        let settings = LaneBuildSettings::default();
        assert!(!settings.incremental());
        assert_eq!(settings.debug_level(), "line-tables-only");
        assert_eq!(settings.build_dir_cap(), 20 * 1024 * 1024 * 1024);
        assert_eq!(settings.build_dir_grace_seconds(), 900);
        let settings: LaneBuildSettings = toml_edit::de::from_str(
            "cargo_incremental = true\ncargo_debug = \"full\"\nbuild_dir_max_bytes = 5\nbuild_dir_grace_seconds = 0",
        )
        .unwrap();
        assert!(settings.incremental());
        assert_eq!(settings.debug_level(), "full");
        assert_eq!(settings.build_dir_cap(), 5);
        assert_eq!(settings.build_dir_grace_seconds(), 0);
        assert!(settings.invalid_keys().is_empty());
        let invalid: LaneBuildSettings = toml_edit::de::from_str("cargo_debug = \"loud\"").unwrap();
        assert_eq!(invalid.invalid_keys(), ["cargo_debug"]);
    }
}
