//! Tests for `when`: a node's condition over launch values, resolved at
//! submission, and the skipped rows it leaves in the graph.

use super::*;
use serde_json::json;
use std::collections::BTreeMap;

/// The stage resolvers a former `f?-v?-s?` topology code stood for.
pub(super) fn stages(topology: &str) -> [(String, Vec<String>); 3] {
    let on = |letter: char| {
        let at = topology.find(letter).expect("topology letter") + 1;
        topology.as_bytes()[at] == b'1'
    };
    when::review_stages(on('f'), on('v'), on('s'))
}

fn resolvers(topology: &str) -> BTreeMap<String, Vec<String>> {
    let mut resolvers = BTreeMap::from([
        (
            "review-panel".to_owned(),
            vec!["correctness".to_owned(), "tests".to_owned()],
        ),
        (
            "review-verifiers".to_owned(),
            vec!["1".to_owned(), "2".to_owned()],
        ),
        ("review-advice-correctness".to_owned(), vec![String::new()]),
        ("review-advice-tests".to_owned(), vec![String::new()]),
    ]);
    resolvers.extend(stages(topology));
    resolvers
}

fn compile(
    resolvers: &BTreeMap<String, Vec<String>>,
    node: serde_json::Value,
) -> Result<GraphNode, template::CompileError> {
    let library = template::TemplateLibrary::bundled();
    template::Compiler {
        library: &library,
        resolvers,
    }
    .compile(&node)
}

fn document(nodes: Vec<GraphNode>) -> GraphDocument {
    GraphDocument {
        graph_id: "when".into(),
        schema_version: 2,
        nodes,
        ..serde_json::from_value(json!({"graph_id":"when","schema_version":2,"nodes":[]})).unwrap()
    }
}

fn submitted(topology: &str) -> GraphDocument {
    let node = compile(
        &resolvers(topology),
        json!({"id":"panel","use":"review-panel"}),
    )
    .unwrap();
    document(vec![node])
}

fn expanded(topology: &str) -> GraphDocument {
    submitted(topology)
        .expanded(&crate::config::GraphLimits::default())
        .unwrap()
}

fn active(document: &GraphDocument) -> Vec<String> {
    let mut ids: Vec<_> = document
        .nodes
        .iter()
        .filter(|node| node.when.iter().all(WhenTerm::holds))
        .filter(|node| node.composite.is_none())
        .map(|node| node.id.clone())
        .collect();
    ids.sort();
    ids
}

#[test]
fn each_review_shape_activates_the_nodes_it_always_did() {
    for topology in [
        "f0-v0-s0", "f0-v0-s1", "f0-v1-s0", "f0-v1-s1", "f1-v0-s0", "f1-v0-s1", "f1-v1-s0",
        "f1-v1-s1",
    ] {
        let mut expected = vec![
            "panel--selected--lenses--correctness".to_owned(),
            "panel--selected--lenses--tests".to_owned(),
        ];
        let flag = |letter: char| topology.contains(&format!("{letter}1"));
        if flag('f') {
            expected.push("panel--selected--frame".into());
        }
        if flag('v') {
            expected.push("panel--selected--verify--1".into());
            expected.push("panel--selected--verify--2".into());
        }
        if flag('s') {
            expected.push("panel--selected--synthesis".into());
        }
        expected.sort();
        assert_eq!(active(&expanded(topology)), expected, "{topology}");
    }
}

#[test]
fn an_unmet_condition_is_a_skipped_row_that_names_it() {
    let document = expanded("f0-v0-s0");
    let frame = document
        .nodes
        .iter()
        .find(|node| node.id == "panel--selected--frame")
        .unwrap();
    assert_eq!(
        frame.when,
        vec![WhenTerm {
            name: "review-frame".into(),
            expected: "true".into(),
            actual: "false".into(),
        }]
    );
    let submitted = submitted("f0-v0-s0");
    let (plan, _) = validation::validate_plan(&submitted).unwrap();
    let checkpoint =
        transitions::initial_checkpoint(&submitted, &plan, "digest".into(), None, None, None, None);
    let row = checkpoint
        .nodes
        .iter()
        .find(|row| row.id == "panel--selected--frame")
        .unwrap();
    assert_eq!(row.state, NodeState::Skipped);
    assert!(row.skip_reason.as_deref().unwrap().contains("review-frame"));
    let lens = checkpoint
        .nodes
        .iter()
        .find(|row| row.id == "panel--selected--lenses--tests")
        .unwrap();
    assert_eq!(lens.state, NodeState::Pending);
    assert_eq!(
        when::skipped(&document.nodes)
            .iter()
            .filter(|(id, _)| *id == "panel--selected--frame")
            .count(),
        1
    );
}

