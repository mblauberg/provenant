#[cfg(test)]
#[allow(unused_imports, dead_code)] // Included by the custom worktree driver without a test harness.
mod graph_writer_admission_tests {
    use super::*;
    use provenant_confinement::worktrees::WorktreeError;
    use provenant_os::fs::FsError;
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn registry_lock_timeout_is_retryable_but_writer_refusals_are_not() {
        let lock = |path: &str| WorktreeError::Filesystem(FsError::LockTimeout {
            path: PathBuf::from(path),
            timeout: Duration::from_secs(5),
        });
        assert!(matches!(
            graph_writer_initialisation_error("head", WorktreeError::RegistryLockTimeout),
            FacadeError::WriterLockBusy
        ));
        assert!(matches!(
            graph_writer_initialisation_error("head", WorktreeError::LockBusy),
            FacadeError::WriterLockBusy
        ));
        assert!(matches!(
            graph_writer_initialisation_error("head", WorktreeError::UnknownWriter),
            FacadeError::WriterUnknown
        ));
        for error in [
            lock("writer.lock"),
            WorktreeError::LiveWriter,
            WorktreeError::WriterBusy {
                holder: Some(provenant_core::AttemptId::from_bytes([7; 16])),
            },
            WorktreeError::Dirty(String::new()),
        ] {
            assert!(matches!(
                graph_writer_initialisation_error("head", error),
                FacadeError::DispatchRefused { .. }
            ));
        }
    }

    #[derive(Default)]
    struct MockWriterOwner {
        errors: std::collections::VecDeque<WorktreeError>,
        calls: usize,
        now: Option<provenant_core::WallTime>,
    }

    impl provenant_fabric_runtime::graph::AttemptOwner for MockWriterOwner {
        type Error = FacadeError;

        fn actor(&self) -> String {
            "fixture".to_string()
        }

        fn wall_now(&self) -> Option<provenant_core::WallTime> {
            self.now
        }

        fn admission_timeout_seconds(&self) -> Option<u64> {
            Some(3600)
        }

        fn observe_or_start(
            &mut self,
            _key: &provenant_fabric_runtime::graph::AttemptKey,
            _node: provenant_fabric_runtime::graph::NodeInput<'_>,
            _route: &provenant_fabric_runtime::graph::GraphRoute,
            _access: provenant_fabric_runtime::graph::AccessMode,
            _worktree: Option<&str>,
            _replay: provenant_fabric_runtime::graph::Replay<'_>,
        ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, FacadeError> {
            self.calls += 1;
            if let Some(err) = self.errors.pop_front() {
                Err(graph_writer_initialisation_error("HEAD", err))
            } else {
                Ok(
                    provenant_fabric_runtime::graph::AttemptObservation::Terminal {
                        attempt_ref: None,
                        outcome:
                            provenant_fabric_runtime::graph::AttemptOutcome::Succeeded,
                        launched: true,
                        result: None,
                        result_warning: None,
                        result_path: None,
                        branch: None,
                        commit: None,
                    },
                )
            }
        }
    }

    #[derive(Default)]
    struct MemoryJournal(Option<provenant_fabric_runtime::graph::GraphCheckpoint>);

    impl provenant_fabric_runtime::graph::GraphJournal for MemoryJournal {
        type Error = std::convert::Infallible;

        fn load(
            &mut self,
            _graph_id: &str,
        ) -> Result<Option<provenant_fabric_runtime::graph::GraphCheckpoint>, Self::Error> {
            Ok(self.0.clone())
        }
        fn save(
            &mut self,
            checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
        ) -> Result<(), Self::Error> {
            self.0 = Some(checkpoint.clone());
            Ok(())
        }
    }

    fn advance<R: provenant_fabric_runtime::graph::AttemptOwner, J: provenant_fabric_runtime::graph::GraphJournal>(
        document: &provenant_fabric_runtime::graph::GraphDocument,
        owner: &mut R,
        journal: &mut J,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, provenant_fabric_runtime::graph::GraphError<R::Error, J::Error>> {
        provenant_fabric_runtime::graph::advance_with_budget(
            document,
            owner,
            journal,
            &provenant_fabric_runtime::config::GraphLimits::default(),
            &provenant_fabric_runtime::config::GraphBudget::default(),
        )
    }

    #[test]
    fn graph_writer_launch_retries_lock_timeout_and_runs_not_blocked() {
        use provenant_fabric_runtime::graph::{GraphDocument, NodeState};
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": "test-writer-lock",
            "nodes": [{
                "id": "writer-node",
                "role": "plan",
                "access": "worktree_write",
                "worktree": "lane",
                "prompt": "write code",
                "route": {
                    "adapter": "mock",
                    "model": "mock-model"
                }
            }]
        }))
        .unwrap();

        let lock_timeout = WorktreeError::LockBusy;

        let mut owner = MockWriterOwner {
            errors: std::collections::VecDeque::from([lock_timeout]),
            calls: 0,
            now: Some(provenant_core::WallTime::from_micros(0).unwrap()),
        };
        let mut journal = MemoryJournal::default();

        // First advance: launch encounters a held writer lock.
        let first = advance(&document, &mut owner, &mut journal).unwrap();
        // The node must be retryable Running, NOT Blocked!
        assert_eq!(first.checkpoint.nodes[0].state, NodeState::Running);
        let retry = first.checkpoint.nodes[0]
            .launch_retry
            .as_ref()
            .expect("launch retry must be recorded");
        assert_eq!(retry.failures, 1);
        assert!(retry.deadline.is_some());
        assert!(retry.wake_at.is_some());

        // Advance at wake_at: lock has freed, node runs and completes
        owner.now = retry.wake_at;
        let second = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(second.checkpoint.nodes[0].state, NodeState::Succeeded);
        assert_eq!(owner.calls, 2);
    }

    #[test]
    fn real_graph_writer_retries_registry_lock_and_launches() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::worktrees::{self, WorktreeCreate, WorkerObserver, WorkerState};
        use provenant_fabric_runtime::graph::{self, NodeState};
        use provenant_os::fs::{Directory, LockMode};
        if super::peer_tests::host_test("fabric::graph_writer_admission_tests::real_graph_writer_retries_registry_lock_and_launches")? { return Ok(()); }
        struct Free;
        impl WorkerObserver for Free {
            fn observe_roots(&self, roots: &[PathBuf]) -> std::collections::BTreeMap<PathBuf, WorkerState> {
                roots.iter().cloned().map(|root| (root, WorkerState::Free)).collect()
            }
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        for args in [vec!["config", "user.name", "Fixture"], vec!["config", "user.email", "fixture@example.invalid"], vec!["commit", "--allow-empty", "-qm", "base"]] {
            assert_eq!(crate::worktree_tests::support::code(&rig.execute(&git, &args, None)?), 0);
        }
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots.clone(), &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &Free,
        };
        let cancel = CancelToken::new();
        let (info, registry) = worktrees::with_context(&roots, &rig.home, &rig.project, &factory.clock, &cancel, |ctx| {
            let registry = roots.state.as_path().join("worktrees").join(ctx.binding.project_id().to_string());
            let info = worktrees::create(&worktrees::WorktreeContext { workers: &Free, ..*ctx }, WorktreeCreate { branch: "graph-lane".into(), base: Some("HEAD".into()) }, &cancel)?;
            Ok((info, registry))
        })?;
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"real-graph-lock", "nodes":[{
                "id":"writer", "role":"plan", "access":"worktree_write", "worktree":info.path,
                "source":{"type":"commit", "commit":info.head.to_string()}, "prompt":"write code",
                "route":{"adapter":"fixture", "model":"fixture/model"}
            }]
        }))?;
        let budget = WaitBudget::cli(factory.clock.mono_now(), 60)?;
        let mut owner = BoundGraphOwner { factory: &factory, integration_base: None, integration_branch: None, network: None, capability_overrides: Default::default(), exclude: Vec::new(), budget, cancel: &cancel, prices: None, tick: false };
        let mut journal = MemoryJournal::default();
        let directory = Directory::open_anchor(&registry)?;
        let guard = directory.lock(std::ffi::OsStr::new("worktrees.lock"), LockMode::Exclusive, budget, &cancel, &factory.clock)?;
        let first = advance(&document, &mut owner, &mut journal)?;
        assert_eq!(first.checkpoint.nodes[0].state, NodeState::Running);
        let wake = first.checkpoint.nodes[0].launch_retry.as_ref().and_then(|retry| retry.wake_at).ok_or("retry wake")?;
        drop(guard);
        let now = factory.clock.wall_now()?;
        let delay = u64::try_from(wake.as_micros().saturating_sub(now.as_micros()))?;
        factory.clock.wait_until(factory.clock.mono_now().checked_add(provenant_core::DurationMicros::from_micros(delay))?)?;
        let second = advance(&document, &mut owner, &mut journal)?;
        assert_ne!(second.checkpoint.nodes[0].state, NodeState::Blocked, "{:?}", second.checkpoint.nodes[0].advance_error);
        assert!(second.checkpoint.nodes[0].attempts[0].attempt_ref.is_some(), "graph must submit the writer");
        Ok(())
    }

    #[test]
    fn graph_writer_launch_fails_closed_on_permanent_refusals() {
        use provenant_fabric_runtime::graph::{GraphDocument, NodeState};
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": "test-writer-refusal",
            "nodes": [{
                "id": "writer-node",
                "role": "plan",
                "access": "worktree_write",
                "worktree": "lane",
                "prompt": "write code",
                "route": {
                    "adapter": "mock",
                    "model": "mock-model"
                }
            }]
        }))
        .unwrap();

        for non_transient in [
            WorktreeError::LiveWriter,
            WorktreeError::Dirty("file.txt".into()),
            WorktreeError::WriterBusy {
                holder: Some(provenant_core::AttemptId::from_bytes([7; 16])),
            },
        ] {
            let mut owner = MockWriterOwner {
                errors: std::collections::VecDeque::from([non_transient]),
                calls: 0,
                now: Some(provenant_core::WallTime::from_micros(0).unwrap()),
            };
            let mut journal = MemoryJournal::default();

            // Permanent refusal blocks the node immediately
            let result = advance(&document, &mut owner, &mut journal).unwrap();
            assert_eq!(result.checkpoint.nodes[0].state, NodeState::Blocked);
            assert!(result.checkpoint.nodes[0].launch_retry.is_none());
            assert_eq!(owner.calls, 1);
        }
    }
}

