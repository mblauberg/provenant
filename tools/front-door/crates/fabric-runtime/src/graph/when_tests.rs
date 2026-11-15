//! Tests for `when`: a node's condition over launch values, resolved at
//! submission, and the skipped rows it leaves in the graph.

use super::*;
use serde_json::json;
use std::collections::BTreeMap;

/// Stage values declared by the review resolver.
pub(super) fn stages(flags: [bool; 3]) -> [(String, Vec<String>); 3] {
    when::review_stages(flags[0], flags[1], flags[2])
}

fn resolvers(flags: [bool; 3]) -> BTreeMap<String, Vec<String>> {
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
    resolvers.extend(stages(flags));
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
    super::model::document_fixture(json!({"graph_id":"when","nodes":nodes}))
}

fn submitted(flags: [bool; 3]) -> GraphDocument {
    let node = compile(
        &resolvers(flags),
        json!({"id":"panel","use":"review-panel"}),
    )
    .unwrap();
    document(vec![node])
}

fn expanded(flags: [bool; 3]) -> GraphDocument {
    submitted(flags)
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
    for flags in [
        [false, false, false],
        [false, false, true],
        [false, true, false],
        [false, true, true],
        [true, false, false],
        [true, false, true],
        [true, true, false],
        [true, true, true],
    ] {
        let document = expanded(flags);
        let active = active(&document);
        for (name, values) in stages(flags) {
            let enabled = values.iter().any(|value| value == "true");
            let nodes = document
                .nodes
                .iter()
                .filter(|node| node.when.iter().any(|term| term.name == name))
                .collect::<Vec<_>>();
            assert!(!nodes.is_empty(), "{name}");
            for node in nodes {
                assert_eq!(active.contains(&node.id), enabled, "{name}");
            }
        }
    }
}

#[test]
fn an_unmet_condition_is_a_skipped_row_that_names_it() {
    let document = expanded([false, false, false]);
    let frame = document
        .nodes
        .iter()
        .find(|node| node.id == "panel--selected--frame")
        .unwrap();
    assert_eq!(
        frame.when,
        vec![WhenTerm {
            extra: Default::default(),
            name: "review-frame".into(),
            expected: "true".into(),
            actual: "false".into(),
        }]
    );
    let submitted = submitted([false, false, false]);
    let (plan, _) = validation::validate_plan(&submitted).unwrap();
    let checkpoint =
        transitions::initial_checkpoint(&submitted, &plan, "digest".into(), None, None, None);
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
    let (plan, _) = validation::validate_plan(&submitted([true, false, true])).unwrap();
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
        &resolvers([false, false, false]),
        json!({"id":"x","use":"task","prompt":"b","when":{"review-frames":true}}),
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
fn when_arrays_pass_through_to_graph_term_decoding() {
    let node = json!({"id":"x","use":"task","prompt":"b","when":["x"]});
    let library = template::TemplateLibrary::bundled();
    let resolvers = resolvers([false, false, false]);
    let compiler = template::Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    let lowered = compiler.lower(&node).unwrap();
    assert_eq!(lowered["when"], json!(["x"]));
    assert!(serde_json::from_value::<GraphNode>(lowered).is_err());
}

#[test]
fn when_tables_accept_only_scalar_values() {
    for when in [json!(true), json!({"review-frame":[1]})] {
        let result = compile(
            &resolvers([false, false, false]),
            json!({"id":"x","use":"task","prompt":"b","when":when}),
        );
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
        &resolvers([false, false, false]),
        json!({"id":"x","use":"task","prompt":"b","when":{"review-frame":true},
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

#[test]
fn a_condition_on_a_loop_is_refused_whether_it_holds_or_not() {
    for actual in ["true", "false"] {
        let document = super::model::document_fixture(json!({
            "graph_id":"when","nodes":[{
                "id":"repair","kind":"loop","access":"read_only",
                "when":[{"name":"extra","expected":"true","actual":actual}],
                "loop":{"max_rounds":2,"until":"accepted","body":[
                    {"id":"writer","access":"read_only","prompt":"repair","route":{"alias":"senior"}},
                    {"id":"accepted","kind":"gate","access":"read_only","depends_on":["writer"],
                        "inputs":[{"node":"writer","outputs":["verdict"]}],
                        "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
                ]}
            }]
        }));
        let error = when::prune(&document).unwrap_err();
        assert!(
            matches!(&error, AdmissionError::Plan(_)),
            "{actual}: {error:?}"
        );
    }
}

#[test]
fn a_skipped_stage_does_not_need_the_resolver_it_fans_out_over() {
    let mut bound = resolvers([false, false, false]);
    bound.remove("review-verifiers");
    let node = compile(&bound, json!({"id":"panel","use":"review-panel"}))
        .expect("the verify stage is skipped, so its verifiers are not needed");
    assert!(node.composite.is_some());

    let mut bound = resolvers([false, true, false]);
    bound.remove("review-verifiers");
    let error = compile(&bound, json!({"id":"panel","use":"review-panel"})).unwrap_err();
    assert!(matches!(
        error,
        template::CompileError::Parameter(refusal)
            if matches!(refusal.fault, template::ParameterFault::When(when)
                if when.reason == template::WhenFault::Unknown
                    && when.key.as_deref() == Some("review-verifiers"))
    ));
}
