//! Owner-supplied `[[adapter_capabilities]]` entries.
//!
//! Entries live in the same host and project config layers as the other
//! overlays ([`crate::overlays`]). A later layer replaces the whole entry for
//! an adapter; it never merges one entry into another. An invalid entry is
//! skipped with a warning naming the layer, the key and the reason, so one
//! bad row cannot disable the rest of the table.
//!
//! An entry may only narrow a row's facts: `disable` names capabilities to
//! switch off, and `on_gap` sets the action for a degradable gap. Widening a
//! fact is not expressible. Write, network, resume and skill-location gaps
//! always refuse, so they have no `on_gap` key.
use provenant_records::{
    AdapterOverride, AdapterSpec, CapabilityOverlay, GapAction, LaunchField, OverridableCapability,
    Source,
};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};

use crate::overlays::{OverlayWarning, read_layers};

#[derive(Debug, Deserialize)]
struct EntryAsset {
    adapter: String,
    #[serde(default)]
    disable: Vec<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    on_gap: BTreeMap<String, String>,
}

/// Decode one layer row. Every invalid part is reported and skips the whole
/// entry: a partially applied narrowing would overstate what the owner asked.
fn decode_entry(
    row: EntryAsset,
    source: Source,
    warnings: &mut Vec<OverlayWarning>,
) -> Option<AdapterOverride> {
    let key = format!("adapter_capabilities[{}]", row.adapter);
    if AdapterSpec::get(row.adapter.as_str()).is_none() {
        warnings.push(OverlayWarning::new(
            source.layer,
            key,
            format!("unknown adapter `{}`", row.adapter),
        ));
        return None;
    }
    let mut disable = Vec::new();
    for name in row.disable {
        let Some(capability) = OverridableCapability::parse(&name) else {
            warnings.push(OverlayWarning::new(
                source.layer,
                key,
                format!("unknown capability `{name}` in disable"),
            ));
            return None;
        };
        disable.push(capability);
    }
    let mut on_gap = Vec::new();
    for (name, action) in row.on_gap {
        let field = match name.as_str() {
            "model" => LaunchField::Model,
            "effort" => LaunchField::Effort,
            "image_input" => LaunchField::ImageInput,
            "write" | "network" | "resume" | "skill_location" => {
                warnings.push(OverlayWarning::new(
                    source.layer,
                    key,
                    "write, network, resume and skill-location gaps always refuse",
                ));
                return None;
            }
            _ => {
                warnings.push(OverlayWarning::new(
                    source.layer,
                    key,
                    format!("unknown gap field `{name}`"),
                ));
                return None;
            }
        };
        let action = match action.as_str() {
            "warn" => GapAction::Warn,
            "off" => GapAction::Off,
            "refuse" => GapAction::Refuse,
            _ => {
                warnings.push(OverlayWarning::new(
                    source.layer,
                    key,
                    format!("invalid action `{action}`"),
                ));
                return None;
            }
        };
        on_gap.push((field, action));
    }
    Some(AdapterOverride {
        adapter: row.adapter,
        disable,
        reason: row.reason,
        on_gap,
        source,
    })
}

fn insert(overlay: &mut CapabilityOverlay, entry: AdapterOverride) {
    if let Some(existing) = overlay
        .entries
        .iter_mut()
        .find(|row| row.adapter == entry.adapter)
    {
        *existing = entry;
    } else {
        overlay.entries.push(entry);
    }
}

