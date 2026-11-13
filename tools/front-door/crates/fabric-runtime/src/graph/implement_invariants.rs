//! Bounded briefs keep a direct writer; open briefs must be sized first.
use super::*;
use serde_json::json;

#[test]
fn implementation_is_planned_unless_its_brief_is_already_bounded() {
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
        let mut raw = json!({"id":"work", "use":template, "brief":"Meet the acceptance criteria",
            "worktree":"/fixture/writer", "write_scope":["src"]});
        if bounded {
            raw["with"] = json!({"bounded":true});
        }
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1, "graph_id":"bounded", "nodes":[compiler.compile(&raw).unwrap()]
        }))
        .unwrap();
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
            .filter(|node| {
                node.extra
                    .get(template::TEMPLATE_FIELD)
                    .and_then(serde_json::Value::as_str)
                    == Some("plan")
            })
            .collect();
        if expects_plan {
            let planner = planners.first().expect("an open brief needs a sizing plan");
            assert_eq!(planner.access, AccessMode::ReadOnly);
            assert!(writer.depends_on.contains(&planner.id));
            assert!(writer.inputs.iter().any(
                |input| input.node == planner.id && input.outputs.contains(&OutputKind::Result)
            ));
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
                    .any(|export| export.node == writer.id
                        && export.outputs.contains(&OutputKind::Commit))
            );
        } else {
            assert!(planners.is_empty());
        }
    }
    // A plan result bounds the consumer; an arbitrary task's result does not.
    for source in ["plan", "task"] {
        let raw = [
            json!({"id":"source", "use":source}),
            json!({"id":"work", "use":"implement", "inputs":[{"node":"source","outputs":["result"]}],
                "worktree":"/fixture/writer", "write_scope":["src"]}),
        ];
        let nodes = compiler.compile_all(&raw).unwrap();
        assert_eq!(nodes[1].composite.is_some(), source != "plan");
        // A raw composite must use the same sibling inference as a template.
        let nested = compiler
            .compile(
                &json!({"id":"outer", "kind":"composite", "access":"read_only",
            "composite":{"nodes":raw, "exports":[{"node":"work","outputs":["result"]}]}}),
            )
            .unwrap();
        assert_eq!(
            nested.composite.unwrap().nodes[1].composite.is_some(),
            source != "plan"
        );
        // The consumer may sit below several composites while its producer
        // stays visible in the enclosing scope.
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
            assert_eq!(member.composite.is_some(), source != "plan");
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
            source != "plan"
        );
    }
    let shadow = compiler.compile(&json!({"id":"outer", "kind":"composite", "access":"read_only",
        "composite":{"nodes":[{"id":"source","use":"plan"},
            {"id":"group","kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":"source","use":"task"},
                    {"id":"work","use":"implement","worktree":"/fixture/writer","write_scope":["src"],
                        "inputs":[{"node":"source","outputs":["result"]}]}],"exports":[]}}],"exports":[]}})).unwrap();
    assert!(
        shadow.composite.unwrap().nodes[1]
            .composite
            .as_ref()
            .unwrap()
            .nodes[1]
            .composite
            .is_some()
    );
    let over = compiler
        .compile(
            &json!({"id":"slices", "use":"implement", "over":["one", "two"],
        "worktree":"/fixture/writer", "write_scope":["src"]}),
        )
        .unwrap();
    assert!(
        over.composite
            .unwrap()
            .nodes
            .iter()
            .all(|node| node.composite.is_none()
                && node.route.as_ref().unwrap().alias.as_deref() == Some("junior"))
    );
}
