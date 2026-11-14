//! Lane cache collection settings shared by the CLI and scheduled agent.
// Retirement is an audit record, independent of the ref ledger and retention age.
pub use provenant_records::landing::RetirementRecord;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

/// Retention policy routing. Owners alone interpret custody and delete bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionKind {
    GitWorktrees,
    ObservationLedgers,
    Staging,
    LaneCargoCaches,
    ProviderState,
    ChairLogs,
    TargetSeeds,
    BuildDirectories,
    QuotaHistory,
    Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionTrigger {
    Custody,
    Gc,
    FullGc,
    Append,
    Never,
}

#[derive(Clone, Copy, Debug)]
pub struct RetentionClass {
    pub class: RetentionKind,
    pub owner: &'static str,
    pub custody_proof: &'static str,
    /// Effective config keys, interpreted by the owner; None forbids ageing.
    pub age_rule: Option<&'static str>,
    pub trigger: RetentionTrigger,
}

pub const RETENTION_CLASSES: &[RetentionClass] = &[
    RetentionClass {
        class: RetentionKind::LaneCargoCaches,
        owner: "confinement/cache",
        custody_proof: "verified cache owner; free writer custody; atomic detachment",
        age_rule: Some("gc.idle_days"),
        trigger: RetentionTrigger::Gc,
    },
    RetentionClass {
        class: RetentionKind::GitWorktrees,
        owner: "confinement/worktrees",
        custody_proof: "terminal writer; clean Git; landed unchanged tip",
        age_rule: None,
        trigger: RetentionTrigger::Custody,
    },
    RetentionClass {
        class: RetentionKind::ObservationLedgers,
        owner: "telemetry/ledger",
        custody_proof: "permanent attempt evidence",
        age_rule: None,
        trigger: RetentionTrigger::Never,
    },
    RetentionClass {
        class: RetentionKind::Staging,
        owner: "staging_gc",
        custody_proof: "free ingest/install item lock",
        age_rule: Some("gc.idle_days"),
        trigger: RetentionTrigger::Gc,
    },
    RetentionClass {
        class: RetentionKind::ProviderState,
        owner: "run/retention",
        custody_proof: "terminal; free publication/writer custody; no retained private Git",
        age_rule: Some("retention.provider_state_days, retention.logs_days"),
        trigger: RetentionTrigger::Gc,
    },
    RetentionClass {
        class: RetentionKind::ChairLogs,
        owner: "workflow/log",
        custody_proof: "owner transaction; replay horizon",
        age_rule: Some("retention.chair_log_days"),
        trigger: RetentionTrigger::FullGc,
    },
    RetentionClass {
        class: RetentionKind::TargetSeeds,
        owner: "run/target_seed_gc",
        custody_proof: "free store/clone and staging build locks",
        age_rule: None,
        trigger: RetentionTrigger::Gc,
    },
    RetentionClass {
        class: RetentionKind::BuildDirectories,
        owner: "run/build_dir_gc",
        custody_proof: "GC boundary; free Cargo profile locks",
        age_rule: Some("domains.fabric.lanes.build_dir_grace_seconds"),
        trigger: RetentionTrigger::Gc,
    },
    RetentionClass {
        class: RetentionKind::QuotaHistory,
        owner: "cli/quota_history",
        custody_proof: "whole-line append and rename",
        age_rule: None,
        trigger: RetentionTrigger::Append,
    },
    RetentionClass {
        class: RetentionKind::Usage,
        owner: "telemetry/usage",
        custody_proof: "whole-line append and rename",
        age_rule: None,
        trigger: RetentionTrigger::Append,
    },
];

/// One scheduler for retention adapters. Full GC includes regular GC classes;
/// pressure GC excludes the full-only history pass. Never ages custody fences.
pub fn retention_schedule(
    trigger: RetentionTrigger,
) -> impl Iterator<Item = &'static RetentionClass> {
    RETENTION_CLASSES.iter().filter(move |class| {
        class.trigger == trigger
            || (trigger == RetentionTrigger::FullGc && class.trigger == RetentionTrigger::Gc)
    })
}

