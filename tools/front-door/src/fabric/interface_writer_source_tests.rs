//! The one writer base: a writer's identity and its read-only members read the
//! base the launch names, not the caller checkout's stale upstream.
//!
//! Spec: docs/specs/run-launch.md#inputs-and-resources

use super::tests::factory;
use super::*;
use crate::worktree_tests::support::{Result, TestRig, git};
use std::path::PathBuf;

/// A primary checkout that is behind its fetched upstream, with a linked
/// writer worktree sitting on an older commit. The read-only default would pin
/// the upstream; only the writer base may bind these launches.
struct StaleWriter {
    writer: PathBuf,
    base: String,
    upstream: String,
}

fn stale_checkout_with_writer(rig: &TestRig) -> Result<StaleWriter> {
    git(rig, &["config", "user.name", "Test"])?;
    git(rig, &["config", "user.email", "test@example.invalid"])?;
    std::fs::write(rig.project.join("file"), "one\n")?;
    git(rig, &["add", "file"])?;
    git(rig, &["commit", "-q", "-m", "one"])?;
    let stale = git(rig, &["rev-parse", "HEAD"])?;
    git(rig, &["branch", "-M", "master"])?;

    std::fs::write(rig.project.join("file"), "two\n")?;
    git(rig, &["add", "file"])?;
    git(rig, &["commit", "-q", "-m", "two"])?;
    let base = git(rig, &["rev-parse", "HEAD"])?;
    let writer = rig.root.join("writer");
    git(
        rig,
        &[
            "worktree",
            "add",
            "--detach",
            writer.to_str().ok_or("writer path")?,
            &base,
        ],
    )?;

    std::fs::write(rig.project.join("file"), "three\n")?;
    git(rig, &["add", "file"])?;
    git(rig, &["commit", "-q", "-m", "three"])?;
    let upstream = git(rig, &["rev-parse", "HEAD"])?;

    let remote = rig.root.join("remote.git");
    let remote = remote.to_str().ok_or("remote path")?;
    git(rig, &["init", "-q", "--bare", remote])?;
    git(
        rig,
        &["-C", remote, "symbolic-ref", "HEAD", "refs/heads/master"],
    )?;
    git(rig, &["remote", "add", "origin", remote])?;
    git(rig, &["push", "-q", "origin", "master"])?;
    git(rig, &["remote", "set-head", "origin", "master"])?;
    git(rig, &["reset", "-q", "--hard", &stale])?;
    Ok(StaleWriter {
        writer,
        base,
        upstream,
    })
}

/// Every plan node in an explain document, including composite and loop
/// members.
fn plan_nodes(node: &Value, found: &mut Vec<Value>) {
    if node.get("template").and_then(Value::as_str) == Some("plan") {
        found.push(node.clone());
    }
    if let Some(children) = node
        .get("composite")
        .and_then(|composite| composite.get("nodes"))
        .and_then(Value::as_array)
    {
        for child in children {
            plan_nodes(child, found);
        }
    }
    if let Some(body) = node
        .get("loop")
        .and_then(|loop_spec| loop_spec.get("body"))
        .and_then(Value::as_array)
    {
        for child in body {
            plan_nodes(child, found);
        }
    }
}

/// Regression #3870 F2: a `--worktree` writer's identity is the worktree HEAD,
/// so advancing the caller checkout between two identical launches replays the
/// same graph instead of starting NAME-2.
#[test]
fn writer_worktree_base_drives_identity_not_the_caller_head() -> Result {
    let rig = TestRig::new()?;
    let setup = stale_checkout_with_writer(&rig)?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let launch = || -> Result<Value> {
        Ok(factory.graph_document(
            json!({
                "op":"run", "template":"implement", "brief":"Fix the parser",
                "name":"writer-identity", "worktree":setup.writer, "explain":true,
                "set":{"plan":false}
            }),
            budget,
            &cancel,
        )?)
    };
    let first = launch()?;
    assert_eq!(
        first["document"]["nodes"][0]["source"],
        Value::Null,
        "the writer keeps its own base: {first}"
    );
    let digest = first["document"]["replay_digest"]
        .as_str()
        .ok_or("replay digest")?
        .to_owned();

    // Advance the primary checkout. The writer worktree has not moved.
    std::fs::write(rig.project.join("file"), "four\n")?;
    git(&rig, &["add", "file"])?;
    git(&rig, &["commit", "-q", "-m", "four"])?;
    let second = launch()?;
    assert_eq!(
        digest, second["document"]["replay_digest"],
        "the writer's identity must follow its worktree HEAD: {second}"
    );

    use provenant_fabric_runtime::graph::template::{NameDecision, decide_graph_name};
    let existing = vec!["writer-identity".to_owned()];
    let stored = |id: &str| (id == "writer-identity").then(|| digest.clone());
    match decide_graph_name("writer-identity", &digest, true, &existing, stored)? {
        NameDecision::Replay(id) => assert_eq!(id, "writer-identity"),
        other => panic!("the same launch must replay one graph, got {other:?}"),
    }
    Ok(())
}

