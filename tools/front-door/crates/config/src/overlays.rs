//! Shared config layers for owner-supplied catalogue and price overlays.
//!
//! Overlays live in the same host and project configuration files the other
//! cross-cutting domains use, in the same precedence order as admission and
//! ports: host `<state>/config.toml`, host-project
//! `<state>/projects/<ProjectId>/config.toml`, then project
//! `<project>/.provenant/config.toml`. The host-project directory is resolved
//! once by [`crate::host_project_dir`], the same binding resolver install and
//! the port layer use. A later layer replaces an entry with the same key; it
//! never merges one entry into another. The bundled asset is the base layer and
//! is supplied by the owning domain, not read here.
//!
//! A single invalid entry does not refuse the whole overlay. Each owner skips
//! the entry and records an [`OverlayWarning`] naming the layer and the reason.

use provenant_records::{Layer, Precedence};
use serde::Serialize;
use std::{fmt, path::Path};

/// The overlay family's layers, lowest first. The bundled asset is the base;
/// host and project configuration replace its entries. Host-project state is
/// host state bound to a project, so it reports as the host layer to owners.
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
pub const OVERLAY_PRECEDENCE: Precedence =
    Precedence(&[Layer::Default, Layer::Host, Layer::Project]);

/// One skipped entry, with the layer and the reason for skipping it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlayWarning {
    pub source: Layer,
    pub key: String,
    pub reason: String,
}

impl OverlayWarning {
    pub fn new(source: Layer, key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            source,
            key: key.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for OverlayWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} overlay entry {} skipped: {}",
            self.source, self.key, self.reason
        )
    }
}

/// One layer's config text plus the source to attribute its entries to.
#[derive(Debug, Clone)]
pub struct OverlayLayer {
    pub source: Layer,
    pub text: String,
    pub path: std::path::PathBuf,
}

/// Read one named overlay table from the host and project config layers, in
/// precedence order. A missing file or absent table is silent; an unreadable or
/// malformed file is reported as a warning and the layer is skipped so the
/// remaining layers still apply.
pub fn read_layers(
    state: &Path,
    project: Option<&Path>,
    name: &str,
) -> (Vec<OverlayLayer>, Vec<OverlayWarning>) {
    let mut paths = vec![(Layer::Host, state.join("config.toml"))];
    if let Some(dir) = project.and_then(|project| crate::host_project_dir(state, project)) {
        paths.push((Layer::Host, dir.join("config.toml")));
    }
    if let Some(project) = project {
        paths.push((
            Layer::Project,
            project.join(".provenant").join("config.toml"),
        ));
    }
    let mut warnings = Vec::new();
    let mut layers = Vec::new();
    for (source, path) in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                warnings.push(OverlayWarning::new(
                    source,
                    name,
                    format!("{}: {error}", path.display()),
                ));
                continue;
            }
        };
        // Validate TOML syntax here so a malformed layer cannot leak into an
        // owner's entry decoder as an opaque error. Unknown top-level tables
        // are ordinary config owned by other domains and stay untouched.
        if let Err(error) = text.parse::<toml_edit::DocumentMut>() {
            warnings.push(OverlayWarning::new(
                source,
                name,
                format!("{}: invalid TOML: {error}", path.display()),
            ));
            continue;
        }
        layers.push(OverlayLayer { source, text, path });
    }
    // The one declaration owns the order: stable sorting keeps host and
    // host-project (both reported as Host) in the order they were read.
    layers.sort_by_key(|layer| {
        OVERLAY_PRECEDENCE
            .0
            .iter()
            .position(|declared| *declared == layer.source)
            .unwrap_or(usize::MAX)
    });
    (layers, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec: docs/specs/state.md#host-project-state-has-one-key
    #[test]
    fn host_project_overlay_is_read_from_the_binding_directory() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();

        let bound = crate::host_project_dir(&state, &project).unwrap();
        std::fs::create_dir_all(&bound).unwrap();
        std::fs::write(
            bound.join("config.toml"),
            "[catalogue]\nentries = [\"from-binding\"]\n",
        )
        .unwrap();

        // The obsolete sha256 key names a different directory. A layer filed
        // there is never read, even though it carries a conflicting value.
        let canonical = std::fs::canonicalize(project.join(".git")).unwrap();
        let old = state
            .join("projects")
            .join(crate::project_key(&canonical))
            .join("config.toml");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "[catalogue]\nentries = [\"from-old-key\"]\n").unwrap();

        let (layers, warnings) = read_layers(&state, Some(&project), "catalogue");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(layers.len(), 1, "{layers:?}");
        assert_eq!(layers[0].source, Layer::Host);
        assert!(layers[0].text.contains("from-binding"), "{:?}", layers[0]);
        assert!(!layers[0].text.contains("from-old-key"), "{:?}", layers[0]);
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn every_overlay_layer_set_picks_the_project_and_shadows_the_rest() {
        use provenant_records::{Source, Sourced};

        let candidates = OVERLAY_PRECEDENCE.0.iter().copied().map(|layer| Sourced {
            value: layer.as_str(),
            source: Source {
                layer,
                origin: None,
            },
        });
        let merge = OVERLAY_PRECEDENCE.merge(candidates);
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
            [Layer::Default, Layer::Host]
        );
        assert!(merge.ignored.is_empty());
    }
}
