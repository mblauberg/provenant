//! Implementation planning is an explicit template choice.
use super::*;
use serde_json::json;

// Spec: docs/specs/fabric.md#implementation-planning
#[test]
fn implementation_plans_only_when_requested() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    for template in [
        "implement",
        "implement.plan",
        "implement.fix",
        "implement.issue",
    ] {
        let raw = json!({"id":"work", "use":template, "prompt":"Meet acceptance",
            "worktree":"/fixture/writer", "write_scope":["src"]});
        let direct = compiler.compile(&raw).unwrap();
        assert!(direct.composite.is_none());
        assert_eq!(direct.access, AccessMode::WorktreeWrite);
        let mut requested = raw.clone();
        requested["with"] = json!({"plan":true});
        let planned = compiler.compile(&requested).unwrap();
        let members = &planned.composite.as_ref().unwrap().nodes;
        let planner = members
            .iter()
            .find(|n| n.template.as_deref() == Some("plan"))
            .unwrap();
        let writer = members
            .iter()
            .find(|n| n.access == AccessMode::WorktreeWrite)
            .unwrap();
        assert!(writer.depends_on.contains(&planner.id));
        assert!(
            writer.inputs.iter().any(
                |input| input.node == planner.id && input.outputs.contains(&OutputKind::Result)
            )
        );
        assert!(
            planned
                .composite
                .as_ref()
                .unwrap()
                .exports
                .iter()
                .any(|export| export.node == writer.id
                    && export.outputs.contains(&OutputKind::Commit))
        );
        let mut retired = raw;
        retired["with"] = json!({"bounded":true});
        assert!(compiler.compile(&retired).is_err());
    }
}

// Spec: docs/specs/fabric.md#implementation-planning
#[test]
fn batch_slice_can_override_plan_default() {
    let library = TemplateLibrary::bundled();
    let compiler = Compiler::new(&library);
    let slices = compiler
        .compile(&json!({"id":"slices", "use":"implement", "over":[
        "direct", {"item":"planned", "with":{"plan":true}}
    ], "with":{"plan":false}, "worktree":"/fixture/writer"}))
        .unwrap();
    let members = &slices.composite.unwrap().nodes;
    assert!(
        members
            .iter()
            .find(|node| node.id == "direct")
            .unwrap()
            .composite
            .is_none()
    );
    assert!(
        members
            .iter()
            .find(|node| node.id == "planned")
            .unwrap()
            .composite
            .is_some()
    );
}

// Spec: docs/specs/fabric.md#implementation-planning
#[test]
fn configured_plan_default_is_overridable() {
    let library = TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([(
        template::IMPLEMENT_PLAN_DEFAULT.into(),
        vec!["true".into()],
    )]);
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let mut raw = json!({"id":"work", "use":"implement", "worktree":"/fixture/writer"});
    assert!(compiler.compile(&raw).unwrap().composite.is_some());
    raw["with"] = json!({"plan":false});
    assert!(compiler.compile(&raw).unwrap().composite.is_none());
}