/// Regression #3870 F1: a plan node inside a writer graph reads the writer
/// base, not the caller checkout's stale upstream.
#[test]
fn implement_plan_reads_the_writer_worktree_base() -> Result {
    let rig = TestRig::new()?;
    let setup = stale_checkout_with_writer(&rig)?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let launch = factory.graph_document(
        json!({
            "op":"run", "template":"implement", "brief":"Plan the fix", "explain":true,
            "worktree":setup.writer, "set":{"plan":true}
        }),
        budget,
        &cancel,
    )?;

    let mut found = Vec::new();
    for node in launch["document"]["nodes"]
        .as_array()
        .ok_or("document nodes")?
    {
        plan_nodes(node, &mut found);
    }
    assert_eq!(found.len(), 1, "one plan node: {launch}");
    assert_eq!(
        found[0]["source"]["commit"],
        json!(setup.base),
        "the plan must read the writer base, not the fetched upstream {}: {launch}",
        setup.upstream
    );
    assert_eq!(
        launch["read_source"]["commit"],
        Value::Null,
        "a writer launch does not pin a read-only source: {launch}"
    );
    assert!(
        !launch["warnings"].as_array().is_some_and(|warnings| {
            warnings.iter().any(|warning| {
                warning
                    .as_str()
                    .is_some_and(|text| text.contains("read-only lane will read"))
            })
        }),
        "a writer launch must not warn about a stale read-only source: {launch}"
    );
    Ok(())
}

/// Regression #3870 F1: a composite writer launch binds its plan node the same
/// way, including a `--set worktree=PATH` binding.
#[test]
fn deliver_plan_reads_the_writer_worktree_base() -> Result {
    let rig = TestRig::new()?;
    let setup = stale_checkout_with_writer(&rig)?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
    let launch = factory.graph_document(
        json!({
            "op":"run", "template":"deliver", "brief":"Deliver the fix", "explain":true,
            "set":{"plan":true, "worktree":setup.writer}
        }),
        budget,
        &cancel,
    )?;

    let mut found = Vec::new();
    for node in launch["document"]["nodes"]
        .as_array()
        .ok_or("document nodes")?
    {
        plan_nodes(node, &mut found);
    }
    assert!(
        !found.is_empty(),
        "deliver --set plan=true has a plan node: {launch}"
    );
    for plan in &found {
        assert_eq!(
            plan["source"]["commit"],
            json!(setup.base),
            "the plan must read the writer base, not the fetched upstream {}: {launch}",
            setup.upstream
        );
    }
    Ok(())
}

/// Invariant 1 (origin leg): admission records the exact head of the chosen
/// source as a full object ID with the origin and policy status projects. A
/// later push cannot change this admitted subject; a new admission resolves
/// the new head instead.
#[test]
fn admitted_subject_records_the_resolved_origin() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let head = git(&rig, &["rev-parse", "HEAD"])?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let document: provenant_fabric_runtime::graph::GraphDocument = serde_json::from_value(json!({
        "schema_version":2, "graph_id":"subject-origin", "nodes":[]
    }))?;
    let resolved = factory.admit_subject(&json!({"op":"run"}), &document, false, &cancel)?;
    let subject = resolved.subject.ok_or("an admitted graph has a subject")?;
    assert_eq!(subject.head.to_string(), head);
    assert!(matches!(
        subject.origin,
        provenant_records::subject::SubjectOrigin::LocalRef { .. }
    ));
    assert_eq!(
        subject.policy,
        provenant_records::subject::SubjectPolicy::FollowLatest
    );
    assert_eq!(resolved.read, super::super::subject::ReadPlan::Live);
    Ok(())
}

/// Regression #3870: a writer that names no base still says its identity falls
/// back to the caller checkout.
#[test]
fn writer_without_a_base_names_the_caller_checkout_fallback() {
    let base = super::super::subject::resolve_writer_base(
        &json!({"access":"worktree_write"}),
        std::path::Path::new("/project"),
        std::path::Path::new("/project"),
        std::path::Path::new("/state"),
        &provenant_cli::run::ReadSourceChoice::automatic(),
        None,
    );
    assert!(base.commit.is_none());
    let warning = base
        .fallback_warning(std::path::Path::new("/project"))
        .ok_or("fallback warning")
        .unwrap();
    assert!(warning.contains("caller checkout"), "{warning}");
}
