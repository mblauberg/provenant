//! Tests for linking a node ask to its inbox request.

use super::*;
use std::collections::BTreeMap;

/// What an inbox keeps of one request.
struct Stored {
    ask: AskRequest,
    answer: Option<AskReply>,
    closed: bool,
}

/// An inbox that files idempotently by graph instance and ask ID, as the host
/// does, and answers on request.
#[derive(Default)]
struct InboxOwner {
    requests: BTreeMap<String, Stored>,
}

impl InboxOwner {
    fn open(&self) -> Vec<&str> {
        self.requests
            .iter()
            .filter(|(_, stored)| stored.answer.is_none() && !stored.closed)
            .map(|(id, _)| id.as_str())
            .collect()
    }

    fn answer(&mut self, request: &str, option: &str) {
        self.requests.get_mut(request).unwrap().answer = Some(AskReply {
            option: Some(option.into()),
            text: None,
            actor: "chair".into(),
            at: None,
        });
    }
}

impl AttemptOwner for InboxOwner {
    type Error = std::convert::Infallible;
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }

    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        Some(provenant_core::WallTime::from_micros(0).unwrap())
    }
    fn actor(&self) -> String {
        "chair".into()
    }
    fn file_ask(&mut self, filing: AskFiling<'_>) -> Result<AskFiled, Self::Error> {
        let key = format!("{}/{}", filing.instance_id, filing.ask.ask_id);
        if !self.requests.contains_key(&key) {
            self.requests.insert(
                key.clone(),
                Stored {
                    ask: filing.ask.clone(),
                    answer: None,
                    closed: false,
                },
            );
        }
        let stored = &self.requests[&key];
        Ok(AskFiled {
            request: key,
            deadline: stored.ask.wake_at,
            closed: stored.closed,
        })
    }
    fn node_ask(&mut self, scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        Ok(self.requests[&scope.ask.request].answer.clone())
    }
    fn close_node_ask(&mut self, scope: AskScope<'_>) -> Result<(), Self::Error> {
        let stored = self.requests.get_mut(&scope.ask.request).unwrap();
        stored.closed = stored.answer.is_none();
        Ok(())
    }
    fn observe_or_start(
        &mut self,
        _key: &AttemptKey,
        _node: NodeInput<'_>,
        _route: &GraphRoute,
        _access: AccessMode,
        _worktree: Option<&str>,
        _replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        unreachable!("the graph has no agent nodes")
    }
}

/// A journal that fails one save, as a crash between filing and saving would.
#[derive(Default)]
struct FlakyJournal {
    saved: Option<GraphCheckpoint>,
    fail_after_ask: bool,
}

impl GraphJournal for FlakyJournal {
    type Error = std::io::Error;

    fn load(&mut self, _graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.saved.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        if self.fail_after_ask && !checkpoint.asks.is_empty() {
            self.fail_after_ask = false;
            return Err(std::io::Error::other(
                "crash after filing, before the checkpoint save",
            ));
        }
        self.saved = Some(checkpoint.clone());
        Ok(())
    }
}

fn question() -> GraphDocument {
    crate::graph::model::document_fixture(serde_json::json!({
        "graph_id":"asking", "decision_session":"chair-session", "nodes":[
            {"id":"choose", "kind":"decide", "access":"read_only", "decide":{"on_unanswered":"block_dependants",
                "question":"Ship the change?", "default":"hold",
                "options":[{"id":"ship", "label":"Ship"}, {"id":"hold", "label":"Hold"}]}}
        ]
    }))
}

#[test]
fn saving_or_replaying_a_filed_ask_preserves_its_link() {
    for fail_after_filing in [false, true] {
        let document = question();
        let mut owner = InboxOwner::default();
        let mut journal = FlakyJournal {
            fail_after_ask: fail_after_filing,
            ..Default::default()
        };
        if fail_after_filing {
            assert!(advance(&document, &mut owner, &mut journal).is_err());
            assert!(journal.saved.as_ref().unwrap().asks.is_empty());
        }

        let first = advance(&document, &mut owner, &mut journal).unwrap();
        let [link] = &first.checkpoint.asks[..] else {
            panic!("one link for the one ask");
        };
        let request = link.request.clone();
        assert_eq!(owner.open(), [request.as_str()]);

        let replayed = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(replayed.checkpoint, first.checkpoint);
        assert_eq!(replayed.checkpoint.asks[0].request, request);
    }
}

#[test]
fn an_answer_resumes_the_node_and_drops_the_link() {
    let document = question();
    let mut owner = InboxOwner::default();
    let mut journal = FlakyJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.status, GraphStatus::Pending);
    let request = waiting.checkpoint.asks[0].request.clone();

    owner.answer(&request, "ship");
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(run.checkpoint.asks.is_empty());
    let choose = &run.checkpoint.nodes[0];
    assert_eq!(
        choose.outputs.as_ref().unwrap().verdict.as_deref(),
        Some("ship")
    );
    // The inbox keeps the answered request; nothing is filed or closed again.
    assert!(!owner.requests[&request].closed);
}

#[test]
fn a_request_closed_by_a_cancel_is_not_reused_after_a_resume() {
    let document = question();
    let mut owner = InboxOwner::default();
    let mut journal = FlakyJournal::default();
    let first = advance(&document, &mut owner, &mut journal).unwrap();
    let first_ask = first.checkpoint.asks[0].ask_id.clone();
    journal.saved.as_mut().unwrap().cancelled = true;
    let cancelled = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(cancelled.status, GraphStatus::Cancelled);
    assert!(cancelled.checkpoint.asks.is_empty());
    assert!(owner.open().is_empty());

    let resumed = resume(&document, &mut owner, &mut journal).unwrap();
    let [link] = &resumed.checkpoint.asks[..] else {
        panic!("one link for the new ask");
    };
    assert_ne!(link.ask_id, first_ask);
    assert_eq!(owner.open(), [link.request.as_str()]);
}

fn retry_node(document: &GraphDocument, checkpoint: &mut GraphCheckpoint, id: &str) {
    patch::retry_node(
        &mut patch::LiveGraph {
            document,
            checkpoint,
        },
        id,
        None,
        &patch::PatchContext {
            library: &template::TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            scan: &|_| Ok(()),
            journal: None,
            limits: &plans::generous(),

            worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
        },
    )
    .unwrap();
}

pub(super) fn retried_decide_ask_ids() -> (String, String) {
    let document = question();
    let mut owner = InboxOwner::default();
    let mut journal = FlakyJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let first = waiting.checkpoint.asks[0].request.clone();
    let first_ask = waiting.checkpoint.asks[0].ask_id.clone();
    owner.answer(&first, "ship");
    let done = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);

    let mut checkpoint = done.checkpoint;
    retry_node(&document, &mut checkpoint, "choose");
    journal.saved = Some(checkpoint);
    let retried = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(retried.status, GraphStatus::Pending);
    let [link] = &retried.checkpoint.asks[..] else {
        panic!("one link for the new ask");
    };
    assert_ne!(link.request, first);
    assert_eq!(owner.open(), [link.request.as_str()]);
    (first_ask, link.ask_id.clone())
}
