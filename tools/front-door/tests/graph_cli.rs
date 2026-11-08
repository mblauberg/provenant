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
fn file_briefs_reach_the_compiled_prompt_or_refuse_before_launch() -> Result {
    use std::io::Write;
    let rig = GraphProject::new()?;
    let brief = "# Brief at a named commit\n# Implement the task\n";
    for path in [
        rig.temp.path().join("jr-census-api.ABC123"),
        rig.project.join("brief.md"),
        rig.temp.path().join("brief.json"),
    ] {
        std::fs::write(&path, brief)?;
        let output = rig
            .command()
            .args(["run", "--route", "junior", "--file"])
            .arg(&path)
            .arg("--explain")
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(output.status.success(), "{value}");
        assert_eq!(value["data"]["document"]["nodes"][0]["prompt"], brief);
    }
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(rig.project.join("brief.md"))
        .arg("--explain")
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{value}");
    assert_eq!(value["data"]["document"]["nodes"][0]["prompt"], brief);
    let graph = rig.document(&json!({"schema_version":2,"graph_id":"file-graph","nodes":[{"id":"task","prompt":"graph prompt","access":"read_only","route":{"alias":"junior"}}]}))?;
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(graph)
        .arg("--explain")
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{value}");
    assert_eq!(
        value["data"]["document"]["nodes"][0]["prompt"],
        "graph prompt"
    );
    let path = rig.temp.path().join("structured.toml");
    std::fs::write(&path, "task = 'do this'\n")?;
    let output = rig
        .command()
        .args(["run", "--route", "junior", "--file"])
        .arg(&path)
        .arg("--explain")
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        value["data"]["document"]["nodes"][0]["prompt"], "task = 'do this'\n",
        "{value}"
    );

    for (name, contents) in [
        ("empty", &b""[..]),
        ("whitespace", &b" \n\t"[..]),
        ("invalid-utf8", &b"\xff"[..]),
    ] {
        let path = rig.temp.path().join(name);
        std::fs::write(&path, contents)?;
        let output = rig
            .command()
            .args(["run", "--route", "junior", "--file"])
            .arg(&path)
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(!output.status.success(), "{value}");
        assert!(
            value["error"]["message"].as_str().unwrap().contains(name),
            "{value}"
        );
        assert!(graph_records(&rig.state())?.is_empty());
    }
    for path in [
        rig.temp.path().join("missing"),
        rig.temp.path().to_path_buf(),
    ] {
        let output = rig
            .command()
            .args(["run", "--route", "junior", "--file"])
            .arg(&path)
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(!output.status.success(), "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains(path.to_str().unwrap()),
            "{value}"
        );
        assert!(graph_records(&rig.state())?.is_empty());
    }
    let mut child = rig
        .command()
        .args([
            "run",
            "--route",
            "junior",
            "--file",
            "/dev/stdin",
            "--explain",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(brief.as_bytes())?;
    let output = child.wait_with_output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{value}");
    assert_eq!(value["data"]["document"]["nodes"][0]["prompt"], brief);
    Ok(())
}

#[test]
fn explicit_graph_file_keeps_nodes_and_dependencies_with_route_override() -> Result {
    let rig = GraphProject::new()?;
    let graph = rig.document(&json!({
        "schema_version": 2,
        "graph_id": "routed-file-graph",
        "nodes": [
            {"id":"first","prompt":"first task","access":"read_only","route":{"alias":"junior"}},
            {"id":"second","prompt":"second task","access":"read_only","route":{"alias":"junior"},"depends_on":["first"]}
        ]
    }))?;
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(graph)
        .args(["--route", "senior", "--explain"])
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{value}");
    let document = &value["data"]["document"];
    assert_eq!(document["graph_id"], "routed-file-graph");
    let nodes = document["nodes"].as_array().ok_or("missing nodes")?;
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["id"], "first");
    assert_eq!(nodes[0]["prompt"], "first task");
    assert_eq!(nodes[1]["id"], "second");
    assert_eq!(nodes[1]["prompt"], "second task");
    assert_eq!(nodes[1]["depends_on"], json!(["first"]));
    for node in nodes {
        assert_eq!(node["route"]["alias"], "senior");
    }
    assert!(graph_records(&rig.state())?.is_empty());
    Ok(())
}

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
        "--operation-id",
        "worktree-remedy",
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
    let (_, replay) = rig.run(&[
        "run",
        "inspect",
        "--branch",
        "topic/remedy",
        "--base",
        "HEAD",
        "--operation-id",
        "worktree-remedy",
        "--write-scope",
        ".",
        "--wait=10s",
    ])?;
    assert_eq!(replay["data"]["checkpoint"]["graph_id"], graph, "{replay}");
    assert_eq!(replay["data"]["local_worktree_remedy"], action, "{replay}");
    let (_, status) = rig.run(&["status", graph])?;
    assert!(
        status["data"]["rows"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["kind"] == "node" && row["cause_code"] == "dispatch_refused")),
        "{status}"
    );
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
        } else if matches!(
            entry.file_name().to_str(),
            Some("graph-template-cutover.lock" | "graph-template-cutover-complete")
        ) {
            // The one-shot launch guard is state metadata, not a graph journal.
            continue;
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
fn corrupt_graph_listing_gives_a_working_retirement_command() -> Result {
    let rig = GraphProject::new()?;
    for id in ["broken", "healthy"] {
        let path = rig.document(&json!({"schema_version":1,"graph_id":id,"nodes":[{"id":"gate","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
        let output = rig
            .command()
            .args(["run", "--file"])
            .arg(path)
            .arg("--wait=10s")
            .output()?;
        assert!(output.status.success(), "{output:?}");
    }
    let broken = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "graph-broken.json")
        })
        .ok_or("broken checkpoint")?;
    std::fs::write(&broken, b"PRIVATE_CONTENT_SENTINEL")?;
    let (output, listing) = rig.run(&["status"])?;
    assert!(output.status.success(), "{listing}");
    let rows = listing["data"]["rows"].as_array().ok_or("rows")?;
    let bad = rows
        .iter()
        .find(|row| row["id"] == "broken")
        .ok_or("broken row")?;
    assert_eq!(bad["state"], "unreadable");
    let reason = bad["reason"].as_str().ok_or("reason")?;
    assert!(reason.contains("provenant stop -- broken"), "{reason}");
    assert!(reason.contains("Other graphs"), "{reason}");
    assert!(rows.iter().any(|row| row["id"] == "healthy"));
    assert!(!listing.to_string().contains("PRIVATE_CONTENT_SENTINEL"));
    assert!(
        !listing
            .to_string()
            .contains(&rig.state().display().to_string())
    );
    let human = fixture_cli()
        .current_dir(&rig.project)
        .env("PROVENANT_ROOT", &rig.root)
        .env("XDG_CONFIG_HOME", rig.temp.path().join("config"))
        .arg("--state-dir")
        .arg(rig.state())
        .arg("status")
        .output()?;
    assert!(human.status.success(), "{human:?}");
    let text = String::from_utf8(human.stdout)?;
    assert!(text.contains("provenant stop -- broken"), "{text}");
    assert!(text.contains("healthy"), "{text}");
    assert!(!text.contains("PRIVATE_CONTENT_SENTINEL"));
    assert!(!text.contains(&rig.state().display().to_string()));
    let (output, status) = rig.run(&["status", "broken"])?;
    assert!(output.status.success(), "{status}");
    assert!(
        status.to_string().contains("provenant stop -- broken"),
        "{status}"
    );
    assert_eq!(
        status["data"]["rows"][0]["next"],
        "provenant stop -- broken"
    );
    assert!(!status.to_string().contains("PRIVATE_CONTENT_SENTINEL"));
    assert!(
        !status
            .to_string()
            .contains(&rig.state().display().to_string())
    );
    let (output, stopped) = rig.run(&["stop", "--", "broken"])?;
    assert!(output.status.success(), "{stopped}");
    let (_, after) = rig.run(&["status"])?;
    let rows = after["data"]["rows"].as_array().ok_or("rows after stop")?;
    assert!(!rows.iter().any(|row| row["id"] == "broken"));
    assert!(rows.iter().any(|row| row["id"] == "healthy"));
    Ok(())
}

#[test]
fn graph_secrets_are_refused_before_publication() -> Result {
    let rig = GraphProject::new()?;
    let secret = format!("ghp_{}", "a1".repeat(18));
    for explain in [false, true] {
        for source in ["brief", "review", "parameter", "file", "inline"] {
            let mut command = rig.command();
            command.arg("run");
            match source {
                "brief" => {
                    command.arg(&secret);
                }
                "review" => {
                    command.arg(&secret).args(["--use", "review"]);
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
fn explain_shows_the_resolved_review_lens_tier() -> Result {
    fn lenses<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if map.contains_key("review_lens") {
                    found.push(value);
                }
                for child in map.values() {
                    lenses(child, found);
                }
            }
            Value::Array(items) => {
                for child in items {
                    lenses(child, found);
                }
            }
            _ => {}
        }
    }
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({
        "review": {"shape":"quick", "lenses":[{"name":"tests", "tier":"principal"}]},
        "nodes": [{"id":"review", "use":"review-panel"}]
    }))?;
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(path)
        .arg("--explain")
        .output()?;
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{result}");
    let mut found = Vec::new();
    lenses(&result["data"]["document"]["nodes"], &mut found);
    assert_eq!(found.len(), 1, "{result}");
    assert_eq!(found[0]["review_lens"], "tests");
    assert_eq!(found[0]["routes"][0]["alias"], "principal");
    assert!(!found[0].to_string().contains("${tier}"));
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
fn trace_reports_recorded_patch_and_attempt_in_text_and_json() -> Result {
    use std::io::Write;
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":1,"graph_id":"trace-test","nodes":[{"id":"gate","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(path)
        .arg("--wait=10s")
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let journal = graph_records(&rig.state())?
        .into_iter()
        .find(|p| p.ends_with("trace-test.events.jsonl"))
        .ok_or("journal missing")?;
    let mut file = std::fs::OpenOptions::new().append(true).open(journal)?;
    for event in [
        json!({"timestamp_micros":1,"kind":"patch","subject":"host","code":"admitted","digest":"d","details":{"revision":2,"patch_digest":"p"}}),
        json!({"timestamp_micros":2,"kind":"attempt","subject":"gate","code":"settled","digest":"d","details":{"state":"terminal"},"attempt":{"identity":"a","tier":"senior","template":"raw","tokens":100,"cost_micro_usd":null,"wall_time_seconds":5,"has_usage":true,"cache_read_tokens":20,"input_tokens":40,"queue_time_ms":12,"outcome":"succeeded","route":{"model":"m"}}}),
        json!({"timestamp_micros":3,"kind":"attempt","subject":"gate","code":"started","digest":"d","details":{"state":"running"},"attempt":{"identity":"b","tier":"junior","template":"raw","tokens":null,"cost_micro_usd":null,"wall_time_seconds":null,"has_usage":false,"cache_read_tokens":null,"input_tokens":null,"queue_time_ms":null,"outcome":null,"route":{}}}),
    ] {
        writeln!(file, "{event}")?;
    }
    let (output, help) = rig.run(&["help", "--all"])?;
    assert!(output.status.success());
    assert!(
        help["data"]["commands"]
            .as_array()
            .ok_or("commands")?
            .iter()
            .any(|c| c["command"] == "provenant trace")
    );
    let (output, envelope) = rig.run(&["trace", "trace-test"])?;
    assert!(output.status.success(), "{envelope}");
    assert_eq!(envelope["command"], "trace");
    let events = envelope["data"]["events"].as_array().ok_or("events")?;
    assert!(events.iter().any(|e| e["details"]["revision"] == 2));
    let attempt = events
        .iter()
        .find(|e| e["attempt"]["identity"] == "a")
        .ok_or("attempt")?;
    assert_eq!(attempt["attempt"]["cache_read_share"], 0.5);
    assert_eq!(attempt["attempt"]["route"]["model"], "m");
    assert_eq!(attempt["attempt"]["tier"], "senior");
    assert_eq!(attempt["details"]["state"], "terminal");
    assert_eq!(attempt["attempt"]["tokens"], 100);
    assert_eq!(attempt["attempt"]["wall_time_seconds"], 5);
    assert_eq!(attempt["attempt"]["queue_time_ms"], 12);
    assert_eq!(attempt["attempt"]["outcome"], "succeeded");
    let unknown = events
        .iter()
        .find(|e| e["attempt"]["identity"] == "b")
        .ok_or("unknown attempt")?;
    assert!(unknown["attempt"]["cache_read_share"].is_null());
    assert!(unknown["attempt"]["tokens"].is_null());
    let output = fixture_cli()
        .current_dir(&rig.project)
        .env("PROVENANT_ROOT", &rig.root)
        .env("XDG_CONFIG_HOME", rig.temp.path().join("config"))
        .arg("--state-dir")
        .arg(rig.state())
        .args(["trace", "trace-test"])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<Value> = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(&lines, events);
    Ok(())
}

#[test]
fn content_rerun_reattaches_and_fresh_run_after_cancel_has_a_new_identity() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    for identity in ["content", "fresh"] {
        std::fs::write(
            templates.join(format!("{identity}.toml")),
            format!("identity='{identity}'\nkind='gate'\n[node.condition]\ntype='budget'\n"),
        )?;
    }
    let run = |template: &str| -> Result<Value> {
        let (output, result) = rig.run(&["run", "--use", template, "--wait=10s"])?;
        assert!(output.status.success(), "{result}");
        Ok(result["data"].clone())
    };
    let first = run("content")?;
    let id = first["checkpoint"]["graph_id"].as_str().ok_or("graph ID")?;
    assert_eq!(first["status"], "complete");
    assert_eq!(run("content")?["checkpoint"], first["checkpoint"]);
    let (output, stopped) = rig.run(&["stop", id])?;
    assert!(output.status.success(), "{stopped}");
    let (output, replay) = rig.run(&["run", "--use", "content", "--wait=10s"])?;
    assert!(!output.status.success(), "{replay}");
    let replay = &replay["data"];
    assert_eq!(replay["checkpoint"]["graph_id"], id);
    assert_eq!(replay["checkpoint"]["cancelled"], true);

    let fresh = run("fresh")?;
    let fresh_id = fresh["checkpoint"]["graph_id"].as_str().ok_or("fresh ID")?;
    assert_ne!(fresh_id, id);
    let (output, stopped) = rig.run(&["stop", fresh_id])?;
    assert!(output.status.success(), "{stopped}");
    let rerun = run("fresh")?;
    assert_ne!(rerun["checkpoint"]["graph_id"], fresh_id);
    assert_eq!(rerun["checkpoint"]["cancelled"], false);
    assert_eq!(rerun["status"], "complete");
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

/// Drive one `fabric_run` MCP tool call through a real server process.
fn mcp_run(rig: &GraphProject, arguments: Value) -> Result<Value> {
    use std::io::Write;
    let frames = [
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fabric_run","arguments":arguments}}),
    ];
    let input = frames
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n")
        + "\n";
    let mut command = fixture_cli();
    command
        .current_dir(&rig.project)
        .env("PROVENANT_ROOT", &rig.root)
        .env("XDG_CONFIG_HOME", rig.temp.path().join("config"))
        .arg("--state-dir")
        .arg(rig.state())
        .args(["fabric", "mcp", "--client", "codex"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or("MCP stdin")?
        .write_all(input.as_bytes())?;
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for line in output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: Value = serde_json::from_slice(line)?;
        if value["id"] == 1 {
            return Ok(value["result"].clone());
        }
    }
    Err("missing MCP reply".into())
}

/// One state directory that a compile failure can be observed against.
fn prepare_state(rig: &GraphProject) -> Result {
    std::fs::create_dir_all(rig.state())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(rig.state(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Assert that the template CLI, a raw document on the CLI and the MCP tool
/// all refuse the same compile failure with `expected` and journal nothing.
fn assert_compile_refusal_matches(
    rig: &GraphProject,
    templates: &[(&str, String)],
    name: &str,
    document: &Value,
    config: &str,
    expected: &str,
) -> Result {
    let directory = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&directory)?;
    for (file, text) in templates {
        std::fs::write(directory.join(format!("{file}.toml")), text)?;
    }
    std::fs::write(rig.state().join("config.toml"), config)?;
    let path = rig.document(document)?;
    let mut messages = Vec::new();
    for args in [
        vec!["run", "--use", name, "--explain"],
        vec!["run", "--use", name],
    ] {
        let (output, value) = rig.run(&args)?;
        assert!(!output.status.success(), "{name} {args:?}: {value}");
        messages.push(
            value["error"]["message"]
                .as_str()
                .ok_or("message")?
                .to_owned(),
        );
    }
    for explain in [true, false] {
        let mut command = rig.command();
        command.args(["run", "--file"]).arg(&path);
        if explain {
            command.arg("--explain");
        }
        let output = command.output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(
            !output.status.success(),
            "{name} --file explain={explain}: {value}"
        );
        messages.push(
            value["error"]["message"]
                .as_str()
                .ok_or("message")?
                .to_owned(),
        );
    }
    for arguments in [
        json!({"op":"run","use":name}),
        json!({"op":"run","use":name,"explain":true}),
        json!({"op":"run","document":document}),
    ] {
        let value = mcp_run(rig, arguments.clone())?;
        assert_eq!(value["isError"], true, "{name} {arguments}: {value}");
        messages.push(
            value["structuredContent"]["message"]
                .as_str()
                .ok_or("MCP message")?
                .to_owned(),
        );
    }
    assert!(
        messages.iter().all(|message| message == expected),
        "{name}: {messages:?}"
    );
    assert!(graph_records(&rig.state())?.is_empty(), "{name}");
    Ok(())
}

#[test]
fn oversized_graph_expansion_is_refused_before_journalling() -> Result {
    let rig = GraphProject::new()?;
    prepare_state(&rig)?;
    // The compiler-only fan-out cap is redacted consistently before admission.
    let over = (0..257)
        .map(|number| format!("'area-{number}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let template = format!("nodes = [{{ id = 'swarm', use = 'task', over = [{over}] }}]\n");
    let document = json!({"schema_version":1,"graph_id":"swarm","nodes":[{"id":"swarm","use":"task","over":(0..257).map(|n|format!("area-{n}")).collect::<Vec<_>>()}]});
    assert_compile_refusal_matches(
        &rig,
        &[("swarm", template)],
        "swarm",
        &document,
        "schema_version = 1\n",
        "graph input could not be processed",
    )?;

    // A graph that compiles to more nodes than the state admits: the template
    // CLI, the raw document CLI and the MCP tool must read the same refusal.
    // The template compiles to one composite boundary over two leaves, so the
    // document carries that shape and both expand to the same node count.
    let node_cap = provenant_fabric_runtime::config::GraphLimits {
        nodes: 1,
        ..Default::default()
    };
    assert_compile_refusal_matches(
        &rig,
        &[(
            "two",
            "nodes = [\n  { id = 'a', use = 'task', brief = 'first' },\n  { id = 'b', use = 'task', brief = 'second' },\n]\n"
                .into(),
        )],
        "two",
        &json!({"schema_version":1,"graph_id":"two","nodes":[{"id":"two","kind":"composite","access":"read_only","composite":{
            "nodes":[
                {"id":"a","prompt":"first","access":"read_only","route":{"alias":"senior"}},
                {"id":"b","prompt":"second","access":"read_only","route":{"alias":"senior"}}
            ],
            "exports":[{"node":"a","outputs":["result"]},{"node":"b","outputs":["result"]}]
        }}]}),
        "schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 1\n",
        &node_cap
            .nodes_bound()
            .refusal(3)
            .ok_or("node cap must refuse the expanded composite")?,
    )?;

    // A three-node chain against a one-level depth limit.
    let depth_cap = provenant_fabric_runtime::config::GraphLimits {
        expansion_depth: 1,
        ..Default::default()
    };
    assert_compile_refusal_matches(
        &rig,
        &[(
            "chain",
            "nodes = [\n  { id = 'a', use = 'task', brief = 'one' },\n  { id = 'b', use = 'task', brief = 'two', after = ['a'] },\n  { id = 'c', use = 'task', brief = 'three', after = ['b'] },\n]\n"
                .into(),
        )],
        "chain",
        &json!({"schema_version":1,"graph_id":"chain","nodes":[
            {"id":"a","prompt":"one","access":"read_only","route":{"alias":"senior"}},
            {"id":"b","prompt":"two","access":"read_only","route":{"alias":"senior"},"depends_on":["a"]},
            {"id":"c","prompt":"three","access":"read_only","route":{"alias":"senior"},"depends_on":["b"]}
        ]}),
        "schema_version = 1\n[domains.fabric.graph.limits]\nexpansion_depth = 1\n",
        &depth_cap
            .expansion_depth_bound()
            .refusal(2)
            .ok_or("depth cap must refuse two levels")?,
    )?;

    // Nested composites whose scoped ids pass the 200-byte node identifier
    // limit. Every level is a separate template, as a repository would write it.
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    let third = "c".repeat(64);
    let fourth = "d".repeat(64);
    assert_compile_refusal_matches(
        &rig,
        &[
            (
                "deep",
                format!("nodes = [{{ id = '{first}', use = 'level1' }}]\n"),
            ),
            (
                "level1",
                format!("nodes = [{{ id = '{second}', use = 'level2' }}]\n"),
            ),
            (
                "level2",
                format!("nodes = [{{ id = '{third}', use = 'level3' }}]\n"),
            ),
            (
                "level3",
                format!("nodes = [{{ id = '{fourth}', use = 'level4' }}]\n"),
            ),
            ("level4", "brief = 'deep'\ntier = 'junior'\n".into()),
        ],
        "deep",
        &json!({"schema_version":1,"graph_id":"deep","nodes":[{"id":first,"kind":"composite","access":"read_only","composite":{
            "nodes":[{"id":second,"kind":"composite","access":"read_only","composite":{
                "nodes":[{"id":third,"kind":"composite","access":"read_only","composite":{
                    "nodes":[{"id":fourth,"prompt":"deep","access":"read_only","route":{"alias":"junior"}}],
                    "exports":[{"node":fourth,"outputs":["result"]}]
                }}],
                "exports":[{"node":third,"outputs":["result"]}]
            }}],
            "exports":[{"node":second,"outputs":["result"]}]
        }}]}),
        "schema_version = 1\n",
        "graph composite or loop expansion is invalid",
    )?;

    // Composite expansion refuses duplicate exports of one member.
    assert_compile_refusal_matches(
        &rig,
        &[(
            "duplicate-export",
            "nodes = [{ id = 'member', use = 'task' }]\nexports = [{ node = 'member', outputs = ['result'] }, { node = 'member', outputs = ['result'] }]\n"
                .into(),
        )],
        "duplicate-export",
        &json!({"schema_version":1,"graph_id":"duplicate-export","nodes":[{"id":"duplicate-export","kind":"composite","access":"read_only","composite":{
            "nodes":[{"id":"member","prompt":"work","access":"read_only","route":{"alias":"senior"}}],
            "exports":[{"node":"member","outputs":["result"]},{"node":"member","outputs":["result"]}]
        }}]}),
        "schema_version = 1\n",
        "graph composite or loop expansion is invalid",
    )?;
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
        fn launch_refused_before_acceptance(
            &mut self,
            _: &graph::GraphAttempt,
        ) -> std::result::Result<bool, Self::Error> {
            Ok(true)
        }
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
        (
            "oversized-input-refusal",
            Some(InvalidInputDetail::InputTooLarge),
            "1 MiB",
        ),
        (
            "unreadable-input-refusal",
            Some(InvalidInputDetail::InputUnreadable),
            "unreadable",
        ),
        ("code-only-refusal", None, "invalid_input"),
    ] {
        let mut document: GraphDocument = serde_json::from_value(
            json!({"schema_version":1,"graph_id":id,"nodes":[{"id":"one","prompt":"fixture","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]}),
        )?;
        graph::patch::submission(&mut document, &format!("submit:{id}"), &Default::default())?;
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
        if id == "schema-refusal" {
            let (_, invalid) = rig.run(&["retry", id, "--node", "missing"])?;
            assert!(
                invalid["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("node does not exist"),
                "{invalid}"
            );
            let (output, retried) = rig.run(&["retry", id, "--node", "one"])?;
            assert!(
                output.status.success(),
                "retry: {retried}, stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                retried["data"]["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["kind"] == "attempt"),
                "{retried}"
            );
        }
    }
    Ok(())
}
