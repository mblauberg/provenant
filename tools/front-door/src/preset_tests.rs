use super::preset::{Layers, expand};
use serde_json::json;

fn nodes(name: &str) -> serde_json::Value {
    expand(&Layers::empty(), name, "brief", &[])
        .unwrap()
        .document["nodes"]
        .clone()
}

#[test]
fn bundled_expansions_match_golden_graphs() {
    assert_eq!(
        nodes("single"),
        json!([
            {"id":"task", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]}
        ])
    );
    assert_eq!(
        nodes("chain"),
        json!([
            {"id":"first", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"second", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":["first"], "inputs":[{"node":"first","outputs":["result"]}]}
        ])
    );
    assert_eq!(
        nodes("fan-out"),
        json!([
            {"id":"branch-1", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"branch-2", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]}
        ])
    );
    assert_eq!(
        nodes("fan-out-synthesise"),
        json!([
            {"id":"branch-1", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"branch-2", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":[]},
            {"id":"synthesis", "kind":"synthesise", "access":"read_only", "prompt":"brief", "route":{"alias":"senior"}, "depends_on":["branch-1","branch-2"], "inputs":[{"node":"branch-1","outputs":["result"]},{"node":"branch-2","outputs":["result"]}]}
        ])
    );
    for name in ["single", "chain", "fan-out", "fan-out-synthesise"] {
        let first = expand(&Layers::empty(), name, "brief", &[]).unwrap();
        let second = expand(&Layers::empty(), name, "brief", &[]).unwrap();
        assert_eq!(first.document, second.document);
        let _: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(first.document).unwrap();
    }
}

#[test]
fn layering_composition_and_refusals() {
    let mut layers = Layers::empty();
    layers
        .insert("user", "single", "[parameters]\nroute = 'utility'\n")
        .unwrap();
    assert_eq!(
        layers.resolve("single").unwrap().1["parameters.route"],
        "user"
    );
    layers
        .insert("repo", "single", "[parameters]\nroute = 'senior'\n")
        .unwrap();
    assert_eq!(
        layers.resolve("single").unwrap().1["parameters.route"],
        "repo"
    );
    let result = expand(
        &layers,
        "single",
        "brief",
        &["parameters.route=\"fast\"".into()],
    )
    .unwrap();
    assert_eq!(result.document["nodes"][0]["route"]["alias"], "fast");
    assert_eq!(result.origins["parameters.route"], "--set");
    layers
        .insert(
            "repo",
            "chain",
            "[slots.first]\npreset = 'single'\n[slots.first.parameters]\nroute = 'utility'\n",
        )
        .unwrap();
    let result = expand(&layers, "chain", "brief", &[]).unwrap();
    assert_eq!(result.document["nodes"][0]["id"], "first-task");
    assert_eq!(result.document["nodes"][0]["route"]["alias"], "utility");
    let result = expand(
        &layers,
        "chain",
        "brief",
        &["slots.first.parameters.route='fast'".into()],
    )
    .unwrap();
    assert_eq!(result.document["nodes"][0]["route"]["alias"], "fast");
    assert_eq!(result.origins["expanded.first.parameters.route"], "--set");
    assert!(
        expand(&layers, "single", "brief", &["parameters.nope=1".into()])
            .unwrap_err()
            .contains("unknown")
    );
    assert!(
        expand(
            &layers,
            "chain",
            "brief",
            &["slots.nope.preset=\"single\"".into()]
        )
        .unwrap_err()
        .contains("unknown")
    );
    layers
        .insert("repo", "single", "[slots.task]\npreset = 'single'\n")
        .unwrap();
    assert!(
        expand(&layers, "single", "brief", &[])
            .unwrap_err()
            .contains("cycle")
    );
}

#[test]
fn named_presets_inherit_and_cycles_are_refused() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "user",
            "mine",
            "extends = 'fan-out'\n[parameters]\nwidth = 3\n",
        )
        .unwrap();
    let expanded = expand(&layers, "mine", "brief", &[]).unwrap();
    assert_eq!(expanded.document["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(expanded.origins["parameters.width"], "user");
    layers
        .insert("repo", "mine", "extends = 'other'\n")
        .unwrap();
    layers
        .insert("repo", "other", "extends = 'mine'\n")
        .unwrap();
    assert!(layers.resolve("mine").unwrap_err().contains("cycle"));
    let mut deep = Layers::empty();
    for index in 0..9 {
        let parent = if index == 0 {
            "single".to_owned()
        } else {
            format!("level-{}", index - 1)
        };
        deep.insert(
            "repo",
            &format!("level-{index}"),
            &format!("extends = '{parent}'\n"),
        )
        .unwrap();
    }
    assert!(deep.resolve("level-8").unwrap_err().contains("depth"));
}

#[test]
fn graph_publication_is_private_and_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let graphs = temp.path().join("graphs");
    std::fs::create_dir(&graphs)?;
    std::fs::set_permissions(&graphs, std::fs::Permissions::from_mode(0o700))?;
    let graph = expand(&Layers::empty(), "single", "brief", &[])?.document;
    let first = super::fabric::publish_preset_graph(temp.path(), &graph)?;
    let second = super::fabric::publish_preset_graph(temp.path(), &graph)?;
    assert_eq!(first, second);
    assert_eq!(std::fs::read(&first)?, serde_json::to_vec(&graph)?);
    assert_eq!(std::fs::metadata(&first)?.permissions().mode() & 0o077, 0);
    std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o644))?;
    assert!(super::fabric::publish_preset_graph(temp.path(), &graph).is_err());
    Ok(())
}
