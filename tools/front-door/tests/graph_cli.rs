#![allow(
    clippy::disallowed_methods,
    reason = "integration tests exercise the CLI process"
)]
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
type Result<T = (), E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

#[test]
fn malformed_graph_reports_safe_output_field_and_action() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({
        "schema_version": 2,
        "graph_id": "malformed-exports",
        "nodes": [{
            "id": "inner", "kind": "composite", "access": "read_only",
            "composite": {
                "nodes": [{"id":"look", "prompt":"PROMPT_SENTINEL", "access":"read_only", "route":{"alias":"senior"}}],
                "exports": [{"node":"look", "outputs":["result", "commit"]}]
            }
        }]
    }))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["error"]["code"], "dispatch_refused", "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("composite.exports[].outputs"),
        "{value}"
    );
    assert!(
        value["error"]["repair"]
            .as_str()
            .unwrap()
            .contains("output"),
        "{value}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PROMPT_SENTINEL"));
    Ok(())
}

#[test]
fn existing_git_worktree_with_spaces_has_a_followable_branch_remedy() -> Result {
    let rig = GraphProject::new()?;
    let checkout = rig.temp.path().join("external writer's checkout");
    let git = provenant_os::fixture::git_command()
        .current_dir(&rig.project)
        .args(["worktree", "add", "-b", "topic/remedy"])
        .arg(&checkout)
        .arg("HEAD")
        .output()?;
    assert!(git.status.success(), "{git:?}");
    let (output, refusal) = rig.run(&[
        "run",
        "inspect",
        "--branch",
        "topic/remedy",
        "--base",
        "HEAD",
        "--write-scope",
        ".",
        "--wait=10s",
    ])?;
    if refusal.to_string().contains("ProbeFailed") {
        provenant_test_support::skip_sandbox_dependent_test("native confinement probe failed")?;
        return Ok(());
    }
    assert!(!output.status.success(), "{refusal}");
    let action = refusal["data"]["local_worktree_remedy"]
        .as_str()
        .ok_or_else(|| format!("missing local remedy: {refusal}"))?;
    let quoted = provenant_records::diagnostic::shell_path(
        &checkout.canonicalize()?,
        provenant_records::diagnostic::Destination::Local,
    )
    .ok_or("path not shell-safe")?;
    assert!(action.contains(&format!("--worktree {quoted}")), "{action}");
    let quoted_argument = action
        .split_once("--worktree ")
        .ok_or("missing worktree flag")?
        .1;
    let resolved = Command::new("sh")
        .args([
            "-c",
            &format!("set -- {quoted_argument}; printf '%s' \"$1\""),
        ])
        .output()?;
    assert!(resolved.status.success(), "{resolved:?}");
    let selected = String::from_utf8(resolved.stdout)?;
    assert_eq!(Path::new(&selected), checkout.canonicalize()?);
    let graph = refusal["data"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or("missing graph id")?;
    let (_, status) = rig.run(&["status", graph])?;
    assert!(
        status["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["next"]
                .as_str()
                .is_some_and(|next| next.contains(&format!("--worktree {quoted}"))))),
        "{status}"
    );
    let (_, graph_output) = rig.run(&["output", graph])?;
    assert!(
        graph_output["data"]["graph_failures"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["next"]
                .as_str()
                .is_some_and(|next| next.contains(&format!("--worktree {quoted}"))))),
        "{graph_output}"
    );
    let output = rig
        .command()
        .args(["run", "inspect", "--worktree"])
        .arg(&selected)
        .args(["--write-scope", ".", "--wait=10s"])
        .output()?;
    let followed: Value = serde_json::from_slice(&output.stdout)?;
    let binding = followed["data"]["checkpoint"]["nodes"]
        .as_array()
        .and_then(|nodes| {
            nodes
                .iter()
                .find_map(|node| node["writer_binding"].as_object())
        })
        .ok_or_else(|| format!("missing writer binding: {followed}"))?;
    assert_eq!(binding["worktree"], "[redacted]");
    let followed_id = followed["data"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or("missing graph id")?;
    let checkpoint_name = format!("graph-{followed_id}.json");
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == checkpoint_name.as_str())
        })
        .ok_or("missing private checkpoint")?;
    let mut inputs =
        provenant_core::RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = provenant_core::StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let checkpoint = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
        &state,
        project,
        followed_id,
    )?
    .ok_or("checkpoint")?;
    assert!(checkpoint.nodes.iter().any(|node| {
        node.writer_binding
            .as_ref()
            .is_some_and(|binding| binding.worktree == selected)
    }));
    Ok(())
}

