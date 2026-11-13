//! Submission replays compiled topology without planning the same brief again.
use super::*;
use serde_json::json;

#[test]
fn submitted_implementation_preserves_its_compiled_topology_across_replays() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    let limits = plans::generous();
    for (template, bounded) in [
        ("implement", false),
        ("implement", true),
        ("implement.plan", false),
        ("implement.fix", false),
        ("implement.issue", false),
    ] {
        for depth in 0..=2 {
            let mut raw = json!({"id":"work", "use":template,
                "brief":"Meet the acceptance criteria", "worktree":"/fixture/writer",
                "write_scope":["src"]});
            if bounded {
                raw["with"] = json!({"bounded":true});
            }
            for level in 0..depth {
                let child = raw["id"].clone();
                raw = json!({"id":format!("group-{level}"), "kind":"composite",
                    "access":"read_only", "composite":{"nodes":[raw],
                    "exports":[{"node":child,"outputs":["result"]}]}});
            }
            let mut document: GraphDocument = serde_json::from_value(json!({
                "schema_version":2, "graph_id":"submitted-implementation",
                "nodes":[compiler.compile(&raw).unwrap()],
                // A node's bounded marker can differ from the run's defaults.
                "provenance":{"parameters":{"bounded":false}}
            }))
            .unwrap();
            patch::submission(&mut document, "initial", &limits).unwrap();
            let expected = document.nodes.clone();
            for replay in 0..=2 {
                patch::submission(&mut document, &format!("round-{replay}"), &limits).unwrap();
                let submitted: patch::Patch =
                    serde_json::from_value(document.extra["submission_patch"].clone()).unwrap();
                let resolvers = Default::default();
                let context = patch::PatchContext {
                    library: &library,
                    resolvers: &resolvers,
                    scan: &patch::scan_for_secrets,
                    journal: None,
                    limits: &limits,
                };
                document = patch::submit(&document.graph_id, &submitted, &context, None).unwrap();
                assert_eq!(
                    document.nodes, expected,
                    "{template}, depth {depth}, replay {replay}"
                );
            }
        }
    }
}