#[cfg(test)]
mod graph_review_tests {
    #[test]
    fn expanded_review_roles_include_lenses_verification_and_synthesis()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::{config::GraphLimits, graph::GraphDocument};
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "review-rows",
            "nodes": [{
                "id": "council", "kind": "composite", "access": "read_only",
                "composite": {
                    "nodes": [
                        {"id":"subject", "route":{"alias":"junior"}, "role":"plan.review", "access":"read_only", "prompt":"Frame"},
                        {"id":"lens", "route":{"alias":"junior"}, "role":"lens.correctness-spec", "access":"read_only", "prompt":"Review"},
                        {"id":"verify", "route":{"alias":"junior"}, "role":"review.verify", "access":"read_only", "prompt":"Verify"},
                        {"id":"synthesis", "route":{"alias":"junior"}, "role":"review.synthesis", "access":"read_only", "prompt":"Reconcile"},
                        {"id":"other", "route":{"alias":"junior"}, "role":"plan", "access":"read_only", "prompt":"Plan"},
                        {"id":"writer", "route":{"alias":"junior"}, "role":"review.verify", "access":"worktree_write", "worktree":"lane", "prompt":"Write"}
                    ],
                    "exports": [{"node":"synthesis","outputs":["result"]}]
                }
            }]
        }))?;
        let expanded = document.expanded(&GraphLimits::default())?;
        for suffix in ["lens", "verify", "synthesis"] {
            assert!(super::graph_review_node(
                &expanded,
                &format!("council--{suffix}")
            ));
        }
        for suffix in ["subject", "other", "writer"] {
            assert!(!super::graph_review_node(
                &expanded,
                &format!("council--{suffix}")
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod graph_ask_inbox_tests {
    use super::*;
    use provenant_fabric_runtime::{
        graph::{AskScope, LoopDecision, loop_ask},
        inbox::{InboxFilter, InboxInput, InboxResult, InboxService},
    };
    use provenant_records::inbox::{
        AnswerAction, AnswerDeliveryState, InteractionChannel, RequestState,
    };

    #[test]
    fn graph_ask_inbox_replays_requests_and_acknowledged_answers()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let host = provenant_core::HostId::from_bytes([1; 16]);
        let identity = Identity {
            project: provenant_core::ProjectId::from_bytes([2; 16]),
            host,
            inbox: provenant_fabric_runtime::identity::Address::new(host, "chair")?,
            client: "test".into(),
            cwd: provenant_records::AbsolutePath::new(&base)?,
            common_dir: provenant_records::AbsolutePath::new(&base)?,
            federation: None,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let service = InboxService::open(&roots.state, &identity, &clock, &cancel)?;
        for (index, (option, text, expected)) in [
            (Some("stop"), None, Some(LoopDecision::Stop)),
            (Some("accept"), None, Some(LoopDecision::Accept)),
            (
                Some("extend"),
                None,
                Some(LoopDecision::Extend { rounds: 1 }),
            ),
            (
                None,
                Some("{\"decision\":\"extend\",\"rounds\":3}"),
                Some(LoopDecision::Extend { rounds: 3 }),
            ),
            (None, Some("extend by two"), None),
        ]
        .into_iter()
        .enumerate()
        {
            let mut ask = loop_ask("fixture", "review", index + 1, "chair-session");
            if index == 2 {
                // A deadline and default reach the inbox as its deadline and recommendation.
                ask.default = Some("stop".into());
                ask.wake_at = Some(
                    clock
                        .wall_now()?
                        .checked_add(provenant_core::DurationMicros::from_seconds(3600)?)?,
                );
            }
            let actor = identity.inbox.actor.to_string();
            let scope = AskScope {
                graph_id: "fixture",
                instance_id: "instance-one",
                actor: &actor,
                ask: &ask,
            };
            assert_eq!(graph_ask_poll(&service, &scope, &cancel)?, None);
            let InboxResult::Saved { receipt } =
                service.execute(graph_ask_input(&scope)?, &cancel)?
            else {
                return Err("missing ask receipt".into());
            };
            service.execute(
                InboxInput::Answer {
                    extra: Default::default(),
                    id: receipt.request_id,
                    action: if option.is_some() {
                        AnswerAction::Choose
                    } else {
                        AnswerAction::Reply
                    },
                    option_id: option.map(String::from),
                    text: text.map(String::from),
                    expect_revision: receipt.revision,
                    channel: InteractionChannel::Cli,
                    operation_id: None,
                },
                &cancel,
            )?;
            for _ in 0..2 {
                let reply = graph_ask_poll(&service, &scope, &cancel)?.ok_or("missing answer")?;
                assert_eq!(
                    (reply.option.as_deref(), reply.text.as_deref()),
                    (option, text)
                );
                assert_eq!(reply.actor, actor);
                assert!(reply.at.is_some());
                assert_eq!(
                    LoopDecision::parse(reply.option.as_deref(), reply.text.as_deref()).ok(),
                    expected
                );
            }
            let InboxResult::Item { item, .. } = service.execute(
                InboxInput::List {
                    extra: Default::default(),
                    id: Some(receipt.request_id),
                    session: None,
                    state: InboxFilter::All,
                    limit: 1,
                    cursor: None,
                },
                &cancel,
            )?
            else {
                return Err("missing answered request".into());
            };
            assert_eq!(
                item.delivery.ok_or("missing delivery")?.state,
                AnswerDeliveryState::Acknowledged
            );
            assert_eq!(item.request.chair_actor, identity.inbox.actor);
            assert_eq!(item.request.chair_session_id, ask.session);
            assert_eq!(item.request.request.title, ask.title);
            assert_eq!(item.request.request.recommended_option_id, ask.default);
            assert_eq!(item.request.request.deadline, ask.wake_at);
            if index == 1 {
                let fresh = AskScope {
                    instance_id: "instance-two",
                    ..scope
                };
                assert_eq!(
                    graph_ask_poll(&service, &fresh, &cancel)?,
                    None,
                    "same document recreated state must ask again"
                );
                graph_ask_close(&service, &fresh, identity.inbox.actor, &cancel)?;
            }
            if index == 0 {
                service.execute(
                    InboxInput::Close {
                        extra: Default::default(),
                        id: receipt.request_id,
                        state: RequestState::Superseded,
                        reason: "Replace the decision".into(),
                        expect_revision: item.request.revision,
                        operation_id: None,
                    },
                    &cancel,
                )?;
                assert_eq!(
                    graph_ask_poll(&service, &scope, &cancel)?,
                    None,
                    "a superseded request reused its historical answer"
                );
            }
            graph_ask_close(&service, &scope, identity.inbox.actor, &cancel)?;
            graph_ask_close(&service, &scope, identity.inbox.actor, &cancel)?;
            assert_eq!(graph_ask_poll(&service, &scope, &cancel)?, None);
        }
        let InboxResult::Listed { items, .. } = service.execute(
            InboxInput::List {
                extra: Default::default(),
                id: None,
                session: Some("chair-session".into()),
                state: InboxFilter::All,
                limit: 50,
                cursor: None,
            },
            &cancel,
        )?
        else {
            return Err("missing inbox listing".into());
        };
        assert_eq!(
            items.len(),
            6,
            "replay or cleanup created duplicate decisions"
        );
        assert!(
            items
                .iter()
                .all(|item| item.state == RequestState::Superseded)
        );
        Ok(())
    }

    /// A graph owner whose asks go through a real inbox and which runs no agents.
    struct InboxOwner<'a> {
        service: &'a InboxService<'a>,
        clock: &'a SystemClock,
        cancel: &'a CancelToken,
        actor: String,
    }

    impl InboxOwner<'_> {
        fn budget(&self) -> Result<WaitBudget, FacadeError> {
            WaitBudget::cli(self.clock.mono_now(), 5).map_err(|_| FacadeError::Busy)
        }
    }

    impl provenant_fabric_runtime::graph::AttemptOwner for InboxOwner<'_> {
        type Error = FacadeError;

        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn node_ask(
            &mut self,
            scope: AskScope<'_>,
        ) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, FacadeError> {
            graph_ask_poll(self.service, &scope, self.cancel)
        }
        fn close_node_ask(&mut self, scope: AskScope<'_>) -> Result<(), FacadeError> {
            let actor = scope.actor.parse().map_err(|_| FacadeError::StateCorrupt)?;
            graph_ask_close(self.service, &scope, actor, self.cancel)
        }
        fn observe_or_start(
            &mut self,
            _key: &provenant_fabric_runtime::graph::AttemptKey,
            _node: provenant_fabric_runtime::graph::NodeInput<'_>,
            _route: &provenant_fabric_runtime::graph::GraphRoute,
            _access: provenant_fabric_runtime::graph::AccessMode,
            _worktree: Option<&str>,
            _replay: provenant_fabric_runtime::graph::Replay<'_>,
        ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, FacadeError> {
            Err(FacadeError::OwnerUnavailable)
        }
    }

    #[derive(Default)]
    struct MemoryJournal(Option<provenant_fabric_runtime::graph::GraphCheckpoint>);

    impl provenant_fabric_runtime::graph::GraphJournal for MemoryJournal {
        type Error = std::convert::Infallible;

        fn load(
            &mut self,
            _graph_id: &str,
        ) -> Result<Option<provenant_fabric_runtime::graph::GraphCheckpoint>, Self::Error> {
            Ok(self.0.clone())
        }
        fn save(
            &mut self,
            checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
        ) -> Result<(), Self::Error> {
            self.0 = Some(checkpoint.clone());
            Ok(())
        }
    }

    #[test]
    fn graph_decide_asks_stay_fresh_across_repeated_cancel_and_resume()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphDocument, GraphStatus, NodeState, advance_with_budget, resume_with_budget,
        };
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let host = provenant_core::HostId::from_bytes([1; 16]);
        let identity = Identity {
            project: provenant_core::ProjectId::from_bytes([2; 16]),
            host,
            inbox: provenant_fabric_runtime::identity::Address::new(host, "chair")?,
            client: "test".into(),
            cwd: provenant_records::AbsolutePath::new(&base)?,
            common_dir: provenant_records::AbsolutePath::new(&base)?,
            federation: None,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let service = InboxService::open(&roots.state, &identity, &clock, &cancel)?;
        let mut owner = InboxOwner {
            service: &service,
            clock: &clock,
            cancel: &cancel,
            actor: identity.inbox.actor.to_string(),
        };
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "decide-cancel", "decision_session": "chair-session",
            "nodes": [{"id": "choose", "kind": "decide", "access": "read_only", "decide": {
                "question": "Ship the change?",
                "options": [{"id": "ship", "label": "Ship"}, {"id": "hold", "label": "Hold"}]}}]
        }))?;
        let mut journal = MemoryJournal::default();
        let limits = Default::default();
        let graph_budget = Default::default();
        advance_with_budget(&document, &mut owner, &mut journal, &limits, &graph_budget)?;
        for _ in 0..2 {
            journal.0.as_mut().ok_or("missing checkpoint")?.cancelled = true;
            let cancelled =
                advance_with_budget(&document, &mut owner, &mut journal, &limits, &graph_budget)?;
            assert_eq!(cancelled.status, GraphStatus::Cancelled);
            resume_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &limits,
                &graph_budget,
                &[],
            )?;
        }
        let checkpoint = journal.0.as_ref().ok_or("missing checkpoint")?;
        assert_eq!(checkpoint.asks[0].ask_id, "choose.ask-3");
        assert_eq!(checkpoint.asks[0].inbox_error, None);
        let InboxResult::Listed { items, .. } = service.execute(
            InboxInput::List {
                extra: Default::default(),
                id: None,
                session: Some("chair-session".into()),
                state: InboxFilter::All,
                limit: 50,
                cursor: None,
            },
            &cancel,
        )?
        else {
            return Err("missing inbox listing".into());
        };
        // Each resume raised a new request; the third is not a replay of the second.
        assert_eq!(items.len(), 3);
        let open: Vec<_> = items
            .iter()
            .filter(|item| item.state == RequestState::Open)
            .collect();
        assert_eq!(open.len(), 1);
        service.execute(
            InboxInput::Answer {
                extra: Default::default(),
                id: open[0].id,
                action: AnswerAction::Choose,
                option_id: Some("ship".into()),
                text: None,
                expect_revision: open[0].revision,
                channel: InteractionChannel::Cli,
                operation_id: None,
            },
            &cancel,
        )?;
        let run = advance_with_budget(&document, &mut owner, &mut journal, &limits, &graph_budget)?;
        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
        let verdict = run.checkpoint.nodes[0]
            .outputs
            .as_ref()
            .and_then(|o| o.verdict.as_deref());
        assert_eq!(verdict, Some("ship"));
        Ok(())
    }


}

