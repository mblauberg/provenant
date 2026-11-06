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

/// A host-selected panel of one is one provider attempt, with no synthesis.
pub(super) fn single_review(children: &mut Vec<serde_json::Value>) -> bool {
    let Some(members) = children
        .first()
        .and_then(|node| node.pointer("/composite/nodes"))
        .and_then(serde_json::Value::as_array)
        .filter(|members| members.len() == 1)
    else {
        return false;
    };
    let mut review = members[0].clone();
    review["id"] = serde_json::json!("review");
    *children = vec![review];
    true
}
