//! Named admission classes configuration and layering.
//!
//! Provenant ships the `build` admission class by default, whose default behaviour
//! reproduces host-wide build slots. Host and project configurations can override
//! `build` or declare custom classes (such as `test`, `browser`, `heavy`).
use crate::gc::{LaneBuildSettings, parse_cache_size};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_CLASS: &str = "build";
pub const DEFAULT_WAIT_SECONDS: u64 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionSource {
    Default,
    Host,
    Project,
}

impl AdmissionSource {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Host => "host",
            Self::Project => "project",
        }
    }
}

/// Raw parsed TOML configuration for one admission class.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionClassConfig {
    #[serde(default)]
    pub slots: Option<u64>,
    #[serde(default)]
    pub memory_per_slot: Option<String>,
    #[serde(default)]
    pub disk_floor: Option<String>,
    #[serde(default)]
    pub trim_action: Option<String>,
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

fn parse_size(s: &str) -> Option<u64> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    parse_cache_size(&clean).ok()
}

impl AdmissionClassConfig {
    pub fn memory_per_slot_bytes(&self) -> Option<u64> {
        self.memory_per_slot.as_deref().and_then(parse_size)
    }

    pub fn disk_floor_bytes(&self) -> Option<u64> {
        self.disk_floor.as_deref().and_then(parse_size)
    }
}

/// Resolved effective admission class configuration with its provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAdmissionClass {
    pub name: String,
    pub slots: u64,
    pub slots_source: AdmissionSource,
    pub memory_per_slot_bytes: Option<u64>,
    pub memory_per_slot_source: Option<AdmissionSource>,
    pub disk_floor_bytes: Option<u64>,
    pub disk_floor_source: Option<AdmissionSource>,
    pub trim_action: Option<String>,
    pub trim_action_source: Option<AdmissionSource>,
    pub wait_seconds: u64,
    pub wait_seconds_source: AdmissionSource,
}

impl ResolvedAdmissionClass {
    /// Resolves the effective class configuration by layering:
    /// 1. Shipped defaults (`build` derives slots from cores/RAM; custom classes default to 1 slot)
    /// 2. Host `<state>/config.toml` (under `[admission.<class>]`, or legacy `domains.fabric.lanes.max_concurrent_builds` for `build`)
    /// 3. Project `<project>/.provenant/config.toml` (under `[admission.<class>]`)
    pub fn resolve(class: &str, state: &Path, project: Option<&Path>) -> Self {
        let is_build = class == DEFAULT_CLASS;

        // 1. Shipped default
        let host_build_settings = LaneBuildSettings::read(state).unwrap_or_default();
        let total_ram =
            provenant_os::sys::resources::memory().map_or(0, |memory| memory.total_bytes);

        let (mut slots, mut slots_source) = if is_build {
            if host_build_settings.max_concurrent_builds.is_some() {
                (
                    host_build_settings.build_limit(total_ram),
                    AdmissionSource::Host,
                )
            } else {
                (
                    host_build_settings.build_limit(total_ram),
                    AdmissionSource::Default,
                )
            }
        } else {
            (1, AdmissionSource::Default)
        };

        let mut memory_bytes = None;
        let mut memory_source = None;
        let mut disk_bytes = None;
        let mut disk_source = None;
        let mut trim = None;
        let mut trim_source = None;
        let mut wait = DEFAULT_WAIT_SECONDS;
        let mut wait_source = AdmissionSource::Default;

        // 2. Host configuration: `<state>/config.toml` [admission.<class>]
        if let Some(host_cfg) = read_class_from_file(&state.join("config.toml"), class) {
            if let Some(s) = host_cfg.slots {
                slots = s;
                slots_source = AdmissionSource::Host;
            }
            if let Some(bytes) = host_cfg.memory_per_slot_bytes() {
                memory_bytes = Some(bytes);
                memory_source = Some(AdmissionSource::Host);
            }
            if let Some(bytes) = host_cfg.disk_floor_bytes() {
                disk_bytes = Some(bytes);
                disk_source = Some(AdmissionSource::Host);
            }
            if let Some(action) = host_cfg.trim_action {
                trim = Some(action);
                trim_source = Some(AdmissionSource::Host);
            }
            if let Some(w) = host_cfg.wait_seconds {
                wait = w;
                wait_source = AdmissionSource::Host;
            }
        }

        // 3. Project configuration: `<project>/.provenant/config.toml` [admission.<class>]
        if let Some(project) = project {
            let project_config_path = project.join(".provenant").join("config.toml");
            if let Some(proj_cfg) = read_class_from_file(&project_config_path, class) {
                if let Some(s) = proj_cfg.slots {
                    slots = s;
                    slots_source = AdmissionSource::Project;
                }
                if let Some(bytes) = proj_cfg.memory_per_slot_bytes() {
                    memory_bytes = Some(bytes);
                    memory_source = Some(AdmissionSource::Project);
                }
                if let Some(bytes) = proj_cfg.disk_floor_bytes() {
                    disk_bytes = Some(bytes);
                    disk_source = Some(AdmissionSource::Project);
                }
                if let Some(action) = proj_cfg.trim_action {
                    trim = Some(action);
                    trim_source = Some(AdmissionSource::Project);
                }
                if let Some(w) = proj_cfg.wait_seconds {
                    wait = w;
                    wait_source = AdmissionSource::Project;
                }
            }
        }

        Self {
            name: class.to_string(),
            slots,
            slots_source,
            memory_per_slot_bytes: memory_bytes,
            memory_per_slot_source: memory_source,
            disk_floor_bytes: disk_bytes,
            disk_floor_source: disk_source,
            trim_action: trim,
            trim_action_source: trim_source,
            wait_seconds: wait,
            wait_seconds_source: wait_source,
        }
    }

