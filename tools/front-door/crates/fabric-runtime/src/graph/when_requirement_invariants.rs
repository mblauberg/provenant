//! Explain and the persisted settlement must disclose the same narrowing.
use super::*;
use serde_json::json;

#[derive(Default)]
struct Journal(Option<GraphCheckpoint>);
impl GraphJournal for Journal {
    type Error = std::convert::Infallible;
    fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.0.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        // Exercise the settlement's wire shape as well as the in-memory view.
        self.0 = Some(serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap());
        Ok(())
    }
}

struct Owner;
impl AttemptOwner for Owner {
    type Error = std::convert::Infallible;
    fn actor(&self) -> String {
        "chair".into()
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        _: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        _: Option<&str>,
        _: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        assert!(!key.node_id.ends_with("skipped"));
        let mut capture = crate::result_capture::ResultCapture::new(None);
        capture.push(b"VERDICT: PASS");
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(key.node_id.clone()),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: Some(Box::new(capture.finish())),
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            stop: None,
        })
    }
}

#[test]
fn skipped_required_and_verdict_inputs_are_disclosed_through_settlement() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2, "graph_id":"narrowing", "nodes":[{
            "id":"panel", "kind":"composite", "access":"read_only", "composite":{
                "reduce_verdict":true,
                "nodes":[
                    {"id":"skipped", "access":"read_only", "prompt":"review",
                        "route":{"alias":"senior"},
                        "when":[{"name":"on","expected":"true","actual":"false"}]},
                    {"id":"kept", "access":"read_only", "prompt":"review",
                        "route":{"alias":"senior"}, "depends_on":["skipped"],
                        "inputs":[{"node":"skipped", "outputs":["result"], "required_outputs":["result"]}]}
                ],
                "exports":[{"node":"skipped", "outputs":["result","verdict"]},
                    {"node":"kept", "outputs":["result","verdict"]}]
            }
        }]
    })).unwrap();
    let limits = crate::config::GraphLimits::default();
    let rows = when::skipped_rows(&document, &limits);
    let row = rows
        .iter()
        .find(|row| row["node"] == "panel--skipped")
        .unwrap();
    let removed = row["removed_requirements"].as_array().unwrap();
    assert!(
        removed
            .iter()
            .any(|item| item.get("required_outputs") == Some(&json!(["result"])))
    );
    assert!(removed.iter().any(|item| item["verdict_input"] == true));

    let mut journal = Journal::default();
    let run = advance(&document, &mut Owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    let persisted = journal.0.unwrap();
    let skipped = persisted
        .nodes
        .iter()
        .find(|node| node.id == "panel--skipped")
        .unwrap();
    assert_eq!(skipped.state, NodeState::Skipped);
    assert_eq!(
        skipped.extra["removed_requirements"],
        row["removed_requirements"]
    );
}
