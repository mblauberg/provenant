//! Bundled composite topology, separate from the leaf catalogue.
use super::template::{Layer, Template, TemplateLibrary};
use std::collections::BTreeMap;

pub(super) fn install(library: &mut TemplateLibrary) -> Result<(), String> {
    let templates: BTreeMap<String, Template> =
        toml_edit::de::from_str(include_str!("../../assets/templates/composites.toml"))
            .map_err(|error| format!("bundled composites: {error}"))?;
    for (name, template) in templates {
        library.insert(Layer::Bundled, &name, template)?;
    }
    Ok(())
}
