#![allow(
    clippy::disallowed_methods,
    reason = "integration tests exercise the CLI process"
)]
use crate::support;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
type Result<T = (), E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

// Spec: docs/specs/retry-owner.md#admission-is-distinct-from-observation
// Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
#[test]
fn secret_shaped_launch_inputs_warn_and_preserve_examples() -> Result {
    let rig = GraphProject::new()?;
    rig.enable_secret_redaction()?;
    let example = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("example.toml"),
        format!("brief='{example}'\ntier='junior'\n"),
    )?;
    let document = rig.document(&json!({"schema_version":2,"graph_id":"example-input",
        "nodes":[{"id":"task","use":"task","prompt":example}]}))?;
    let cases = [
        (
            vec!["run", "task", "--brief", example, "--explain"],
            "brief",
        ),
        (vec!["run", "example", "--explain"], "nodes[0]"),
        (
            vec![
                "run",
                "--file",
                document.to_str().ok_or("document path")?,
                "--explain",
            ],
            "document.nodes[0].prompt",
        ),
    ];
    for (args, position) in cases {
        let (output, reply) = rig.run(&args)?;
        assert!(output.status.success(), "{reply}; {output:?}");
        let warnings = reply["data"]["warnings"]
            .as_array()
            .ok_or("visible warnings")?;
        assert!(warnings.iter().any(|w| w.as_str().is_some_and(
            |w| w.contains("SECRET_SHAPED_INPUT")
                && w.contains("looks like a credential")
                && w.contains(position)
        )));
        assert!(warnings.iter().all(|w| {
            !w.to_string()
                .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
        }));
        assert!(
            reply["data"]
                .to_string()
                .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
            "example is retained: {reply}"
        );
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#checkpoint-envelope
#[test]
fn graph_metadata_warns_and_preserves_local_preview() -> Result {
    let rig = GraphProject::new()?;
    rig.enable_secret_redaction()?;
    let credential = "plain-registered-metadata-3391";
    let example = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let path = rig.document(&json!({"schema_version":2,"graph_id":"metadata-test",
        "metadata_note":format!("{example} {credential}"),
        "nodes":[{"id":"task","prompt":"do the task","access":"read_only","route":{"alias":"junior"}}]}))?;
    let output = rig
        .command()
        .env("FIXTURE_API_KEY", credential)
        .args(["run", "--file"])
        .arg(path)
        .arg("--explain")
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{reply}");
    let wire = reply.to_string();
    assert!(
        wire.contains(credential),
        "local preview retains exact bytes: {reply}"
    );
    assert!(wire.contains(example), "{reply}");
    assert!(
        reply["data"]["warnings"]
            .to_string()
            .contains("SECRET_SHAPED_INPUT"),
        "{reply}"
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#checkpoint-envelope
#[test]
fn ambient_credential_is_masked_in_published_checkpoint_and_events() -> Result {
    let rig = GraphProject::new()?;
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    let credential = "plain-registered-credential-3391";
    let prompt = format!("Use {credential} for the fixture.");
    let output = rig
        .command()
        .env("FIXTURE_API_KEY", credential)
        .args([
            "run",
            "task",
            "--brief",
            &prompt,
            "--model",
            "fixture/model",
        ])
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    // The fixture adapter cannot execute. Successful admission still retains
    // a checkpoint and events before that unrelated provider refusal.
    assert!(reply["error"].is_null(), "{reply}");
    assert!(
        reply["data"]["checkpoint"]["graph_id"].is_string(),
        "{reply}"
    );
    let records = graph_records(&rig.state())?;
    assert!(
        records
            .iter()
            .any(|p| p.to_string_lossy().ends_with(".events.jsonl"))
    );
    let mut retained_raw = false;
    let projector =
        provenant_confinement::publication::Projector::new([credential.as_bytes().to_vec()])?;
    let mut published_has_mask = false;
    for path in &records {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        retained_raw |= text.contains(credential);
        let exported = projector.project(
            provenant_confinement::publication::Destination::GraphPublication,
            &bytes,
        )?;
        let exported_text = String::from_utf8_lossy(exported.bytes());
        assert!(
            !exported_text.contains(credential),
            "publication disclosed credential in {}",
            path.display()
        );
        published_has_mask |= exported_text.contains("[REDACTED]");
        assert_eq!(
            std::fs::read(path)?,
            bytes,
            "publication preserves local bytes"
        );
    }
    assert!(retained_raw, "local graph records keep the exact bytes");
    assert!(
        published_has_mask,
        "what leaves through a publication or peer/disclosure output is masked"
    );
    Ok(())
}

#[test]
// Spec: docs/specs/cli.md#results
fn scanner_shape_keeps_local_brief_without_refusal() -> Result {
    let rig = GraphProject::new()?;
    prepare_state(&rig)?;
    let shaped = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let (output, value) = rig.run(&["run", "task", "--brief", shaped, "--explain"])?;
    assert!(output.status.success(), "{value}");
    assert_eq!(value["data"]["document"]["nodes"][0]["prompt"], shaped);
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(!warning.contains("rule=known_secret_shape"), "{warning}");
    assert!(!warning.contains(shaped), "warning disclosed matched value");
    assert!(graph_records(&rig.state())?.is_empty());
    Ok(())
}

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
            .args(["run", "task", "--route", "junior", "--brief-file"])
            .arg(&path)
            .arg("--explain")
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(output.status.success(), "{value}");
        assert_eq!(value["data"]["document"]["nodes"][0]["prompt"], brief);
    }
    let output = rig
        .command()
        .args(["run", "task", "--brief-file"])
        .arg(rig.project.join("brief.md"))
        .arg("--explain")
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{value}");
    assert_eq!(value["data"]["document"]["nodes"][0]["prompt"], brief);
    let graph = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"file-graph","nodes":[{"id":"task","prompt":"graph prompt","access":"read_only","route":{"alias":"junior"}}]}))?;
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
        .args(["run", "task", "--route", "junior", "--brief-file"])
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
            .args(["run", "task", "--route", "junior", "--brief-file"])
            .arg(&path)
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(!output.status.success(), "{value}");
        assert_eq!(value["error"]["code"], "invalid_input", "{value}");
        assert!(graph_records(&rig.state())?.is_empty());
    }
    for path in [
        rig.temp.path().join("missing"),
        rig.temp.path().to_path_buf(),
    ] {
        let output = rig
            .command()
            .args(["run", "task", "--route", "junior", "--brief-file"])
            .arg(&path)
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(!output.status.success(), "{value}");
        assert_eq!(value["error"]["code"], "invalid_input", "{value}");
        assert!(graph_records(&rig.state())?.is_empty());
    }
    let mut child = rig
        .command()
        .args([
            "run",
            "task",
            "--route",
            "junior",
            "--brief-file",
            "-",
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
    // `--file` takes a graph document only; a brief in it is refused with the
    // flag that does take one.
    let output = rig
        .command()
        .args(["run", "--file"])
        .arg(rig.project.join("brief.md"))
        .arg("--explain")
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(!output.status.success(), "{value}");
    assert_eq!(value["error"]["code"], "invalid_input", "{value}");
    Ok(())
}

#[test]
fn explicit_graph_file_keeps_nodes_and_dependencies_with_route_override() -> Result {
    let rig = GraphProject::new()?;
    let graph = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
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
    // Routed graph documents compile through the existing composite path.
    // Its children must remain executable tasks, rather than graph JSON text.
    assert_eq!(document["nodes"][0]["kind"], "composite");
    let nodes = document["nodes"][0]["composite"]["nodes"]
        .as_array()
        .ok_or("missing graph tasks")?;
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
fn graph_refuses_unavailable_outputs_without_disclosing_prompt() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "malformed-exports",
        "nodes": [{
            "id": "inner", "kind": "composite", "access": "read_only",
            "composite": {
                "nodes": [{"id":"look", "prompt":"PROMPT_SENTINEL", "access":"read_only", "route":{"alias":"senior"}}],
                "exports": [{"node":"look", "outputs":["result", "branch"]}]
            }
        }]
    }))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["error"]["code"], "dispatch_refused", "{value}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PROMPT_SENTINEL"));
    Ok(())
}

// Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
#[test]
fn invalid_plan_refusal_names_its_node_and_validator_reason() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "invalid-plan-reason",
        "nodes": [{
            "id": "reader", "prompt": "read", "access": "read_only",
            "route": {"alias": "senior"}, "deadline_seconds": 0
        }]
    }))?;
    let (output, refusal) =
        rig.run(&["run", "--file", path.to_str().ok_or("path")?, "--explain"])?;
    assert!(!output.status.success(), "{refusal}");
    assert_eq!(refusal["error"]["code"], "dispatch_refused", "{refusal}");
    let wire = refusal.to_string();
    assert!(wire.contains("node reader"), "{wire}");
    assert!(wire.contains("deadline_seconds must be positive"), "{wire}");
    assert!(
        wire.contains("fix the named graph node and validator reason, then retry"),
        "{wire}"
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
#[test]
fn composite_admission_refusal_names_its_node_and_validator_reason() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "invalid-composite-reason",
        "nodes": [{
            "id": "expand", "kind": "loop", "access": "read_only",
            "loop": {"body": [], "until": "done", "max_rounds": 0}
        }]
    }))?;
    let (output, refusal) =
        rig.run(&["run", "--file", path.to_str().ok_or("path")?, "--explain"])?;
    assert!(!output.status.success(), "{refusal}");
    assert_eq!(refusal["error"]["code"], "dispatch_refused", "{refusal}");
    let wire = refusal.to_string();
    assert!(wire.contains("node expand"), "{wire}");
    assert!(wire.contains("loop.max_rounds must be positive"), "{wire}");
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
        "task",
        "inspect",
        "--branch",
        "topic/remedy",
        "--base",
        "HEAD",
        "--name",
        "worktree-remedy",
        "--write-scope",
        ".",
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
        "task",
        "inspect",
        "--branch",
        "topic/remedy",
        "--base",
        "HEAD",
        "--name",
        "worktree-remedy",
        "--write-scope",
        ".",
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
        .args(["run", "task", "inspect", "--worktree"])
        .arg(&selected)
        .args(["--write-scope", "."])
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
    assert_eq!(binding["worktree"], selected.as_str());
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

/// Native provider custody (the guardian that accepts a provider hand-off and
/// spawns it) is macOS-only. Elsewhere a launch reaches preparation and stops
/// at the platform boundary, so admission fixtures assert that boundary rather
/// than a provider process that cannot exist.
const PROVIDER_CUSTODY: bool = cfg!(target_os = "macos");

/// A fake `codex` executable: it answers the version and login probes, then
/// holds the lane open until the test releases it. The provider's working
/// directory is the writer checkout, so `fixture-release` is per slice.
const CODEX_FIXTURE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'codex-cli 0.159.0\n'
  exit 0
fi
if [ "$1" = "auth" ] || [ "$1" = "login" ]; then
  printf '{"loggedIn":true,"email":"fixture@example.test"}\n'
  exit 0
fi
/bin/cat >/dev/null
printf 'started\n' > fixture-started
i=0
while [ "$i" -lt 200 ]; do
  if [ -f fixture-release ]; then
    exit 7
  fi
  i=$((i + 1))
  /bin/sleep 0.05
done
exit 7
"#;

struct GraphProject {
    temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    /// Extra environment for runs that need an isolated provider home.
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
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
            env: Vec::new(),
        })
    }
    fn add_review_subject(&self) -> Result {
        std::fs::write(self.project.join("subject.txt"), "review subject\n")?;
        for args in [vec!["add", "subject.txt"], vec!["commit", "-qm", "subject"]] {
            let output = provenant_os::fixture::git_command()
                .current_dir(&self.project)
                .args(args)
                .output()?;
            assert!(output.status.success(), "{output:?}");
        }
        Ok(())
    }
    fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }
    /// Declare the one forge context a delivery graph needs. The batch resolves
    /// it lazily and never guesses a remote or default branch, so a graph that
    /// checks a delivery target in a local-only fixture must declare both.
    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    fn declare_forge(&self, default_branch: &str) -> Result {
        std::fs::create_dir_all(self.project.join(".provenant"))?;
        std::fs::write(
            self.project.join(provenant_config::PORTS_CONFIG),
            serde_json::to_vec(&json!({
                "schema_version": provenant_config::PORTS_SCHEMA_VERSION,
                "git": {"remote": "origin", "default_branch": default_branch, "protected": []},
            }))?,
        )?;
        Ok(())
    }
    /// A runnable Codex lane: an isolated login home and a fake `codex` first
    /// on PATH. The route is a real, catalogued model, so admission is not
    /// decided by a placeholder adapter.
    fn enable_codex_fixture(&mut self) -> Result {
        use std::os::unix::fs::PermissionsExt;
        let home = self.temp.path().join("home");
        let codex_home = home.join(".codex");
        let bin = self.temp.path().join("provider/bin");
        for path in [&home, &codex_home, &bin] {
            std::fs::create_dir_all(path)?;
        }
        std::fs::write(
            codex_home.join("auth.json"),
            br#"{"tokens":{"account_id":"fixture-one"}}"#,
        )?;
        let executable = bin.join("codex");
        std::fs::write(&executable, CODEX_FIXTURE)?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        provenant_os::fixture::private_state_dir(&self.state())?;
        std::fs::write(
            self.state().join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='1B'\n",
        )?;
        self.env = vec![
            ("HOME".into(), home.into_os_string()),
            ("CODEX_HOME".into(), codex_home.into_os_string()),
            (
                "PATH".into(),
                format!("{}:/usr/bin:/bin", bin.display()).into(),
            ),
        ];
        Ok(())
    }
    fn enable_secret_redaction(&self) -> Result {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(self.state())?;
        std::fs::set_permissions(self.state(), std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(
            self.state().join("config.toml"),
            "schema_version=1\n[knobs]\nredaction='secrets'\n",
        )?;
        Ok(())
    }
    fn command(&self) -> Command {
        let mut command = fixture_cli();
        command
            .current_dir(&self.project)
            .env("PROVENANT_ROOT", &self.root)
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(self.state());
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }
    fn run(&self, args: &[&str]) -> Result<(Output, Value)> {
        let output = self.command().args(args).output()?;
        let value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("{error}: {output:?}"))?;
        Ok((output, value))
    }
    /// The same fixture without `--json`, so the compact human view is on stdout.
    fn human_command(&self) -> Command {
        let mut command = fixture_cli();
        command
            .current_dir(&self.project)
            .env("PROVENANT_ROOT", &self.root)
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .args(["--state-dir"])
            .arg(self.state());
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
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

/// Attempt rows in the run journal, so a read-only preview can prove it
/// admitted none. A missing database, or one without the table yet, holds none.
fn attempt_records(state: &Path) -> Result<i64> {
    let database = state.join("run.sqlite3");
    if !database.exists() {
        return Ok(0);
    }
    let connection = rusqlite::Connection::open(&database)?;
    let table: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='attempts'",
        [],
        |row| row.get(0),
    )?;
    if table == 0 {
        return Ok(0);
    }
    Ok(connection.query_row("SELECT count(*) FROM attempts", [], |row| row.get(0))?)
}