#[test]
fn a_dependent_of_a_skipped_node_keeps_its_place_without_it() {
    let (plan, _) = validation::validate_plan(&submitted("f1-v0-s1")).unwrap();
    let synthesis = plan
        .nodes
        .iter()
        .find(|node| node.id == "panel--selected--synthesis")
        .unwrap();
    assert!(
        synthesis
            .inputs
            .iter()
            .all(|input| !input.node.contains("verify"))
    );
    assert!(
        synthesis
            .dependencies()
            .all(|parent| !parent.contains("verify"))
    );
    let shape = plan
        .nodes
        .iter()
        .find(|node| node.id == "panel--selected")
        .unwrap();
    assert!(
        shape
            .composite_members
            .iter()
            .all(|member| !member.contains("verify"))
    );
    let exports: Vec<_> = shape
        .composite
        .as_ref()
        .unwrap()
        .exports
        .iter()
        .map(|export| export.node.as_str())
        .collect();
    assert_eq!(exports, ["panel--selected--synthesis"]);
}

#[test]
fn a_reference_to_a_skipped_writer_is_refused() {
    let nodes: Vec<GraphNode> = serde_json::from_value(json!([
        {"id":"w","access":"read_only","prompt":"x","when":[{"name":"on","expected":"true","actual":"false"}]},
        {"id":"r","access":"read_only","prompt":"x","depends_on":["w"],"source":{"type":"head","node":"w"}}
    ]))
    .unwrap();
    assert!(validation::validate_plan(&document(nodes)).is_err());
}

fn refusal(error: template::CompileError) -> template::ParameterRefusal {
    match error {
        template::CompileError::Parameter(refusal) => refusal,
        other => panic!("expected a typed refusal, got {other:?}"),
    }
}

#[test]
fn a_mistyped_condition_name_is_refused_with_the_name() {
    let error = compile(
        &resolvers("f0-v0-s0"),
        json!({"id":"x","use":"task","brief":"b","when":{"review-frames":true}}),
    )
    .unwrap_err();
    let refusal = refusal(error);
    assert_eq!(refusal.node.as_deref(), Some("x"));
    assert!(matches!(
        &refusal.fault,
        template::ParameterFault::When(when)
            if when.reason == template::WhenFault::Unknown
                && when.key.as_deref() == Some("review-frames")
                && when.suggestion.as_deref() == Some("review-frame")
    ));
    let wire = serde_json::to_value(&refusal).unwrap();
    assert_eq!(wire["fault"]["kind"], "when");
}

#[test]
fn a_condition_must_be_a_table_of_scalars() {
    for when in [json!(["x"]), json!(true), json!({"review-frame":[1]})] {
        let result = compile(
            &resolvers("f0-v0-s0"),
            json!({"id":"x","use":"task","brief":"b","when":when}),
        );
        // A list is the resolved form and passes through to graph decoding.
        if when.is_array() {
            assert!(result.is_err());
            continue;
        }
        assert!(matches!(
            refusal(result.unwrap_err()).fault,
            template::ParameterFault::When(when)
                if matches!(when.reason, template::WhenFault::NotTable | template::WhenFault::NotScalar)
        ));
    }
}

#[test]
fn a_condition_cannot_sit_on_a_repeat() {
    let error = compile(
        &resolvers("f0-v0-s0"),
        json!({"id":"x","use":"task","brief":"b","when":{"review-frame":true},
               "repeat":{"until":"x","max_rounds":1}}),
    )
    .unwrap_err();
    assert!(matches!(
        refusal(error).fault,
        template::ParameterFault::When(when) if when.reason == template::WhenFault::Unsupported
    ));
}

#[test]
fn a_condition_reads_a_template_parameter_before_a_launch_value() {
    let mut library = template::TemplateLibrary::bundled();
    library
        .insert_toml(
            template::Layer::Inline,
            "pair",
            r#"
doc = "Two stages, the second optional."
nodes = [
  { id = "first", use = "task", brief = "First." },
  { id = "second", use = "task", brief = "Second.", after = ["first"], when = { extra = true } },
]
[parameters.extra]
type = "boolean"
default = false
"#,
        )
        .unwrap();
    let none = BTreeMap::new();
    let compiler = template::Compiler {
        library: &library,
        resolvers: &none,
    };
    let off = compiler.compile(&json!({"id":"p","use":"pair"})).unwrap();
    let on = compiler
        .compile(&json!({"id":"p","use":"pair","with":{"extra":true}}))
        .unwrap();
    let second = |node: &GraphNode| {
        node.composite
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|member| member.id == "second")
            .unwrap()
            .when
            .iter()
            .all(WhenTerm::holds)
    };
    assert!(!second(&off));
    assert!(second(&on));
}