    /// Discover and resolve all classes defined across host, project, plus default `build`.
    pub fn resolve_all(
        state: &Path,
        project: Option<&Path>,
    ) -> BTreeMap<String, ResolvedAdmissionClass> {
        let mut classes = BTreeMap::new();
        classes.insert(
            DEFAULT_CLASS.to_string(),
            Self::resolve(DEFAULT_CLASS, state, project),
        );

        for name in list_classes_in_file(&state.join("config.toml")) {
            if !classes.contains_key(&name) {
                classes.insert(name.clone(), Self::resolve(&name, state, project));
            }
        }

        if let Some(project) = project {
            for name in list_classes_in_file(&project.join(".provenant").join("config.toml")) {
                if !classes.contains_key(&name) {
                    classes.insert(name.clone(), Self::resolve(&name, state, Some(project)));
                }
            }
        }

        classes
    }
}

fn read_class_from_file(path: &Path, class: &str) -> Option<AdmissionClassConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    let doc: toml_edit::DocumentMut = content.parse().ok()?;
    let admission_table = doc.get("admission")?.as_table()?;
    let class_item = admission_table.get(class)?;
    let text = class_item.to_string();
    toml_edit::de::from_str::<AdmissionClassConfig>(&text).ok()
}

fn list_classes_in_file(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let Some(admission_table) = doc.get("admission").and_then(|item| item.as_table()) else {
        return Vec::new();
    };
    admission_table
        .iter()
        .map(|(key, _)| key.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_class_resolves_default_when_unconfigured() {
        let temp = tempfile::tempdir().unwrap();
        let resolved = ResolvedAdmissionClass::resolve(DEFAULT_CLASS, temp.path(), None);
        assert_eq!(resolved.name, "build");
        assert_eq!(resolved.slots_source, AdmissionSource::Default);
        assert_eq!(resolved.wait_seconds, DEFAULT_WAIT_SECONDS);
        assert_eq!(resolved.wait_seconds_source, AdmissionSource::Default);
    }

    #[test]
    fn host_config_overrides_build_class() {
        let temp = tempfile::tempdir().unwrap();
        let config_toml = r#"
[admission.build]
slots = 8
memory_per_slot = "4 GiB"
disk_floor = "10 GiB"
trim_action = "cargo clean"
wait_seconds = 30
"#;
        std::fs::write(temp.path().join("config.toml"), config_toml).unwrap();
        let resolved = ResolvedAdmissionClass::resolve(DEFAULT_CLASS, temp.path(), None);
        assert_eq!(resolved.slots, 8);
        assert_eq!(resolved.slots_source, AdmissionSource::Host);
        assert_eq!(resolved.memory_per_slot_bytes, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(resolved.memory_per_slot_source, Some(AdmissionSource::Host));
        assert_eq!(resolved.disk_floor_bytes, Some(10 * 1024 * 1024 * 1024));
        assert_eq!(resolved.disk_floor_source, Some(AdmissionSource::Host));
        assert_eq!(resolved.trim_action.as_deref(), Some("cargo clean"));
        assert_eq!(resolved.trim_action_source, Some(AdmissionSource::Host));
        assert_eq!(resolved.wait_seconds, 30);
        assert_eq!(resolved.wait_seconds_source, AdmissionSource::Host);
    }

    #[test]
    fn project_config_overrides_host_config() {
        let host_temp = tempfile::tempdir().unwrap();
        let proj_temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj_temp.path().join(".provenant")).unwrap();

        let host_toml = r#"
[admission.test]
slots = 4
wait_seconds = 20
"#;
        let proj_toml = r#"
[admission.test]
slots = 2
wait_seconds = 10
"#;
        std::fs::write(host_temp.path().join("config.toml"), host_toml).unwrap();
        std::fs::write(
            proj_temp.path().join(".provenant").join("config.toml"),
            proj_toml,
        )
        .unwrap();

        let resolved =
            ResolvedAdmissionClass::resolve("test", host_temp.path(), Some(proj_temp.path()));
        assert_eq!(resolved.name, "test");
        assert_eq!(resolved.slots, 2);
        assert_eq!(resolved.slots_source, AdmissionSource::Project);
        assert_eq!(resolved.wait_seconds, 10);
        assert_eq!(resolved.wait_seconds_source, AdmissionSource::Project);
    }

    #[test]
    fn legacy_max_concurrent_builds_provides_host_build_slots() {
        let temp = tempfile::tempdir().unwrap();
        let config_toml = r#"
schema_version = 1
[domains.fabric.lanes]
max_concurrent_builds = 3
"#;
        std::fs::write(temp.path().join("config.toml"), config_toml).unwrap();
        let resolved = ResolvedAdmissionClass::resolve(DEFAULT_CLASS, temp.path(), None);
        assert_eq!(resolved.slots, 3);
        assert_eq!(resolved.slots_source, AdmissionSource::Host);
    }
}
