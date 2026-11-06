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

fn golden_in(dir: &str, name: &str, actual: &str) -> Result<(), Box<dyn std::error::Error>> {
    let actual = format!("{}\n", actual.trim_end());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(dir)
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().ok_or("golden path has no parent")?)?;
        std::fs::write(&path, &actual)?;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        actual, expected,
        "{name}: rerun with UPDATE_GOLDEN=1 to refresh"
    );
    Ok(())
}

#[test]
fn templates_list_and_show_human_output_match_goldens() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let run = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let out = fixture_cli()
            .current_dir(&root)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path())
            .arg("--state-dir")
            .arg(temp.path().join("state"))
            .args(args)
            .output()?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?)
    };
    golden_in("tests/golden", "help.txt", &run(&["help"])?)?;
    golden_in("tests/golden", "templates-list.txt", &run(&["templates"])?)?;
    golden_in(
        "tests/golden",
        "templates-show-task.txt",
        &run(&["templates", "task"])?,
    )?;
    Ok(())
}
