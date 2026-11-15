//! Bounded briefs keep a direct writer; open briefs must be sized first.
use super::*;
use serde_json::json;

#[test]
fn implementation_templates_follow_their_declared_planning_policy() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    let limits = crate::config::GraphLimits::default();
    for (template, bounded, expects_plan) in [
        ("implement", false, true),
        ("implement", true, false),
        ("implement.plan", false, false),
        ("implement.fix", false, false),
        ("implement.issue", false, true),
    ] {
        let mut raw = json!({"id":"work", "use":template, "prompt":"Meet the acceptance criteria",
            "worktree":"/fixture/writer", "write_scope":["src"]});
        if bounded {
            raw["with"] = json!({"bounded":true});
        }
        let document = super::model::document_fixture(json!({
            "graph_id":"bounded", "nodes":[compiler.compile(&raw).unwrap()]
        }));
        let expanded = document.expanded(&limits).unwrap();
        let writer = expanded
            .nodes
            .iter()
            .find(|node| node.access == AccessMode::WorktreeWrite)
            .unwrap();
        assert_eq!(
            writer.route.as_ref().unwrap().alias.as_deref(),
            Some("junior")
        );
        assert_eq!(writer.worktree.as_deref(), Some("/fixture/writer"));
        assert_eq!(writer.write_scope, ["src"]);
        let planners: Vec<_> = expanded
            .nodes
            .iter()
            .filter(|node| node.template.as_deref() == Some("plan"))
            .collect();
        if expects_plan {
            let planner = planners.first().expect("an open brief needs a sizing plan");
            assert_eq!(planner.access, AccessMode::ReadOnly);
            assert!(writer.depends_on.contains(&planner.id));
            assert!(writer.inputs.iter().any(|input| {
                input.node == planner.id && input.outputs.contains(&OutputKind::Result)
            }));
            let boundary = expanded
                .nodes
                .iter()
                .find(|node| node.id == "work")
                .unwrap();
            assert!(
                boundary
                    .composite
                    .as_ref()
                    .unwrap()
                    .exports
                    .iter()
                    .any(|export| {
                        export.node == writer.id && export.outputs.contains(&OutputKind::Commit)
                    })
            );
        } else {
            assert!(planners.is_empty());
        }
    }
}

#[test]
fn implementation_is_bounded_only_by_a_plan_result() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    for (source, expects_composite) in [("plan", false), ("task", true)] {
        let raw = [
            json!({"id":"source", "use":source}),
            json!({"id":"work", "use":"implement", "inputs":[{"node":"source","outputs":["result"]}],
                "worktree":"/fixture/writer", "write_scope":["src"]}),
        ];
        let nodes = compiler.compile_all(&raw).unwrap();
        assert_eq!(nodes[1].composite.is_some(), expects_composite, "{source}");
    }
}

#[test]
fn implementation_plan_inference_survives_nested_and_scoped_templates() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    for (source, expects_composite) in [("plan", false), ("task", true)] {
        let raw = [
            json!({"id":"source", "use":source}),
            json!({"id":"work", "use":"implement", "inputs":[{"node":"source","outputs":["result"]}],
                "worktree":"/fixture/writer", "write_scope":["src"]}),
        ];
        for depth in 1..=3 {
            let mut group = raw[1].clone();
            for level in 0..depth {
                let child = group["id"].clone();
                group = json!({"id":format!("group-{level}"), "kind":"composite", "access":"read_only",
                    "composite":{"nodes":[group],"exports":[{"node":child,"outputs":["result"]}]}});
            }
            let outer = compiler
                .compile(
                    &json!({"id":"outer", "kind":"composite", "access":"read_only",
                    "composite":{"nodes":[raw[0],group],"exports":[]}}),
                )
                .unwrap();
            let mut member = outer.composite.unwrap().nodes.remove(1);
            for _ in 0..depth {
                member = member.composite.unwrap().nodes.remove(0);
            }
            assert_eq!(
                member.composite.is_some(),
                expects_composite,
                "{source}/{depth}"
            );
        }

        let mut scoped_library = library.clone();
        scoped_library
            .insert(
                Layer::Inline,
                "group",
                Template {
                    nodes: vec![raw[1].clone()],
                    ..Default::default()
                },
            )
            .unwrap();
        let outer = Compiler::new(&scoped_library)
            .compile(
                &json!({"id":"outer", "kind":"composite", "access":"read_only",
                "composite":{"nodes":[raw[0],{"id":"group","use":"group"}],"exports":[]}}),
            )
            .unwrap();
        assert_eq!(
            outer.composite.unwrap().nodes[1]
                .composite
                .as_ref()
                .unwrap()
                .nodes[0]
                .composite
                .is_some(),
            expects_composite,
            "{source}"
        );
    }
}

#[test]
fn a_shadowed_sibling_result_does_not_bound_an_implementation() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    let shadow = compiler
        .compile(&json!({"id":"outer", "kind":"composite", "access":"read_only",
            "composite":{"nodes":[{"id":"source","use":"plan"},
                {"id":"group","kind":"composite","access":"read_only","composite":{
                    "nodes":[{"id":"source","use":"task"},
                        {"id":"work","use":"implement","worktree":"/fixture/writer","write_scope":["src"],
                            "inputs":[{"node":"source","outputs":["result"]}]}],"exports":[]}}],"exports":[]}}))
        .unwrap();
    assert!(
        shadow.composite.unwrap().nodes[1]
            .composite
            .as_ref()
            .unwrap()
            .nodes[1]
            .composite
            .is_some()
    );
}

#[test]
fn implementation_over_slices_keep_the_writer_contract() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    let over = compiler
        .compile(
            &json!({"id":"slices", "use":"implement", "over":["one", "two"],
            "worktree":"/fixture/writer", "write_scope":["src"]}),
        )
        .unwrap();
    assert!(over.composite.unwrap().nodes.iter().all(|node| {
        node.composite.is_none() && node.route.as_ref().unwrap().alias.as_deref() == Some("junior")
    }));
}