/// A command's terminal record. `wait` emits progress events before its final
/// envelope, so only the last non-empty line is the command result.
fn last_record(output: &Output) -> Result<Value> {
    let text = std::str::from_utf8(&output.stdout)?;
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or("command produced no record")?;
    Ok(serde_json::from_str(line)?)
}

/// A launch must admit the graph its explanation admitted. Where native
/// provider custody exists the process must also start. Elsewhere the attempt
/// ends at that platform boundary after the host bound and provisioned its
/// slices; a route or worktree refusal is still a failure on every host.
fn assert_launch_admitted(launch: &Output, launched: &Value) -> Result {
    if PROVIDER_CUSTODY {
        assert!(
            launch.status.success(),
            "launch was refused after explain admitted the same input: {launched}"
        );
        return Ok(());
    }
    let rendered = launched.to_string();
    for refusal in [
        "unknown_adapter",
        "route_resolution_refused",
        "worktree_missing",
        "invalid_input",
    ] {
        assert!(
            !rendered.contains(refusal),
            "launch was refused after explain admitted the same input ({refusal}): {launched}"
        );
    }
    assert!(
        launched["error"].is_null(),
        "launch envelope refused admission: {launched}"
    );
    assert!(
        launched["data"]["checkpoint"]["graph_id"].is_string(),
        "launch admitted no graph: {launched}"
    );
    assert!(
        rendered.contains("unsupported_platform"),
        "launch did not reach the provider-custody boundary after admission: {launched}"
    );
    Ok(())
}

#[test]
fn corrupt_graph_listing_gives_a_working_retirement_command() -> Result {
    let rig = GraphProject::new()?;
    for id in ["broken", "healthy"] {
        let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":id,"nodes":[{"id":"gate","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
        let output = rig.command().args(["run", "--file"]).arg(path).output()?;
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
    let rows = listing["data"]["graphs"].as_array().ok_or("graphs")?;
    let bad = rows
        .iter()
        .find(|row| row["name"] == "broken")
        .ok_or("broken row")?;
    assert_eq!(bad["state"], "unreadable");
    // #3530 counts settled graphs in the default listing; --full shows them.
    assert!(!rows.iter().any(|row| row["name"] == "healthy"));
    let (output, full) = rig.run(&["status", "--full"])?;
    assert!(output.status.success(), "{full}");
    assert!(
        full["data"]["graphs"]
            .as_array()
            .ok_or("full graphs")?
            .iter()
            .any(|row| row["name"] == "healthy")
    );
    assert!(!listing.to_string().contains("PRIVATE_CONTENT_SENTINEL"));
    // Spec: docs/specs/cli.md#warnings-are-typed-values -- under the v2 envelope
    // typed warnings legitimately carry paths in their subject. The corrupt
    // record must not echo a host path into the command's data.
    assert!(
        !listing["data"]
            .to_string()
            .contains(&rig.state().display().to_string())
    );
    let (output, status) = rig.run(&["status", "broken"])?;
    assert!(output.status.success(), "{status}");
    assert!(!status.to_string().contains("PRIVATE_CONTENT_SENTINEL"));
    assert!(
        !status["data"]
            .to_string()
            .contains(&rig.state().display().to_string())
    );
    // The row's own next action is the command that retires the record.
    let next = status["data"]["rows"][0]["next"]
        .as_str()
        .ok_or("next action")?;
    let words: Vec<&str> = next.split_whitespace().collect();
    let command = words.strip_prefix(&["provenant"]).ok_or("next command")?;
    let (output, stopped) = rig.run(command)?;
    assert!(output.status.success(), "{stopped}");
    let (_, after) = rig.run(&["status", "--full"])?;
    let rows = after["data"]["graphs"]
        .as_array()
        .ok_or("graphs after stop")?;
    assert!(!rows.iter().any(|row| row["name"] == "broken"), "{after}");
    assert!(rows.iter().any(|row| row["name"] == "healthy"), "{after}");
    Ok(())
}

#[test]
fn graph_shaped_identities_keep_local_bytes_and_schema_validation() -> Result {
    // Spec: docs/specs/fabric.md#checkpoint-envelope
    // Explain is a host-local read, not publication. Token shape alone must
    // neither rewrite identities nor bypass graph-handle validation.
    let shaped = format!("ghp_{}", "a1".repeat(18));
    for source in ["graph_id", "node_id", "route", "context_key", "template"] {
        let rig = GraphProject::new()?;
        let mut document = json!({
            "schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,
            "graph_id":"safe-graph", "nodes":[{
                "id":"safe-node", "prompt":"Inspect", "access":"read_only",
                "route":{"alias":"junior"}
            }]
        });
        match source {
            "graph_id" => document["graph_id"] = shaped.clone().into(),
            "node_id" => document["nodes"][0]["id"] = shaped.clone().into(),
            "route" => document["nodes"][0]["route"]["alias"] = shaped.clone().into(),
            "context_key" => document["context"] = json!({(shaped.clone()):"note"}),
            "template" => {
                document = json!({"nodes":[{"id":"x","use":shaped}],
                    "templates":{(shaped.clone()):{"brief":"Inspect","tier":"senior"}}});
            }
            _ => unreachable!(),
        }
        let path = rig.document(&document)?;
        let output = rig
            .command()
            .args(["run", "--file"])
            .arg(&path)
            .arg("--explain")
            .output()?;
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&path)?)?,
            document,
            "explain rewrote local source"
        );
        assert!(
            !value.to_string().contains("credential token"),
            "{source}: {value}"
        );
        if source == "graph_id" {
            // A ghp_ value is not a valid graph handle (underscores are
            // forbidden). This schema check remains mandatory, independently
            // of shape. Explain retains aliases for later route resolution.
            assert!(
                !output.status.success(),
                "schema validation must still fail: {source}: {value}"
            );
        } else {
            assert!(output.status.success(), "{source}: {value}");
            if source == "template" {
                // A shorthand document lowers into a composite. Its member
                // retains both the authored template identity and prompt.
                let member = &value["data"]["document"]["nodes"][0]["composite"]["nodes"][0];
                assert_eq!(member["prompt"], "Inspect");
                assert_eq!(member["template"], shaped);
            } else {
                assert!(
                    value["data"]["document"].to_string().contains(&shaped),
                    "{source}: {value}"
                );
            }
        }
        assert!(graph_records(&rig.state())?.is_empty(), "{source}");
        assert!(
            graph_records(&rig.project.join(".provenant/scratch"))?.is_empty(),
            "{source}"
        );
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
        "map",
        "--route",
        "senior",
        "--over",
        "core,cli",
        "--set",
        "topic=Review",
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
fn cloud_placement_is_refused_and_names_the_launch_command() -> Result {
    let rig = GraphProject::new()?;
    let graph = rig.project.join("cloud.json");
    std::fs::write(
        &graph,
        serde_json::to_vec(&json!({
            "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
            "graph_id": "cloud",
            "nodes": [{
                "id": "x",
                "access": "read_only",
                "prompt": "inspect",
                "attempt": {"cloud": {"target": "verify", "provider": "claude"}},
                "route": {"adapter": "claude", "model": "test"}
            }]
        }))?,
    )?;
    let (output, value) = rig.run(&[
        "run",
        "--file",
        graph.to_str().ok_or("graph path")?,
        "--explain",
    ])?;
    assert!(!output.status.success(), "{value}");
    assert!(value.to_string().contains("cloud launch"), "{value}");
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
    let (_, value) = rig.run(&["templates", "show", "task"])?;
    assert_eq!(value["data"]["templates"][0]["template"]["brief"], "user");
    std::fs::write(
        repo.join("task.toml"),
        "brief='repository'\ntier='senior'\n",
    )?;
    let (_, value) = rig.run(&["templates", "show", "task"])?;
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
        .args(["templates", "show", "task", "--file"])
        .arg(inline)
        .output()?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{value}");
    assert_eq!(value["data"]["templates"][0]["template"]["brief"], "inline");
    Ok(())
}

#[test]
fn every_bundled_template_is_listed_and_compiles_with_its_declared_parameters() -> Result {
    // Spec: docs/specs/fabric.md#bundled-template-declarations
    use provenant_fabric_runtime::graph::template::{Compiler, TemplateLibrary};

    let rig = GraphProject::new()?;
    rig.declare_forge("master")?;
    let (output, value) = rig.run(&["templates"])?;
    assert!(output.status.success(), "{value}");
    let listed = value["data"]["templates"].as_array().ok_or("templates")?;
    let names: Vec<_> = listed
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    let library = TemplateLibrary::bundled();
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

    let (output, shown) = rig.run(&["templates", "show", "task"])?;
    assert!(output.status.success(), "{shown}");
    assert_eq!(
        &shown["data"]["templates"][0],
        listed
            .iter()
            .find(|entry| entry["name"] == "task")
            .ok_or("task template")?
    );

    // Keep one representative CLI show; exercise every declaration and its
    // parameter bindings through the in-process compiler.
    let resolvers = [
        ("review-frame".to_owned(), vec!["true".to_owned()]),
        ("review-verify".to_owned(), vec!["true".to_owned()]),
        ("review-synthesis".to_owned(), vec!["true".to_owned()]),
        ("review-verifiers".to_owned(), vec!["1".to_owned()]),
        ("review-panel".to_owned(), vec!["correctness".to_owned()]),
        ("review-advice-correctness".to_owned(), vec![String::new()]),
    ]
    .into_iter()
    .collect();
    let compiler = Compiler {
        library: &library,
        resolvers: &resolvers,
    };
    for entry in listed {
        let name = entry["name"].as_str().ok_or("template name")?;
        let template = &entry["template"];
        let mut node = json!({"id":"exercise", "use":name});
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
        let compiled = compiler
            .compile_all(&nodes)
            .map_err(|error| format!("{name}: {error}"))?;
        assert!(!compiled.is_empty(), "{name}");
    }

    // These CLI paths exercise behaviour specific to repair loops,
    // synthesis inputs, and worktree parameter binding.
    for (name, nodes) in [
        (
            "repair",
            vec![
                json!({"id":"exercise", "use":"repair", "with":{"worktree":rig.project, "write_scope":["."]}, "repeat":{"until":"accepted", "max_rounds":1, "review_repair":true}}),
            ],
        ),
        (
            "review-repair",
            vec![
                json!({"id":"exercise", "use":"review-repair", "with":{"worktree":rig.project, "write_scope":["."], "max_repair_cycles":1}}),
            ],
        ),
        (
            "deliver",
            vec![
                json!({"id":"exercise", "use":"deliver", "with":{"worktree":rig.project, "write_scope":["."], "max_ci_rounds":1}}),
            ],
        ),
        (
            "synthesise",
            vec![
                json!({"id":"source", "use":"task"}),
                json!({"id":"exercise", "use":"synthesise", "after":["source"], "inputs":[{"node":"source", "outputs":["result"]}]}),
            ],
        ),
        (
            "implement",
            vec![
                json!({"id":"exercise", "use":"implement", "worktree":rig.project, "write_scope":["."]}),
            ],
        ),
    ] {
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
        if matches!(name, "review-repair" | "deliver") {
            use provenant_fabric_runtime::graph::{GraphDocument, NodeKind};
            let document: GraphDocument =
                serde_json::from_value(explained["data"]["document"].clone())?;
            let expanded =
                document.expanded(&provenant_fabric_runtime::config::GraphLimits::default())?;
            let rows = explained["data"]["node_routes"]
                .as_array()
                .ok_or("node routes")?;
            let structural: Vec<_> = expanded
                .nodes
                .iter()
                .filter(|node| {
                    node.composite.is_some()
                        || node.condition.is_some()
                        || node.decide.is_some()
                        || node.loop_body.is_some()
                        || node.land.is_some()
                        || matches!(
                            node.kind,
                            Some(
                                NodeKind::Composite
                                    | NodeKind::Gate
                                    | NodeKind::Loop
                                    | NodeKind::Decide
                                    | NodeKind::Land
                                    | NodeKind::Hygiene
                                    | NodeKind::Publish
                            )
                        )
                })
                .collect();
            assert!(
                !structural.is_empty(),
                "{name} must exercise structural nodes"
            );
            for node in structural {
                assert!(
                    rows.iter().all(|row| row["node"] != node.id),
                    "{name}: structural node {} has a route: {rows:?}",
                    node.id
                );
            }
            assert!(!rows.is_empty(), "{name} must report launchable routes");
        }
    }
    let worktree = format!("worktree={}", rig.project.display());
    let (output, explained) = rig.run(&[
        "run",
        "batch",
        "--set",
        &worktree,
        "--set",
        "write_scope=['.']",
        "--set",
        "integration_branch=batch/example",
        "--explain",
    ])?;
    assert!(output.status.success(), "{explained}");
    assert_eq!(
        explained["data"]["document"]["integration_branch"],
        "batch/example"
    );
    let proposed: Vec<_> = explained["data"]["source_admission"]
        .as_array()
        .ok_or("batch source admission")?
        .iter()
        .filter(|row| row["status"] == "pending_registration")
        .collect();
    assert!(!proposed.is_empty(), "batch slices need proposed worktrees");
    for row in proposed {
        assert!(
            !Path::new(row["path"].as_str().ok_or("proposed worktree path")?).exists(),
            "explanation must not create a worktree: {row}"
        );
    }
    Ok(())
}