fn fixture_cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_provenant"));
    provenant_os::fixture::scrub_git_env(&mut command);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PROVENANT_") {
            command.env_remove(key);
        }
    }
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

struct GraphProject {
    temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
}
impl GraphProject {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let project = temp.path().join("project");
        std::fs::create_dir(&project)?;
        for args in [
            vec!["init", "-q"],
            vec!["commit", "--allow-empty", "-qm", "initial"],
        ] {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(output.status.success(), "{output:?}");
        }
        Ok(Self {
            temp,
            root,
            project,
        })
    }
    fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }
    fn command(&self) -> Command {
        let mut command = fixture_cli();
        command
            .current_dir(&self.project)
            .env("PROVENANT_ROOT", &self.root)
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(self.state());
        command
    }
    fn run(&self, args: &[&str]) -> Result<(Output, Value)> {
        let output = self.command().args(args).output()?;
        let value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("{error}: {output:?}"))?;
        Ok((output, value))
    }
    fn document(&self, document: &Value) -> Result<PathBuf> {
        let path = self.project.join("graph.json");
        std::fs::write(&path, serde_json::to_vec(document)?)?;
        Ok(path)
    }
}

fn graph_records(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            found.extend(graph_records(&entry.path())?);
        } else if entry
            .file_name()
            .to_string_lossy()
            .ends_with(".events.jsonl")
            || entry.file_name().to_string_lossy().starts_with("graph-")
        {
            found.push(entry.path());
        }
    }
    Ok(found)
}

#[test]
fn graph_secrets_are_refused_before_publication() -> Result {
    let rig = GraphProject::new()?;
    let secret = format!("ghp_{}", "a1".repeat(18));
    for explain in [false, true] {
        for source in ["brief", "parameter", "file", "inline"] {
            let mut command = rig.command();
            command.arg("run");
            match source {
                "brief" => {
                    command.arg(&secret);
                }
                "parameter" => {
                    let templates = rig.project.join(".provenant/templates");
                    std::fs::create_dir_all(&templates)?;
                    std::fs::write(
                        templates.join("parameterised.toml"),
                        "brief='${text}'\ntier='senior'\n[parameters.text]\ntype='string'\n",
                    )?;
                    command
                        .args(["--use", "parameterised", "--set"])
                        .arg(format!("text='{secret}'"));
                }
                "file" => {
                    let path = rig.project.join("brief.txt");
                    std::fs::write(&path, &secret)?;
                    command.arg("--file").arg(path);
                }
                "inline" => {
                    let path = rig.document(&json!({"nodes":[{"id":"x","use":"inline"}],"templates":{"inline":{"brief":secret,"tier":"senior"}}}))?;
                    command.arg("--file").arg(path);
                }
                _ => unreachable!(),
            }
            if explain {
                command.arg("--explain");
            }
            let output = command.output()?;
            assert!(!output.status.success(), "{source}: {output:?}");
            assert!(!String::from_utf8_lossy(&output.stdout).contains(&secret));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(&secret));
            assert!(graph_records(&rig.state())?.is_empty());
            assert!(graph_records(&rig.project.join(".provenant/scratch"))?.is_empty());
        }
    }
    Ok(())
}

#[test]
fn explain_compiles_template_parameters_and_over_without_graph_state() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("map.toml"),
        "brief='${topic}: ${item}'\ntier='junior'\n[parameters.topic]\ntype='string'\n[parameters.item]\ntype='string'\n",
    )?;
    let (output, value) = rig.run(&[
        "run",
        "--use",
        "map",
        "--tier",
        "senior",
        "--over",
        "core,cli",
        "--set",
        "topic='Review'",
        "--explain",
    ])?;
    assert!(output.status.success(), "{value}");
    let nodes = value["data"]["document"]["nodes"]
        .as_array()
        .ok_or("nodes")?;
    assert_eq!(nodes.len(), 1);
    let copies = nodes[0]["composite"]["nodes"].as_array().ok_or("copies")?;
    assert_eq!(copies.len(), 2);
    assert_eq!(copies[0]["prompt"], "Review: core");
    assert_eq!(copies[1]["prompt"], "Review: cli");
    assert_eq!(copies[0]["route"]["alias"], "senior");
    assert!(graph_records(&rig.state())?.is_empty());
    Ok(())
}