/// Kernel pressure level at which new lanes wait. The memory floor is advisory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressureThreshold {
    Warning,
    #[default]
    Critical,
    Never,
}
impl MemoryPressureThreshold {
    pub fn diagnostic_value(self) -> provenant_records::diagnostic::PressureThreshold {
        match self {
            Self::Warning => provenant_records::diagnostic::PressureThreshold::Warning,
            Self::Critical => provenant_records::diagnostic::PressureThreshold::Critical,
            Self::Never => provenant_records::diagnostic::PressureThreshold::Never,
        }
    }

    pub fn defers(self, pressure: provenant_os::sys::resources::MemoryPressure) -> bool {
        use provenant_os::sys::resources::MemoryPressure;
        matches!(
            (self, pressure),
            (
                Self::Warning,
                MemoryPressure::Warning | MemoryPressure::Critical
            ) | (Self::Critical, MemoryPressure::Critical)
        )
    }
}

/// Host-wide build limits, read independently by admission guardians.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaneBuildSettings {
    /// Milliseconds between starts of the same class; zero unless configured.
    pub start_spacing_ms: Option<u64>,
    /// Kernel pressure threshold for admission; Critical unless set.
    pub memory_pressure_threshold: Option<MemoryPressureThreshold>,
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub max_concurrent_builds: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_auto")]
    /// Compiler jobs per lane; at most two by default to bound memory on laptops.
    pub cargo_build_jobs: Option<u64>,
    /// Most lanes of any class live at once across the host.
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub max_lanes: Option<u64>,
    /// Most lanes of any class live at once for one project.
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub max_project_lanes: Option<u64>,
    /// Most writer lanes live at once across the host.
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub max_writer_lanes: Option<u64>,
    /// Most writer lanes live at once for one project.
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub max_project_writer_lanes: Option<u64>,
    /// First port of the range split into per-writer-lane blocks.
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub lane_port_base: Option<u64>,
    /// Ports in each writer lane's block.
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub lane_port_block: Option<u64>,
    /// Blocks in the range; 0 turns port allocation off.
    #[serde(default, deserialize_with = "deserialize_auto")]
    pub lane_port_blocks: Option<u64>,
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
    /// files. Only recent writes are protected: a binary that runs longer than the
    /// grace without writing is unprotected unless a Cargo lock is held. 15
    /// minutes unless set.
    pub build_dir_grace_seconds: Option<u64>,
}
/// Longest configurable gap between lane starts: ten minutes.
pub const MAX_START_SPACING_MS: u64 = 600_000;
/// Default lane port space: 64 blocks of 16 ports from 30000, below the
/// ephemeral ranges of macOS and Linux and one block per possible writer lane.
pub const DEFAULT_LANE_PORT_BASE: u64 = 30000;
pub const DEFAULT_LANE_PORT_BLOCK: u64 = 16;
pub const DEFAULT_LANE_PORT_BLOCKS: u64 = 64;
/// Hard ceilings on the configurable lane caps.
pub const WRITER_LANES_CEILING: u64 = 64;
pub const PROJECT_WRITER_LANES_CEILING: u64 = 32;
/// Cargo's accepted `debug` levels, including current numeric and boolean forms.
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
    pub fn memory_pressure_threshold(&self) -> MemoryPressureThreshold {
        self.memory_pressure_threshold.unwrap_or_default()
    }

    pub fn memory_pressure_source(&self) -> provenant_records::diagnostic::SettingSource {
        if self.memory_pressure_threshold.is_some() {
            provenant_records::diagnostic::SettingSource::Configuration
        } else {
            provenant_records::diagnostic::SettingSource::ShippedDefault
        }
    }
    pub fn read(state: &Path) -> std::io::Result<Self> {
        crate::loader::read_settings(state, "domains.fabric.lanes", |settings: &Self| {
            if settings.invalid_keys().is_empty() {
                Ok(())
            } else {
                Err("invalid_value")
            }
        })
    }
    /// Out-of-range setting names, checked when configuration loads. Admission
    /// never re-validates them: it clamps through the accessors below.
    pub fn invalid_keys(&self) -> Vec<&'static str> {
        let mut invalid = Vec::new();
        let in_range = |value: Option<u64>, max: u64| value.is_none_or(|n| (1..=max).contains(&n));
        if !in_range(self.max_concurrent_builds, 64) {
            invalid.push("max_concurrent_builds");
        }
        if self
            .start_spacing_ms
            .is_some_and(|spacing| spacing > MAX_START_SPACING_MS)
        {
            invalid.push("start_spacing_ms");
        }
        if !in_range(self.cargo_build_jobs, 1024) {
            invalid.push("cargo_build_jobs");
        }
        if !in_range(self.max_lanes, WRITER_LANES_CEILING) {
            invalid.push("max_lanes");
        }
        if !in_range(self.max_project_lanes, PROJECT_WRITER_LANES_CEILING)
            || self
                .max_project_lanes
                .is_some_and(|project| project > self.total_lanes())
        {
            invalid.push("max_project_lanes");
        }
        if !in_range(self.max_writer_lanes, WRITER_LANES_CEILING) {
            invalid.push("max_writer_lanes");
        }
        if !in_range(self.max_project_writer_lanes, PROJECT_WRITER_LANES_CEILING)
            || self
                .max_project_writer_lanes
                .is_some_and(|project| project > self.writer_lanes())
        {
            invalid.push("max_project_writer_lanes");
        }
        if self
            .lane_port_base
            .is_some_and(|n| !(1024..=65535).contains(&n))
        {
            invalid.push("lane_port_base");
        }
        if !in_range(self.lane_port_block, 1024) {
            invalid.push("lane_port_block");
        }
        if self.lane_port_blocks.is_some_and(|n| n > 1024) {
            invalid.push("lane_port_blocks");
        }
        if self.lane_port_blocks() > 0
            && !invalid.contains(&"lane_port_base")
            && !invalid.contains(&"lane_port_block")
            && !invalid.contains(&"lane_port_blocks")
            && self.lane_port_base_value() + self.lane_port_block_size() * self.lane_port_blocks()
                > 65536
        {
            invalid.push("lane_port_blocks");
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
    pub fn lane_port_base_value(&self) -> u64 {
        self.lane_port_base.unwrap_or(DEFAULT_LANE_PORT_BASE)
    }
    pub fn lane_port_block_size(&self) -> u64 {
        self.lane_port_block.unwrap_or(DEFAULT_LANE_PORT_BLOCK)
    }
    pub fn lane_port_blocks(&self) -> u64 {
        self.lane_port_blocks.unwrap_or(DEFAULT_LANE_PORT_BLOCKS)
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
    /// Concurrent cargo builds the host allows: the setting (clamped to 1..=64)
    /// or max(1, min(cores / cargo_build_jobs, total GiB / 6)), so a 10-core
    /// 16 GiB host with the default 2 jobs gets 2.
    pub fn build_limit(&self, total_bytes: u64) -> u64 {
        if let Some(limit) = self.max_concurrent_builds {
            return limit.clamp(1, 64);
        }
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
        Self::automatic_builds(cores, self.jobs(), total_bytes)
    }
    /// The automatic build limit for a host with `cores` and `total_bytes`.
    pub fn automatic_builds(cores: u64, jobs: u64, total_bytes: u64) -> u64 {
        let by_cpu = cores / jobs.max(1);
        let by_memory = total_bytes / (6 * 1024_u64.pow(3));
        by_cpu.min(by_memory).clamp(1, 64)
    }
    /// Unconfigured caps are disabled. Explicit settings retain their bounds.
    pub fn total_lanes(&self) -> u64 {
        self.max_lanes
            .map(|limit| limit.clamp(1, WRITER_LANES_CEILING))
            .unwrap_or(u64::MAX)
    }

    pub fn project_total_lanes(&self) -> u64 {
        self.max_project_lanes
            .map(|limit| limit.clamp(1, PROJECT_WRITER_LANES_CEILING))
            .unwrap_or(u64::MAX)
            .min(self.total_lanes())
    }

    pub fn writer_lanes(&self) -> u64 {
        self.max_writer_lanes
            .map(|limit| limit.clamp(1, WRITER_LANES_CEILING))
            .unwrap_or(u64::MAX)
    }
    /// An unset project cap adds no limit beyond the host cap.
    pub fn project_writer_lanes(&self) -> u64 {
        self.max_project_writer_lanes
            .map(|limit| limit.clamp(1, PROJECT_WRITER_LANES_CEILING))
            .unwrap_or(u64::MAX)
            .min(self.writer_lanes())
    }

    pub fn jobs(&self) -> u64 {
        self.cargo_build_jobs.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 2) as u64)
        })
    }
}