// Spec: docs/specs/routing.md#exact-selection-no-inference
#[test]
fn batch_item_exact_model_matches_run_model_route() -> Result {
    let rig = GraphProject::new()?;
    // The bundled batch expands a repair loop that checks its
    // `integrate:<integration_branch>` delivery target. That check needs the
    // one forge context; a local-only fixture must declare it rather than let
    // resolution guess a remote. Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    rig.declare_forge("master")?;
    let model = "opencode/opencode/hy4-flash-free";
    let worktree = rig.project.to_str().ok_or("worktree path")?;
    let integration_branch = "batch/exact-model-route";
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "batch-exact-model-route",
        "integration_branch": integration_branch,
        "nodes": [{
            "id": "batch",
            "use": "batch",
            "with": {
                "worktree": worktree,
                "write_scope": ["."],
                // The bundled batch expands a repair loop whose writer delivers
                // to `integrate:<integration_branch>`. It must name the same
                // branch as the document or loop expansion refuses the graph.
                "integration_branch": integration_branch,
                "items": [{"id": "slice", "brief": "tiny slice", "model": model}]
            }
        }]
    }))?;
    let (_, batch) = rig.run(&[
        "run",
        "--file",
        document.to_str().ok_or("document path")?,
        "--explain",
    ])?;
    assert!(batch["error"].is_null(), "{batch}");
    let members = batch["data"]["document"]["nodes"][0]["composite"]["nodes"]
        .as_array()
        .ok_or_else(|| format!("batch composite nodes: {batch}"))?;
    // The compiled batch is a composite of `slices`, `review` and `repair`.
    // The item slices live one level below, inside the `slices` composite.
    let slice = members
        .iter()
        .find(|node| node["id"] == "slices")
        .and_then(|node| node["composite"]["nodes"].as_array())
        .and_then(|nodes| nodes.iter().find(|node| node["id"] == "slice"))
        .ok_or_else(|| format!("batch slice node: {batch}"))?;

    let (_, direct) = rig.run(&[
        "run",
        "implement",
        "tiny slice",
        "--model",
        model,
        "--worktree",
        worktree,
        "--write-scope",
        ".",
        "--explain",
    ])?;
    assert!(direct["error"].is_null(), "{direct}");
    let direct_route = &direct["data"]["document"]["nodes"][0]["route"];
    assert_eq!(
        slice["route"], *direct_route,
        "batch: {batch} direct: {direct}"
    );
    assert_eq!(slice["route"]["adapter"], "opencode");
    assert_eq!(slice["route"]["model"], model);
    Ok(())
}

// Spec: docs/specs/fabric.md#one-model-spelling-per-route
#[test]
fn node_route_table_reaches_explain_with_one_full_model_spelling() -> Result {
    let rig = GraphProject::new()?;
    let full = "opencode/opencode/hy4-flash-free";
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "node-exact-model-route",
        "nodes": [
            {"id":"exact","prompt":"inspect","access":"read_only",
             "route":{"adapter":"opencode","model":full}},
            {"id":"relative","prompt":"inspect","access":"read_only",
             "route":{"adapter":"opencode","model":"opencode/hy4-flash-free"}},
        ]
    }))?;
    let (_, explained) = rig.run(&[
        "run",
        "--file",
        document.to_str().ok_or("document path")?,
        "--explain",
    ])?;
    assert!(explained["error"].is_null(), "{explained}");
    let nodes = explained["data"]["document"]["nodes"]
        .as_array()
        .ok_or_else(|| format!("explained nodes: {explained}"))?;
    for node in nodes {
        assert_eq!(node["route"]["adapter"], "opencode", "{node}");
        assert_eq!(node["route"]["model"], full, "{node}");
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#one-model-spelling-per-route
#[test]
fn explain_reports_each_nodes_route_with_its_sources() -> Result {
    let rig = GraphProject::new()?;
    let full = "opencode/opencode/hy4-flash-free";
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "node-route-sources",
        "nodes": [
            {"id":"exact","prompt":"inspect","access":"read_only",
             "route":{"adapter":"opencode","model":full,"effort":"high"}},
            {"id":"named-adapter","prompt":"inspect","access":"read_only",
             "route":{"adapter":"codex","name":"configured-entry"}},
            {"id":"pooled","prompt":"inspect","access":"read_only",
             "route":{"alias":"senior"}},
            {"id":"pooled-override","prompt":"inspect","access":"read_only",
             "route":{"alias":"senior","effort":"low"}}
        ]
    }))?;
    let (_, explained) = rig.run(&[
        "run",
        "--file",
        document.to_str().ok_or("document path")?,
        "--explain",
    ])?;
    assert!(explained["error"].is_null(), "{explained}");
    let rows = explained["data"]["node_routes"]
        .as_array()
        .ok_or_else(|| format!("node routes: {explained}"))?;
    let exact = rows
        .iter()
        .find(|row| row["node"] == "exact")
        .ok_or_else(|| format!("exact route: {rows:?}"))?;
    assert_eq!(exact["adapter"], "opencode", "{exact}");
    assert_eq!(exact["model"], full, "{exact}");
    assert_eq!(exact["effort"], "high", "{exact}");
    assert_eq!(exact["sources"]["model"], "node field", "{exact}");
    let named = rows
        .iter()
        .find(|row| row["node"] == "named-adapter")
        .ok_or_else(|| format!("named adapter route: {rows:?}"))?;
    assert_eq!(named["adapter"], "codex", "{named}");
    assert_eq!(named["route_name"], "configured-entry", "{named}");
    let pooled = rows
        .iter()
        .find(|row| row["node"] == "pooled")
        .ok_or_else(|| format!("pool route: {rows:?}"))?;
    assert_eq!(pooled["selection"], "pool", "{pooled}");
    assert_eq!(pooled["sources"]["model"], "pool", "{pooled}");
    assert_eq!(pooled["sources"]["effort"], "pool", "{pooled}");
    let candidates = pooled["candidates"]
        .as_array()
        .ok_or_else(|| format!("pool candidates: {pooled}"))?;
    assert!(!candidates.is_empty(), "{pooled}");
    assert!(
        candidates.iter().any(|candidate| {
            candidate["model"].is_string()
                && candidate["adapter"].is_string()
                && candidate["effort"].is_string()
                && candidate["sources"]["effort"] == "pool"
        }),
        "pool candidates retain member effort and provenance: {pooled}"
    );
    for effort in ["medium", "high"] {
        assert!(
            candidates.iter().any(|candidate| {
                candidate["effort"] == effort && candidate["sources"]["effort"] == "pool"
            }),
            "pool candidate effort {effort} is visible with pool provenance: {pooled}"
        );
    }
    let overridden = rows
        .iter()
        .find(|row| row["node"] == "pooled-override")
        .ok_or_else(|| format!("overridden pool route: {rows:?}"))?;
    assert!(
        overridden["candidates"]
            .as_array()
            .is_some_and(|candidates| {
                candidates.iter().all(|candidate| {
                    candidate["effort"] == "low" && candidate["sources"]["effort"] == "node field"
                })
            }),
        "node effort overrides every pool candidate: {overridden}"
    );
    Ok(())
}

// Spec: docs/specs/supervisor.md#attempt-silence-is-reported-action-is-opt-in
#[test]
fn silent_provider_is_visible_in_status_and_ends_wait() -> Result {
    if !PROVIDER_CUSTODY {
        return Ok(());
    }
    let mut rig = GraphProject::new()?;
    rig.enable_codex_fixture()?;
    rig.declare_forge("master")?;
    std::fs::write(
        rig.state().join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[providers.codex]\nstall_after_seconds=1\n",
    )?;
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "silent-provider",
        "integration_branch": "silent/integration",
        "nodes": [{
            "id": "writer", "prompt": "write the change", "access": "worktree_write",
            "worktree": ".worktrees/silent-provider-writer", "write_scope": ["."],
            "delivery": "integrate:silent/integration", "provision_worktree": true,
            "route": {"adapter": "codex", "model": "codex/gpt-6-luna"}
        }]
    }))?;
    let document = document.to_str().ok_or("document path")?;
    let (launch, launched) = rig.run(&["run", "--file", document])?;
    assert_launch_admitted(&launch, &launched)?;
    let graph = launched["data"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or("graph id")?;
    let output = rig
        .command()
        .args(["wait", graph, "--timeout", "8s"])
        .output()?;
    let waited = last_record(&output)?;
    let (_, status) = rig.run(&["status", graph])?;
    std::fs::write(
        rig.project
            .join(".worktrees/silent-provider-writer/fixture-release"),
        b"release",
    )?;
    assert_eq!(output.status.code(), Some(6), "{waited}");
    assert_eq!(
        waited["data"]["targets"][0]["settle"], "stalled",
        "{waited}"
    );
    let rows = status["data"]["rows"].as_array().ok_or("status rows")?;
    let attempt = rows
        .iter()
        .find(|row| row["kind"] == "attempt" && row["stalled"] == true)
        .ok_or_else(|| format!("no stalled attempt: {status}"))?;
    assert_eq!(attempt["last_event_source"], "started", "{attempt}");
    assert!(attempt["last_event_at"].is_string(), "{attempt}");
    Ok(())
}

#[test]
fn batch_launch_provisions_planned_slice_worktrees_and_explains_same_admission() -> Result {
    let mut rig = GraphProject::new()?;
    rig.enable_codex_fixture()?;
    rig.declare_forge("master")?;
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "batch-provision",
        "integration_branch": "batch/integration",
        "nodes": [
            {
                "id": "task--slices--alpha",
                "prompt": "implement alpha",
                "access": "worktree_write",
                "worktree": ".worktrees/batch-provision-task--slices--alpha",
                "write_scope": ["."],
                "delivery": "integrate:batch/integration",
                "provision_worktree": true,
                "route": {"adapter": "codex", "model": "codex/gpt-6-luna"},
            },
            {
                "id": "task--slices--beta",
                "prompt": "implement beta",
                "access": "worktree_write",
                "worktree": ".worktrees/batch-provision-task--slices--beta",
                "write_scope": ["."],
                "delivery": "integrate:batch/integration",
                "provision_worktree": true,
                "route": {"adapter": "codex", "model": "codex/gpt-6-luna"},
            },
        ],
    }))?;
    let document = document.to_str().ok_or("document path")?;
    let (explained_output, explained) = rig.run(&["run", "--file", document, "--explain"])?;
    assert!(explained_output.status.success(), "{explained}");
    let managed = rig.project.join(".worktrees");
    assert!(
        !managed.exists() || std::fs::read_dir(&managed)?.next().is_none(),
        "explain must not create managed worktrees: {explained}"
    );
    // Explain is a reader: it must not journal the graph execution or admit an
    // attempt for the input it previews.
    assert!(
        graph_records(&rig.state())?.is_empty(),
        "explain allocated a graph execution record: {explained}"
    );
    assert_eq!(
        attempt_records(&rig.state())?,
        0,
        "explain allocated an attempt record: {explained}"
    );

    let (launch, launched) = rig.run(&["run", "--file", document])?;
    assert_launch_admitted(&launch, &launched)?;
    let graph = launched["data"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or_else(|| format!("launch must admit the graph explain admitted: {launched}"))?
        .to_owned();
    if PROVIDER_CUSTODY {
        // The fake providers hold each lane open until released, so the launch
        // returns while both slices are running rather than after they settle.
        for slice in ["alpha", "beta"] {
            let release = rig.project.join(format!(
                ".worktrees/batch-provision-task--slices--{slice}/fixture-release"
            ));
            std::fs::write(&release, b"release")
                .map_err(|error| format!("release fixture slice {}: {error}", release.display()))?;
        }
    }
    let wait_output = rig
        .command()
        .args(["wait", &graph, "--timeout", "15s"])
        .output()?;
    let waited = last_record(&wait_output)?;
    assert_eq!(
        waited["data"]["timed_out"].as_bool(),
        Some(false),
        "wait did not settle the graph the launch admitted: {waited}"
    );
    let settle = waited["data"]["targets"]
        .as_array()
        .and_then(|targets| {
            targets
                .iter()
                .find(|target| target["target"] == graph.as_str())
        })
        .and_then(|target| target["settle"].as_str())
        .ok_or_else(|| format!("wait did not report {graph}: {waited}"))?;
    let expected_exit = match settle {
        "succeeded" => 0,
        "unsuccessful" => 1,
        "needs_input" => 5,
        other => return Err(format!("unexpected wait settle {other}: {waited}").into()),
    };
    assert_eq!(
        wait_output.status.code(),
        Some(expected_exit),
        "wait exit disagrees with settle {settle}: {waited}"
    );
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| *name == *format!("graph-{graph}.json"))
        })
        .ok_or("checkpoint file")?;
    let stored: Value = serde_json::from_slice(&std::fs::read(checkpoint_path)?)?;
    let slices: Vec<_> = stored["checkpoint"]["nodes"]
        .as_array()
        .ok_or("checkpoint nodes")?
        .iter()
        .filter(|node| {
            node["id"]
                .as_str()
                .is_some_and(|id| id.contains("task--slices--"))
        })
        .collect();
    assert_eq!(slices.len(), 2, "both batch slices must start: {stored}");
    let mut provisioned = std::collections::BTreeSet::new();
    for slice in slices {
        let binding = slice["writer_binding"]
            .as_object()
            .ok_or_else(|| format!("slice was not bound: {slice}"))?;
        let path = binding["worktree"].as_str().ok_or("bound worktree")?;
        assert!(
            Path::new(path).is_dir(),
            "host did not provision planned slice worktree {path}: {slice}"
        );
        provisioned.insert(path.to_owned());
    }
    assert_eq!(provisioned.len(), 2, "each slice owns a distinct worktree");
    if PROVIDER_CUSTODY {
        for path in &provisioned {
            assert!(
                Path::new(path.as_str()).join("fixture-started").is_file(),
                "fixture provider did not run in the slice it was bound to: {path}"
            );
        }
    }

    // The negative case keeps the dead-guard's intent: a refusal is reported
    // with the same code by explain and launch, so a passing explanation is
    // checked against a refusal and not only against an admission.
    let missing = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "batch-missing-writer",
        "nodes": [{
            "id": "writer",
            "prompt": "write the change",
            "access": "worktree_write",
            "worktree": ".worktrees/batch-missing-writer",
            "write_scope": ["."],
            "delivery": "push",
            "route": {"adapter": "codex", "model": "codex/gpt-6-luna"},
        }],
    }))?;
    let missing = missing.to_str().ok_or("document path")?;
    let (missing_explain_output, missing_explain) =
        rig.run(&["run", "--file", missing, "--explain"])?;
    assert!(
        !missing_explain_output.status.success(),
        "{missing_explain}"
    );
    assert_eq!(
        missing_explain["error"]["code"], "worktree_missing",
        "{missing_explain}"
    );
    let (missing_launch_output, missing_launch) = rig.run(&["run", "--file", missing])?;
    assert!(!missing_launch_output.status.success(), "{missing_launch}");
    assert_eq!(
        missing_launch["error"]["code"], missing_explain["error"]["code"],
        "{missing_launch}"
    );
    Ok(())
}