#[test]
fn template_show_resolves_user_repository_and_inline_layers() -> Result {
    let rig = GraphProject::new()?;
    let user = rig.temp.path().join("config/provenant/templates");
    let repo = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&user)?;
    std::fs::create_dir_all(&repo)?;
    std::fs::write(user.join("task.toml"), "brief='user'\ntier='junior'\n")?;
    let (_, value) = rig.run(&["templates", "task"])?;
    assert_eq!(value["data"]["templates"][0]["template"]["brief"], "user");
    std::fs::write(
        repo.join("task.toml"),
        "brief='repository'\ntier='senior'\n",
    )?;
    let (_, value) = rig.run(&["templates", "task"])?;
    assert_eq!(
        value["data"]["templates"][0]["template"]["brief"],
        "repository"
    );
    let inline = rig.project.join("inline.toml");
    std::fs::write(
        &inline,
        "[templates.task]\nbrief='inline'\ntier='principal'\n",
    )?;
    let output = rig
        .command()
        .args(["templates", "task", "--file"])
        .arg(inline)
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{value}");
    assert_eq!(value["data"]["templates"][0]["template"]["brief"], "inline");
    Ok(())
}

#[test]
fn every_bundled_template_is_listed_shown_and_explained() -> Result {
    let rig = GraphProject::new()?;
    let (output, value) = rig.run(&["templates"])?;
    assert!(output.status.success(), "{value}");
    let listed = value["data"]["templates"].as_array().ok_or("templates")?;
    let names: Vec<_> = listed
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    let library = provenant_fabric_runtime::graph::TemplateLibrary::bundled();
    assert_eq!(names, library.names().collect::<Vec<_>>());
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/fabric-runtime/assets/templates");
    let mut expected = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("template filename")?;
        if name == "composites" {
            let composites: std::collections::BTreeMap<String, Value> =
                toml_edit::de::from_str(&std::fs::read_to_string(path)?)?;
            expected.extend(composites.into_keys());
        } else {
            expected.insert(name.to_owned());
        }
    }
    assert_eq!(
        names
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        expected.iter().map(String::as_str).collect()
    );
    for entry in listed {
        let name = entry["name"].as_str().ok_or("template name")?;
        let (output, shown) = rig.run(&["templates", name])?;
        assert!(output.status.success(), "{name}: {shown}");
        assert_eq!(shown["data"]["templates"][0], *entry, "{name}");
        let template = &entry["template"];
        let mut node = json!({"id":"exercise", "use":name});
        if name == "repair" {
            node["repeat"] = json!({"until":"accepted", "max_rounds":1, "review_repair":true});
        }
        let mut parameters = json!({});
        if template["parameters"].get("worktree").is_some() {
            parameters["worktree"] = json!(rig.project);
        } else if template["access"] == "worktree_write" {
            node["worktree"] = json!(rig.project);
            node["write_scope"] = json!(["."]);
        }
        if template["parameters"].get("write_scope").is_some() {
            parameters["write_scope"] = json!(["."]);
        }
        node["with"] = parameters;
        let mut nodes = Vec::new();
        if name == "synthesise" || template["review"] == "synthesis" {
            let source = if template["review"] == "synthesis" {
                "review"
            } else {
                "task"
            };
            nodes.push(json!({"id":"source", "use":source}));
            node["after"] = json!(["source"]);
            node["inputs"] = if template["review"] == "synthesis" {
                json!([{"node":"source", "outputs":["result", "findings"]}])
            } else {
                json!([{"node":"source", "outputs":["result"]}])
            };
        }
        nodes.push(node);
        let path =
            rig.document(&json!({"nodes":nodes, "integration_branch":"batch/integration"}))?;
        let output = rig
            .command()
            .args(["run", "--file"])
            .arg(path)
            .arg("--explain")
            .output()?;
        let explained: Value = serde_json::from_slice(&output.stdout)?;
        assert!(output.status.success(), "{name}: {explained}");
    }
    let worktree = format!("worktree='{}'", rig.project.display());
    let (output, explained) = rig.run(&[
        "run",
        "--use",
        "batch",
        "--set",
        &worktree,
        "--set",
        "write_scope=['.']",
        "--set",
        "integration_branch='batch/example'",
        "--explain",
    ])?;
    assert!(output.status.success(), "{explained}");
    assert_eq!(
        explained["data"]["document"]["integration_branch"],
        "batch/example"
    );
    Ok(())
}

