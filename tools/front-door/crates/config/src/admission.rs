//! Named admission classes configuration and layering.
//!
//! Provenant ships the `build` admission class by default, whose default behaviour
//! reproduces host-wide build slots. Host and project configurations can override
//! `build` or declare custom classes (such as `test`, `browser`, `heavy`).
use crate::gc::{LaneBuildSettings, parse_cache_size};
use provenant_records::{Layer, Precedence};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_CLASS: &str = "build";
pub const DEFAULT_WAIT_SECONDS: u64 = 15;

/// The admission family's layers, lowest first. Host-project state is host
/// state bound to one project, so it outranks the host's own file.
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
pub const ADMISSION_PRECEDENCE: Precedence = Precedence(&[
    Layer::Default,
    Layer::Host,
    Layer::HostProject,
    Layer::Project,
]);

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

/// Merge one admission-class field across the layers that declare it. The
/// class's precedence const is the one declaration of order, and an ignored
/// candidate is reported rather than silently dropped.
fn candidates<'a, T>(
    layers: &'a [(Layer, AdmissionClassConfig)],
    pick: impl Fn(&AdmissionClassConfig) -> Option<T> + 'a,
) -> impl Iterator<Item = provenant_records::Sourced<T>> + 'a {
    use provenant_records::{Source, Sourced};
    layers.iter().filter_map(move |(layer, config)| {
        pick(config).map(|value| Sourced {
            value,
            source: Source {
                layer: *layer,
                origin: None,
            },
        })
    })
}

/// A field with no built-in default: absent when no layer declares it.
fn resolve_field<T>(
    layers: &[(Layer, AdmissionClassConfig)],
    pick: impl Fn(&AdmissionClassConfig) -> Option<T>,
) -> (Option<T>, Option<Layer>) {
    let merge = ADMISSION_PRECEDENCE.merge(candidates(layers, pick));
    provenant_records::warn_ignored(&merge.ignored, "admission class");
    match merge.winner {
        Some(winner) => (Some(winner.value), Some(winner.source.layer)),
        None => (None, None),
    }
}

/// A field with a built-in default: always resolves, with the winning layer.
fn resolve_defaulted<T>(
    layers: &[(Layer, AdmissionClassConfig)],
    default: T,
    pick: impl Fn(&AdmissionClassConfig) -> Option<T>,
) -> (T, Layer) {
    let resolved = ADMISSION_PRECEDENCE.merge_over_default(default, candidates(layers, pick));
    provenant_records::warn_ignored(&resolved.ignored, "admission class");
    (resolved.winner.value, resolved.winner.source.layer)
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
    pub slots_source: Layer,
    pub memory_per_slot_bytes: Option<u64>,
    pub memory_per_slot_source: Option<Layer>,
    pub disk_floor_bytes: Option<u64>,
    pub disk_floor_source: Option<Layer>,
    pub trim_action: Option<String>,
    pub trim_action_source: Option<Layer>,
    pub wait_seconds: u64,
    pub wait_seconds_source: Layer,
}