#[test]
fn explain_refuses_a_writer_on_the_integration_branch_like_launch() -> Result {
    let rig = GraphProject::new()?;
    rig.declare_forge("master")?;
    let writer = rig.project.join(".worktrees/integration-writer");
    let output = provenant_os::fixture::git_command()
        .current_dir(&rig.project)
        .args([
            "worktree",
            "add",
            "-q",
            "-b",
            "batch/integration",
            writer.to_str().ok_or("writer path")?,
        ])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "explain-integration-parity",
        "integration_branch": "batch/integration",
        "nodes": [{
            "id": "writer",
            "prompt": "write the change",
            "access": "worktree_write",
            "worktree": ".worktrees/integration-writer",
            "write_scope": ["."],
            "delivery": "integrate:batch/integration",
            "route": {"adapter": "fixture", "model": "fixture/model"},
        }],
    }))?;
    let document = document.to_str().ok_or("document path")?;
    let (explained_output, explained) = rig.run(&["run", "--file", document, "--explain"])?;
    assert!(!explained_output.status.success(), "{explained}");
    let explained_cause = explained["error"]["message"]
        .as_str()
        .or_else(|| explained["error"]["cause"].as_str())
        .unwrap_or_default();
    assert!(
        explained_cause.contains("on the integration branch batch/integration"),
        "{explained}"
    );
    let (launch_output, launched) = rig.run(&["run", "--file", document])?;
    assert!(!launch_output.status.success(), "{launched}");
    let launch_cause = launched["error"]["message"]
        .as_str()
        .or_else(|| launched["error"]["cause"].as_str())
        .unwrap_or_default();
    assert!(
        launch_cause.contains("on the integration branch batch/integration"),
        "{launched}"
    );
    Ok(())
}

// Spec: docs/specs/run-launch.md#writer-binding
#[test]
fn explain_previews_a_new_writer_without_creating_its_checkout() -> Result {
    let mut rig = GraphProject::new()?;
    rig.enable_codex_fixture()?;
    rig.declare_forge("master")?;
    // Push delivery needs a remote; a local bare repository stands in for it.
    let remote = rig.temp.path().join("origin.git");
    for args in [
        vec![
            "init",
            "-q",
            "--bare",
            remote.to_str().ok_or("remote path")?,
        ],
        vec![
            "-C",
            rig.project.to_str().ok_or("project path")?,
            "remote",
            "add",
            "origin",
            remote.to_str().ok_or("remote path")?,
        ],
    ] {
        let output = provenant_os::fixture::git_command().args(args).output()?;
        assert!(output.status.success(), "{output:?}");
    }
    let checkout = rig.project.join(".worktrees/explain-new-writer");
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "explain-new-writer",
        "nodes": [{
            "id": "writer",
            "prompt": "write the change",
            "access": "worktree_write",
            "worktree": ".worktrees/explain-new-writer",
            "write_scope": ["."],
            "delivery": "push",
            "attempt": {"new_worktree": {"branch": "explain-new-writer", "base": "HEAD"}},
            "route": {"adapter": "codex", "model": "codex/gpt-6-luna"},
        }],
    }))?;
    let document = document.to_str().ok_or("document path")?;
    // The checkout does not exist yet; explain admits the node from its
    // declared branch rather than resolving a writer Git home for it.
    let (explained_output, explained) = rig.run(&["run", "--file", document, "--explain"])?;
    assert!(
        explained_output.status.success(),
        "explain must preview a declared new worktree: {explained}"
    );
    assert!(
        !checkout.exists(),
        "explain created the writer checkout: {explained}"
    );

    let (launch, launched) = rig.run(&["run", "--file", document])?;
    assert_launch_admitted(&launch, &launched)?;
    let graph = launched["data"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or_else(|| {
            format!("launch must admit the writer explain previewed: {launch:?}; {launched}")
        })?
        .to_owned();
    if PROVIDER_CUSTODY {
        // The fake provider holds the lane open until released, so the launch
        // returns with the writer running rather than already settled.
        std::fs::write(checkout.join("fixture-release"), b"release")?;
    }
    let wait_output = rig
        .command()
        .args(["wait", &graph, "--timeout", "15s"])
        .output()?;
    let waited = last_record(&wait_output)?;
    assert_eq!(
        waited["data"]["timed_out"].as_bool(),
        Some(false),
        "wait did not settle the writer graph: {waited}"
    );
    assert!(
        checkout.is_dir(),
        "launch did not provision the checkout it previewed: {waited}"
    );
    if PROVIDER_CUSTODY {
        assert!(
            checkout.join("fixture-started").is_file(),
            "fixture provider did not run in the previewed checkout: {waited}"
        );
    }
    Ok(())
}

#[test]
fn missing_writer_checkout_error_names_its_path_and_node() -> Result {
    let rig = GraphProject::new()?;
    let document = rig.document(&json!({
        "schema_version": support::GRAPH_DOCUMENT_SCHEMA_VERSION,
        "graph_id": "missing-writer",
        "nodes": [{
            "id": "slice",
            "prompt": "write the change",
            "access": "worktree_write",
            "worktree": ".worktrees/missing-slice",
            "write_scope": ["."],
            "delivery": "push",
            "route": {"adapter": "fixture", "model": "fixture/model"},
        }],
    }))?;
    let (output, reply) = rig.run(&["run", "--file", document.to_str().ok_or("path")?])?;
    assert!(!output.status.success(), "{reply}");
    let rendered = reply.to_string();
    assert!(rendered.contains(".worktrees/missing-slice"), "{reply}");
    assert!(rendered.contains("node slice"), "{reply}");
    assert_eq!(reply["error"]["code"], "worktree_missing", "{reply}");
    Ok(())
}

#[test]
fn relative_worktree_path_from_a_linked_checkout_uses_the_project_root() -> Result {
    let rig = GraphProject::new()?;
    let tracked_path = rig.project.join(".worktrees/writer");
    let invoker = rig.project.join(".worktrees/invoker");
    for (branch, path) in [("writer-branch", &tracked_path), ("invoker", &invoker)] {
        let output = provenant_os::fixture::git_command()
            .current_dir(&rig.project)
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().ok_or("worktree path")?,
            ])
            .output()?;
        assert!(output.status.success(), "{output:?}");
    }
    let tracked = std::fs::canonicalize(&tracked_path)?;
    let output = rig
        .command()
        .current_dir(&invoker)
        .args([
            "run",
            "task",
            "fixture writer",
            "--mode",
            "worktree-write",
            "--worktree",
            ".worktrees/writer",
            "--model",
            "fixture/model",
        ])
        .output()?;
    let launched: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| format!("{error}: {output:?}"))?;
    let graph = launched["data"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or_else(|| format!("graph id: {launched}"))?;
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| *name == *format!("graph-{graph}.json"))
        })
        .ok_or("checkpoint file")?;
    let stored: Value = serde_json::from_slice(&std::fs::read(checkpoint_path)?)?;
    let binding = stored["checkpoint"]["nodes"]
        .as_array()
        .and_then(|nodes| nodes.first())
        .and_then(|node| node["writer_binding"]["worktree"].as_str())
        .ok_or_else(|| format!("writer binding: {stored}"))?;
    assert_eq!(binding, tracked.to_str().ok_or("tracked path")?);
    assert!(
        !invoker.join(".worktrees/writer").exists(),
        "relative writer path was nested under the linked checkout"
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
                // A stage whose condition does not hold is a skipped row.
                let skipped = map
                    .get("when")
                    .and_then(Value::as_array)
                    .is_some_and(|terms| {
                        terms.iter().any(|term| term["expected"] != term["actual"])
                    });
                if let Some(id) = map.get("id").and_then(Value::as_str)
                    && !skipped
                {
                    out.push(id.to_owned());
                    if matches!(id, "lenses" | "verify") && map.contains_key("composite") {
                        // A lens panel needs all but one lens, at least one
                        // (docs/specs/agent-native.md, review panels);
                        // verification still needs one success.
                        let minimum = if id == "lenses" {
                            json!(
                                map["composite"]["nodes"]
                                    .as_array()
                                    .map_or(0, Vec::len)
                                    .saturating_sub(1)
                                    .max(1)
                            )
                        } else {
                            json!(1)
                        };
                        assert_eq!(map["composite"]["min_successful"], minimum, "{id}");
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
            // Each stage that is off is listed as skipped, with its condition.
            let listed = result["data"]["skipped"].as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["node"]
                        .as_str()
                        .is_some_and(|id| id.ends_with(&format!("--{stage}")))
                })
            });
            assert_eq!(listed, !expected, "{risk}: {stage} skipped listing");
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
    let advice = include_str!("../assets/review-tests-advice.txt").trim();
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
            tests["prompt"].as_str().ok_or("prompt")?.contains(advice),
            expected,
            "{risk}"
        );
        for other in reviews.iter().filter(|node| node["review_lens"] != "tests") {
            assert!(!other["prompt"].as_str().unwrap_or("").contains(advice));
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
    for args in [vec!["templates"], vec!["templates", "show", "broken"]] {
        let (output, _) = rig.run(&args)?;
        assert!(!output.status.success());
    }
    let (output, refusal) = rig.run(&["run", "broken", "inspect"])?;
    assert!(!output.status.success(), "{refusal}");
    assert_eq!(refusal["error"]["code"], "invalid_input", "{refusal}");
    assert!(
        !refusal["next"].as_array().ok_or("next steps")?.is_empty(),
        "{refusal}"
    );
    assert!(graph_records(&rig.state())?.is_empty());
    Ok(())
}

#[test]
fn deterministic_graph_wait_status_trace_and_replay_share_one_journal() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"gate-replay","nodes":[{"id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let run = || -> Result<Value> {
        let output = rig.command().args(["run", "--file"]).arg(&path).output()?;
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
    let (output, trace) = rig.run(&["trace", "gate-replay"])?;
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
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"trace-test","nodes":[{"id":"gate","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
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
fn a_named_run_replays_its_input_and_a_changed_input_takes_the_next_name() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("stamp.toml"),
        "kind='gate'\n[node.condition]\ntype='budget'\n[parameters.tag]\ntype='string'\ndefault='none'\n",
    )?;
    let run = |args: &[&str]| -> Result<Value> {
        let mut command = vec!["run", "stamp"];
        command.extend_from_slice(args);
        let (output, result) = rig.run(&command)?;
        assert!(output.status.success(), "{args:?}: {result}");
        Ok(result["data"].clone())
    };
    let graph_count = || -> Result<usize> {
        Ok(graph_records(&rig.state())?
            .iter()
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with("graph-") && name.ends_with(".json")
                })
            })
            .count())
    };
    let records = || -> Result<Vec<Vec<u8>>> {
        let mut paths = graph_records(&rig.state())?;
        paths.sort();
        Ok(paths
            .iter()
            .map(std::fs::read)
            .collect::<std::io::Result<_>>()?)
    };
    let first = run(&["--name", "once", "--set", "tag=a"])?;
    assert_eq!(first["checkpoint"]["graph_id"], "once");
    // The same input returns the same graph and writes nothing: no node is
    // launched or advanced by a replay.
    let before = records()?;
    let again = run(&["--name", "once", "--set", "tag=a"])?;
    assert_eq!(again["checkpoint"]["graph_id"], "once");
    assert_eq!(again["named"]["replayed"], true, "{again}");
    assert_eq!(
        records()?,
        before,
        "a replay leaves every graph record as it was"
    );
    assert_eq!(graph_count()?, 1);
    // Changed input takes the next free name and leaves the first graph alone.
    let changed = run(&["--name", "once", "--set", "tag=b"])?;
    assert_eq!(changed["checkpoint"]["graph_id"], "once-2");
    assert_eq!(changed["named"]["superseded"], "once");
    assert_eq!(graph_count()?, 2);
    // Each input finds its own graph.
    for (tag, id) in [("a", "once"), ("b", "once-2")] {
        let replay = run(&["--name", "once", "--set", &format!("tag={tag}")])?;
        assert_eq!(replay["checkpoint"]["graph_id"], id);
    }
    assert_eq!(graph_count()?, 2);
    // A stopped graph is still the answer to its input: the replay reports it.
    let (output, stopped) = rig.run(&["stop", "once"])?;
    assert!(output.status.success(), "{stopped}");
    let replay = run(&["--name", "once", "--set", "tag=a"])?;
    assert_eq!(replay["checkpoint"]["graph_id"], "once");
    assert_eq!(graph_count()?, 2);
    // An edited template is a different launch under the same input.
    std::fs::write(
        templates.join("stamp.toml"),
        "kind='gate'\n[node.condition]\ntype='budget'\n[parameters.tag]\ntype='string'\ndefault='edited'\n",
    )?;
    let edited = run(&["--name", "once", "--set", "tag=a"])?;
    assert_eq!(edited["checkpoint"]["graph_id"], "once-3", "{edited}");
    assert_eq!(graph_count()?, 3);
    // Without a name the host assigns a valid, unique handle and never replays.
    let mut handles = std::collections::BTreeSet::new();
    for _ in 0..3 {
        let unnamed = run(&["--set", "tag=a"])?;
        let id = unnamed["checkpoint"]["graph_id"]
            .as_str()
            .ok_or("graph id")?
            .to_owned();
        assert!(
            provenant_fabric_runtime::graph::template::fold_graph_name(&id).is_ok(),
            "{id}"
        );
        assert!(id.starts_with("stamp-"), "{id}");
        assert!(handles.insert(id), "handles are unique");
    }
    assert_eq!(graph_count()?, 6);
    Ok(())
}