/// Longest configurable store busy timeout, in milliseconds.
pub const STORE_BUSY_TIMEOUT_CEILING_MS: u64 = 600_000;

/// Host-wide store contention settings, read once when a command starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreSettings {
    /// Milliseconds a store waits for a contended lock before it reports
    /// contention. Zero fails fast; 30 seconds unless set.
    pub store_busy_timeout_ms: Option<u64>,
}
impl StoreSettings {
    pub fn read(state: &Path) -> std::io::Result<Self> {
        crate::loader::read_settings(state, "domains.fabric.limits", |settings: &Self| {
            if settings
                .store_busy_timeout_ms
                .is_some_and(|ms| ms > STORE_BUSY_TIMEOUT_CEILING_MS)
            {
                Err("invalid_value")
            } else {
                Ok(())
            }
        })
    }
    /// The configured wait, or `None` for the store's compiled default.
    pub fn busy_timeout(&self) -> Option<std::time::Duration> {
        self.store_busy_timeout_ms
            .map(std::time::Duration::from_millis)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GcSettings {
    pub schedule: bool,
    pub idle_days: u64,
    pub graph_checkpoint_days: u64,
    pub free_floor: Option<String>,
    /// Total lane Cargo cache cap; defaults to 32 GiB.
    pub lane_cargo_cache_max: Option<String>,
    pub alert_below: Option<String>,
    pub projects: BTreeMap<String, ProjectGcSettings>,
}

impl Default for GcSettings {
    fn default() -> Self {
        Self {
            schedule: true,
            idle_days: 7,
            graph_checkpoint_days: 90,
            free_floor: None,
            lane_cargo_cache_max: None,
            alert_below: None,
            projects: BTreeMap::new(),
        }
    }
}

impl GcSettings {
    /// Resolve a project's explicit settings, or its language-specific default.
    pub fn project_settings(&self, project: &str, root: &Path) -> ProjectGcSettings {
        self.projects.get(project).cloned().unwrap_or_else(|| {
            let mut settings = ProjectGcSettings::default();
            if root.join("Cargo.toml").is_file() {
                settings.regenerable.push("target".into());
            }
            settings
        })
    }

    /// Read the owning section without resolving unrelated domain settings.
    /// Invalid optional settings warn and fall back to compiled defaults.
    pub fn read(state: &Path) -> std::io::Result<Self> {
        crate::loader::read_settings(state, "gc", Self::validate)
    }

    /// The configured floor, or 1% of the state volume capped at 5 GiB.
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

    /// Total bound includes busy caches, which remain protected even above it.
    pub fn lane_cargo_cache_max_bytes(&self) -> std::io::Result<u64> {
        self.lane_cargo_cache_max
            .as_deref()
            .map(parse_cache_size)
            .transpose()
            .map(|cap| cap.unwrap_or(32 * 1024_u64.pow(3)))
            .map_err(std::io::Error::other)
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3650).contains(&self.idle_days) {
            return Err("idle_days_range");
        }
        if !(1..=3650).contains(&self.graph_checkpoint_days) {
            return Err("graph_checkpoint_days_range");
        }
        if let Some(size) = &self.free_floor {
            parse_cache_size(size)?;
        }
        if let Some(size) = &self.lane_cargo_cache_max {
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
    (total_bytes / 100).min(5 * 1024_u64.pow(3))
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
    fn retention_never_ages_git_fences_or_observation_evidence() {
        use super::*;
        for kind in [
            RetentionKind::GitWorktrees,
            RetentionKind::ObservationLedgers,
        ] {
            let class = RETENTION_CLASSES
                .iter()
                .find(|class| class.class == kind)
                .unwrap();
            assert!(class.age_rule.is_none());
            assert!(!retention_schedule(RetentionTrigger::FullGc).any(|entry| entry.class == kind));
        }
        for class in retention_schedule(RetentionTrigger::Gc) {
            assert!(
                retention_schedule(RetentionTrigger::FullGc)
                    .any(|entry| entry.class == class.class)
            );
            assert!(!class.owner.is_empty() && !class.custody_proof.is_empty());
        }
    }

    #[test]
    fn start_spacing_above_the_ceiling_is_invalid() {
        let spacing = |ms| super::LaneBuildSettings {
            start_spacing_ms: Some(ms),
            ..Default::default()
        };
        assert!(
            spacing(super::MAX_START_SPACING_MS)
                .invalid_keys()
                .is_empty()
        );
        assert_eq!(
            spacing(super::MAX_START_SPACING_MS + 1).invalid_keys(),
            ["start_spacing_ms"]
        );
    }

    #[test]
    fn direct_settings_reads_recover_invalid_optional_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        std::fs::write(
            temp.path().join("config.toml"),
            "schema_version=1\n[gc]\nidle_days=0\nschedule=false\nfuture_option=true\n[domains.fabric.lanes]\ncargo_build_jobs=-2\nmax_concurrent_builds=3\nmemory_pressure_threshold='unknown'\n",
        )?;
        let gc = super::GcSettings::read(temp.path())?;
        assert_eq!(gc.idle_days, super::GcSettings::default().idle_days);
        assert!(!gc.schedule);
        let lanes = super::LaneBuildSettings::read(temp.path())?;
        assert_eq!(lanes.cargo_build_jobs, None);
        assert_eq!(lanes.max_concurrent_builds, Some(3));
        assert_eq!(
            lanes.memory_pressure_threshold(),
            super::MemoryPressureThreshold::Critical
        );
        assert!(lanes.invalid_keys().is_empty());
        Ok(())
    }

    #[test]
    fn store_settings_read_the_busy_timeout_and_drop_an_out_of_range_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        assert_eq!(
            super::StoreSettings::read(temp.path())?.busy_timeout(),
            None
        );
        for (text, expected) in [
            ("store_busy_timeout_ms=250\nbody_bytes=1", Some(250)),
            ("store_busy_timeout_ms=0", Some(0)),
            ("store_busy_timeout_ms=600001", None),
            ("store_busy_timeout_ms='soon'", None),
        ] {
            std::fs::write(
                temp.path().join("config.toml"),
                format!("schema_version=1\n[domains.fabric.limits]\n{text}\n"),
            )?;
            assert_eq!(
                super::StoreSettings::read(temp.path())?.busy_timeout(),
                expected.map(std::time::Duration::from_millis),
                "{text}"
            );
        }
        Ok(())
    }

    #[test]
    fn direct_settings_reads_keep_syntax_size_and_version_errors_hard()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        for invalid in [
            "schema_version=2",
            "schema_version=1\n[gc",
            "schema_version='1'",
        ] {
            std::fs::write(temp.path().join("config.toml"), invalid)?;
            assert!(super::GcSettings::read(temp.path()).is_err());
            assert!(super::LaneBuildSettings::read(temp.path()).is_err());
        }
        std::fs::write(
            temp.path().join("config.toml"),
            " ".repeat(provenant_records::Limits::CONFIG.max_bytes as usize + 1),
        )?;
        assert!(super::GcSettings::read(temp.path()).is_err());
        assert!(super::LaneBuildSettings::read(temp.path()).is_err());
        Ok(())
    }