impl ResolvedAdmissionClass {
    /// Resolves the effective class configuration by layering:
    /// 1. Shipped defaults (`build` derives slots from cores/RAM; custom classes default to 1 slot)
    /// 2. Host `<state>/config.toml` (under `[admission.<class>]`)
    /// 3. Host-project `<state>/projects/<ProjectId>/config.toml` (under `[admission.<class>]`)
    /// 4. Project `<project>/.provenant/config.toml` (under `[admission.<class>]`)
    ///
    /// The host-project directory comes from [`crate::host_project_dir`], the
    /// same binding resolver install and the other config layers use.
    pub fn resolve(class: &str, state: &Path, project: Option<&Path>) -> Self {
        let is_build = class == DEFAULT_CLASS;
        let host_build_settings = LaneBuildSettings::read(state).unwrap_or_default();
        let total_ram =
            provenant_os::sys::resources::memory().map_or(0, |memory| memory.total_bytes);
        let default_slots = if is_build {
            host_build_settings.automatic_build_limit(total_ram)
        } else {
            1
        };

        // Collect one config per declaring layer, then merge each field with
        // the family's single precedence declaration.
        let mut layers: Vec<(Layer, AdmissionClassConfig)> = Vec::new();
        if let Some(config) = read_class_from_file(&state.join("config.toml"), class) {
            layers.push((Layer::Host, config));
        }
        if let Some(dir) = project.and_then(|project| crate::host_project_dir(state, project))
            && let Some(config) = read_class_from_file(&dir.join("config.toml"), class)
        {
            layers.push((Layer::HostProject, config));
        }
        if let Some(project) = project {
            let path = project.join(".provenant").join("config.toml");
            if let Some(config) = read_class_from_file(&path, class) {
                layers.push((Layer::Project, config));
            }
        }

        let (slots, slots_source) =
            resolve_defaulted(&layers, default_slots, |config| config.slots);
        let (memory_bytes, memory_source) =
            resolve_field(&layers, |config| config.memory_per_slot_bytes());
        let (disk_bytes, disk_source) = resolve_field(&layers, |config| config.disk_floor_bytes());
        let (trim_action, trim_source) =
            resolve_field(&layers, |config| config.trim_action.clone());
        let (wait_seconds, wait_source) =
            resolve_defaulted(&layers, DEFAULT_WAIT_SECONDS, |config| config.wait_seconds);

        Self {
            name: class.to_string(),
            slots,
            slots_source,
            memory_per_slot_bytes: memory_bytes,
            memory_per_slot_source: memory_source,
            disk_floor_bytes: disk_bytes,
            disk_floor_source: disk_source,
            trim_action,
            trim_action_source: trim_source,
            wait_seconds,
            wait_seconds_source: wait_source,
        }
    }

    /// Discover and resolve all classes defined across host, host-project,
    /// project, plus the default `build`.
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