#[test]
fn a_changed_brief_leads_the_launch_with_the_new_handle_and_json_field() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("stamp.toml"),
        "kind='gate'\n[node.condition]\ntype='budget'\n[parameters.tag]\ntype='string'\ndefault='none'\n",
    )?;
    let json_run = |args: &[&str]| -> Result<Value> {
        let mut command = vec!["run", "stamp"];
        command.extend_from_slice(args);
        let (output, result) = rig.run(&command)?;
        assert!(output.status.success(), "{args:?}: {result}");
        Ok(result["data"].clone())
    };
    let first = json_run(&["--name", "once", "--set", "tag=a"])?;
    assert_eq!(first["checkpoint"]["graph_id"], "once");
    // The JSON form names the prior handle under `superseded`.
    let second = json_run(&["--name", "once", "--set", "tag=b"])?;
    assert_eq!(second["checkpoint"]["graph_id"], "once-2");
    assert_eq!(second["named"]["superseded"], "once");
    // The human form leads with the new handle and the reason, not a headline.
    let output = rig
        .human_command()
        .args(["run", "stamp", "--name", "once", "--set", "tag=c"])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.starts_with("started once-3 (once exists with a different input)\n"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.starts_with("next: ") && line.ends_with("provenant output once-3")),
        "{text}"
    );
    Ok(())
}

#[test]
fn waiting_on_a_superseded_name_points_at_the_newer_handle() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("stamp.toml"),
        "kind='gate'\n[node.condition]\ntype='budget'\n[parameters.tag]\ntype='string'\ndefault='none'\n",
    )?;
    for tag in ["a", "b", "c"] {
        let (output, result) = rig.run(&[
            "run",
            "stamp",
            "--name",
            "wait-once",
            "--set",
            &format!("tag={tag}"),
        ])?;
        assert!(output.status.success(), "{result}");
    }
    let output = rig
        .human_command()
        .args(["wait", "wait-once", "--timeout", "2"])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("; provenant wait wait-once-3\n"), "{text}");
    assert!(
        warning.contains("wait-once") && warning.contains("wait-once-3"),
        "{warning}"
    );
    Ok(())
}

#[test]
fn status_on_a_superseded_name_points_at_the_newer_handle() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("stamp.toml"),
        "kind='gate'\n[node.condition]\ntype='budget'\n[parameters.tag]\ntype='string'\ndefault='none'\n",
    )?;
    for tag in ["a", "b"] {
        let (output, result) = rig.run(&[
            "run",
            "stamp",
            "--name",
            "status-once",
            "--set",
            &format!("tag={tag}"),
        ])?;
        assert!(output.status.success(), "{result}");
    }
    let output = rig
        .human_command()
        .args(["status", "status-once"])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(
        text.lines()
            .any(|line| line.starts_with("next: ")
                && line.ends_with("provenant status status-once-2")),
        "{text}"
    );
    assert!(
        warning.contains("status-once") && warning.contains("status-once-2"),
        "{warning}"
    );
    Ok(())
}

#[test]
fn a_bad_name_refusal_suggests_a_valid_handle() -> Result {
    let rig = GraphProject::new()?;
    let output = rig
        .human_command()
        .args(["run", "stamp", "--name", "Foo-", "--set", "tag=a"])
        .output()?;
    assert!(!output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("try `foo`"), "{text}");
    Ok(())
}

#[test]
fn concurrent_named_runs_with_different_input_each_launch_under_their_own_name() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("stamp.toml"),
        "kind='gate'\n[node.condition]\ntype='budget'\n[parameters.tag]\ntype='string'\ndefault='none'\n",
    )?;
    let launches = 4;
    let barrier = std::sync::Barrier::new(launches);
    let ids = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..launches)
            .map(|index| {
                let (rig, barrier) = (&rig, &barrier);
                scope.spawn(move || -> std::result::Result<String, String> {
                    let tag = format!("tag={index}");
                    barrier.wait();
                    let (output, result) = rig
                        .run(&["run", "stamp", "--name", "race", "--set", &tag])
                        .map_err(|error| error.to_string())?;
                    if !output.status.success() {
                        return Err(result.to_string());
                    }
                    result["data"]["checkpoint"]["graph_id"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| result.to_string())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().map_err(|_| "launch panicked".to_owned())?)
            .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()
    })?;
    assert_eq!(ids.len(), launches, "each input owns one graph: {ids:?}");
    assert!(
        ids.iter().all(|id| id == "race" || id.starts_with("race-")),
        "{ids:?}"
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

// Spec: docs/specs/cli.md#upgrade-verification
#[test]
fn upgrade_verify_dry_run_explains_a_fixture_project() -> Result {
    let rig = GraphProject::new()?;
    let home = rig.temp.path().join("home");
    let scratch = rig.temp.path().join("scratch");
    let fixture_root = home.join(".local/state/provenant-smoke");
    std::fs::create_dir_all(home.join(".ssh"))?;
    std::fs::create_dir_all(&fixture_root)?;
    std::fs::create_dir_all(&scratch)?;
    let output = rig
        .command()
        .current_dir(&home)
        .env("HOME", &home)
        .env("TMPDIR", &scratch)
        .args(["upgrade", "--verify", "--dry-run"])
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{reply}; {output:?}");
    let preview: Value = serde_json::from_str(reply["data"]["preview"].as_str().ok_or("preview")?)?;
    let rows = preview["data"]["source_admission"]
        .as_array()
        .ok_or("source admission")?;
    assert!(!rows.is_empty(), "{preview}");
    for row in rows {
        // Issue #4103: the shared smoke argv passes the writer's branch and
        // base, so its worktree is proposed and admission waits for peer
        // registration. Read-only sources still settle to pending_route. The
        // credential decision stays pending until route selection in both.
        let expected_status = if row["node"]
            .as_str()
            .is_some_and(|node| node.ends_with("write"))
        {
            "pending_registration"
        } else {
            "pending_route"
        };
        assert_eq!(row["status"], expected_status, "{row}");
        assert_eq!(row["credential_admission"], "pending_route", "{row}");
        let path = Path::new(row["path"].as_str().ok_or("source path")?);
        assert!(path.starts_with(fixture_root.canonicalize()?), "{row}");
        assert!(!path.starts_with(scratch.canonicalize()?), "{row}");
    }
    Ok(())
}

// Spec: docs/specs/run-launch.md#writer-binding
#[test]
fn smoke_explanation_uses_selected_project_from_credential_containing_cwd() -> Result {
    let rig = GraphProject::new()?;
    rig.add_review_subject()?;
    let home = rig.temp.path().join("home");
    std::fs::create_dir_all(home.join(".ssh"))?;
    let output = rig
        .command()
        .current_dir(&home)
        .env("HOME", &home)
        .arg("--project")
        .arg(&rig.project)
        .args(["run", "smoke", "--explain"])
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    assert!(output.status.success(), "{reply}; {output:?}");
    let rows = reply["data"]["source_admission"]
        .as_array()
        .ok_or("source admission")?;
    for node in ["read", "review-fixture"] {
        assert!(
            rows.iter().any(|row| row["node"]
                .as_str()
                .is_some_and(|id| id.split("--").any(|part| part == node))),
            "missing {node}: {rows:?}"
        );
    }
    for row in rows {
        // The source root is admitted, but its credential decision is resolved
        // only when the route is selected.
        assert_eq!(row["status"], "pending_route", "{row}");
        assert_eq!(row["credential_admission"], "pending_route", "{row}");
        assert_eq!(
            Path::new(row["path"].as_str().ok_or("source path")?).canonicalize()?,
            rig.project.canonicalize()?
        );
    }
    Ok(())
}

/// `run smoke` is a hermetic lifecycle: whichever checkout the caller stands
/// in, the re-exec'd child is pinned to a nonce fixture under the test HOME and
/// never to the caller's checkout.
// Spec: docs/specs/fabric.md#hermetic-smoke
#[test]
fn smoke_never_reads_the_caller_checkout() -> Result {
    use std::os::unix::fs::PermissionsExt;

    let rig = GraphProject::new()?;
    prepare_state(&rig)?;
    // HEAD and HEAD^ must differ, so a lifecycle that read the caller checkout
    // would carry one of these OIDs into the child it launches.
    rig.add_review_subject()?;
    let oid = |revision: &str| -> Result<String> {
        let output = provenant_os::fixture::git_command()
            .current_dir(&rig.project)
            .args(["rev-parse", revision])
            .output()?;
        assert!(output.status.success(), "{output:?}");
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let caller_head = oid("HEAD")?;
    let caller_parent = oid("HEAD^")?;
    let detached = rig.temp.path().join("detached");
    let git = provenant_os::fixture::git_command()
        .current_dir(&rig.project)
        .args(["worktree", "add", "-q", "--detach"])
        .arg(&detached)
        .arg("HEAD^")
        .output()?;
    assert!(git.status.success(), "{git:?}");
    let plain = rig.temp.path().join("plain");
    std::fs::create_dir(&plain)?;

    // A pure recording `provenant` stub stands in for the re-exec'd child: it
    // logs every argv, reads the nonce out of its fixture, and returns the same
    // typed envelopes run_lifecycle verifies. It never launches a graph or
    // waits on a provider. A recording `git` shim observes fixture creation.
    let home = rig.temp.path().join("home");
    std::fs::create_dir_all(home.join(".local/state"))?;
    let fixture_root = home.join(".local/state/provenant-smoke");
    let bin = rig.temp.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let argv_log = rig.temp.path().join("smoke-argv");
    let git_log = rig.temp.path().join("smoke-git");
    let status_log = rig.temp.path().join("smoke-status");
    let program = bin.join("provenant");
    let stub = r##"#!/bin/sh
printf '%s\n' "$*" >> '@ARGV@'
args="$*"
project=""
while [ $# -gt 0 ]; do
  case "$1" in
    --project) project="$2"; shift 2;;
    *) shift;;
  esac
done
nonce=$(sed -n 's/^nonce: //p' "$project/fixture.txt" 2>/dev/null || true)
summary="nonce: $nonce"
if [ -z "$project" ]; then
  nonce=$(printf '%012x' "$$")
  fixture="$HOME/.local/state/provenant-smoke/$nonce"
  mkdir -p "$fixture"
  (
    cd "$fixture" || exit 1
    git init -q -b main
    printf 'nonce: %s\n' "$nonce" > fixture.txt
    git add fixture.txt
    git -c user.name=Smoke -c user.email=smoke@localhost commit -qm base
  )
  launched=$("$0" --project "$fixture" --json run smoke --name smoke)
  waited=$("$0" --project "$fixture" --json wait smoke --timeout 3600)
  status=$("$0" --project "$fixture" --json status smoke)
  printf '%s\n' "$status" > '@STATUS@'
  smoke=$(printf '%s%s%s' "$launched" "$waited" "$status" | sed 's/"/\\"/g')
  printf '{"schema_version":1,"command":"run","data":{"smoke":"%s"}}\n' "$smoke"
  exit 0
fi
case "$args" in
  *' run smoke '*)
    printf '%s\n' '{"schema_version":1,"command":"run","data":{"status":"pending"}}'
    ;;
  *' wait '*)
    printf '%s\n' '{"schema_version":1,"command":"wait","data":{"status":"complete"}}'
    ;;
  *' status '*)
    printf '{"schema_version":1,"command":"status","data":{"rows":[{"kind":"node","node_id":"read","state":"succeeded","result_summary":"%s"},{"kind":"node","node_id":"review-write","state":"succeeded","result_summary":"%s"},{"kind":"node","node_id":"write","state":"succeeded"}]}}\n' "$summary" "$summary"
    ;;
  *) exit 77;;
