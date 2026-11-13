//! A dirty writer is refused before admission but remains observable by its operation.
#![allow(clippy::disallowed_methods)]
mod support;
use provenant_core::{CancelToken, RootInputs};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, OperationState, Request, RunStore};
use serde_json::{Value, json};
use std::{
    fs,
    time::{Duration, Instant},
};
use support::{Result, TestRig, code};

#[test]
fn dirty_writer_refusal_is_durable_visible_and_notified_once() -> Result {
    for notice in ["inbox", "off"] {
        let rig = TestRig::new()?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        fs::write(rig.project.join("tracked"), "original")?;
        for args in [vec!["add", "tracked"], vec!["commit", "-qm", "initial"]] {
            let output = rig.execute(&git, &args, None)?;
            assert_eq!(code(&output), 0, "{output:?}");
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
                    "brief":"PROMPT_SENTINEL", "tier":"senior",
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
        let target = format!("operation:{}", refusal.operation_id);
        let next = diagnostic.next.as_deref().ok_or("recovery action")?;
        for args in [vec!["status", target.as_str()], vec!["status"]] {
            let human = rig.command(&args)?;
            let stdout = std::str::from_utf8(&human.stdout.bytes)?;
            assert!(stdout.contains(next), "{stdout}");
        }
        let start = Instant::now();
        let waited = rig.command(&["status", &target, "--wait=30s", "--json"])?;
        assert_eq!(code(&waited), 1, "{waited:?}");
        assert!(start.elapsed() < Duration::from_secs(2));
        let observed: Value = serde_json::from_slice(&waited.stdout.bytes)?;
        assert_eq!(
            observed["data"]["operation"]["refusal"],
            serde_json::to_value(diagnostic)?
        );
        let listed = rig.command(&["status", "--json"])?;
        let listed: Value = serde_json::from_slice(&listed.stdout.bytes)?;
        assert!(
            listed["data"]["refused_operations"]
                .as_array()
                .is_some_and(|rows| rows
                    .iter()
                    .any(|row| row["operation_id"] == refusal.operation_id.as_str())),
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