        if let Some(dir) = project.and_then(|project| crate::host_project_dir(state, project)) {
            for name in list_classes_in_file(&dir.join("config.toml")) {
                if !classes.contains_key(&name) {
                    classes.insert(name.clone(), Self::resolve(&name, state, project));
                }
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
        assert_eq!(resolved.slots_source, Layer::Default);
        assert_eq!(resolved.wait_seconds, DEFAULT_WAIT_SECONDS);
        assert_eq!(resolved.wait_seconds_source, Layer::Default);
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
        assert_eq!(resolved.slots_source, Layer::Host);
        assert_eq!(resolved.memory_per_slot_bytes, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(resolved.memory_per_slot_source, Some(Layer::Host));
        assert_eq!(resolved.disk_floor_bytes, Some(10 * 1024 * 1024 * 1024));
        assert_eq!(resolved.disk_floor_source, Some(Layer::Host));
        assert_eq!(resolved.trim_action.as_deref(), Some("cargo clean"));
        assert_eq!(resolved.trim_action_source, Some(Layer::Host));
        assert_eq!(resolved.wait_seconds, 30);
        assert_eq!(resolved.wait_seconds_source, Layer::Host);
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
        assert_eq!(resolved.slots_source, Layer::Project);
        assert_eq!(resolved.wait_seconds, 10);
        assert_eq!(resolved.wait_seconds_source, Layer::Project);
    }

    #[test]
    fn retired_build_cap_key_does_not_override_the_build_class() {
        let temp = tempfile::tempdir().unwrap();
        let config_toml = r#"
schema_version = 1
[domains.fabric.lanes]
max_concurrent_builds = 3
"#;
        std::fs::write(temp.path().join("config.toml"), config_toml).unwrap();
        let resolved = ResolvedAdmissionClass::resolve(DEFAULT_CLASS, temp.path(), None);
        let defaults_dir = tempfile::tempdir().unwrap();
        let defaults = ResolvedAdmissionClass::resolve(DEFAULT_CLASS, defaults_dir.path(), None);
        assert_eq!(resolved.slots, defaults.slots);
        assert_eq!(resolved.slots_source, Layer::Default);
    }

    /// Spec: docs/specs/state.md#host-project-state-has-one-key
    #[test]
    fn host_project_layer_order() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path();
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::write(
            state.join("config.toml"),
            r#"
[admission.test]
slots = 8
wait_seconds = 30
memory_per_slot = "4 GiB"
"#,
        )
        .unwrap();
        let bound = crate::host_project_dir(state, &project).unwrap();
        std::fs::create_dir_all(&bound).unwrap();
        std::fs::write(
            bound.join("config.toml"),
            r#"
[admission.test]
slots = 4
wait_seconds = 20
disk_floor = "10 GiB"
"#,
        )
        .unwrap();

        // The obsolete sha256 key names a directory that is never read.
        let canonical = std::fs::canonicalize(project.join(".git")).unwrap();
        let old = state.join("projects").join(crate::project_key(&canonical));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join("config.toml"),
            "[admission.test]\nslots = 1\nwait_seconds = 99\n",
        )
        .unwrap();

        // With no project layer, the binding layer overrides the host layer.
        let resolved = ResolvedAdmissionClass::resolve("test", state, Some(&project));
        assert_eq!(resolved.slots, 4);
        assert_eq!(resolved.slots_source, Layer::HostProject);
        assert_eq!(resolved.wait_seconds, 20);
        assert_eq!(resolved.wait_seconds_source, Layer::HostProject);

        // A project layer overrides the binding layer per leaf; leaves the
        // project does not set keep the binding layer's value and source.
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        std::fs::write(
            project.join(".provenant").join("config.toml"),
            "[admission.test]\nslots = 2\n",
        )
        .unwrap();
        let resolved = ResolvedAdmissionClass::resolve("test", state, Some(&project));
        assert_eq!(resolved.slots, 2);
        assert_eq!(resolved.slots_source, Layer::Project);
        assert_eq!(resolved.wait_seconds, 20);
        assert_eq!(resolved.wait_seconds_source, Layer::HostProject);
        assert_eq!(resolved.memory_per_slot_bytes, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(resolved.memory_per_slot_source, Some(Layer::Host));
        assert_eq!(resolved.disk_floor_bytes, Some(10 * 1024 * 1024 * 1024));
        assert_eq!(resolved.disk_floor_source, Some(Layer::HostProject));
        assert_eq!(Layer::HostProject.description(), "host_project");
    }

    #[test]
    fn host_project_absent_is_silent() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path();
        let project = temp.path().join("unbound");
        std::fs::write(state.join("config.toml"), "[admission.test]\nslots = 6\n").unwrap();
        for project in [None, Some(project.as_path())] {
            let resolved = ResolvedAdmissionClass::resolve("test", state, project);
            assert_eq!(resolved.slots, 6);
            assert_eq!(resolved.slots_source, Layer::Host);
        }
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn every_admission_layer_set_picks_the_project_and_shadows_the_rest() {
        use provenant_records::{Source, Sourced};

        let candidates = ADMISSION_PRECEDENCE.0.iter().copied().map(|layer| Sourced {
            value: layer.as_str(),
            source: Source {
                layer,
                origin: None,
            },
        });
        let merge = ADMISSION_PRECEDENCE.merge(candidates);
        assert_eq!(
            merge
                .winner
                .as_ref()
                .map(|candidate| candidate.source.layer),
            Some(Layer::Project)
        );
        assert_eq!(
            merge
                .shadowed
                .iter()
                .map(|candidate| candidate.source.layer)
                .collect::<Vec<_>>(),
            [Layer::Default, Layer::Host, Layer::HostProject]
        );
        assert!(merge.ignored.is_empty());
    }
}