esac
"##;
    std::fs::write(
        &program,
        stub.replace("@ARGV@", &argv_log.display().to_string())
            .replace("@STATUS@", &status_log.display().to_string()),
    )?;
    let git_shim = bin.join("git");
    let real_git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    std::fs::write(
        &git_shim,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$PWD\" \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            git_log.display(),
            real_git.display()
        ),
    )?;
    for shim in [&program, &git_shim] {
        std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o755))?;
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // Build the same CLI command the rig uses, but launch it through the
    // recording stub so its argv is observable.
    let smoke_command = |directory: &Path, name: &str| {
        let mut command = Command::new(&program);
        provenant_os::fixture::scrub_git_env(&mut command);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("PROVENANT_") {
                command.env_remove(key);
            }
        }
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("PROVENANT_ROOT", &rig.root)
            .env("XDG_CONFIG_HOME", rig.temp.path().join("config"))
            .env("HOME", &home)
            .env("PATH", &path)
            .env("PROVENANT_GIT", &git_shim)
            .args(["--json", "--state-dir"])
            .arg(rig.state())
            .args(["run", "smoke", "--name", name]);
        command.current_dir(directory);
        command
    };

    for (name, directory) in [
        ("primary", rig.project.clone()),
        ("detached", detached),
        ("plain", plain),
    ] {
        std::fs::write(&argv_log, "")?;
        std::fs::write(&git_log, "")?;
        std::fs::write(&status_log, "")?;
        let output = smoke_command(&directory, name).output()?;
        let reply: Value = serde_json::from_slice(&output.stdout)?;
        assert!(reply["data"]["smoke"].is_string(), "{name}: {reply}");
        let invoked = std::fs::read_to_string(&argv_log)?;
        assert!(
            invoked.lines().any(|line| line.contains("run smoke")),
            "{name}: the smoke edge was not invoked: {invoked}"
        );
        // Every child invocation is pinned to a fixture project under the test
        // HOME, never to the checkout the caller stood in.
        assert!(
            invoked.lines().any(|line| {
                line.split_once("--project ").is_some_and(|(_, rest)| {
                    Path::new(rest.split_whitespace().next().unwrap_or_default())
                        .starts_with(&fixture_root)
                })
            }),
            "{name}: no argv named a fixture project: {invoked}"
        );
        let calls = std::fs::read_to_string(&git_log)?;
        assert!(
            calls.lines().any(|line| {
                line.split_once(' ').is_some_and(|(cwd, args)| {
                    Path::new(cwd).starts_with(&fixture_root) && args.contains("init")
                })
            }),
            "{name}: no fixture was created under the test home: {calls}"
        );
        let status: Value = serde_json::from_str(&std::fs::read_to_string(&status_log)?)?;
        let rows = status["data"]["rows"]
            .as_array()
            .ok_or_else(|| format!("{name}: status rows: {status}"))?;
        for node in ["read", "review-write"] {
            let row = rows
                .iter()
                .find(|row| row["node_id"].as_str() == Some(node))
                .ok_or_else(|| format!("{name}: no {node} row: {rows:?}"))?;
            assert_eq!(row["state"], "succeeded", "{name}: {row}");
            assert!(
                row["result_summary"]
                    .as_str()
                    .is_some_and(|summary| summary.contains("nonce:")),
                "{name}: {row}"
            );
        }
        let write = rows
            .iter()
            .find(|row| row["node_id"].as_str() == Some("write"))
            .ok_or_else(|| format!("{name}: no write row: {rows:?}"))?;
        assert_eq!(write["state"], "succeeded", "{name}: {write}");
        let wire = reply.to_string();
        assert!(
            !wire.contains(&caller_head),
            "{name}: caller head leaked: {wire}"
        );
        assert!(
            !wire.contains(&caller_parent),
            "{name}: caller parent leaked: {wire}"
        );
    }
    Ok(())
}

// Spec: docs/specs/run-launch.md#writer-binding
#[test]
fn bundled_smoke_binds_branch_and_worktree_controls_to_its_write_node() -> Result {
    let rig = GraphProject::new()?;
    rig.add_review_subject()?;
    let (_, explained) = rig.run(&[
        "run",
        "smoke",
        "--explain",
        "--branch",
        "smoke/x",
        "--base",
        "HEAD",
    ])?;
    let write = explained["data"]["document"]["nodes"][0]["composite"]["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["id"] == "write"))
        .ok_or_else(|| format!("write node: {explained}"))?;
    assert_eq!(write["attempt"]["new_worktree"]["branch"], "smoke/x");
    assert_eq!(write["attempt"]["new_worktree"]["base"], "HEAD");
    assert!(
        !explained["data"]["warnings"]
            .to_string()
            .contains("smoke skipped"),
        "{explained}"
    );

    let (_, explained) = rig.run(&["run", "smoke", "--explain", "--worktree", "elsewhere"])?;
    let write = explained["data"]["document"]["nodes"][0]["composite"]["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["id"] == "write"))
        .ok_or_else(|| format!("write node: {explained}"))?;
    assert!(
        write["worktree"]
            .as_str()
            .is_some_and(|path| Path::new(path).is_absolute() && path.ends_with("elsewhere")),
        "{write}"
    );
    Ok(())
}

/// `--route` takes what `run implement --model` takes: an alias or an exact
/// model. The smoke graph is the upgrade verification path, so it must compile
/// with a short or full catalogue id and carry one canonical spelling.
// Spec: docs/specs/routing.md#launch-route-and-role-tiers
#[test]
fn smoke_explains_with_an_exact_model_route() -> Result {
    fn leaves(nodes: &[Value], out: &mut Vec<Value>) {
        for node in nodes {
            if let Some(children) = node["composite"]["nodes"].as_array() {
                leaves(children, out);
            } else if let Some(body) = node["loop"]["body"].as_array() {
                leaves(body, out);
            } else {
                out.push(node.clone());
            }
        }
    }
    let rig = GraphProject::new()?;
    for (tier, expected) in [
        (
            "opencode/opencode/deepseek-v4.1-flash-free",
            "opencode/opencode/deepseek-v4.1-flash-free",
        ),
        (
            "opencode/hy4-flash-free",
            "opencode/opencode/hy4-flash-free",
        ),
    ] {
        let (output, explained) = rig.run(&["run", "smoke", "--explain", "--route", tier])?;
        assert!(output.status.success(), "{tier}: {explained}");
        let nodes = explained["data"]["document"]["nodes"][0]["composite"]["nodes"]
            .as_array()
            .ok_or_else(|| format!("smoke nodes: {explained}"))?;
        let mut routed = Vec::new();
        leaves(nodes, &mut routed);
        routed.retain(|node| !node["route"].is_null());
        assert!(!routed.is_empty(), "{tier}: {explained}");
        for node in &routed {
            assert_eq!(node["route"]["adapter"], "opencode", "{tier}: {node}");
            assert_eq!(node["route"]["model"], expected, "{tier}: {node}");
            assert!(node["route"]["alias"].is_null(), "{tier}: {node}");
        }
    }
    Ok(())
}

/// A refusal of a launch input names its field, never a bare "invalid input".
// Spec: docs/specs/run-launch.md#writer-binding
#[test]
fn refused_launch_controls_name_their_field() -> Result {
    let rig = GraphProject::new()?;
    let templates = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&templates)?;
    std::fs::write(
        templates.join("pair.toml"),
        "nodes = [\n  { id = 'a', use = 'task', prompt = 'first' },\n  { id = 'b', use = 'task', prompt = 'second' },\n]\n",
    )?;
    for (args, field) in [
        (
            vec![
                "run",
                "pair",
                "--explain",
                "--branch",
                "b/x",
                "--base",
                "HEAD",
            ],
            "branch",
        ),
        (
            vec!["run", "pair", "--explain", "--worktree", "w"],
            "worktree",
        ),
        (
            vec!["run", "pair", "--explain", "--mode", "read-only"],
            "access",
        ),
        (
            vec!["run", "smoke", "--explain", "--mode", "read-only"],
            "access",
        ),
        (
            vec!["run", "smoke", "--explain", "--write-scope", "src"],
            "write_scope",
        ),
        (
            vec![
                "run",
                "smoke",
                "--explain",
                "--branch",
                "bad name",
                "--base",
                "HEAD",
            ],
            "branch",
        ),
    ] {
        let (output, reply) = rig.run(&args)?;
        assert!(!output.status.success(), "{args:?}: {reply}");
        let message = reply["error"]["message"].as_str().ok_or("message")?;
        assert!(
            message.contains(&format!("invalid input field `{field}`")),
            "{args:?}: {reply}"
        );
    }
    Ok(())
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

/// Assert that the template CLI, and a raw document on the CLI
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
    for args in [vec!["run", name, "--explain"], vec!["run", name]] {
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
    assert!(
        messages.iter().all(|message| message == expected),
        "{name}: {messages:?}"
    );
    assert!(graph_records(&rig.state())?.is_empty(), "{name}");
    Ok(())
}

/// A composite admission refusal names the validator reason. Unlike the exact
/// helper above, this pins the parts of the reason that are stable while the
/// offending node id may be long or scoped.
fn assert_compile_refusal_contains(
    rig: &GraphProject,
    templates: &[(&str, String)],
    name: &str,
    document: &Value,
    config: &str,
    expected: &[&str],
) -> Result {
    let directory = rig.project.join(".provenant/templates");
    std::fs::create_dir_all(&directory)?;
    for (file, text) in templates {
        std::fs::write(directory.join(format!("{file}.toml")), text)?;
    }
    std::fs::write(rig.state().join("config.toml"), config)?;
    let path = rig.document(document)?;
    let mut messages = Vec::new();
    for args in [vec!["run", name, "--explain"], vec!["run", name]] {
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
    assert!(
        messages
            .iter()
            .all(|message| expected.iter().all(|part| message.contains(*part))),
        "{name}: {messages:?}"
    );
    assert!(graph_records(&rig.state())?.is_empty(), "{name}");
    Ok(())
}

#[test]
fn oversized_graph_expansion_is_refused_before_journalling() -> Result {
    let rig = GraphProject::new()?;
    prepare_state(&rig)?;
    // The compiler-only fan-out cap is reported consistently before admission.
    let over = (0..257)
        .map(|number| format!("'area-{number}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let template = format!("nodes = [{{ id = 'swarm', use = 'task', over = [{over}] }}]\n");
    let document = json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"swarm","nodes":[{"id":"swarm","use":"task","over":(0..257).map(|n|format!("area-{n}")).collect::<Vec<_>>()}]});
    assert_compile_refusal_matches(
        &rig,
        &[("swarm", template)],
        "swarm",
        &document,
        "schema_version = 1\n",
        "`over` requires 1..=256 items",
    )?;

    // A graph that compiles to more nodes than the state admits: the template
    // CLI and the raw document CLI must read the same refusal.
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
            "nodes = [\n  { id = 'a', use = 'task', prompt = 'first' },\n  { id = 'b', use = 'task', prompt = 'second' },\n]\n"
                .into(),
        )],
        "two",
        &json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"two","nodes":[{"id":"two","kind":"composite","access":"read_only","composite":{
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
            "nodes = [\n  { id = 'a', use = 'task', prompt = 'one' },\n  { id = 'b', use = 'task', prompt = 'two', after = ['a'] },\n  { id = 'c', use = 'task', prompt = 'three', after = ['b'] },\n]\n"
                .into(),
        )],
        "chain",
        &json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"chain","nodes":[
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
    assert_compile_refusal_contains(
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
        &json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"deep","nodes":[{"id":first,"kind":"composite","access":"read_only","composite":{
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
        &[
            "expanded node id",
            "is invalid; use at most 200 ASCII letters, digits, dots, underscores or hyphens",
        ],
    )?;

    // Composite expansion refuses duplicate exports of one member and names why.
    assert_compile_refusal_contains(
        &rig,
        &[(
            "duplicate-export",
            "nodes = [{ id = 'member', use = 'task' }]\nexports = [{ node = 'member', outputs = ['result'] }, { node = 'member', outputs = ['result'] }]\n"
                .into(),
        )],
        "duplicate-export",
        &json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"duplicate-export","nodes":[{"id":"duplicate-export","kind":"composite","access":"read_only","composite":{
            "nodes":[{"id":"member","prompt":"work","access":"read_only","route":{"alias":"senior"}}],
            "exports":[{"node":"member","outputs":["result"]},{"node":"member","outputs":["result"]}]
        }}]}),
        "schema_version = 1\n",
        &["composite export must select a unique direct child"],
    )?;
    Ok(())
}

#[test]
fn steer_graph_and_node_refuse_an_unlaunched_attempt_before_reading_stdin() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"steer-ambiguous","nodes":[{"id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
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
    let attempt = json!({"key":{"graph_id":"steer-ambiguous","node_id":"accept","route_index":0, "attempt": 1},
        "launched":false,"route":{"alias":"senior"},"state":"starting","attempt_ref":null,
        "outcome":null,"launch_actor":"fixture"});
    checkpoint.nodes[0].state = NodeState::Running;
    checkpoint.nodes[0].attempts = serde_json::from_value(json!([attempt]))?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5)?;
    let mut journal =
        FileJournal::open(&state, project, "steer-ambiguous", budget, &cancel, &clock)?;
    journal.bind(execution)?;
    journal.save(&checkpoint)?;
    drop(journal);
    // A node target names its newest attempt; one with no provider session
    // yet is refused as a target before any steer text is read.
    for target in ["steer-ambiguous", "steer-ambiguous/accept"] {
        let (output, value) = rig.run(&["steer", target, "--brief-file", "-"])?;
        assert!(!output.status.success(), "{value}");
        assert_eq!(value["status"], "refused", "{value}");
        assert_eq!(value["error"]["code"], "steer_target_refused", "{value}");
    }
    checkpoint.nodes[0].state = NodeState::Succeeded;
    checkpoint.nodes[0].attempts[0].state = provenant_fabric_runtime::graph::AttemptState::Terminal;
    checkpoint.nodes[0].attempts[0].outcome =
        Some(provenant_fabric_runtime::graph::GraphAttemptOutcome::Succeeded);
    let mut journal = FileJournal::open(
        &state,
        project,
        "steer-ambiguous",
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.save(&checkpoint)?;
    drop(journal);
    // Spec: docs/specs/fabric.md#session-ready-steering
    // The node's graph is now complete: no future attempt can read the note.
    // Steer refuses with a typed cause and a new-round command instead of
    // reporting a queued follow-up that will never run.
    let (output, value) =
        rig.run(&["steer", "steer-ambiguous/accept", "--brief", "amended work"])?;
    assert!(!output.status.success(), "{value}");
    assert_eq!(value["status"], "refused", "{value}");
    assert_eq!(value["error"]["code"], "steer_graph_complete", "{value}");
    assert!(
        serde_json::to_string(&value["next"][0])?.contains("provenant run implement --worktree"),
        "{value}"
    );
    let human = rig
        .human_command()
        .args(["steer", "steer-ambiguous/accept", "--brief", "amended work"])
        .output()?;
    assert!(!human.status.success(), "{human:?}");
    // Non-JSON refusals are rendered on stderr, not stdout.
    let text = String::from_utf8_lossy(&human.stderr);
    assert!(
        text.contains("has no further attempt")
            && text.contains("provenant run implement --worktree"),
        "{text}"
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
// Invariant: a node-scoped steer at a settled node refuses with a next step
// even when a sibling is still running. A sibling's turn cannot read this
// node's note, so graph-wide "unfinished" is not a follow-up for the target.
#[test]
fn steer_at_a_settled_node_refuses_even_while_a_sibling_is_running() -> Result {
    let rig = GraphProject::new()?;
    let document = json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"steer-sibling","nodes":[
        {"id":"done","kind":"gate","access":"read_only","condition":{"type":"budget"}},
        {"id":"live","kind":"gate","access":"read_only","condition":{"type":"budget"}}
    ]});
    let path = rig.document(&document)?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(output.status.success(), "{output:?}");
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphJournal, NodeState};
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "graph-steer-sibling.json")
        })
        .ok_or("checkpoint")?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let execution = FileJournal::execution(&state, project, "steer-sibling")?.ok_or("execution")?;
    let mut checkpoint =
        FileJournal::read_checkpoint(&state, project, "steer-sibling")?.ok_or("checkpoint")?;
    let attempt = json!({"key":{"graph_id":"steer-sibling","node_id":"done","route_index":0,"attempt":1},
        "launched":false,"route":{"alias":"senior"},"state":"starting","attempt_ref":null,
        "outcome":null,"launch_actor":"fixture"});
    checkpoint.nodes[0].state = NodeState::Succeeded;
    checkpoint.nodes[0].attempts = serde_json::from_value(json!([attempt]))?;
    checkpoint.nodes[0].attempts[0].state = AttemptState::Terminal;
    checkpoint.nodes[0].attempts[0].outcome =
        Some(provenant_fabric_runtime::graph::GraphAttemptOutcome::Succeeded);
    // A sibling still running keeps the graph unfinished, but cannot read the
    // settled node's steering note.
    checkpoint.nodes[1].state = NodeState::Running;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &state,
        project,
        "steer-sibling",
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.bind(execution)?;
    journal.save(&checkpoint)?;
    drop(journal);

    let (output, value) = rig.run(&["steer", "steer-sibling/done", "--brief", "amended work"])?;
    assert!(!output.status.success(), "{value}");
    assert_eq!(value["status"], "refused", "{value}");
    assert_eq!(value["error"]["code"], "steer_graph_complete", "{value}");
    assert!(
        serde_json::to_string(&value["next"][0])?.contains("steer-sibling/done"),
        "{value}"
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn steer_on_a_complete_graph_refuses_even_when_warn_is_configured() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"steer-settled-warn","nodes":[{"id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(output.status.success(), "{output:?}");
    std::fs::write(
        rig.state().join("config.toml"),
        "schema_version=1\n[knobs]\n'lanes.steer_settled'='warn'\n",
    )?;
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "graph-steer-settled-warn.json")
        })
        .ok_or("checkpoint")?;
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{
        AttemptState, FileJournal, GraphAttemptOutcome, GraphJournal, NodeState,
    };
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
        FileJournal::execution(&state, project, "steer-settled-warn")?.ok_or("execution")?;
    let mut checkpoint =
        FileJournal::read_checkpoint(&state, project, "steer-settled-warn")?.ok_or("checkpoint")?;
    let attempt = json!({"key":{"graph_id":"steer-settled-warn","node_id":"accept","route_index":0, "attempt": 1},
        "launched":false,"route":{"alias":"senior"},"state":"starting","attempt_ref":null,
        "outcome":null,"launch_actor":"fixture"});
    checkpoint.nodes[0].state = NodeState::Succeeded;
    checkpoint.nodes[0].attempts = serde_json::from_value(json!([attempt]))?;
    checkpoint.nodes[0].attempts[0].state = AttemptState::Terminal;
    checkpoint.nodes[0].attempts[0].outcome = Some(GraphAttemptOutcome::Succeeded);
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &state,
        project,
        "steer-settled-warn",
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.bind(execution)?;
    journal.save(&checkpoint)?;
    drop(journal);
    // A completed graph has no follow-up to warn about: the steer is refused
    // before either settled mode can accept text nothing will read.
    let (output, value) = rig.run(&[
        "steer",
        "steer-settled-warn/accept",
        "--brief",
        "ignored steer note",
    ])?;
    assert!(!output.status.success(), "{value}");
    assert_eq!(value["status"], "refused", "{value}");
    assert_eq!(value["error"]["code"], "steer_graph_complete", "{value}");
    assert!(
        serde_json::to_string(&value["next"][0])?.contains("provenant run implement --worktree"),
        "{value}"
    );
    // No note is written for a follow-up that will never run.
    let (output, notes) = rig.run(&[
        "notes",
        "read",
        "--scope",
        "steer-settled-warn/accept",
        "--replay",
    ])?;
    assert!(output.status.success(), "{notes}");
    let events = notes["data"]["events"].as_array().ok_or("notes events")?;
    assert!(
        events.iter().all(|event| !event["detail"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("ignored steer note"))),
        "{notes}"
    );
    Ok(())
}

#[test]
fn unreadable_graph_file_is_refused_by_real_cli_before_admission() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if provenant_os::fs::effective_uid() == 0 {
        return Ok(());
    }
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"unreadable","nodes":[]}))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))?;
    let output = rig.command().args(["run", "--file"]).arg(&path).output()?;
    assert!(!output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["error"]["code"], "invalid_input", "{value}");
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
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
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
    let seed = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"seed","nodes":[{"id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}}]}))?;
    let output = rig.command().args(["run", "--file"]).arg(seed).output()?;
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
    for (id, fact) in [
        ("schema-refusal", Some(InvalidInputDetail::Schema)),
        (
            "oversized-input-refusal",
            Some(InvalidInputDetail::InputTooLarge),
        ),
        (
            "unreadable-input-refusal",
            Some(InvalidInputDetail::InputUnreadable),
        ),
        ("code-only-refusal", None),
    ] {
        let mut document: GraphDocument = serde_json::from_value(
            json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":id,"nodes":[{"id":"one","prompt":"fixture","access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]}),
        )?;
        graph::patch::submission(
            &mut document,
            &format!("submit:{id}"),
            &Default::default(),
            provenant_config::PortTable::builtin().worktree_layout,
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
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
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
            run.checkpoint.nodes[0].advance_code.as_deref(),
            Some("launch_refused")
        );
        assert_eq!(
            run.checkpoint.nodes[0].advance_owner_code.as_deref(),
            Some("invalid_input")
        );
        assert!(
            run.checkpoint.nodes[0]
                .advance_note
                .as_deref()
                .is_some_and(|error| !error.is_empty())
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
            row["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
        assert!(row["next"].as_str().is_some_and(|next| !next.is_empty()));
        let (_, output) = rig.run(&["output", id])?;
        let failure = &output["data"]["graph_failures"][0];
        assert_eq!(failure["cause_code"], row["cause_code"], "{output}");
        assert_eq!(failure["reason"], row["reason"], "{output}");
        assert_eq!(failure["next"], row["next"], "{output}");
        if id == "schema-refusal" {
            let (refused, invalid) = rig.run(&["retry", &format!("{id}/missing")])?;
            assert_eq!(refused.status.code(), Some(2), "{invalid}");
            assert_eq!(invalid["error"]["code"], "NOT_FOUND", "{invalid}");
            assert!(
                invalid["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("{id}/missing")),
                "{invalid}"
            );
            let (output, retried) = rig.run(&["retry", &format!("{id}/one")])?;
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

/// The newest stored prompt of a graph node, read from its checkpoint.
fn stored_prompt(rig: &GraphProject, graph: &str, node: &str) -> Result<(String, usize)> {
    use provenant_core::{RootInputs, StateRoot};
    use provenant_fabric_runtime::graph::FileJournal;
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| *name == *format!("graph-{graph}.json"))
        })
        .ok_or("checkpoint file")?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let checkpoint = FileJournal::read_checkpoint(&state, project, graph)?.ok_or("checkpoint")?;
    let prompt = checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .flat_map(|patch| &patch.replaced)
        .rfind(|candidate| candidate.id == node)
        .map(|candidate| candidate.prompt.clone())
        .ok_or("replacement node")?;
    let previous = checkpoint
        .nodes
        .iter()
        .find(|candidate| candidate.id == node)
        .map_or(0, |candidate| candidate.previous_attempts.len());
    Ok((prompt, previous))
}

fn settled_task_graph(rig: &GraphProject) -> Result<String> {
    let (_, started) = rig.run(&["run", "task", "old brief", "--model", "fixture/model"])?;
    let id = started["data"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or_else(|| format!("graph id: {started}"))?;
    // The fixture provider cannot run, so the graph settles unsuccessfully
    // (or asks for input); either ends the wait.
    // In JSON mode wait emits progress event rows before its final command
    // record. This helper only needs the graph to settle; status below checks
    // the public failed-node state.
    let _wait = rig
        .command()
        .args(["wait", id, "--timeout", "10s"])
        .output()?;
    let (_, settled) = rig.run(&["status", id])?;
    assert!(
        settled["data"]["rows"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["kind"] == "node" && row["state"] == "failed")),
        "{settled}"
    );
    Ok(id.to_owned())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn settled_single_node_graph_retry_infers_the_node() -> Result {
    let rig = GraphProject::new()?;
    let id = settled_task_graph(&rig)?;
    let (output, retried) = rig.run(&["retry", &id])?;
    assert!(
        output.status.success(),
        "retry: {retried}, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // An inferred node is a changed retry: it resets the node and stages a new
    // attempt, rather than being admitted as a running no-op.
    assert!(
        retried["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["kind"] == "attempt")),
        "{retried}"
    );
    Ok(())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn graph_retry_extends_budget_after_advisory_failure() -> Result {
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, NodeState};

    let rig = GraphProject::new()?;
    let id = "advisory-budget";
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":id,"nodes":[
        {"id":"advisory","kind":"gate","access":"read_only","advisory":true,"condition":{"type":"budget"}},
        {"id":"pending","kind":"gate","access":"read_only","depends_on":["advisory"],"condition":{"type":"budget"}}
    ]}))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(output.status.success(), "{output:?}");
    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "graph-advisory-budget.json")
        })
        .ok_or("checkpoint")?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let mut checkpoint = FileJournal::read_checkpoint(&state, project, id)?.ok_or("checkpoint")?;
    checkpoint.nodes[0].state = NodeState::Failed;
    let gate = checkpoint.nodes[0].gate.as_mut().ok_or("gate result")?;
    gate.passed = false;
    gate.reason = Some("advisory failed".into());
    checkpoint.nodes[0]
        .outputs
        .as_mut()
        .ok_or("gate outputs")?
        .verdict = Some("FAIL".into());
    checkpoint.nodes[1].state = NodeState::Pending;
    checkpoint.nodes[1].gate = None;
    checkpoint.nodes[1].outputs = None;
    checkpoint.pause = Some(serde_json::from_value(
        json!({"cause":{"kind":"budget_exceeded"},"reason":"budget spent"}),
    )?);
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &state,
        project,
        id,
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.save(&checkpoint)?;
    drop(journal);
    std::fs::write(
        rig.state().join("config.toml"),
        "schema_version = 1\n[domains.fabric.graph.budget.per_graph]\ntokens = 10\n",
    )?;

    let (output, retried) = rig.run(&["retry", id, "--extend", "budget.per_graph.tokens=30"])?;
    assert!(output.status.success(), "{retried}");
    let resumed = FileJournal::read_checkpoint(&state, project, id)?.ok_or("checkpoint")?;
    assert!(
        resumed
            .extensions
            .iter()
            .any(|extension| extension.contains("per_graph.tokens=30")),
        "{resumed:?}"
    );
    assert_ne!(resumed.nodes[1].state, NodeState::Pending, "{resumed:?}");
    Ok(())
}

