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
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
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
            "nodes":[{"id":"choose","kind":"decide","access":"read_only","decide":{"on_unanswered":"block_dependants", "question":"Ship?",
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
    let gc_result = rig.state.join("tick/gc-result.json");
    let result = provenant_test_support::wait_for(
        "scheduled GC",
        std::time::Duration::from_secs(30),
        || {
            std::fs::read(&gc_result)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        },
        |result| {
            result
                .as_ref()
                .is_some_and(|result| result["state"] == "complete" || result["state"] == "failed")
        },
    )?
    .ok_or("scheduled GC wrote no result")?;
    assert_eq!(result["state"], "complete", "{result}");
    let second = rig.command(&["tick", "--json"])?;
    assert_eq!(code(&second), 0, "{second:?}");
    let second: Value = serde_json::from_slice(&second.stdout.bytes)?;
    assert!(
        second["data"]["graphs"]["warnings"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{second}"
    );
    // The daily tick and manual GC share graph checkpoint retention. Age a
    // terminal checkpoint beyond the configured window and exercise `gc`.
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\ngraph_checkpoint_days=1\n",
    )?;
    let checkpoint = state
        .as_path()
        .join("fabric/graphs")
        .join(project.to_string())
        .join("graph-present.json");
    let file = std::fs::OpenOptions::new().write(true).open(&checkpoint)?;
    file.set_times(
        std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400),
        ),
    )?;
    let manual = rig.command(&["gc", "--apply", "--json"])?;
    assert_eq!(code(&manual), 0, "{manual:?}");
    let manual: Value = serde_json::from_slice(&manual.stdout.bytes)?;
    assert_eq!(manual["data"]["graph_checkpoints_removed"], 1, "{manual}");
    assert!(
        !checkpoint.exists(),
        "manual GC left the old terminal checkpoint"
    );
    Ok(())
}

#[test]
fn deadline_free_graph_tick_launches_waiting_fake_provider() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    if let Err(error) = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    ) {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "graph fixture skipped: {error}"
        ))?;
        return Ok(());
    }
    struct Seed(String);
    impl graph::AttemptOwner for Seed {
        type Error = std::convert::Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            self.0.clone()
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
            Ok(graph::AttemptObservation::Pending { attempt_ref: None })
        }
    }
    let mut rig = TestRig::new()?;
    let _cleanup = rig.process_cleanup();
    let bin = rig.root.join("bin");
    std::fs::create_dir(&bin)?;
    let provider = bin.join("claude");
    std::fs::write(&provider, b"#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'claude 1.0.0'; exit 0; fi\nif [ \"$1\" = auth ]; then echo '{\"loggedIn\":true,\"email\":\"fixture@example.test\"}'; exit 0; fi\nprintf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"fixture-session\"}' '{\"type\":\"result\",\"session_id\":\"fixture-session\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"OK\"}'\n")?;
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let account = rig.home.join(".claude-pro");
    std::fs::create_dir(&account)?;
    std::fs::write(
        account.join(".credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"synthetic-token","expiresAt":4102444800000}}"#,
    )?;
    let registered = rig.command(&[
        "accounts",
        "register",
        "pro",
        "--adapter",
        "claude",
        "--directory",
        account.to_str().ok_or("account path")?,
        "--json",
    ])?;
    assert_eq!(code(&registered), 0, "{registered:?}");
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0, "{identity:?}");
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = ProjectId::parse(identity["data"]["project"].as_str().ok_or("project")?)?;
    let actor = identity["data"]["inbox"]["actor"].as_str().ok_or("actor")?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let state = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 10)?;
    let document: graph::GraphDocument = serde_json::from_value(
        json!({"schema_version":1,"graph_id":"waiter","template_library":{},
        "nodes":[{"id":"one","prompt":"reply OK","access":"read_only",
        "route":{"adapter":"claude","model":"claude/claude-sonnet-5-5","account":"pro"}}]}),
    )?;
    let mut journal = FileJournal::open(&state, project, "waiter", budget, &cancel, &clock)?;
    journal.bind(GraphExecution {
        extra: Default::default(),
        integration_base: None,
        review_base: None,
        directory: rig.project.clone(),
        document: document.clone(),
    })?;
    let seeded = graph::advance_with_budget(
        &document,
        &mut Seed(actor.into()),
        &mut journal,
        &Default::default(),
        &Default::default(),
    )?;
    assert!(seeded.checkpoint.wake_at.is_none());
    drop(journal);
    let tick = rig.command(&["tick", "--json"])?;
    assert_eq!(code(&tick), 0, "{tick:?}");
    let tick: Value = serde_json::from_slice(&tick.stdout.bytes)?;
    assert_eq!(tick["data"]["graphs"]["advanced"], 1, "{tick}");
    let checkpoint = provenant_test_support::wait_for(
        "fake provider launch",
        std::time::Duration::from_secs(30),
        || {
            FileJournal::read_checkpoint(&state, project, "waiter")
                .ok()
                .flatten()
        },
        |checkpoint| {
            checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.nodes[0].state == graph::NodeState::Failed
                    || checkpoint.nodes[0]
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.launched)
            })
        },
    )?
    .ok_or("checkpoint")?;
    if checkpoint.nodes[0].state == graph::NodeState::Failed {
        let lanes = rig.command(&["status", "waiter", "--json"])?;
        panic!(
            "fake provider failed: {}",
            String::from_utf8_lossy(&lanes.stdout.bytes)
        );
    }
    Ok(())
}

#[test]
fn steward_is_listed_compiles_and_stays_idle_without_config() -> Result {
    let rig = TestRig::new()?;
    let listed = rig.command(&["templates", "--json"])?;
    assert_eq!(code(&listed), 0, "{listed:?}");
    let listed: Value = serde_json::from_slice(&listed.stdout.bytes)?;
    assert!(
        listed["data"]["templates"]
            .as_array()
            .ok_or("templates")?
            .iter()
            .any(|template| template["name"] == "steward")
    );
    let compiled = rig.command(&["run", "steward", "--explain", "--json"])?;
    assert_eq!(code(&compiled), 0, "{compiled:?}");
    let compiled: Value = serde_json::from_slice(&compiled.stdout.bytes)?;
    let brief = compiled["data"]["document"]["nodes"][0]["prompt"]
        .as_str()
        .ok_or("compiled brief")?;
    assert!(brief.contains("proposal mode"));
    assert!(brief.contains("Never close an"));
    std::fs::create_dir_all(&rig.state)?;
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nschedule=false\n",
    )?;
    let tick = rig.command(&["tick", "--json"])?;
    assert_eq!(code(&tick), 0, "{tick:?}");
    let tick: Value = serde_json::from_slice(&tick.stdout.bytes)?;
    let steward = tick["data"]["tasks"]
        .as_array()
        .ok_or("tasks")?
        .iter()
        .find(|task| task["task"] == "steward")
        .ok_or("steward task")?;
    assert_eq!(steward["state"], "idle");
    assert_eq!(tick["data"]["graphs"]["advanced"], 0);
    assert!(!rig.state.join("tick/steward").exists());
    Ok(())
}