#[cfg(test)]
mod result_tests {
    #[allow(dead_code)] // The custom worktree driver includes this module without libtest.
    fn graph_status_fixture() -> provenant_fabric_runtime::graph::GraphRun {
        let checkpoint = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "fixture", "plan_digest": "digest",
            "nodes": [
                {"id":"ordinary", "state":"failed", "worktree":null,
                 "advance_error":"repair this", "attempts":[
                    {"launched":true,"key":{"graph_id":"fixture","node_id":"ordinary","route_index":0},
                     "route":{"alias":"first"},"state":"terminal","attempt_ref":"old","outcome":"task_failed","launch_actor":"actor"},
                    {"launched":true,"key":{"graph_id":"fixture","node_id":"ordinary","route_index":1},
                     "route":{"adapter":"codex","model":"latest","effort":"high"},"state":"terminal","attempt_ref":"new","outcome":"task_failed","launch_actor":"actor"}]},
                {"id":"composite", "state":"running", "worktree":null, "children":["member"], "attempts":[]},
                {"id":"member", "state":"running", "worktree":null, "attempts":[],
                 "ci":{"state":"pending","summary":"waiting","head_sha":"abc","check_names":["build"],"elapsed_seconds":12}},
                {"id":"loop-member", "state":"pending", "worktree":null, "attempts":[],
                 "round":{"loop_id":"loop","round":1,"node_id":"member","until":false,"previous_gate":null}}
            ]
        })).unwrap();
        super::graph_snapshot(checkpoint)
    }

    #[test]
    fn graph_status_waits_match_json_and_human() {
        let run = graph_status_fixture();
        let wire = serde_json::to_value(&run).unwrap();
        let waits = wire["waits"].as_array().unwrap();
        let ci = waits.iter().find(|wait| wait["node"] == "member").unwrap();
        assert_eq!(ci["wait"], "ci");
        assert_eq!(ci["disposition"], "running");
        let human = super::graph_human(&run);
        let row = human.lines().find(|line| line.starts_with("  member:")).unwrap();
        for part in ["wait=ci", "disposition=running", "code=unavailable", "current_attempt=unavailable", "failures=\"unavailable\"", "wake_at=unavailable", "next=unavailable"] {
            assert!(row.contains(part), "{row}: missing {part}");
        }
    }

    #[test]
    fn node_review_base_uses_its_worktree_not_the_invoking_checkout() -> Result<(), Box<dyn std::error::Error>> {
        if super::peer_tests::host_test("fabric::result_tests::node_review_base_uses_its_worktree_not_the_invoking_checkout")? {
            return Ok(());
        }
        use super::*;
        use provenant_fabric_runtime::graph::AttemptOwner;
        struct FreeWorkers;
        impl WorkerObserver for FreeWorkers {
            fn observe_roots(&self, _: &[PathBuf]) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState> {
                Default::default()
            }
        }
        let rig = crate::worktree_tests::repository()?;
        let original = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let lane = rig.root.join("lane");
        crate::worktree_tests::git(&rig, &["worktree", "add", "--detach", lane.to_str().ok_or("lane path")?, "HEAD"])?;
        std::fs::write(rig.project.join("changed"), "new")?;
        crate::worktree_tests::git(&rig, &["add", "changed"])?;
        crate::worktree_tests::git(&rig, &["commit", "-qm", "advance invoking checkout"])?;
        let moved = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        assert_ne!(original, moved);
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false, clock: SystemClock, backoff: ThreadPause,
            executor: SystemExecutor::new(1)?, home: rig.home.clone(), cwd: rig.project.clone(),
            assets: rig.project.clone(), host_launch: None, mcp_project: None, workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let mut owner = BoundGraphOwner {
            capability_overrides: (Vec::new(), Vec::new()),
            factory: &factory, integration_base: None, integration_branch: None, network: None,
            exclude: Vec::new(), budget: WaitBudget::cli(factory.clock.mono_now(), 30)?,
            cancel: &cancel, prices: None, tick: false,
        };
        assert_eq!(owner.resolve_review_base("HEAD", Some(lane.to_str().ok_or("lane path")?))?, Some(original));
        assert_eq!(owner.resolve_review_base("HEAD", None)?, Some(moved));
        Ok(())
    }

    #[test]
    fn graph_status_names_blocking_upstream() {
        let mut run = graph_status_fixture();
        let node = &mut run.checkpoint.nodes[1];
        node.state = provenant_fabric_runtime::graph::NodeState::Blocked;
        node.advance_error = Some("blocked by ordinary (Failed)".into());
        assert!(super::graph_human(&run).contains("composite: Blocked (blocked by ordinary (Failed))"));
        assert!(super::graph_human::graph_human_full(&run).contains("error=blocked by ordinary (Failed)"));
        let summary = super::graph_human::graph_summary(&serde_json::to_value(&run).unwrap());
        assert!(format!("{summary:?}").contains("blocked by ordinary (Failed)"));
    }

    #[test]
    fn graph_status_shows_review_substitution_for_a_completed_member() {
        let mut run = graph_status_fixture();
        let member = &mut run.checkpoint.nodes[2];
        member.state = provenant_fabric_runtime::graph::NodeState::Succeeded;
        member.ci = None;
        let mut attempt = run.checkpoint.nodes[0].attempts[1].clone();
        attempt.review_substitution = true;
        run.checkpoint.nodes[2].attempts.push(attempt);
        assert!(super::graph_human(&run).contains("member: Succeeded (review substitute)"));
        assert!(serde_json::to_value(&run).unwrap()["checkpoint"]["nodes"][2]["attempts"][0]["review_substitution"] == true);
    }

    #[test]
    fn graph_status_full_shows_node_warnings() {
        let mut run = graph_status_fixture();
        run.checkpoint.nodes[0].outputs = Some(serde_json::from_value(serde_json::json!({
            "schema_version": 1, "result": null, "verdict": null, "findings": [],
            "branch": null, "commit": null, "warnings": ["equal_base_and_head: review range is empty"]
        })).unwrap());
        assert!(super::graph_human::graph_human_full(&run).contains("warning: equal_base_and_head: review range is empty"));
    }

    #[test]
    fn graph_status_full_lists_checkpoint_nodes_attempts_and_errors() {
        let run = graph_status_fixture();
        let full = super::graph_human::graph_human_full(&run);
        let rows: Vec<_> = full.lines().filter(|line| line.starts_with("  ")).collect();
        assert_eq!(rows.len(), 4);
        for (row, id) in rows
            .iter()
            .zip(["ordinary", "composite", "member", "loop-member"])
        {
            assert!(row.starts_with(&format!("  {id}: ")), "{row}");
        }
        assert!(
            rows[0]
                .contains("adapter=codex model=latest effort=high attempt=new outcome=TaskFailed error=repair this")
        );
        assert!(!rows[0].contains("first"));
        let mut review = run.clone();
        review.checkpoint.nodes[0].attempts.last_mut().unwrap().outcome =
            Some(provenant_fabric_runtime::graph::AttemptOutcome::ReviewRequired);
        assert!(super::graph_human::graph_human_full(&review).contains("outcome=ReviewRequired"));
        assert!(rows[1].contains("route=none attempt=none"));
        assert!(rows[2].contains("CI waiting elapsed=12s checks=build head=abc"));
        assert!(full.contains("Advance error: repair this"));
        let mut without_ref = run.clone();
        without_ref.checkpoint.nodes[0]
            .attempts
            .last_mut()
            .unwrap()
            .attempt_ref = None;
        assert!(
            super::graph_human::graph_human_full(&without_ref)
                .contains("model=latest effort=high attempt=none")
        );
    }

    #[test]
    fn graph_status_full_shows_each_member_source_commit() {
        let mut run = graph_status_fixture();
        let first = "1".repeat(40);
        let second = "2".repeat(40);
        run.checkpoint.bound_review_base = Some(first.clone());
        run.checkpoint.bound_integration_base = Some(second.clone());
        run.checkpoint.nodes[1].source_commit = Some(first.clone());
        run.checkpoint.nodes[1].review_base = Some(second.clone());
        run.checkpoint.nodes[2].source_commit = Some(second.clone());
        run.checkpoint.nodes[3].source_binding = Some(serde_json::from_value(serde_json::json!({"type":"node", "node":"member"})).unwrap());
        let full = super::graph_human::graph_human_full(&run);
        assert!(full.contains(&format!("Graph bases: review={first} integration={second}")));
        assert!(full.lines().any(|row| row.starts_with("  composite:") && row.contains(&format!("source={first}"))));
        assert!(full.lines().any(|row| row.starts_with("  member:") && row.contains(&format!("source={second}"))));
        assert!(full.lines().any(|row| row.starts_with("  composite:") && row.contains(&format!("review_base={second}"))));
        assert!(full.lines().any(|row| row.starts_with("  loop-member:") && row.contains("source=pending")));
        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["checkpoint"]["bound_review_base"], first);
        assert_eq!(json["checkpoint"]["bound_integration_base"], second);
        assert_eq!(json["checkpoint"]["nodes"][3]["source_binding"]["node"], "member");
        assert_eq!(json["checkpoint"]["nodes"][1]["review_base"], second);
    }

    #[test]
    fn source_identity_must_match_the_bound_writer() {
        let mut run = graph_status_fixture();
        let identity = provenant_records::run::SourceIdentity { device: 7, inode: 9 };
        let path = std::path::Path::new("/fixture/writer");
        run.checkpoint.nodes[0].writer_binding = Some(provenant_fabric_runtime::graph::WriterWorktreeBinding {
            extra: [("source_directory_identity".into(), serde_json::json!({"device":7,"inode":9}))].into(),
            worktree: path.display().to_string(), head: "1".repeat(40),
        });
        assert!(super::writer_source_identity_matches(&run.checkpoint, path, &identity));
        let other = provenant_records::run::SourceIdentity { device: 7, inode: 10 };
        assert!(!super::writer_source_identity_matches(&run.checkpoint, path, &other));
        run.checkpoint.nodes[0].writer_binding.as_mut().unwrap().extra.clear();
        assert!(!super::writer_source_identity_matches(&run.checkpoint, path, &identity));
    }

    #[test]
    fn graph_status_hides_bound_host_checkout_paths() {
        let mut run = graph_status_fixture();
        run.checkpoint.nodes[0].writer_binding = Some(provenant_fabric_runtime::graph::WriterWorktreeBinding {
            extra: Default::default(),
            worktree: "/private/host/writer".into(),
            head: "1".repeat(40),
        });
        let snapshot = super::graph_snapshot(run.checkpoint);
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("/private/host/writer"));
        assert!(json.contains("[redacted]"));
        assert!(json.contains(&"1".repeat(40)));
        assert!(!super::graph_human::graph_human_full(&snapshot).contains("/private/host/writer"));
    }

    #[test]
    fn graph_full_status_connects_attempts_inputs_and_settlement() {
        let mut run = graph_status_fixture();
        let node = &mut run.checkpoint.nodes[0];
        node.attempts[1].input_cap = Some(262144);
        node.attempts[1].input_cap_source = Some(provenant_fabric_runtime::graph::InputCapSource::Configured);
        node.attempts[1].input_manifest = Some(serde_json::from_value(serde_json::json!({
            "schema_version":1, "source_commit":"1111111111111111111111111111111111111111",
            "review_base":"2222222222222222222222222222222222222222", "graph_review_base":"3333333333333333333333333333333333333333", "integration_base":"4444444444444444444444444444444444444444",
            "applied_route":{"adapter":"fixture", "model":"fixture/model"}, "omitted":0,
            "inputs":[{"producer_node":"producer", "producer_attempt":{"graph_id":"fixture", "node_id":"producer", "route_index":0},
                "kind":"commit", "required":true, "producer_result_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "commit":"1111111111111111111111111111111111111111", "delivery":"delivered", "file_delivery":"omitted"}]
        })).unwrap());
        node.outputs = Some(serde_json::from_value(serde_json::json!({
            "schema_version":2, "result":null, "verdict":"VERDICT: PASS", "findings":[],
            "branch":null, "commit":"1111111111111111111111111111111111111111", "warnings":[]
        })).unwrap());
        let json = serde_json::to_value(&run).unwrap();
        let attempt = &json["checkpoint"]["nodes"][0]["attempts"][1];
        assert_eq!(attempt["input_manifest"]["applied_route"]["model"], "fixture/model");
        assert_eq!(attempt["input_manifest"]["inputs"][0]["producer_attempt"]["node_id"], "producer");
        assert_eq!(attempt["input_manifest"]["inputs"][0]["delivery"], "delivered");
        assert_eq!(attempt["input_manifest"]["graph_review_base"], "3333333333333333333333333333333333333333");
        assert_eq!(attempt["input_manifest"]["integration_base"], "4444444444444444444444444444444444444444");
        assert_eq!(attempt["outcome"], "task_failed");
        assert_eq!(attempt["input_cap_source"], "configured");
        let text = super::graph_human::graph_human_full(&run);
        for fact in ["fixture/model", "producer", "selected_input", "required=true", "delivery=Delivered", "file=Omitted", "source=111111", "review_base=222222", "input_limit=262144", "input_limit_source=configured", "TaskFailed", "outputs digest="] {
            assert!(text.contains(fact), "missing {fact}: {text}");
        }
    }

    #[test]
    fn cancelled_graph_status_shows_reason() {
        let mut run = graph_status_fixture();
        run.checkpoint.cancelled = true;
        run.checkpoint.cancel_reason = Some("requested by chair".into());
        let run = super::graph_snapshot(run.checkpoint);
        assert!(super::graph_human(&run).contains("Cancellation reason: requested by chair"));
        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["checkpoint"]["cancel_reason"], "requested by chair");
    }

    #[test]
    fn graph_status_compact_and_json_remain_unchanged() {
        let run = graph_status_fixture();
        let ordinary = super::graph_human(&run);
        let globals = provenant_cli::Globals::default();
        assert_eq!(
            super::graph_display(&run, "fabric graph status", &globals),
            ordinary
        );
        let globals = provenant_cli::Globals {
            full: true,
            ..Default::default()
        };
        assert_eq!(
            super::graph_display(&run, "fabric graph status", &globals),
            super::graph_human::graph_human_full(&run)
        );
        assert_eq!(
            super::graph_display(&run, "fabric graph run", &globals),
            ordinary
        );
        assert!(ordinary.lines().any(|row| row.starts_with("  ordinary: Failed outcome=TaskFailed ") && row.contains("wait=none disposition=terminal")));
        assert!(ordinary.contains("  composite: Running (1 members) wait=none disposition=running"));
        assert!(ordinary.contains("  member: Running CI waiting"));
        assert!(!ordinary.contains("loop-member"));
        assert!(!ordinary.contains(" attempt=new"));
        let result = provenant_records::CommandResult::new(
            "fabric graph status",
            provenant_records::Outcome::Ok,
            &run,
        );
        let mut outputs = Vec::new();
        for full in [false, true] {
            let globals = provenant_cli::Globals {
                json: true,
                full,
                ..Default::default()
            };
            assert_eq!(
                super::graph_display(&run, "fabric graph status", &globals),
                ordinary
            );
            let mut bytes = Vec::new();
            provenant_cli::render(
                &result,
                globals.render_mode(&ordinary, false, None),
                &mut bytes,
            )
            .unwrap();
            outputs.push(bytes);
        }
        assert_eq!(outputs[0], outputs[1]);
        let json: serde_json::Value = serde_json::from_slice(&outputs[0]).unwrap();
        assert_eq!(json["data"], serde_json::to_value(&run).unwrap());
    }

    #[test]
    fn paused_graph_status_and_resume_show_the_pause_reason() {
        let mut run = graph_status_fixture();
        run.checkpoint.pause_reason = Some("invalid planner fragment: missing nodes".into());
        run.status = provenant_fabric_runtime::graph::GraphStatus::Paused;
        let globals = provenant_cli::Globals::default();
        for command in ["fabric graph status", "fabric graph resume"] {
            let human = super::graph_display(&run, command, &globals);
            assert!(
                human.contains("Pause reason: invalid planner fragment: missing nodes"),
                "{command}: {human}"
            );
        }
        let summary = super::graph_human::graph_summary(&serde_json::to_value(&run).unwrap());
        assert!(summary.lines.iter().any(|row| {
            row == "paused: invalid planner fragment: missing nodes"
        }));

        run.checkpoint.pause_reason = Some("x".repeat(2048));
        let human = super::graph_display(&run, "fabric graph status", &globals);
        let reason_line = human
            .lines()
            .find(|line| line.starts_with("Pause reason: "))
            .unwrap_or_default();
        assert!(reason_line.chars().count() <= 1024 + "Pause reason: ".chars().count() + 1);

        run.checkpoint.pause_reason = Some("before\u{1b}[31mafter".into());
        let human = super::graph_display(&run, "fabric graph status", &globals);
        assert!(human.contains("Pause reason: before[31mafter"));
        assert!(!human.contains('\u{1b}'));
        let summary = super::graph_human::graph_summary(&serde_json::to_value(&run).unwrap());
        assert!(summary.lines.iter().any(|line| line == "paused: before[31mafter"));
    }

    #[test]
    fn graph_status_reads_without_advancing_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
        if super::peer_tests::host_test("fabric::result_tests::graph_status_reads_without_advancing_checkpoint")? {
            return Ok(());
        }
        use super::*;
        use provenant_fabric_runtime::graph::{
            FileJournal, GraphDocument, GraphExecution, GraphJournal,
        };
        struct FreeWorkers;
        impl WorkerObserver for FreeWorkers {
            fn observe_roots(
                &self,
                roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState>
            {
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
                    .collect()
            }
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots.clone(), &rig.home, &rig.project)
                .map_err(|error| format!("open fixture context: {error}"))?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let project = factory.context.identity.project;
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":"fixture", "nodes":[
                {"id":"ordinary","role":"plan","access":"read_only","prompt":"test"}]
        }))?;
        let mut checkpoint = graph_status_fixture().checkpoint;
        checkpoint.nodes[0].attempts.remove(0);
        checkpoint.nodes[0].attempts[0].key.route_index = 0;
        let display = "node ordinary expansion paused: planner fragment invalid: invalid fragment: missing nodes; fallback expansion paused: source requires a full commit ID";
        checkpoint.pause_reason = Some(display.into());
        checkpoint.nodes[0].expansion_pause = Some(format!("{display}\n[planner-fragment-retry-context:{}", provenant_core::hex(b"invalid fragment: missing nodes")));
        let mut journal = FileJournal::open(
            &roots.state,
            project,
            "fixture",
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(GraphExecution {review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: rig.project.clone(),
            document,
        })?;
        journal.save(&checkpoint)?;
        drop(journal);
        let before = FileJournal::read_checkpoint(&roots.state, project, "fixture")?.unwrap();
        let status = factory.graph(
            GraphInput::Status {
                extra: Default::default(),
                id: "fixture".into(),
            },
            budget,
            &cancel,
        )?;
        let reason = status.checkpoint.pause_reason.as_deref().unwrap();
        assert!(reason.contains("invalid fragment: missing nodes"), "{reason}");
        assert!(reason.find("invalid fragment") < reason.find("fallback expansion paused"));
        let human = super::graph_display(&status, "fabric graph status", &provenant_cli::Globals::default());
        assert!(human.contains("invalid fragment: missing nodes"), "{human}");
        let json = serde_json::to_value(&status)?;
        assert!(json["checkpoint"]["pause_reason"].as_str().unwrap().contains("invalid fragment: missing nodes"));
        assert_eq!(
            FileJournal::read_checkpoint(&roots.state, project, "fixture")?.unwrap(),
            before
        );
        Ok(())
    }

    #[test]
    fn labels_differing_only_by_case_get_distinct_lowercase_files() {
        use super::graph_inputs::file_name;
        let (upper, lower) = (file_name("Review"), file_name("review"));
        assert_ne!(upper, lower);
        for name in [&upper, &lower] {
            let stem = name.strip_suffix(".md").expect("markdown file");
            assert!(
                stem.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
        }
        assert_eq!(file_name("review"), lower, "stable across calls");
    }

    #[test]
    fn remote_upstream_is_materialised_and_unreachable_peer_is_named()
    -> Result<(), Box<dyn std::error::Error>> {
        if super::peer_tests::host_test("fabric::result_tests::remote_upstream_is_materialised_and_unreachable_peer_is_named")? {
            return Ok(());
        }
        use super::*;
        use provenant_fabric_runtime::graph::{AttemptKey, InputRequest, MaterialisedInput};
        use std::os::unix::fs::PermissionsExt;
        struct FreeWorkers;
        impl WorkerObserver for FreeWorkers {
            fn observe_roots(
                &self,
                _roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState>
            {
                Default::default()
            }
        }
        let mut rig = crate::worktree_tests::support::TestRig::new()?;
        let project = rig.home.join("project");
        std::fs::rename(&rig.project, &project)?;
        rig.project = project;
        let remote_host = provenant_core::HostId::from_bytes([9; 16]);
        let attempt = provenant_core::AttemptId::from_bytes([8; 16]);
        let ssh = rig.root.join("ssh");
        let response = rig.root.join("response");
        std::fs::write(
            &ssh,
            format!("#!/bin/sh\ncat >/dev/null\ncat '{}'\n", response.display()),
        )?;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(
            rig.state.join("config.toml"),
            format!(
                "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n[domains.fabric.peers.workshop]\nhost='{remote_host}'\ndestination='workshop'\n",
                ssh.to_string_lossy()
            ),
        )?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let factory = Factory {
            context: Context::open(roots, &rig.home, &rig.project)
                .map_err(|error| format!("open fixture context: {error}"))?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &FreeWorkers,
        };
        let text = format!("full {} end-of-remote-result", "remote ".repeat(1400));
        let target = AttemptTarget {
            extra: Default::default(),
            host: Some(remote_host),
            attempt,
        };
        let digest = provenant_core::Digest::from_bytes(
            *provenant_records::CanonicalV1::file_digest(text.as_bytes()).bytes(),
        );
        let locator = ProjectLocator::new(&factory.context.identity, &rig.home)?;
        let mut reply = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            host: remote_host,
            project: locator.key,
            result: PeerResult::Output {
                extra: Default::default(),
                data: OutputResult::Chunk {
                    extra: Default::default(),
                    target,
                    part: OutputPart::Result,
                    state: "succeeded".into(),
                    reason: None,
                    offset: 0,
                    next_offset: text.len() as u64,
                    eof: true,
                    digest,
                    bytes: OutputBytes::Utf8(text.clone()),
                },
            },
        };
        std::fs::write(&response, serde_json::to_vec(&reply)?)?;
        let request = |label: &str, attempt_ref: Option<&str>| InputRequest {
            label: label.into(),
            attempt_ref: attempt_ref.map(str::to_owned),
            inline_findings: None,
            finding_prefix: None,
        };
        let key = AttemptKey {
            extra: Default::default(),
            graph_id: "g".into(),
            node_id: "consumer".into(),
            route_index: 0,
        };
        let inputs = graph_inputs::materialise(
            &factory,
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &CancelToken::new(),
            &key,
            &[
                request("review", Some(&format!("{remote_host}/{attempt}"))),
                request(
                    "offline",
                    Some(&format!(
                        "{}/{attempt}",
                        provenant_core::HostId::from_bytes([7; 16])
                    )),
                ),
                request("never-ran", None),
            ],
            16_384,
        );
        let MaterialisedInput::File { path, omitted, .. } = &inputs.entries[0] else {
            return Err(format!("{inputs:?}").into());
        };
        assert_eq!(std::fs::read_to_string(path)?, text);
        assert_eq!(*omitted, 0);
        let index: serde_json::Value = serde_json::from_slice(&std::fs::read(inputs.index.as_ref().ok_or("input index")?)?)?;
        assert_eq!(index[0]["status"], "complete");
        assert_eq!(index[1]["status"], "unavailable");
        assert_eq!(index[1]["attempt_ref"], format!("{}/{attempt}", provenant_core::HostId::from_bytes([7; 16])));
        assert_eq!(index[2]["label"], "never-ran");
        // Exercise the graph scheduler with the real staging adapter and a
        // fixture peer, including a consumer that reads its materialised file.
        use provenant_fabric_runtime::graph;
        struct Journal(Option<graph::GraphCheckpoint>);
        impl graph::GraphJournal for Journal {
            type Error = std::convert::Infallible;
            fn load(&mut self, _: &str) -> Result<Option<graph::GraphCheckpoint>, Self::Error> {
                Ok(self.0.clone())
            }
            fn save(&mut self, checkpoint: &graph::GraphCheckpoint) -> Result<(), Self::Error> {
                self.0 = Some(checkpoint.clone());
                Ok(())
            }
        }
        struct FixtureOwner<'a> {
            factory: &'a Factory,
            reference: String,
            expected: &'a str,
            offline: bool,
            consumed: bool,
        }
        impl graph::AttemptOwner for FixtureOwner<'_> {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "fixture".into()
            }
            fn materialise_inputs(
                &mut self,
                key: &graph::AttemptKey,
                requests: &[graph::InputRequest],
                cap: usize,
            ) -> Option<graph::MaterialisedInputs> {
                Some(graph_inputs::materialise(
                    self.factory,
                    WaitBudget::cli(self.factory.clock.mono_now(), 30).unwrap(),
                    &CancelToken::new(),
                    key,
                    requests,
                    cap,
                ))
            }
            fn observe_or_start(
                &mut self,
                key: &graph::AttemptKey,
                node: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                let producer = key.node_id == "producer";
                if !producer {
                    self.consumed = true;
                    if self.offline {
                        assert!(node.prompt.contains("unavailable:"), "{}", node.prompt);
                    } else {
                        let file = node
                            .inputs_dir
                            .unwrap()
                            .join(graph_inputs::file_name("producer"));
                        assert_eq!(std::fs::read_to_string(file).unwrap(), self.expected);
                    }
                }
                let mut capture =
                    provenant_fabric_runtime::result_capture::ResultCapture::new(None);
                capture.push(b"bounded producer excerpt");
                Ok(graph::AttemptObservation::Terminal {
                    attempt_ref: producer.then(|| self.reference.clone()),
                    outcome: graph::AttemptOutcome::Succeeded,
                    launched: true,
                    result: Some(Box::new(capture.finish())),
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: None,
                })
            }
        }
        for offline in [false, true] {
            let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
                "schema_version": 1, "graph_id": if offline { "offline-graph" } else { "remote-graph" },
                "nodes": [
                    {"id":"producer", "prompt":"produce", "access":"read_only", "route":{"adapter":"fixture", "model":"fixture/model"}},
                    {"id":"consumer", "prompt":"consume", "access":"read_only", "depends_on":["producer"], "inputs":[{"node":"producer", "outputs":["result"]}], "route":{"adapter":"fixture", "model":"fixture/model"}}
                ]
            }))?;
            let host = if offline {
                provenant_core::HostId::from_bytes([7; 16])
            } else {
                remote_host
            };
            let mut owner = FixtureOwner {
                factory: &factory,
                reference: format!("{host}/{attempt}"),
                expected: &text,
                offline,
                consumed: false,
            };
            let run = graph::advance_with_budget(
                &document,
                &mut owner,
                &mut Journal(None),
                &Default::default(),
                &Default::default(),
            )?;
            assert!(run.checkpoint.successful());
            assert!(owner.consumed);
        }
        if let PeerResult::Output {
            data:
                OutputResult::Chunk {
                    extra,
                    next_offset,
                    eof,
                    bytes,
                    ..
                },
            ..
        } = &mut reply.result
        {
            extra.insert("length".into(), serde_json::json!(text.len()));
            *next_offset = 5;
            *eof = false;
            *bytes = OutputBytes::Utf8(text[..5].into());
        }
        std::fs::write(&response, serde_json::to_vec(&reply)?)?;
        let capped = graph_inputs::materialise(
            &factory,
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &CancelToken::new(),
            &key,
            &[request("review", Some(&format!("{remote_host}/{attempt}")))],
            5,
        );
        let MaterialisedInput::File {
            path,
            bytes,
            omitted,
            ..
        } = &capped.entries[0]
        else {
            return Err(format!("{capped:?}").into());
        };
        assert_eq!(*bytes, 5);
        assert_eq!(*omitted, (text.len() - 5) as u64);
        assert!(std::fs::read_to_string(path)?.starts_with("full "));
        let index: serde_json::Value = serde_json::from_slice(&std::fs::read(capped.index.as_ref().ok_or("capped index")?)?)?;
        assert_eq!(index[0]["status"], "capped");
        assert_eq!(index[0]["attempt_ref"], format!("{remote_host}/{attempt}"));
        let finding: graph::Finding = serde_json::from_value(serde_json::json!({"id":"ci-1","severity":"high","file":"","summary":"failed check","status":"open"}))?;
        let inline = graph_inputs::materialise(
            &factory, WaitBudget::cli(factory.clock.mono_now(), 30)?, &CancelToken::new(), &key,
            &[InputRequest { label: "ci".into(), attempt_ref: None, inline_findings: Some(vec![finding]), finding_prefix: None }], 16_384,
        );
        let MaterialisedInput::File { path, omitted: 0, .. } = &inline.entries[0] else { return Err(format!("{inline:?}").into()); };
        let inline_text = std::fs::read_to_string(path)?;
        assert!(inline_text.contains("```findings\n") && inline_text.contains("failed check"));
        assert!(
            matches!(&inputs.entries[1], MaterialisedInput::Unavailable { label, reason } if label == "offline" && reason.contains("result could not be read")),
            "{inputs:?}"
        );
        assert!(
            matches!(&inputs.entries[2], MaterialisedInput::Unavailable { label, reason } if label == "never-ran" && reason == "no attempt was recorded for it")
        );
        Ok(())
    }

    #[test]
    fn graph_status_shows_waiting_ci_checks_head_and_live_elapsed() {
        let checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "graph_id": "delivery",
                "plan_digest": "digest",
                "nodes": [{
                    "id": "ci", "state": "running", "worktree": null, "attempts": [],
                    "ci": {
                        "state": "pending", "summary": "2 pending checks",
                        "head_sha": "abc123", "check_names": ["build", "tests"],
                        "elapsed_seconds": 10,
                        "first_observed_stamp": {
                            "boot_identity": "boot-a", "host_monotonic_micros": 1_000_000
                        }
                    }
                }]
            }))
            .unwrap();
        let mut display = checkpoint.clone();
        super::graph_status_elapsed(&mut display, "boot-a", 66_000_000);
        let run = super::graph_snapshot(display);
        let expected = "CI waiting elapsed=65s checks=build, tests head=abc123";
        assert!(super::graph_human(&run).contains(expected));
        let summary = super::graph_summary(&serde_json::to_value(&run).unwrap());
        assert_eq!(summary.lines[1], format!("ci running: {expected}"));
        assert_eq!(
            summary.next.as_deref(),
            Some("provenant fabric graph status delivery")
        );
        assert_eq!(checkpoint.nodes[0].ci.as_ref().unwrap().elapsed_seconds, 10);

        let mut nested = run.clone();
        let mut parent = nested.checkpoint.nodes[0].clone();
        parent.id = "deliver".into();
        parent.children = vec!["ci".into()];
        parent.ci = None;
        nested.checkpoint.nodes.push(parent);
        assert!(super::graph_human(&nested).contains(expected));

        for (boot, now) in [("boot-b", 66_000_000), ("boot-a", 0)] {
            let mut display = checkpoint.clone();
            super::graph_status_elapsed(&mut display, boot, now);
            assert_eq!(display.nodes[0].ci.as_ref().unwrap().elapsed_seconds, 10);
        }
        assert_eq!(
            super::graph_ci_waiting(0, std::iter::empty(), None),
            "CI waiting elapsed=0s checks=awaiting registration"
        );
    }

    #[test]
    fn graph_usage_maps_attempt_telemetry_and_prices() {
        fn usage_record(
            model: &str,
            usage: serde_json::Value,
            provider_ms: Option<u64>,
        ) -> provenant_records::run::AttemptRecord {
            use provenant_core::{AttemptId, Digest, ProjectId, RunId, TaskId, WallTime};
            use serde_json::json;
            let now = WallTime::from_micros(1).unwrap();
            serde_json::from_value(json!({
                "schema_version": 1,
                "attempt_id": AttemptId::from_bytes([1; 16]),
                "run_id": RunId::from_bytes([2; 16]),
                "project_id": ProjectId::from_bytes([3; 16]),
                "task_id": TaskId::from_bytes([4; 16]),
                "operation_id": "graph-usage-test",
                "attempt_number": 1, "revision": 1, "generation": 0,
                "requested_route": {"kind":"model", "provider":"p", "model":model},
                "resolved_route": {"provider":"p", "model":model, "effort":null, "account_label":null},
                "authority_digest": Digest::from_bytes([5; 32]), "queued_at": now,
                "state": {"state":"terminal", "outcome":"lost", "exit_code":null, "finished_at":now, "reason":null},
                "provider_ms": provider_ms, "usage": usage,
                "prompt": {"path":{"encoding":"utf8", "path":"prompt.txt"},
                    "digest": provenant_records::CanonicalV1::file_digest(b""), "bytes":0, "complete":true, "redacted":false}
            })).unwrap()
        }

        fn usage_prices() -> provenant_telemetry::PriceTable {
            let config = provenant_telemetry::ConfiguredPrices {
                schema_version: 1,
                id: "test".into(),
                reviewed_at: "2026-10-22".into(),
                units: "usd_microdollars_per_million_tokens".into(),
                max_review_age_days: None,
                discrepancy_threshold_bps: None,
                models: vec![provenant_telemetry::ConfiguredModel {
                    provider: "p".into(),
                    model: "priced".into(),
                    sources: vec!["test".into()],
                    billing: provenant_telemetry::BillingKind::Api,
                    zero_rate_verified: false,
                    effective_from: None,
                    effective_to: None,
                    openrouter_id: None,
                    litellm_id: None,
                    rates: provenant_telemetry::Rates {
                        uncached_input: Some(1_000_000),
                        cache_read: Some(2_000_000),
                        cache_write: Some(3_000_000),
                        output_excluding_reasoning: Some(4_000_000),
                    },
                }],
            };
            provenant_telemetry::validate_prices(&config).unwrap()
        }

        use serde_json::json;
        let prices = usage_prices();
        let full = json!({"uncached_input":1_000_000, "cache_read":1_000_000,
            "cache_write":1_000_000, "output_excluding_reasoning":1_000_000, "reasoning":1_000_000});
        let result = super::graph_attempt_usage(
            usage_record("priced", full.clone(), Some(1001)),
            Some(&prices),
            None,
        );
        assert_eq!(result.tokens, Some(5_000_000));
        assert!(!result.tokens_partial);
        assert_eq!(result.cost_micro_usd, Some(14_000_000));
        assert!(!result.cost_partial);
        assert_eq!(result.wall_time_seconds, Some(2));
        assert_eq!(result.cost_unknown_reason, None);

        // No provider reports reasoning apart from output, so its absence is
        // not partial usage.
        let without_reasoning = super::graph_attempt_usage(
            usage_record(
                "priced",
                json!({"uncached_input":1_000_000, "cache_read":1_000_000,
                    "cache_write":1_000_000, "output_excluding_reasoning":1_000_000}),
                Some(1000),
            ),
            Some(&prices),
            None,
        );
        assert!(!without_reasoning.tokens_partial);
        assert!(!without_reasoning.cost_partial);
        assert_eq!(without_reasoning.cost_micro_usd, Some(10_000_000));

        let partial = super::graph_attempt_usage(
            usage_record("priced", json!({"uncached_input":1_000_000}), Some(1000)),
            Some(&prices),
            None,
        );
        assert_eq!(partial.tokens, Some(1_000_000));
        assert!(partial.tokens_partial);
        assert_eq!(partial.cost_micro_usd, Some(1_000_000));
        assert!(partial.cost_partial);
        assert_eq!(partial.wall_time_seconds, Some(1));

        let absent = super::graph_attempt_usage(
            usage_record("priced", json!({}), None),
            Some(&prices),
            None,
        );
        assert_eq!(absent.tokens, None);
        assert!(absent.tokens_partial);
        assert_eq!(absent.cost_micro_usd, None);
        assert!(absent.cost_partial);
        assert_eq!(absent.wall_time_seconds, None);

        let unpriced = super::graph_attempt_usage(
            usage_record("unpriced", full.clone(), Some(0)),
            Some(&prices),
            None,
        );
        assert_eq!(unpriced.tokens, Some(5_000_000));
        assert!(!unpriced.tokens_partial);
        assert_eq!(unpriced.cost_micro_usd, None);
        assert!(unpriced.cost_partial);
        assert_eq!(unpriced.wall_time_seconds, Some(0));

        let unavailable = super::graph_attempt_usage(
            usage_record("priced", full, None),
            None,
            Some("price table unavailable".into()),
        );
        assert_eq!(unavailable.cost_micro_usd, None);
        assert!(unavailable.cost_partial);
        assert_eq!(
            unavailable.cost_unknown_reason.as_deref(),
            Some("price table unavailable")
        );
    }

    #[test]
    fn graph_status_handles_pause_left_after_terminal_checkpoint() {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphStatus, NodeState};

        // This is the durable state if the process stops immediately after saving
        // the final terminal node, before advance clears the structural pause.
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": "graph",
            "plan_digest": "digest",
            "pause_reason": "node limit exceeded",
            "nodes": [{"id": "one", "state": "succeeded", "worktree": null, "attempts": []}]
        }))
        .unwrap();
        let run = super::graph_snapshot(checkpoint.clone());
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.pause_reason.is_none());

        checkpoint.nodes[0].state = NodeState::Running;
        assert_eq!(
            super::graph_snapshot(checkpoint.clone()).status,
            GraphStatus::Paused
        );
        checkpoint.nodes[0].state = NodeState::Succeeded;
        checkpoint.nodes[0].expansion_pause = Some("fragment exceeds node bound".into());
        assert_eq!(
            super::graph_snapshot(checkpoint.clone()).status,
            GraphStatus::Paused
        );
        checkpoint.nodes[0].expansion_pause = None;
        checkpoint.delivery_required = true;
        assert_eq!(
            super::graph_snapshot(checkpoint.clone()).status,
            GraphStatus::Paused
        );
        checkpoint.cancelled = true;
        assert_eq!(
            super::graph_snapshot(checkpoint).status,
            GraphStatus::Cancelled
        );
    }

    #[test]
    fn graph_result_errors_produce_safe_specific_warnings() {
        use provenant_run::RunError;
        for (error, warning) in [
            (
                RunError::ArtifactMismatch(provenant_run::ArtifactProvenance {
                    check: provenant_run::ArtifactCheck::RecordedDigest,
                    artifact: "result.md".into(),
                    expected: None,
                    actual: None,
                }),
                "artifact digest or length does not match its record",
            ),
            (RunError::NotFound, "result artifact is missing"),
            (
                RunError::TooLarge { limit: 1 },
                "result artifact exceeds the byte limit",
            ),
            (
                RunError::Validation("output redaction failed"),
                "result artifact could not be verified",
            ),
        ] {
            assert_eq!(super::graph_result_warning(&error), warning);
        }
    }
    #[test]
    fn missing_graph_price_table_reports_its_path_and_failure() {
        let assets = tempfile::tempdir().unwrap();
        let reason = super::load_graph_prices(assets.path()).unwrap_err();
        assert!(
            reason.contains(
                &assets
                    .path()
                    .join("config/telemetry-prices.toml")
                    .display()
                    .to_string()
            )
        );
        assert!(reason.contains("No such file") || reason.contains("not found"));
    }

    #[test]
    fn newer_graph_schemas_report_upgrade_required() {
        let document_error =
            super::decode_graph_document(br#"{"schema_version":3,"future_shape":true}"#)
                .unwrap_err();
        let decode_error = provenant_records::decode_v1::<serde_json::Value, _>(
            br#"{"schema_version":2,"future_shape":true}"#.as_slice(),
            provenant_records::Limits::default(),
        )
        .unwrap_err();
        let checkpoint_error = super::graph_journal_error(
            provenant_fabric_runtime::graph::JournalError::Decode(decode_error),
        );
        for (error, record_kind, expected_version) in [
            (document_error, "graph document schema", 3),
            (checkpoint_error, "graph checkpoint schema", 2),
        ] {
            assert_eq!(error.code(), "upgrade_required");
            let super::FacadeError::UpgradeRequired {
                reader_release,
                writer_release,
                record,
                version,
            } = error
            else {
                unreachable!()
            };
            assert_eq!(reader_release, env!("CARGO_PKG_VERSION"));
            assert_eq!(writer_release, "unknown");
            assert_eq!(record, record_kind);
            assert_eq!(version, expected_version);
        }
        assert_eq!(
            super::decode_graph_document(br#"{"schema_version":0}"#).unwrap_err(),
            super::FacadeError::invalid("graph document schema_version must be 1 or 2")
        );
    }

    #[test]
    fn oversized_graph_checkpoint_is_refused_with_size_reason() {
        match super::graph_journal_error(provenant_fabric_runtime::graph::JournalError::TooLarge) {
            super::FacadeError::DispatchRefused { reason, .. } => {
                assert!(reason.contains("byte limit"))
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod lane_timeout_tests {
    use super::lane_timeout;

    #[test]
    fn lane_timeout_is_node_deadline_capped_by_graph_timeout() {
        assert_eq!(lane_timeout(Some(120), 60), 60);
        assert_eq!(lane_timeout(Some(30), 60), 30);
    }
}

#[cfg(test)]
#[allow(dead_code, reason = "the custom worktree driver compiles helpers without libtest functions")]
mod retry_allowance_tests {
    use provenant_fabric_runtime::graph;
    struct PolicyOwner((u8, String));
    impl graph::AttemptOwner for PolicyOwner {
        type Error = std::convert::Infallible;
        fn actor(&self) -> String { "test".into() }
        fn retry_allowance(&self, _: &provenant_records::ExtraFields, _: &provenant_records::ExtraFields) -> Result<Option<(u8, String)>, Self::Error> { Ok(Some(self.0.clone())) }
        fn observe_or_start(&mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute, _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>) -> Result<graph::AttemptObservation, Self::Error> {
            Ok(graph::AttemptObservation::Terminal { attempt_ref: None, outcome: graph::AttemptOutcome::Succeeded, launched: false, result: None, result_warning: None, result_path: None, branch: None, commit: None })
        }
    }
    #[derive(Default)]
    struct Journal(Option<graph::GraphCheckpoint>);
    impl graph::GraphJournal for Journal {
        type Error = std::convert::Infallible;
        fn load(&mut self, _: &str) -> Result<Option<graph::GraphCheckpoint>, Self::Error> { Ok(self.0.clone()) }
        fn save(&mut self, checkpoint: &graph::GraphCheckpoint) -> Result<(), Self::Error> { self.0 = Some(checkpoint.clone()); Ok(()) }
    }
    fn assert_display(policy: (u8, String)) -> Result<(), Box<dyn std::error::Error>> {
        let document = serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"policy","nodes":[{"id":"read","prompt":"inspect","access":"read_only","route":{"alias":"junior"}}]}))?;
        let run = graph::advance_with_budget(&document, &mut PolicyOwner(policy.clone()), &mut Journal::default(), &Default::default(), &Default::default())?;
        assert!(run.checkpoint.successful());
        let human = super::graph_human::graph_human_full(&run);
        assert!(human.contains(&format!("settlement_retries={} origin={}", policy.0, policy.1)), "{human}");
        Ok(())
    }
    #[test]
    fn graph_retry_allowance_resolves_default_host_preset_and_node_with_origin() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        let empty = Default::default();
        assert_eq!(super::graph_retry_allowance(&empty, &empty, &roots), (2, "shipped_default".into()));
        std::fs::write(roots.state.as_path().join("config.toml"), "schema_version=1\n[domains.routing]\nsettlement_retries=4\n")?;
        assert_eq!(super::graph_retry_allowance(&empty, &empty, &roots), (4, "host_config".into()));
        let preset = serde_json::from_value(serde_json::json!({"retry":{"retries":3}}))?;
        assert_eq!(super::graph_retry_allowance(&empty, &preset, &roots), (3, "preset".into()));
        let node = serde_json::from_value(serde_json::json!({"retry":{"retries":0}}))?;
        assert_eq!(super::graph_retry_allowance(&node, &preset, &roots), (0, "node".into()));
        let inherited = serde_json::from_value(serde_json::json!({"preset_retry":{"retries":0}}))?;
        assert_eq!(super::graph_retry_allowance(&inherited, &preset, &roots), (0, "preset".into()));
        for (node, graph) in [(&empty, &empty), (&empty, &preset), (&node, &preset), (&inherited, &preset)] {
            assert_display(super::graph_retry_allowance(node, graph, &roots))?;
        }
        for retry in [serde_json::json!({"retries":-1}), serde_json::json!({"retries":33}), serde_json::json!({"retries":"two"})] {
            let invalid = serde_json::from_value(serde_json::json!({"retry":retry}))?;
            let (retries, origin) = super::graph_retry_allowance(&invalid, &empty, &roots);
            assert_eq!(retries, 4);
            assert!(origin.contains("ignored invalid retry.retries"), "{origin}");
        }
        Ok(())
    }
}