#[test]
fn review_shape_selects_the_expected_template_nodes() -> Result {
    fn ids(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(id) = map.get("id").and_then(Value::as_str) {
                    out.push(id.to_owned());
                    if matches!(id, "lenses" | "verify") && map.contains_key("composite") {
                        assert_eq!(map["composite"]["min_successful"], 1);
                        if id == "verify" {
                            assert_eq!(map["composite"]["reduce_verdict"], true);
                        }
                    }
                }
                for child in map.values() {
                    ids(child, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    ids(item, out);
                }
            }
            _ => {}
        }
    }
    let rig = GraphProject::new()?;
    for (risk, expected) in [("routine", false), ("substantial", true), ("crucial", true)] {
        let path = rig.document(&json!({
            "context": {"risk_tier": risk},
            "nodes": [{"id":"review", "use":"review-panel"}]
        }))?;
        let output = rig
            .command()
            .args(["run", "--file"])
            .arg(path)
            .arg("--explain")
            .output()?;
        let result: Value = serde_json::from_slice(&output.stdout)?;
        assert!(output.status.success(), "{risk}: {result}");
        let mut found = Vec::new();
        ids(&result["data"]["document"]["nodes"], &mut found);
        for stage in ["frame", "verify", "synthesis"] {
            assert_eq!(
                found.iter().any(|id| id == stage),
                expected,
                "{risk}: {stage}"
            );
        }
    }
    Ok(())
}

#[test]
fn review_test_lens_receives_risk_selected_mutation_guidance() -> Result {
    fn collect<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if map.contains_key("review_lens") {
                    found.push(value);
                }
                for child in map.values() {
                    collect(child, found);
                }
            }
            Value::Array(items) => {
                for child in items {
                    collect(child, found);
                }
            }
            _ => {}
        }
    }
    let rig = GraphProject::new()?;
    for (risk, expected) in [
        ("substantial", false),
        ("crucial", true),
        ("terminal", true),
    ] {
        let path = rig.document(&json!({
            "context": {"risk_tier": risk},
            "nodes": [{"id":"review", "use":"review-panel"}]
        }))?;
        let output = rig
            .command()
            .args(["run", "--file"])
            .arg(path)
            .arg("--explain")
            .output()?;
        let result: Value = serde_json::from_slice(&output.stdout)?;
        assert!(output.status.success(), "{risk}: {result}");
        let mut reviews = Vec::new();
        collect(&result["data"]["document"], &mut reviews);
        let tests = reviews
            .iter()
            .find(|node| node["review_lens"] == "tests")
            .ok_or("tests lens")?;
        assert_eq!(
            tests["prompt"]
                .as_str()
                .ok_or("prompt")?
                .contains("Diff-scoped mutation evidence"),
            expected,
            "{risk}"
        );
        for other in reviews.iter().filter(|node| node["review_lens"] != "tests") {
            assert!(
                !other["prompt"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Diff-scoped mutation evidence")
            );
        }
    }
    Ok(())
}

#[test]
fn invalid_template_documents_are_refused_by_list_and_show() -> Result {
    let rig = GraphProject::new()?;
    let repo = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&repo)?;
    std::fs::write(
        repo.join("broken.toml"),
        "brief='leaf'\nnodes=[{id='child',use='task'}]\n",
    )?;
    for args in [vec!["templates"], vec!["templates", "broken"]] {
        let (output, _) = rig.run(&args)?;
        assert!(!output.status.success());
    }
    let (output, refusal) = rig.run(&["run", "--use", "broken", "inspect"])?;
    assert!(!output.status.success(), "{refusal}");
    assert_eq!(refusal["error"]["code"], "invalid_input", "{refusal}");
    assert!(refusal["error"]["repair"].is_string(), "{refusal}");
    let detail = refusal["error"]["message"]
        .as_str()
        .ok_or("missing template reason")?;
    assert!(
        detail.contains("templates") && detail.contains("validated"),
        "{refusal}"
    );
    assert!(graph_records(&rig.state())?.is_empty());
    Ok(())
}

