//! Tick and its scheduled collector share an isolated host state.
#![allow(clippy::disallowed_methods)] // Bound a real detached process in this integration fixture.
mod support;
use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
use provenant_fabric_runtime::graph::{self, FileJournal, GraphExecution};
use provenant_os::clock::SystemClock;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

#[test]
fn missing_graph_worktree_warns_once_and_scheduled_gc_completes() -> Result {
    struct Seed {
        actor: String,
        at: provenant_core::WallTime,
    }
    impl graph::AttemptOwner for Seed {
        type Error = std::convert::Infallible;
        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn wall_now(&self) -> Option<provenant_core::WallTime> {
            Some(self.at)
        }
        fn observe_or_start(
            &mut self,
            _: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
            unreachable!("decision graph does not launch a provider")
        }
    }
    let rig = TestRig::new()?;
    let _cleanup = rig.process_cleanup();
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["commit", "--allow-empty", "-qm", "initial"],
        vec!["branch", "-M", "main"],
        vec![
            "remote",
            "add",
            "origin",
            rig.project.to_str().ok_or("project path")?,
        ],
    ] {
        let output = rig.execute(&git, &args, None)?;
        assert_eq!(code(&output), 0, "{output:?}");
    }
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0, "{identity:?}");
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let actor = identity["data"]["inbox"]["actor"]
        .as_str()
        .ok_or("actor")?
        .to_owned();
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let state = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
    let clock = SystemClock::new();
    let now = clock.wall_now()?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 10)?;
    for id in ["missing", "present"] {
        let document: graph::GraphDocument = serde_json::from_value(
            json!({"schema_version":1,"graph_id":id,"decision_session":"chair-session",
            "nodes":[{"id":"choose","kind":"decide","access":"read_only","decide":{"question":"Ship?",
            "options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],"default":"hold","timeout_seconds":60}}]}),
        )?;
        let mut journal = FileJournal::open(&state, project, id, budget, &cancel, &clock)?;
        journal.bind(GraphExecution {
            extra: Default::default(),
            integration_base: None,
            review_base: None,
            directory: if id == "missing" {
                rig.root.join("removed")
            } else {
                rig.project.clone()
            },
            document: document.clone(),
        })?;
        let at = if id == "present" {
            provenant_core::WallTime::from_micros(now.as_micros() - 120_000_000)?
        } else {
            now
        };
        graph::advance_with_budget(
            &document,
            &mut Seed {
                actor: actor.clone(),
                at,
            },
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
    }
    let first = rig.command(&["tick", "--json"])?;
    assert_eq!(code(&first), 0, "{first:?}");
    let first: Value = serde_json::from_slice(&first.stdout.bytes)?;
    assert_eq!(first["data"]["graphs"]["advanced"], 1, "{first}");
    let warnings = first["data"]["graphs"]["warnings"]
        .as_array()
        .ok_or("warnings")?;
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .as_str()
            .is_some_and(|s| s.contains("missing") && !s.contains("upgrade"))
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(bytes) = std::fs::read(rig.state.join("tick/gc-result.json")) {
            let result: Value = serde_json::from_slice(&bytes)?;
            if result["state"] == "complete" {
                break;
            }
            assert_ne!(result["state"], "failed", "{result}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "scheduled GC did not complete"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let second = rig.command(&["tick", "--json"])?;
    assert_eq!(code(&second), 0, "{second:?}");
    let second: Value = serde_json::from_slice(&second.stdout.bytes)?;
    assert!(
        second["data"]["graphs"]["warnings"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{second}"
    );
    Ok(())
}