#[test]
fn secret_shaped_retry_brief_proceeds_with_field_only_warning() -> Result {
    for qualified in [true, false] {
        let rig = GraphProject::new()?;
        rig.enable_secret_redaction()?;
        let id = settled_task_graph(&rig)?;
        let target = if qualified {
            format!("{id}/task")
        } else {
            id.clone()
        };
        let brief = "line one\nghp_abcdefghijklmnopqrstuvwxyz0123456789\n";
        let brief_path = rig.project.join("amended-brief.txt");
        std::fs::write(&brief_path, brief)?;
        let (output, retried) = if qualified {
            rig.run(&[
                "retry",
                &target,
                "--brief-file",
                brief_path.to_str().ok_or("brief path")?,
            ])?
        } else {
            rig.run(&["retry", &target, "--brief", brief])?
        };
        assert!(output.status.success(), "{target}: {retried}");
        let warnings = retried["data"]["warnings"]
            .as_array()
            .ok_or("retry warnings")?;
        assert!(warnings.iter().any(|w| {
            w.as_str()
                .is_some_and(|w| w.contains("brief") && w.contains("looks like a credential"))
        }));
        assert!(warnings.iter().all(|w| {
            !w.to_string()
                .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
        }));
        let (prompt, previous) = stored_prompt(&rig, &id, "task")?;
        assert_eq!(prompt, brief, "{target}");
        assert!(previous > 0, "started attempts are kept: {target}");
    }
    Ok(())
}

#[test]
fn retry_brief_on_a_graph_of_several_nodes_asks_for_the_node() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"two-nodes","nodes":[
        {"id":"first","kind":"gate","access":"read_only","condition":{"type":"budget"}},
        {"id":"second","kind":"gate","access":"read_only","condition":{"type":"budget"}}
    ]}))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(output.status.success(), "{output:?}");
    let (_, before) = rig.run(&["status", "two-nodes"])?;
    let (output, refusal) = rig.run(&["retry", "two-nodes", "--brief", "corrected"])?;
    assert!(!output.status.success(), "{refusal}");
    // The typed code is the contract; the refusal changes nothing.
    assert_eq!(refusal["error"]["code"], "graph_target", "{refusal}");
    let (_, after) = rig.run(&["status", "two-nodes"])?;
    assert_eq!(after["data"]["rows"], before["data"]["rows"], "{after}");
    Ok(())
}

// Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
#[test]
fn shape_and_exact_retry_credential_warnings_allow_retry() -> Result {
    let rig = GraphProject::new()?;
    let id = settled_task_graph(&rig)?;
    fn snapshot_tree(
        root: &Path,
        output: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result {
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                snapshot_tree(&entry.path(), output)?;
            } else {
                output.insert(entry.path(), std::fs::read(entry.path())?);
            }
        }
        Ok(())
    }
    let snapshot = || -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
        let mut output = std::collections::BTreeMap::new();
        snapshot_tree(&rig.state().join("fabric/graphs"), &mut output)?;
        Ok(output)
    };
    let off_credential = "plain-off-retry-credential";
    let off = rig
        .command()
        .env("FIXTURE_API_KEY", off_credential)
        .args(["retry", &id, "--brief", off_credential])
        .output()?;
    assert!(
        off.status.success(),
        "{}",
        String::from_utf8_lossy(&off.stdout)
    );
    assert!(!String::from_utf8_lossy(&off.stderr).contains("declared credential value rule"));
    let before = snapshot()?;
    std::fs::write(
        rig.state().join("config.toml"),
        "schema_version=1\n[knobs]\nredaction='secrets'\n'policy.SECRET_SHAPED_INPUT'='warn'\n",
    )?;
    let shape = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let (output, reply) = rig.run(&["retry", &id, "--brief", shape])?;
    assert!(
        output.status.success(),
        "shape-only input must proceed: {reply}"
    );
    assert!(
        reply["data"]["warnings"]
            .to_string()
            .contains("SECRET_SHAPED_INPUT"),
        "shape-only input still warns: {reply}"
    );
    assert!(
        !reply["data"]["warnings"]
            .to_string()
            .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
    );
    let after_shape = snapshot()?;
    assert_ne!(
        after_shape, before,
        "the admitted retry changes the checkpoint"
    );

    let credential = "plain-registered-retry-3391";
    let output = rig
        .command()
        .env("FIXTURE_API_KEY", credential)
        .args(["retry", &id, "--brief", credential])
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        output.status.success(),
        "exact credential retry must proceed: {reply}"
    );
    let warnings = reply["warnings"].as_array().ok_or("warning envelope")?;
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    assert!(
        warnings.iter().any(|warning| {
            warning["message"].as_str().is_some_and(|message| {
                message.contains("declared credential value rule")
                    && message.contains("matching a known credential")
            })
        }),
        "{reply}"
    );
    assert!(!reply["warnings"].to_string().contains(credential));
    let after_credential = snapshot()?;
    assert_ne!(after_credential, after_shape);

    let (output, reply) = rig.run(&["retry", &id, "--extend", "limits.unconfigured=1"])?;
    assert!(
        !output.status.success(),
        "invalid extension must refuse: {reply}"
    );
    assert!(
        !reply
            .to_string()
            .contains("graph input could not be processed")
    );
    assert_eq!(
        snapshot()?,
        after_credential,
        "invalid extension refusal is atomic"
    );
    Ok(())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn placeholder_and_oversized_ambient_credentials_allow_retry() -> Result {
    let rig = GraphProject::new()?;
    rig.enable_secret_redaction()?;
    let id = settled_task_graph(&rig)?;
    fn snapshot_tree(
        root: &Path,
        output: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result {
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                snapshot_tree(&entry.path(), output)?;
            } else {
                output.insert(entry.path(), std::fs::read(entry.path())?);
            }
        }
        Ok(())
    }
    let snapshot = || -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
        let mut output = std::collections::BTreeMap::new();
        snapshot_tree(&rig.state().join("fabric/graphs"), &mut output)?;
        Ok(output)
    };
    let before = snapshot()?;

    // Short placeholder and ambient values (e.g. ollama, EMPTY, [REDACTED])
    // must not refuse an ordinary retry brief containing those words.
    let output = rig
        .command()
        .env("FIXTURE_API_KEY", "ollama")
        .env("OPENAI_API_KEY", "[REDACTED]")
        .env("X_AUTH", "EMPTY")
        .args([
            "retry",
            &id,
            "--brief",
            "retry with ollama model and EMPTY list",
        ])
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        output.status.success(),
        "short placeholder ambient credentials must not refuse: {reply}"
    );
    let after_placeholder = snapshot()?;
    assert_ne!(
        after_placeholder, before,
        "the placeholder admitted retry changes the checkpoint"
    );

    // An out-of-bounds ambient credential variable (>16 KiB) is skipped only
    // for retry admission, with one typed warning and no disclosed name/value.
    let oversized = "x".repeat(16 * 1024 + 1);
    let output = rig
        .command()
        .env("FIXTURE_API_KEY", &oversized)
        .args(["retry", &id, "--brief", "ordinary retry brief"])
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        output.status.success(),
        "out-of-bounds ambient credentials must not refuse: {reply}"
    );
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty(),
        "--json carries warnings in the envelope, never on stderr: {stderr}"
    );
    let warnings = reply["warnings"].as_array().ok_or("warning envelope")?;
    let skipped: Vec<&Value> = warnings
        .iter()
        .filter(|warning| warning["code"] == "RETRY_CREDENTIALS_SKIPPED")
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "one typed retry-admission warning per invocation: {reply}"
    );
    assert!(
        skipped[0]["message"]
            .as_str()
            .is_some_and(|message| message
                .contains("retry admission skipped out-of-bounds ambient credentials")),
        "{reply}"
    );
    assert!(
        !reply["warnings"].to_string().contains("FIXTURE_API_KEY"),
        "warning must omit variable names: {reply}"
    );
    assert!(
        !reply["warnings"].to_string().contains(&oversized),
        "warning must omit credential values: {reply}"
    );
    Ok(())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn succeeded_graph_retry_suggests_no_succeeded_node() -> Result {
    let rig = GraphProject::new()?;
    let path = rig.document(&json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":"all-succeeded","nodes":[
        {"id":"first","kind":"gate","access":"read_only","condition":{"type":"budget"}},
        {"id":"second","kind":"gate","access":"read_only","condition":{"type":"budget"}}
    ]}))?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(output.status.success(), "{output:?}");
    let (output, refusal) = rig.run(&["retry", "all-succeeded"])?;
    assert!(!output.status.success(), "{refusal}");
    let message = refusal["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("first"),
        "must not suggest succeeded node: {message}"
    );
    assert!(
        !message.contains("second"),
        "must not suggest succeeded node: {message}"
    );
    assert!(message.contains("all-succeeded/NODE"), "{message}");
    Ok(())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn cancelled_graph_retry_resumes_without_refusal() -> Result {
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, NodeState};

    let rig = GraphProject::new()?;
    let id = "cancelled-retry";
    let path = rig.document(
        &json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":id,"nodes":[
            {"id":"work","kind":"gate","access":"read_only","condition":{"type":"budget"}}
        ]}),
    )?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(output.status.success(), "{output:?}");

    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "graph-cancelled-retry.json")
        })
        .ok_or("checkpoint")?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let mut checkpoint = FileJournal::read_checkpoint(&state, project, id)?.ok_or("checkpoint")?;
    checkpoint.cancelled = true;
    checkpoint.nodes[0].state = NodeState::Skipped;
    checkpoint.nodes[0].skip_reason = Some("graph cancelled".into());
    checkpoint.nodes[0].cancelled_from = Some(NodeState::Pending);
    checkpoint.nodes[0].gate = None;
    checkpoint.nodes[0].outputs = None;

    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &state,
        project,
        id,
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.save(&checkpoint)?;
    drop(journal);

    let (output, retried) = rig.run(&["retry", id])?;
    assert!(
        output.status.success(),
        "cancelled graph retry must not be refused: {retried}"
    );
    let resumed = FileJournal::read_checkpoint(&state, project, id)?.ok_or("checkpoint")?;
    assert!(!resumed.cancelled, "cancelled state must be cleared");
    Ok(())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
// A cancel flag alone restores nothing (#3456), but a settled graph of one node
// now infers its only node (#3976): the retry stages a fresh attempt instead of
// being refused as a graph-wide no-op.
#[test]
fn cancelled_flag_without_restorable_work_infers_the_only_node() -> Result {
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal};

    let rig = GraphProject::new()?;
    let id = settled_task_graph(&rig)?;

    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == format!("graph-{id}.json").as_str())
        })
        .ok_or("checkpoint")?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let mut checkpoint = FileJournal::read_checkpoint(&state, project, &id)?.ok_or("checkpoint")?;
    checkpoint.cancelled = true;

    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &state,
        project,
        &id,
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.save(&checkpoint)?;
    drop(journal);

    let (output, retried) = rig.run(&["retry", &id])?;
    assert!(
        output.status.success(),
        "retry: {retried}, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The inferred node is a changed retry: it resets the node and stages a
    // new attempt, and the cancel fence is cleared with it.
    assert!(
        retried["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["kind"] == "attempt")),
        "{retried}"
    );
    let resumed = FileJournal::read_checkpoint(&state, project, &id)?.ok_or("checkpoint")?;
    assert!(!resumed.cancelled, "cancelled state must be cleared");
    Ok(())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn expansion_paused_graph_retry_resumes_without_refusal() -> Result {
    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, NodeDecisionCause};

    let rig = GraphProject::new()?;
    let id = settled_task_graph(&rig)?;

    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == format!("graph-{id}.json").as_str())
        })
        .ok_or("checkpoint")?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let mut checkpoint = FileJournal::read_checkpoint(&state, project, &id)?.ok_or("checkpoint")?;
    checkpoint.nodes[0].expansion_pause = Some("plan expansion limit".into());
    checkpoint.nodes[0].expansion_cause = Some(NodeDecisionCause::PlanExpansionPaused {
        active: true,
        fallback: false,
        fragment_error: None,
        structural_limit: None,
    });
    checkpoint.set_pause(
        provenant_records::pause::PauseCause::InvalidPlanExpansion,
        "plan expansion limit",
    );

    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &state,
        project,
        &id,
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.save(&checkpoint)?;
    drop(journal);

    let (output, retried) = rig.run(&["retry", &id])?;
    assert!(
        output.status.success(),
        "expansion paused retry must not be refused: {retried}"
    );
    let resumed = FileJournal::read_checkpoint(&state, project, &id)?.ok_or("checkpoint")?;
    assert!(!resumed.nodes.iter().any(|n| matches!(
        &n.expansion_cause,
        Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
    )));
    assert!(
        resumed.pause.is_none(),
        "graph pause must be cleared on resume"
    );
    Ok(())
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn retry_after_lowered_host_limit_preserves_structural_pause() -> Result {
    let rig = GraphProject::new()?;
    let id = "lowered-limit-retry";
    let path = rig.document(
        &json!({"schema_version":support::GRAPH_DOCUMENT_SCHEMA_VERSION,"graph_id":id,"nodes":[
            {"id":"first","kind":"gate","access":"read_only","condition":{"type":"budget"}},
            {"id":"second","kind":"gate","access":"read_only","condition":{"type":"budget"}}
        ]}),
    )?;
    let output = rig.command().args(["run", "--file"]).arg(path).output()?;
    assert!(output.status.success(), "{output:?}");

    use provenant_core::{CancelToken, Clock, RootInputs, StateRoot, WaitBudget};
    use provenant_fabric_runtime::graph::{FileJournal, GraphJournal, NodeState};

    let checkpoint_path = graph_records(&rig.state())?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == format!("graph-{id}.json").as_str())
        })
        .ok_or("checkpoint")?;
    let project = checkpoint_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("project id")?
        .parse()?;
    let mut inputs = RootInputs::new(rig.temp.path().to_path_buf(), rig.root.clone());
    inputs.state_dir = Some(rig.state());
    let state = StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let mut checkpoint = FileJournal::read_checkpoint(&state, project, id)?.ok_or("checkpoint")?;
    checkpoint.cancelled = true;
    for node in &mut checkpoint.nodes {
        node.state = NodeState::Skipped;
        node.skip_reason = Some("graph cancelled".into());
        node.cancelled_from = Some(NodeState::Pending);
        node.gate = None;
        node.outputs = None;
    }

    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let mut journal = FileJournal::open(
        &state,
        project,
        id,
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.save(&checkpoint)?;
    drop(journal);

    // Lower the host limit in config.toml to 1 node (the graph has 2 nodes).
    std::fs::write(
        rig.state().join("config.toml"),
        "schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 1\n",
    )?;

    // 1. Graph-wide retry with no brief and no extension leaves topology unchanged:
    // must succeed and preserve/save the StructuralLimit pause rather than refuse.
    let (output, retried) = rig.run(&["retry", id])?;
    assert!(
        output.status.success(),
        "graph-wide retry with lowered limit must succeed and pause, not refuse: {retried}"
    );

    // Verify the checkpoint in journal has the StructuralLimit pause.
    let mut checkpoint = FileJournal::read_checkpoint(&state, project, id)?.ok_or("checkpoint")?;
    assert_eq!(
        checkpoint.pause_cause(),
        Some(provenant_records::pause::PauseCause::StructuralLimit),
        "checkpoint must record StructuralLimit pause"
    );

    // Graph-wide retry reset its nodes to pending. Recreate the explicit
    // settled cancellation fixture so the node-scoped retry reaches candidate
    // structural admission instead of refusing on node state.
    checkpoint.cancelled = true;
    for node in &mut checkpoint.nodes {
        node.state = NodeState::Skipped;
        node.skip_reason = Some("graph cancelled".into());
        node.cancelled_from = Some(NodeState::Pending);
        node.gate = None;
        node.outputs = None;
    }
    assert!(
        checkpoint
            .nodes
            .iter()
            .all(|node| { node.state == NodeState::Skipped && node.cancelled_from.is_some() }),
        "changed-retry fixture must be settled before invocation"
    );
    let mut journal = FileJournal::open(
        &state,
        project,
        id,
        WaitBudget::cli(clock.mono_now(), 5)?,
        &cancel,
        &clock,
    )?;
    journal.save(&checkpoint)?;
    drop(journal);

    // 2. A node-scoped retry builds a replacement candidate even without a
    // brief. Do not put a brief on a gate: that would be an invalid candidate
    // for an unrelated reason before structural admission.
    let (output, refused) = rig.run(&["retry", &format!("{id}/first")])?;
    assert!(
        !output.status.success(),
        "changed candidate retry against lowered limit must be refused: {refused}"
    );
    assert!(
        refused["error"]["code"] == "graph_target"
            && refused["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("limits.nodes=1 reached (2)")),
        "changed candidate must be refused by the lowered structural node limit: {refused}"
    );

    Ok(())
}