    #[test]
    fn direct_settings_reads_isolate_quoted_keys_and_invalid_project_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        std::fs::write(
            temp.path().join("config.toml"),
            "schema_version=1\n[gc]\nschedule=false\n\"future.option\"=true\n[gc.projects.good]\nkeep=['cache']\n[gc.projects.bad]\nkeep=['../outside']\n",
        )?;
        let settings = super::GcSettings::read(temp.path())?;
        assert!(!settings.schedule);
        assert_eq!(settings.projects["good"].keep, ["cache"]);
        assert!(
            settings
                .projects
                .get("bad")
                .is_none_or(|row| row.keep.is_empty())
        );
        Ok(())
    }

    #[test]
    fn direct_settings_reads_preserve_siblings_of_nested_quoted_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        std::fs::write(
            temp.path().join("config.toml"),
            "schema_version=1\n[gc]\nschedule=false\n[gc.projects.'bad.row']\nfuture=true\n[gc.projects.good]\nkeep=['cache']\n",
        )?;
        let settings = super::GcSettings::read(temp.path())?;
        assert!(!settings.schedule);
        assert_eq!(settings.projects["good"].keep, ["cache"]);
        Ok(())
    }

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
    fn gc_project_defaults_cargo_output_only_without_an_override()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let mut settings = super::GcSettings::default();
        assert!(
            settings
                .project_settings("p", root.path())
                .regenerable
                .is_empty()
        );
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n")?;
        assert_eq!(
            settings.project_settings("p", root.path()).regenerable,
            ["target"]
        );
        settings.projects.insert(
            "p".into(),
            super::ProjectGcSettings {
                regenerable: vec!["build".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            settings.project_settings("p", root.path()).regenerable,
            ["build"]
        );
        settings.projects.get_mut("p").unwrap().regenerable.clear();
        assert!(
            settings
                .project_settings("p", root.path())
                .regenerable
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn lane_cache_cap_defaults_and_validates() {
        assert_eq!(
            super::GcSettings::default()
                .lane_cargo_cache_max_bytes()
                .unwrap(),
            32 * 1024_u64.pow(3)
        );
        let configured: super::GcSettings =
            toml_edit::de::from_str("lane_cargo_cache_max = '8G'").unwrap();
        assert_eq!(
            configured.lane_cargo_cache_max_bytes().unwrap(),
            8 * 1024_u64.pow(3)
        );
        assert!(configured.validate().is_ok());
        let invalid: super::GcSettings =
            toml_edit::de::from_str("lane_cargo_cache_max = 'invalid'").unwrap();
        assert!(invalid.validate().is_err());
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
    fn memory_pressure_accepts_host_policy() {
        use provenant_os::sys::resources::MemoryPressure;
        use provenant_records::diagnostic::SettingSource;
        assert_eq!(
            LaneBuildSettings::default().memory_pressure_source(),
            SettingSource::ShippedDefault
        );
        let defaults: LaneBuildSettings =
            toml_edit::de::from_str("memory_pressure_threshold = 'critical'").unwrap();
        assert_eq!(
            defaults.memory_pressure_source(),
            SettingSource::Configuration
        );
        assert!(
            !defaults
                .memory_pressure_threshold()
                .defers(MemoryPressure::Warning)
        );
        assert!(
            defaults
                .memory_pressure_threshold()
                .defers(MemoryPressure::Critical)
        );
        let warning: LaneBuildSettings =
            toml_edit::de::from_str("memory_pressure_threshold = 'warning'").unwrap();
        assert!(
            warning
                .memory_pressure_threshold()
                .defers(MemoryPressure::Warning)
        );
        assert!(
            !warning
                .memory_pressure_threshold()
                .defers(MemoryPressure::Normal)
        );
        let never: LaneBuildSettings =
            toml_edit::de::from_str("memory_pressure_threshold = 'never'").unwrap();
        assert!(
            !never
                .memory_pressure_threshold()
                .defers(MemoryPressure::Critical)
        );
        assert!(
            toml_edit::de::from_str::<LaneBuildSettings>("memory_pressure_threshold = 'invalid'")
                .is_err()
        );
    }
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
    fn lane_port_space_defaults_validates_and_can_be_switched_off() {
        let defaults = LaneBuildSettings::default();
        assert!(defaults.invalid_keys().is_empty());
        assert!(
            defaults.lane_port_base_value()
                + defaults.lane_port_block_size() * defaults.lane_port_blocks()
                <= u16::MAX as u64 + 1
        );
        let off: LaneBuildSettings = toml_edit::de::from_str("lane_port_blocks = 0").unwrap();
        assert!(off.invalid_keys().is_empty());
        assert_eq!(off.lane_port_blocks(), 0);
        let low: LaneBuildSettings = toml_edit::de::from_str("lane_port_base = 80").unwrap();
        assert_eq!(low.invalid_keys(), ["lane_port_base"]);
        let past_the_end: LaneBuildSettings =
            toml_edit::de::from_str("lane_port_base = 65000\nlane_port_block = 64").unwrap();
        assert_eq!(past_the_end.invalid_keys(), ["lane_port_blocks"]);
        let huge: LaneBuildSettings =
            toml_edit::de::from_str("lane_port_block = 1025\nlane_port_blocks = 1025").unwrap();
        assert!(huge.invalid_keys().contains(&"lane_port_block"));
        assert!(huge.invalid_keys().contains(&"lane_port_blocks"));
        let overflow: LaneBuildSettings =
            toml_edit::de::from_str("lane_port_block = 9223372036854775807").unwrap();
        assert_eq!(overflow.invalid_keys(), ["lane_port_block"]);
    }
    #[test]
    fn writer_caps_and_wait_validate_and_default() {
        let defaults = LaneBuildSettings::default();
        assert!(defaults.invalid_keys().is_empty());
        assert_eq!(defaults.writer_lanes(), u64::MAX);
        assert_eq!(defaults.project_writer_lanes(), u64::MAX);
        assert_eq!(defaults.start_spacing_ms.unwrap_or(0), 0);
        let invalid: LaneBuildSettings =
            toml_edit::de::from_str("max_writer_lanes = 65\nmax_project_writer_lanes = 0").unwrap();
        assert_eq!(
            invalid.invalid_keys(),
            ["max_writer_lanes", "max_project_writer_lanes",]
        );
        let wider: LaneBuildSettings =
            toml_edit::de::from_str("max_writer_lanes = 8\nmax_project_writer_lanes = 9").unwrap();
        assert_eq!(wider.invalid_keys(), ["max_project_writer_lanes"]);
        let fine: LaneBuildSettings =
            toml_edit::de::from_str("max_writer_lanes = 8\nmax_project_writer_lanes = 8").unwrap();
        assert!(fine.invalid_keys().is_empty());
        // Only the host cap set: the unset project key is not blamed, and its
        // effective cap follows the host cap down.
        let only_host: LaneBuildSettings =
            toml_edit::de::from_str("max_writer_lanes = 20").unwrap();
        assert!(only_host.invalid_keys().is_empty());
        assert_eq!(only_host.writer_lanes(), 20);
        assert_eq!(only_host.project_writer_lanes(), 20);
        let roomy: LaneBuildSettings = toml_edit::de::from_str("max_writer_lanes = 50").unwrap();
        assert_eq!(
            roomy.project_writer_lanes(),
            defaults.project_writer_lanes().min(50)
        );
    }
    #[test]
    fn automatic_build_slots_follow_cores_jobs_and_memory() {
        let gib = 1024_u64.pow(3);
        // A 10-core 16 GiB host with 5 jobs per build: 2 by cores, 2 by memory.
        assert_eq!(LaneBuildSettings::automatic_builds(10, 5, 16 * gib), 2);
        assert_eq!(LaneBuildSettings::automatic_builds(10, 5, 64 * gib), 2);
        assert_eq!(LaneBuildSettings::automatic_builds(32, 4, 64 * gib), 8);
        assert_eq!(LaneBuildSettings::automatic_builds(32, 4, 12 * gib), 2);
        assert_eq!(LaneBuildSettings::automatic_builds(2, 8, 4 * gib), 1);
    }
    #[test]
    fn defaults_and_overrides() {
        let settings = LaneBuildSettings {
            max_concurrent_builds: Some(3),
            cargo_build_jobs: Some(7),
            ..LaneBuildSettings::default()
        };
        assert_eq!(settings.build_limit(1), 3);
        assert_eq!(settings.jobs(), 7);
        assert!((1..=2).contains(&LaneBuildSettings::default().jobs()));
        assert!(LaneBuildSettings::default().build_limit(0) >= 1);
        // Out-of-range values are rejected at config load and clamped here.
        let wild = LaneBuildSettings {
            max_concurrent_builds: Some(0),
            ..Default::default()
        };
        assert_eq!(wild.build_limit(0), 1);
    }
    #[test]
    fn total_lane_caps_are_optional_and_validate_host_policy() {
        let defaults = LaneBuildSettings::default();
        assert_eq!(defaults.total_lanes(), u64::MAX);
        assert_eq!(defaults.project_total_lanes(), u64::MAX);
        let configured: LaneBuildSettings =
            toml_edit::de::from_str("max_lanes = 12\nmax_project_lanes = 5").unwrap();
        assert!(configured.invalid_keys().is_empty());
        assert_eq!(configured.total_lanes(), 12);
        assert_eq!(configured.project_total_lanes(), 5);
        let invalid: LaneBuildSettings =
            toml_edit::de::from_str("max_lanes = 3\nmax_project_lanes = 4").unwrap();
        assert!(invalid.invalid_keys().contains(&"max_project_lanes"));
        let host_only: LaneBuildSettings = toml_edit::de::from_str("max_lanes = 3").unwrap();
        assert_eq!(host_only.project_total_lanes(), 3);
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