#[test]
fn deterministic_graph_wait_status_trace_and_replay_share_one_journal() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":1,"graph_id":"gate-replay","nodes":[{"id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let run = || -> Result<Value> {
        let output = rig
            .command()
            .args(["run", "--file"])
            .arg(&path)
            .arg("--wait=10s")
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(output.status.success(), "{value}");
        Ok(value["data"].clone())
    };
    let first = run()?;
    assert_eq!(first["status"], "complete");
    let (output, status) = rig.run(&["status", "gate-replay"])?;
    assert!(output.status.success(), "{status}");
    let rows = status["data"]["rows"].as_array().ok_or("status rows")?;
    let graph = rows
        .iter()
        .find(|row| row["kind"] == "graph")
        .ok_or("graph row")?;
    let node = rows
        .iter()
        .find(|row| row["kind"] == "node")
        .ok_or("node row")?;
    assert_eq!(graph["state"], first["status"]);
    assert_eq!(node["state"], first["checkpoint"]["nodes"][0]["state"]);
    let second = run()?;
    assert_eq!(second["checkpoint"]["nodes"], first["checkpoint"]["nodes"]);
    let (output, trace) = rig.run(&["output", "gate-replay", "--trace"])?;
    assert!(output.status.success(), "{trace}");
    assert!(
        !trace["data"]["events"]
            .as_array()
            .ok_or("events")?
            .is_empty()
    );
    Ok(())
}

#[test]
fn explain_refuses_document_read_roots_outside_the_project() -> Result {
    let rig = GraphProject::new()?;
    let outside = rig.temp.path().join("outside");
    std::fs::create_dir(&outside)?;
    let path =
        rig.document(&json!({"nodes":[{"id":"read","use":"task","read_roots":[outside]}]}))?;
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(path)
        .arg("--explain")
        .output()?;
    assert!(!output.status.success(), "{output:?}");
    assert!(graph_records(&rig.state())?.is_empty());
    Ok(())
}

#[test]
fn oversized_graph_expansion_is_refused_before_journalling() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"nodes":[{"id":"swarm","use":"task","over":(0..257).map(|n|format!("area-{n}")).collect::<Vec<_>>()}]}))?;
    for explain in [false, true] {
        let mut command = rig.command();
        command.args(["run", "--file"]).arg(&path);
        if explain {
            command.arg("--explain");
        }
        let output = command.output()?;
        assert!(!output.status.success());
        assert!(graph_records(&rig.state())?.is_empty());
    }
    Ok(())
}

#[test]
fn steer_graph_and_node_refuse_ambiguous_live_attempts_before_reading_stdin() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":1,"graph_id":"steer-ambiguous","nodes":[{"id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(path)
        .arg("--wait=10s")
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "graph-steer-ambiguous.json")
        })
        .ok_or("checkpoint")?;
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, NodeState};
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let execution =
        FileJournal::execution(&state, project, "steer-ambiguous")?.ok_or("execution")?;
    let mut checkpoint =
        FileJournal::read_checkpoint(&state, project, "steer-ambiguous")?.ok_or("checkpoint")?;
    let attempt = json!({"key":{"graph_id":"steer-ambiguous","node_id":"accept","route_index":0},
        "launched":false,"route":{"alias":"senior"},"state":"starting","attempt_ref":null,
        "outcome":null,"launch_actor":"fixture"});
    let mut other = attempt.clone();
    other["key"]["route_index"] = json!(1);
    checkpoint.nodes[0].state = NodeState::Running;
    checkpoint.nodes[0].attempts = serde_json::from_value(json!([attempt, other]))?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5)?;
    let mut journal =
        FileJournal::open(&state, project, "steer-ambiguous", budget, &cancel, &clock)?;
    journal.bind(execution)?;
    journal.save(&checkpoint)?;
    drop(journal);
    for target in ["steer-ambiguous", "steer-ambiguous/accept"] {
        let (output, value) = rig.run(&["steer", target, "--text-stdin"])?;
        assert!(!output.status.success(), "{value}");
        assert_eq!(value["status"], "refused", "{value}");
        assert_eq!(value["error"]["code"], "steer_target_refused", "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("several live attempts")),
            "{value}"
        );
    }
    Ok(())
}

#[test]
fn unreadable_graph_file_is_refused_by_real_cli_before_admission() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if provenant_os::fs::effective_uid() == 0 {
        return Ok(());
    }
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":1,"graph_id":"unreadable","nodes":[]}))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))?;
    let output = rig.command().args(["run", "--file"]).arg(&path).output()?;
    assert!(!output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["error"]["code"], "invalid_input", "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("input could not be read"),
        "{value}"
    );
    let human = fixture_cli()
        .current_dir(&rig.project)
        .env("PROVENANT_ROOT", &rig.root)
        .env("XDG_CONFIG_HOME", rig.temp.path().join("config"))
        .arg("--state-dir")
        .arg(rig.state())
        .args(["run", "--file"])
        .arg(&path)
        .output()?;
    assert!(!human.status.success(), "{human:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&human.stdout),
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(text.contains("input could not be read"), "{text}");
    assert!(graph_records(&rig.state())?.is_empty());
    Ok(())
}