/// The bundled facts with owner entries applied in host, then project order.
pub fn effective(state: &Path, project: Option<&Path>) -> (CapabilityOverlay, Vec<OverlayWarning>) {
    let (layers, mut warnings) = read_layers(state, project, "adapter_capabilities");
    let mut overlay = CapabilityOverlay::default();
    for layer in layers {
        let document: toml_edit::DocumentMut = match layer.text.parse() {
            Ok(document) => document,
            Err(error) => {
                warnings.push(OverlayWarning::new(
                    layer.source,
                    "adapter_capabilities",
                    format!("invalid TOML: {error}"),
                ));
                continue;
            }
        };
        let rows: Vec<&toml_edit::Table> = document
            .as_table()
            .get("adapter_capabilities")
            .and_then(toml_edit::Item::as_array_of_tables)
            .map_or_else(Vec::new, |rows| rows.iter().collect());
        for row in rows {
            let key = row
                .get("adapter")
                .and_then(toml_edit::Item::as_str)
                .unwrap_or("<unreadable adapter>")
                .to_owned();
            match toml_edit::de::from_str::<EntryAsset>(&row.to_string()) {
                Ok(entry) => {
                    if let Some(entry) = decode_entry(
                        entry,
                        Source {
                            layer: layer.source,
                            origin: Some(format!("{}#adapter_capabilities", layer.path.display())),
                        },
                        &mut warnings,
                    ) {
                        insert(&mut overlay, entry);
                    }
                }
                Err(error) => warnings.push(OverlayWarning::new(
                    layer.source,
                    format!("adapter_capabilities[{key}]"),
                    error.to_string(),
                )),
            }
        }
    }
    (overlay, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{ADAPTER_ROW_SOURCE, Layer, OVERLAY_SOURCE, OverridableCapability};

    fn decode(text: &str) -> (CapabilityOverlay, Vec<OverlayWarning>) {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("config.toml"), text).unwrap();
        effective(&state, None)
    }

    #[test]
    fn a_project_entry_replaces_the_host_entry_and_reports_its_layer() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        std::fs::write(
            state.join("config.toml"),
            "[[adapter_capabilities]]\nadapter = \"codex\"\ndisable = [\"image_input\"]\non_gap = { effort = \"refuse\" }\n",
        )
        .unwrap();
        std::fs::write(
            project.join(".provenant").join("config.toml"),
            "[[adapter_capabilities]]\nadapter = \"codex\"\ndisable = [\"model\"]\nreason = \"project narrows the model\"\n",
        )
        .unwrap();

        let (overlay, warnings) = effective(&state, Some(&project));
        assert!(warnings.is_empty(), "{warnings:?}");
        let entry = overlay.entry("codex").expect("codex entry");
        assert_eq!(entry.disable, [OverridableCapability::Model]);
        assert!(entry.on_gap.is_empty());
        assert_eq!(entry.reason.as_deref(), Some("project narrows the model"));
        assert_eq!(entry.source.layer, Layer::Project);
        assert!(
            entry
                .source
                .origin
                .as_deref()
                .is_some_and(|origin| origin.ends_with("#adapter_capabilities")),
            "{entry:?}"
        );
    }

    #[test]
    fn an_unknown_adapter_is_skipped_with_a_warning() {
        let (overlay, warnings) = decode(
            "[[adapter_capabilities]]\nadapter = \"no-such-adapter\"\ndisable = [\"model\"]\n",
        );
        assert!(overlay.entries.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].reason.contains("unknown adapter"),
            "{warnings:?}"
        );
    }

    #[test]
    fn an_unknown_disable_key_is_skipped_with_a_warning() {
        let (overlay, warnings) = decode(
            "[[adapter_capabilities]]\nadapter = \"codex\"\ndisable = [\"model\", \"telepathy\"]\n",
        );
        assert!(overlay.entries.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("telepathy"), "{warnings:?}");
    }

    #[test]
    fn a_refuse_only_gap_is_skipped_with_its_fixed_text() {
        let (overlay, warnings) = decode(
            "[[adapter_capabilities]]\nadapter = \"codex\"\non_gap = { write = \"refuse\" }\n",
        );
        assert!(overlay.entries.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].reason,
            "write, network, resume and skill-location gaps always refuse"
        );
    }

    #[test]
    fn an_invalid_action_is_skipped_with_a_warning() {
        let (overlay, warnings) = decode(
            "[[adapter_capabilities]]\nadapter = \"codex\"\non_gap = { effort = \"maybe\" }\n",
        );
        assert!(overlay.entries.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].reason.contains("invalid action"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_valid_entry_keeps_its_disabled_fact_sourcing() {
        let (overlay, warnings) = decode(
            "[[adapter_capabilities]]\nadapter = \"codex\"\ndisable = [\"model\"]\non_gap = { image_input = \"off\" }\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let spec = provenant_records::AdapterSpec::get("codex")
            .unwrap()
            .with_overlay(&overlay);
        assert!(!spec.capabilities.launch.model.value);
        assert_eq!(
            spec.capabilities.launch.model.reason,
            provenant_records::OVERLAY_REASON
        );
        assert_eq!(spec.capabilities.launch.model.source, OVERLAY_SOURCE);
        assert!(spec.capabilities.launch.image_input.value);
        assert_eq!(spec.capabilities.launch.effort.source, ADAPTER_ROW_SOURCE);
    }
}
