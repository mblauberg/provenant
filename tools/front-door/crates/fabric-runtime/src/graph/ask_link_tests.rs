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
    /// Calls that created a request rather than finding one.
    created: usize,
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

    fn actor(&self) -> String {
        "chair".into()
    }
    fn file_ask(&mut self, filing: AskFiling<'_>) -> Result<AskFiled, Self::Error> {
        let key = format!("{}/{}", filing.instance_id, filing.ask.ask_id);
        if !self.requests.contains_key(&key) {
            self.created += 1;
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
    saves: usize,
    fail_save: Option<usize>,
}

impl GraphJournal for FlakyJournal {
    type Error = std::io::Error;

    fn load(&mut self, _graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.saved.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.saves += 1;
        if self.fail_save == Some(self.saves) {
            return Err(std::io::Error::other("crash before the save"));
        }
        self.saved = Some(checkpoint.clone());
        Ok(())
    }
}

fn question() -> GraphDocument {
    serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":"asking", "decision_session":"chair-session", "nodes":[
            {"id":"choose", "kind":"decide", "access":"read_only", "decide":{
                "question":"Ship the change?", "default":"hold",
                "options":[{"id":"ship", "label":"Ship"}, {"id":"hold", "label":"Hold"}]}}
        ]
    }))
    .unwrap()
}

#[test]
fn saving_a_checkpoint_with_an_ask_files_one_request_and_saving_again_files_none() {
    let document = question();
    let mut owner = InboxOwner::default();
    let mut journal = FlakyJournal::default();
    let first = advance(&document, &mut owner, &mut journal).unwrap();
    let [link] = &first.checkpoint.asks[..] else {
        panic!("one link for the one ask");
    };
    assert_eq!(owner.open(), [link.request.as_str()]);
    assert_eq!(owner.created, 1);

    let again = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(again.checkpoint, first.checkpoint);
    assert_eq!(owner.created, 1);
    assert_eq!(owner.open().len(), 1);
}

#[test]
fn a_crash_after_filing_replays_to_the_same_request() {
    let document = question();
    let mut owner = InboxOwner::default();
    // The first save holds the graph identity, the second the link.
    let mut journal = FlakyJournal {
        fail_save: Some(2),
        ..Default::default()
    };
    assert!(advance(&document, &mut owner, &mut journal).is_err());
    assert_eq!(owner.created, 1);
    assert!(journal.saved.as_ref().unwrap().asks.is_empty());

    let replayed = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.created, 1, "the replay filed a second request");
    let [link] = &replayed.checkpoint.asks[..] else {
        panic!("one link for the one ask");
    };
    assert_eq!(owner.open(), [link.request.as_str()]);
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
    assert_eq!(owner.created, 1);
    assert!(!owner.requests[&request].closed);
}

#[test]
fn a_request_closed_by_a_cancel_is_not_reused_after_a_resume() {
    let document = question();
    let mut owner = InboxOwner::default();
    let mut journal = FlakyJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    journal.saved.as_mut().unwrap().cancelled = true;
    let cancelled = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(cancelled.status, GraphStatus::Cancelled);
    assert!(cancelled.checkpoint.asks.is_empty());
    assert!(owner.open().is_empty());

    let resumed = resume(&document, &mut owner, &mut journal).unwrap();
    let [link] = &resumed.checkpoint.asks[..] else {
        panic!("one link for the new ask");
    };
    assert_eq!(link.ask_id, "choose.ask-2");
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
        },
    )
    .unwrap();
}

#[test]
fn a_retried_decide_node_files_a_new_request_and_ignores_the_old_answer() {
    let document = question();
    let mut owner = InboxOwner::default();
    let mut journal = FlakyJournal::default();
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    let first = waiting.checkpoint.asks[0].request.clone();
    owner.answer(&first, "ship");
    let done = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(done.status, GraphStatus::Complete);

    let mut checkpoint = done.checkpoint;
    retry_node(&document, &mut checkpoint, "choose");
    journal.saved = Some(checkpoint);
    let retried = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        retried.status,
        GraphStatus::Pending,
        "the stale answer was consumed"
    );
    let [link] = &retried.checkpoint.asks[..] else {
        panic!("one link for the new ask");
    };
    assert_ne!(link.request, first);
    assert_eq!(owner.open(), [link.request.as_str()]);
    assert_eq!(owner.created, 2);
}
