//! A dirty writer is refused before admission but remains observable by its operation.
#![allow(clippy::disallowed_methods)]
use crate::support;
use provenant_core::{CancelToken, RootInputs};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, OperationState, Request, RunStore};
use serde_json::{Value, json};
use std::fs;
use support::{Result, TestRig, code};

#[test]
fn dirty_writer_refusal_is_durable_visible_and_notified_once() -> Result {
    for notice in ["inbox", "off"] {
        let rig = TestRig::new()?;
        fs::write(rig.project.join("tracked"), "original")?;
        for args in [vec!["add", "tracked"], vec!["commit", "-qm", "initial"]] {
            let output = provenant_os::fixture::git_command()
                .current_dir(&rig.project)
                .args(args)
                .output()?;
            assert!(output.status.success(), "{output:?}");
        }
        fs::write(
            rig.state.join("config.toml"),
            format!(
                "schema_version=1\n[gc]\nfree_floor='1B'\n{}",
                if notice == "off" {
                    "[knobs]\n'chair.refusal_notice'='off'\n"
                } else {
                    ""
                },
            ),
        )?;
        let shown = rig.command(&["config", "show", "--json"])?;
        assert_eq!(code(&shown), 0, "{shown:?}");
        let shown: Value = serde_json::from_slice(&shown.stdout.bytes)?;
        assert_eq!(
            shown["data"]["knobs"]["chair.refusal_notice"]["value"], notice,
            "{shown}"
        );
        assert_eq!(
            shown["data"]["knobs"]["chair.refusal_notice"]["source"],
            if notice == "off" {
                "override"
            } else {
                "default"
            },
            "{shown}"
        );
        let made = rig.command(&[
            "worktree",
            "create",
            "refusal-writer",
            "--base",
            "HEAD",
            "--json",
        ])?;
        assert_eq!(code(&made), 0, "{made:?}");
        let writer = rig.project.join(".worktrees/refusal-writer");
        fs::write(writer.join("tracked"), "uncommitted work")?;
        let graph = rig.root.join("graph.json");
        fs::write(
            &graph,
            serde_json::to_vec(&json!({
                "graph_id":"dirty-writer", "nodes":[{
                    "id":"writer", "use":"write", "worktree":writer,
                    "write_scope":["."]
                }], "templates":{"write":{
                    "prompt":"PROMPT_SENTINEL", "tier":"senior",
                    "access":"worktree_write", "outputs":["result"]
                }}
            }))?,
        )?;
        let run = rig.command(&[
            "run",
            "--file",
            graph.to_str().ok_or("graph path")?,
            "--json",
        ])?;
        let value: Value = serde_json::from_slice(&run.stdout.bytes)?;
        assert_ne!(code(&run), 0, "{value}");
        let identity = rig.command(&["fabric", "identity", "--json"])?;
        let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
        let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let context = ExecutionContext {
            state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
            project_id: identity["data"]["project"]
                .as_str()
                .ok_or("project")?
                .parse()?,
            actor_id: identity["data"]["inbox"]["actor"]
                .as_str()
                .ok_or("actor")?
                .parse()?,
            read_prompts: false,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = RunStore::open(&context, &call)?;
        let refusals = store.recent_refusals(20, &call)?;
        assert_eq!(refusals.len(), 1, "{value}");
        let refusal = &refusals[0];
        assert_eq!(refusal.state, OperationState::Refused);
        let diagnostic = refusal.refusal.as_ref().ok_or("diagnostic")?;
        assert_eq!(diagnostic.code, "WRITER_SCOPE");
        assert!(
            diagnostic
                .detail
                .as_ref()
                .is_some_and(|detail| !detail.is_empty())
        );
        assert!(
            diagnostic
                .next
                .as_ref()
                .is_some_and(|next| !next.is_empty())
        );
        assert_eq!(refusal.worktree.as_deref(), Some("refusal-writer"));
        assert!(refusal.refused_at.is_some());
        let wire = serde_json::to_string(refusal)?;
        assert!(!wire.contains("PROMPT_SENTINEL"));
        assert!(!wire.contains(rig.root.to_str().ok_or("fixture root")?));
        // The refusal stays observable by the graph's name: its node reads as
        // unsuccessful with a recorded reason, the listing carries the graph,
        // and a wait returns at once instead of running to its deadline.
        let status = rig.command(&["status", "dirty-writer", "--json"])?;
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        let node = status["data"]["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["kind"] == "node"))
            .ok_or("node row")?;
        assert!(
            matches!(node["state"].as_str(), Some("failed" | "blocked")),
            "{status}"
        );
        assert!(
            node["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "{status}"
        );
        let waited = rig.command(&["wait", "dirty-writer", "--timeout", "30s", "--json"])?;
        assert_ne!(code(&waited), 0, "{waited:?}");
        assert_ne!(code(&waited), 124, "{waited:?}");
        let observed = support::last_json_record(&waited.stdout.bytes)?;
        assert_eq!(observed["data"]["timed_out"], false, "{observed}");
        let listed = rig.command(&["status", "--all", "--json"])?;
        let listed: Value = serde_json::from_slice(&listed.stdout.bytes)?;
        assert!(
            listed["data"]["graphs"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["name"] == "dirty-writer")),
            "{listed}"
        );
        // A repeat submission reattaches; the refusal and its notice stay singular.
        let repeated = rig.command(&[
            "run",
            "--file",
            graph.to_str().ok_or("graph path")?,
            "--json",
        ])?;
        // The same input is the same launch: it finds its graph, exits 0 and
        // starts nothing, so the refusal and its notice stay singular.
        assert_eq!(code(&repeated), 0, "{repeated:?}");
        let repeated: Value = serde_json::from_slice(&repeated.stdout.bytes)?;
        assert_eq!(repeated["data"]["named"]["replayed"], true, "{repeated}");
        for _ in 0..2 {
            let reconciled = rig.command(&["reconcile", "--json"])?;
            assert_eq!(code(&reconciled), 0, "{reconciled:?}");
        }
        assert_eq!(store.recent_refusals(20, &call)?.len(), 1);
        assert_eq!(
            fs::read_to_string(writer.join("tracked"))?,
            "uncommitted work"
        );
        assert!(!store.has_runs(&call)?);
        let mail_call = provenant_fabric_store::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let mail = provenant_fabric_store::Store::open(&context.state_root, &mail_call)?;
        let page = mail.read_mail(
            context.project_id,
            context.actor_id,
            Some(0),
            50,
            &mail_call,
        )?;
        let notices: Vec<_> = page
            .items
            .iter()
            .filter(|item| item.message.body.contains(refusal.operation_id.as_str()))
            .collect();
        assert_eq!(notices.len(), usize::from(notice == "inbox"));
        assert!(notices.iter().all(|item| !item.message.body.contains('/')));
        assert!(store.pending_refusal_notices(20, &call)?.is_empty());
    }
    Ok(())
}

fn dirty_checkout() -> Result<(TestRig, std::path::PathBuf)> {
    let rig = TestRig::new()?;
    fs::write(rig.project.join("tracked"), "original")?;
    for args in [
        ["add", "tracked"].as_slice(),
        ["commit", "-qm", "initial"].as_slice(),
    ] {
        let output = provenant_os::fixture::git_command()
            .current_dir(&rig.project)
            .args(args)
            .output()?;
        assert!(output.status.success(), "{output:?}");
    }
    let made = rig.command(&[
        "worktree",
        "create",
        "dirty-lane",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(code(&made), 0, "{made:?}");
    let writer = rig.project.join(".worktrees/dirty-lane");
    fs::write(writer.join("tracked"), "draft")?;
    Ok((rig, writer))
}

#[test]
fn nested_deliver_dirty_writer_refuses_before_plan() -> Result {
    let (rig, writer) = dirty_checkout()?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n",
    )?;
    let worktree = format!("worktree={}", writer.display());
    let output = rig.command(&[
        "run",
        "deliver",
        "--set",
        &worktree,
        "--set",
        "write_scope=['.']",
        "--set",
        "plan=true",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_ne!(code(&output), 0, "{value}");
    assert!(value.to_string().contains("uncommitted edits"), "{value}");
    assert_eq!(fs::read_to_string(writer.join("tracked"))?, "draft");
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let context = provenant_cli::fabric::Context::open(roots, &rig.home, &rig.project)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let execution = ExecutionContext {
        state_root: context.roots.state,
        project_id: context.identity.project,
        actor_id: context.identity.inbox.actor,
        read_prompts: false,
    };
    assert!(!RunStore::open(&execution, &call)?.has_runs(&call)?);
    Ok(())
}

#[test]
fn submitted_writer_cannot_forge_preflight_warning() -> Result {
    let (rig, writer) = dirty_checkout()?;
    let graph = rig.root.join("forged-preflight.json");
    fs::write(
        &graph,
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"forged-preflight", "nodes":[{
                "id":"writer", "prompt":"write", "access":"worktree_write", "worktree":writer,
                "writer_dirty_warning":"pretend preflight passed", "route":{"alias":"junior"}
            }]}),
        )?,
    )?;
    let output = rig.command(&["run", "--file", graph.to_str().ok_or("graph")?, "--json"])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_ne!(code(&output), 0, "{value}");
    assert_eq!(value["error"]["code"], "invalid_input", "{value}");
    assert_eq!(fs::read_to_string(writer.join("tracked"))?, "draft");
    Ok(())
}

#[test]
fn wip_preflight_does_not_change_head_under_writer_custody() -> Result {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::fs::Directory;
    let (rig, writer) = dirty_checkout()?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[knobs]\n'lanes.writer_dirty'='wip_commit'\n",
    )?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let context = provenant_cli::fabric::Context::open(roots, &rig.home, &rig.project)?;
    let slot = rig
        .state
        .join("worktrees")
        .join(context.identity.project.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(&writer));
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let held = provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&slot)?,
        provenant_records::custody::Scope {
            attempt: provenant_core::AttemptId::from_bytes([42; 16]),
            generation: 1,
        },
        WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
    )?;
    let head = || -> Result<String> {
        let output = provenant_os::fixture::git_command()
            .current_dir(&writer)
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    let before = head()?;
    let graph = rig.root.join("held-writer.json");
    fs::write(
        &graph,
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"held-writer", "nodes":[{"id":"writer", "prompt":"write", "access":"worktree_write", "worktree":writer, "route":{"alias":"junior"}}]}),
        )?,
    )?;
    let output = rig.command(&["run", "--file", graph.to_str().ok_or("graph")?, "--json"])?;
    assert_ne!(code(&output), 0, "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(
        matches!(
            value["error"]["code"].as_str(),
            Some("writer_lock_busy" | "writer_lease_held")
        ),
        "{value}"
    );
    assert_eq!(head()?, before);
    assert_eq!(fs::read_to_string(writer.join("tracked"))?, "draft");
    drop(held);
    Ok(())
}
