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

/// Regression #3870: a writer that names no base still says its identity falls
/// back to the caller checkout.
#[test]
fn writer_without_a_base_names_the_caller_checkout_fallback() {
    let base = super::super::writer_base::resolve_writer_base(
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

/// Declare the one forge context a graph with `delivery: push` needs.
fn declare_forge(rig: &TestRig) -> Result {
    std::fs::create_dir_all(rig.project.join(".provenant"))?;
    std::fs::write(
        rig.project.join(provenant_config::PORTS_CONFIG),
        serde_json::to_vec(&json!({
            "schema_version": provenant_config::PORTS_SCHEMA_VERSION,
            "git": {"remote":"origin", "default_branch":"main", "protected":[]},
        }))?,
    )?;
    Ok(())
}

/// A primary checkout whose `main` is pushed to `origin`, plus a linked
/// worktree on `fix` tracking `origin/main` with one new commit. The base is
/// where `fix` diverged; the head is the writer's pushed subject.
fn reused_worktree_with_upstream(rig: &TestRig) -> Result<(PathBuf, String, String)> {
    git(rig, &["config", "user.name", "Test"])?;
    git(rig, &["config", "user.email", "test@example.invalid"])?;
    let remote = rig.root.join("remote.git");
    let remote = remote.to_str().ok_or("remote path")?;
    git(rig, &["init", "-q", "--bare", remote])?;
    git(
        rig,
        &["-C", remote, "symbolic-ref", "HEAD", "refs/heads/main"],
    )?;
    git(rig, &["remote", "add", "origin", remote])?;
    git(rig, &["push", "-q", "origin", "main"])?;
    git(rig, &["remote", "set-head", "origin", "main"])?;
    let base = git(rig, &["rev-parse", "HEAD"])?;
    let writer = rig.root.join("reused-writer");
    let writer_text = writer.to_str().ok_or("writer path")?;
    git(
        rig,
        &["worktree", "add", "-q", "-b", "fix", writer_text, &base],
    )?;
    git(
        rig,
        &[
            "-C",
            writer_text,
            "branch",
            "--set-upstream-to=origin/main",
            "fix",
        ],
    )?;
    std::fs::write(writer.join("change"), "one\n")?;
    git(rig, &["-C", writer_text, "add", "change"])?;
    git(
        rig,
        &["-C", writer_text, "commit", "-q", "-m", "one new commit"],
    )?;
    let head = git(rig, &["-C", writer_text, "rev-parse", "HEAD"])?;
    Ok((writer, base, head))
}

/// The graph shape from the issue: an implement node reusing a worktree and a
/// findings-panel sourcing that node, with no authored review base.
fn reused_writer_panel(writer: &std::path::Path) -> Value {
    json!({
        "op":"run", "explain":true,
        "document":{
            "schema_version":2, "graph_id":"reused-writer-panel",
            "nodes":[
                {"id":"implement","use":"implement","with":{"plan":false},"worktree":writer,"write_scope":["."],"delivery":"push"},
                {"id":"review","use":"findings-panel","after":["implement"],"source":{"type":"node","node":"implement"}}
            ]
        }
    })
}

/// Regression #4064: a reused-worktree writer resolves its base from the
/// branch's upstream, and the findings-panel reads base..pushed head instead
/// of failing every lens on an empty range.
#[test]
fn reused_worktree_writer_pins_a_non_empty_panel_range() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let (writer, base, head) = reused_worktree_with_upstream(&rig)?;
    declare_forge(&rig)?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let response = factory.graph_document(reused_writer_panel(&writer), budget, &cancel)?;
    assert_eq!(response["writer_base"]["commit"], json!(base), "{response}");
    assert_eq!(
        response["writer_base"]["source"],
        json!("the reused worktree branch's upstream"),
        "{response}"
    );
    assert_eq!(
        response["document"]["review_base"],
        json!(base),
        "{response}"
    );
    // The same comparison the launch owner makes for every lens: it must
    // resolve a base that is not the subject commit.
    let range_base = factory.review_comparison_base(&writer, Some(&base), &head)?;
    assert_eq!(range_base, base);
    assert_ne!(range_base, head);
    Ok(())
}

/// Regression #4064: the node accepts a `base` and its source is reported.
#[test]
fn authored_writer_base_is_accepted_and_named_in_explain() -> Result {
    let rig = crate::worktree_tests::repository()?;
    let (writer, base, _head) = reused_worktree_with_upstream(&rig)?;
    declare_forge(&rig)?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut input = reused_writer_panel(&writer);
    input["document"]["nodes"][0]["base"] = json!(base);
    let response = factory.graph_document(input, budget, &cancel)?;
    assert_eq!(response["writer_base"]["commit"], json!(base), "{response}");
    assert_eq!(
        response["writer_base"]["source"],
        json!("the writer node's base"),
        "{response}"
    );
    Ok(())
}

/// Regression #4064: a node-sourced panel with no resolvable base refuses the
/// graph at launch, rather than failing lens by lens at execution.
#[test]
fn node_sourced_panel_without_a_base_refuses_before_launch() -> Result {
    let rig = crate::worktree_tests::repository()?;
    git(rig, &["config", "user.name", "Test"])?;
    git(rig, &["config", "user.email", "test@example.invalid"])?;
    let writer = rig.root.join("no-base-writer");
    git(
        rig,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            writer.to_str().ok_or("writer path")?,
        ],
    )?;
    declare_forge(&rig)?;
    let factory = factory(&rig)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let error = factory
        .graph_document(reused_writer_panel(&writer), budget, &cancel)
        .unwrap_err();
    assert!(
        matches!(error, FacadeError::InvalidInput { detail: Some(InvalidInputDetail::Field(ref message)) } if message.contains("review_base_unresolved")),
        "{error:?}"
    );
    Ok(())
}