#[test]
fn fake_launch_owner_refusal_reaches_real_graph_status() -> Result {
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::coordination::facade::{FacadeError, InvalidInputDetail};
    use provenant_fabric_runtime::graph::{
        self, AccessMode, AttemptKey, AttemptObservation, AttemptOwner, FileJournal, GraphDocument,
        GraphExecution, GraphRoute, NodeInput, Replay,
    };

    struct RefusingOwner(Option<InvalidInputDetail>);
    impl AttemptOwner for RefusingOwner {
        type Error = FacadeError;
        fn actor(&self) -> String {
            "actor".into()
        }
        fn observe_or_start(
            &mut self,
            _: &AttemptKey,
            _: NodeInput<'_>,
            _: &GraphRoute,
            _: AccessMode,
            _: Option<&str>,
            _: Replay<'_>,
        ) -> std::result::Result<AttemptObservation, Self::Error> {
            Err(FacadeError::InvalidInput {
                detail: self.0.clone(),
            })
        }
    }
    let rig = GraphProject::new()?;
    let seed = rig.document(&json!({"schema_version":1,"graph_id":"seed","nodes":[{"id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(seed)
        .arg("--wait=10s")
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let checkpoint = graph_records(&rig.state())?
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n == "graph-seed.json"))
        .ok_or("seed checkpoint")?;
    let project = checkpoint
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .ok_or("project id")?
        .parse()?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    for (id, fact, expected) in [
        (
            "schema-refusal",
            Some(InvalidInputDetail::Schema),
            "schema_version",
        ),
        ("code-only-refusal", None, "invalid_input"),
    ] {
        let document: GraphDocument = serde_json::from_value(
            json!({"schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"fixture","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]}),
        )?;
        let mut journal = FileJournal::open(
            &state,
            project,
            id,
            WaitBudget::cli(clock.mono_now(), 5)?,
            &cancel,
            &clock,
        )?;
        journal.bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: rig.project.clone(),
            document: document.clone(),
        })?;
        let run = graph::advance_with_budget(
            &document,
            &mut RefusingOwner(fact),
            &mut journal,
            &provenant_fabric_runtime::config::GraphLimits::default(),
            &provenant_fabric_runtime::config::GraphBudget::default(),
        )?;
        assert_eq!(
            run.checkpoint.nodes[0].advance_error_code.as_deref(),
            Some("launch_refused")
        );
        assert_eq!(
            run.checkpoint.nodes[0].advance_owner_code.as_deref(),
            Some("invalid_input")
        );
        assert!(
            run.checkpoint.nodes[0]
                .advance_error
                .as_deref()
                .unwrap()
                .contains(expected)
        );
        drop(journal);
        let (_, machine) = rig.run(&["status", id])?;
        let rows = machine["data"]["rows"].as_array().ok_or("status rows")?;
        let row = rows
            .iter()
            .find(|row| row["kind"] == "node")
            .ok_or("node row")?;
        assert_eq!(row["cause_code"], "invalid_input", "{machine}");
        assert!(
            row["reason"].as_str().unwrap().contains("invalid_input"),
            "{machine}"
        );
        assert!(
            row["reason"].as_str().unwrap().contains(expected),
            "{machine}"
        );
        assert!(row["next"].as_str().unwrap().contains("check"), "{machine}");
        let (_, output) = rig.run(&["output", id])?;
        let failure = &output["data"]["graph_failures"][0];
        assert_eq!(failure["cause_code"], row["cause_code"], "{output}");
        assert_eq!(failure["reason"], row["reason"], "{output}");
        assert_eq!(failure["next"], row["next"], "{output}");
        let human = fixture_cli()
            .current_dir(&rig.project)
            .env("PROVENANT_ROOT", &rig.root)
            .env("XDG_CONFIG_HOME", rig.temp.path().join("config"))
            .arg("--state-dir")
            .arg(rig.state())
            .args(["status", id, "--full"])
            .output()?;
        assert!(human.status.success(), "{human:?}");
        let text = String::from_utf8(human.stdout)?;
        assert!(text.contains(expected) && text.contains("check"), "{text}");
    }
    Ok(())
}
