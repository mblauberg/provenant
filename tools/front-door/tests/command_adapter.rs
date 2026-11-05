//! Named commands use real lane custody, capture, settlement and graph gates.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};
use support::{Result, TestRig, code};

#[test]
fn rig_git_commits_use_fixture_identity() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "fixture"], None)?;
    assert_eq!(
        code(&commit),
        0,
        "{}",
        String::from_utf8_lossy(&commit.stderr.bytes)
    );
    let log = rig.execute(&git, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"], None)?;
    assert_eq!(code(&log), 0);
    assert_eq!(
        String::from_utf8(log.stdout.bytes)?.trim(),
        "Fixture <fixture@example.invalid>|Fixture <fixture@example.invalid>"
    );
    Ok(())
}

#[test]
fn macos_ci_requires_writer_continuation_confinement_fixtures() {
    let workflow = include_str!("../../../.github/workflows/ci.yml");
    let job = workflow
        .split("  rust-macos-tests:\n")
        .nth(1)
        .expect("macOS test job")
        .split("\n  rust-macos-release:")
        .next()
        .unwrap();
    let step = job
        .split("      - name:")
        .find(|step| step.contains("Run dispatch confinement integration tests"))
        .expect("dispatch confinement step");
    assert!(!job.contains("continue-on-error: true"));
    assert!(step.contains("PROVENANT_CONFINEMENT_REQUIRED: \"1\""));
    assert!(step.contains("--profile ci"));
    assert!(step.contains(
        "test(=writer_quota_continuation_succeeds_and_lens_diff_base_stays_node_source)"
    ));
    assert!(step.contains("binary(=writer_continuation)"));
    assert!(step.contains("test(=writer_continuation_launches_pinned_to_predecessor_head)"));
    assert!(
        step.contains(
            "test(=unfinished_writer_continuation_launches_in_previously_owned_worktree)"
        )
    );
}

fn available() -> Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    if let Err(error) = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    ) {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "command fixture skipped: {error}"
        ))?;
        return Ok(false);
    }
    Ok(true)
}

fn rig(commands: Value) -> Result<TestRig> {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let out = rig.execute(&git, &["commit", "--allow-empty", "-qm", "seed"], None)?;
    assert_eq!(code(&out), 0);
    for path in [
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    configure_commands(&rig, commands)?;
    Ok(rig)
}

fn configure_commands(rig: &TestRig, commands: Value) -> Result {
    let mut config: toml_edit::DocumentMut =
        fs::read_to_string(rig.state.join("config.toml"))?.parse()?;
    let value = json!({"commands": commands});
    let table = toml_edit::ser::to_document(&value)?;
    config["domains"]["fabric"] = table.as_item().clone();
    fs::write(rig.state.join("config.toml"), config.to_string())?;
    Ok(())
}

#[test]
fn review_preset_launches_read_only_from_detached_head() -> Result {
    let mut rig = rig(
        json!({"review-pass":{"argv":["/bin/sh","-c", "printf '```findings\\n{\"schema_version\":1,\"findings\":[]}\\n```\\nVERDICT: PASS\\n'"]}}),
    )?;
    let _cleanup = rig.process_cleanup();
    let product = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))?;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = product.clone().into_os_string();
        }
    }
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let base = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    let base = String::from_utf8(base.stdout.bytes)?.trim().to_owned();
    assert_eq!(
        code(&rig.execute(&git, &["commit", "--allow-empty", "-qm", "subject"], None)?),
        0
    );
    let head = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    let head = String::from_utf8(head.stdout.bytes)?.trim().to_owned();
    assert_eq!(
        code(&rig.execute(&git, &["checkout", "--detach", &head], None)?),
        0
    );
    let settings = rig.project.join(".provenant");
    fs::create_dir_all(&settings)?;
    fs::write(
        settings.join("roles.toml"),
        "[roles.\"lens.correctness-spec\"]\ntier = [{ adapter = 'command', name = 'review-pass' }]\n[roles.\"plan.review\"]\ntier = [{ adapter = 'command', name = 'review-pass' }]\n[roles.\"review.synthesis\"]\ntier = [{ adapter = 'command', name = 'review-pass' }]\n",
    )?;
    let brief = rig.root.join("review-brief");
    fs::write(&brief, "Review the pinned subject")?;
    let explain = rig.command(&[
        "run",
        "--preset",
        "review",
        "--prompt-file",
        brief.to_str().ok_or("brief")?,
        "--set",
        &format!("parameters.subject.commit='{head}'"),
        "--set",
        &format!("parameters.base='{head}'"),
        "--set",
        "parameters.review_shape='standard'",
        "--set",
        "parameters.lenses=[{name='correctness-spec',tier='',when=''}]",
        "--set",
        "parameters.min_reviews=1",
        "--set",
        "parameters.verifiers=0",
        "--plan",
        "off",
        "--explain",
        "--json",
    ])?;
    assert_eq!(
        code(&explain),
        0,
        "{}",
        String::from_utf8_lossy(&explain.stdout.bytes)
    );
    let plan: Value = serde_json::from_slice(&explain.stdout.bytes)?;
    let nodes = plan["data"]["graph"]["nodes"]
        .as_array()
        .ok_or("plan nodes")?;
    for id in ["subject", "lenses", "synthesis"] {
        let node = nodes.iter().find(|node| node["id"] == id).ok_or(id)?;
        assert!(node.to_string().contains("review-pass"), "{node}");
    }
    if !available()? {
        return Ok(());
    }
    let output = rig.command(&[
        "run",
        "--preset",
        "review",
        "--prompt-file",
        brief.to_str().ok_or("brief")?,
        "--set",
        &format!("parameters.subject.commit='{head}'"),
        "--set",
        &format!("parameters.base='{base}'"),
        "--set",
        "parameters.review_shape='quick'",
        "--plan",
        "off",
        "--wait",
        "30s",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(code(&output), 0, "{value}");
    let nodes = value["data"]["graph"]["checkpoint"]["nodes"]
        .as_array()
        .ok_or("nodes")?;
    let lens = nodes
        .iter()
        .find(|n| {
            n["id"]
                .as_str()
                .is_some_and(|id| id.contains("lens-correctness-spec"))
        })
        .ok_or("lens")?;
    assert_eq!(lens["state"], "succeeded", "{value}");
    assert_eq!(lens["outputs"]["verdict"], "VERDICT: PASS", "{value}");
    let branch = rig.execute(&git, &["symbolic-ref", "-q", "HEAD"], None)?;
    assert_ne!(code(&branch), 0, "review must leave HEAD detached");

    let equal = rig.command(&[
        "run",
        "--preset",
        "review",
        "--prompt-file",
        brief.to_str().ok_or("brief")?,
        "--set",
        &format!("parameters.subject.commit='{head}'"),
        "--set",
        &format!("parameters.base='{head}'"),
        "--set",
        "parameters.review_shape='standard'",
        "--set",
        "parameters.lenses=[{name='correctness-spec',tier='',when=''}]",
        "--set",
        "parameters.min_reviews=1",
        "--set",
        "parameters.verifiers=0",
        "--plan",
        "off",
        "--wait",
        "30s",
        "--json",
    ])?;
    let equal: Value = serde_json::from_slice(&equal.stdout.bytes)?;
    let checkpoint = &equal["data"]["graph"]["checkpoint"];
    let lens = checkpoint["nodes"]
        .as_array()
        .ok_or("equal nodes")?
        .iter()
        .find(|n| {
            n["id"]
                .as_str()
                .is_some_and(|id| id.contains("lens-correctness-spec"))
        })
        .ok_or("equal lens")?;
    assert_eq!(
        lens["outputs"]["verdict"], "VERDICT: INCONCLUSIVE",
        "{equal}"
    );
    let warning = format!("base {head}, head {head}");
    assert!(
        lens["outputs"]["warnings"].to_string().contains(&warning),
        "{equal}"
    );
    let synthesis = checkpoint["nodes"]
        .as_array()
        .ok_or("equal nodes")?
        .iter()
        .find(|n| n["id"] == "synthesis")
        .ok_or("synthesis")?;
    assert_eq!(
        synthesis["outputs"]["verdict"], "VERDICT: INCONCLUSIVE",
        "{equal}"
    );
    assert!(
        synthesis["outputs"]["warnings"]
            .to_string()
            .contains(&warning),
        "{equal}"
    );
    let id = checkpoint["graph_id"].as_str().ok_or("graph id")?;
    let status = rig.command(&["fabric", "graph", "status", id, "--json"])?;
    let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
    assert!(
        status.to_string().contains("VERDICT: INCONCLUSIVE"),
        "{status}"
    );
    assert!(status.to_string().contains(&warning), "{status}");
    Ok(())
}

#[test]
fn local_wait_returns_while_command_preparation_is_held() -> Result {
    let mut rig = rig(json!({"probe":{"argv":["/bin/true"]}}))?;
    let _cleanup = rig.process_cleanup();
    let gate = rig.root.join("prepare-gate");
    fs::create_dir(&gate)?;
    fs::write(gate.join("hold"), b"hold")?;
    rig.env.push((
        "PROVENANT_FIXTURE_HOLD_COMMAND_PREPARATION".into(),
        gate.clone().into_os_string(),
    ));
    let prompt = rig.root.join("prompt");
    fs::write(&prompt, "fixture")?;
    let start = std::time::Instant::now();
    let output = rig.command(&[
        "run",
        "--mode",
        "read-only",
        "--route",
        "command/probe",
        "--operation-id",
        "held-command",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "1s",
        "--json",
    ])?;
    let elapsed = start.elapsed();
    let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let entered = provenant_test_support::wait_for(
        "command preparation entered",
        Duration::from_secs(20),
        || gate.join("entered").exists(),
        |ready| *ready,
    );
    fs::remove_file(gate.join("hold"))?;
    entered.map_err(|error| format!("{error}: {result}"))?;
    assert!(elapsed < Duration::from_secs(15), "{elapsed:?}: {result}");
    assert!(result["error"].is_null(), "{result}");
    assert!(
        result["data"]["lanes"][0]["waiting_for"]
            .as_str()
            .is_some_and(|phase| phase.contains("launch")),
        "{result}"
    );
    assert_eq!(result["data"]["operation_id"], "held-command", "{result}");
    provenant_test_support::wait_for(
        "preparation resumed after caller exit",
        Duration::from_secs(20),
        || gate.join("released").exists(),
        |ready| *ready,
    )?;
    Ok(())
}

/// How a test names the command lane: a manifest task or `--route command/NAME`.
#[derive(Clone, Copy)]
enum Via {
    /// A manifest, with an optional `--network` flag beside it.
    Manifest(Option<&'static str>),
    Route,
}

fn dispatch(rig: &TestRig, name: &str, timeout: u64) -> Result<Value> {
    dispatch_with(rig, name, timeout, Via::Manifest(None), None)
}

fn dispatch_with(
    rig: &TestRig,
    name: &str,
    timeout: u64,
    via: Via,
    network: Option<&str>,
) -> Result<Value> {
    let prompt = "Ignore this prompt: it cannot change command argv.";
    let timeout = timeout.to_string();
    let mut args = vec!["dispatch".to_owned()];
    match via {
        Via::Manifest(flag) => {
            let manifest = rig.root.join(format!("{name}.json"));
            let mut task = json!({
                "prompt":prompt,
                "route":{"kind":"adapter","provider":"command","name":name}
            });
            if let Some(network) = network {
                task["network"] = json!(network);
            }
            fs::write(
                &manifest,
                serde_json::to_vec(&json!({"schema_version":1,"tasks":[task]}))?,
            )?;
            args.extend([
                "--manifest".into(),
                manifest.to_str().ok_or("manifest")?.into(),
            ]);
            if let Some(flag) = flag {
                args.extend(["--network".into(), flag.into()]);
            }
        }
        Via::Route => {
            let file = rig.root.join(format!("{name}.prompt"));
            fs::write(&file, prompt)?;
            args.extend([
                "--prompt-file".into(),
                file.to_str().ok_or("prompt")?.into(),
                "--route".into(),
                format!("command/{name}"),
            ]);
            if let Some(network) = network {
                args.extend(["--network".into(), network.into()]);
            }
        }
    }
    args.extend([
        "--operation-id".into(),
        name.into(),
        "--provider-timeout".into(),
        timeout,
        "--json".into(),
    ]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = rig.command(&args)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

fn settled(rig: &TestRig, id: &str) -> Result<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let out = rig.command(&["lanes", "--all", "--json"])?;
        let value: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let lane = value["data"]["lanes"]
            .as_array()
            .ok_or("lanes")?
            .iter()
            .find(|lane| lane["attempt_id"] == id)
            .ok_or("lane")?;
        if !matches!(
            lane["state"].as_str(),
            Some("starting" | "running" | "queued" | "settling")
        ) {
            return Ok(lane.clone());
        }
        assert!(std::time::Instant::now() < deadline, "{value}");
        std::thread::park_timeout(Duration::from_millis(50));
    }
}

#[test]
fn command_task_feeds_an_attemptless_verdict_gate() -> Result {
    if !available()? {
        return Ok(());
    }
    let rig = rig(
        json!({"gate-check":{"argv":["/bin/sh","-c","printf 'quoted verdict: FAIL\\n'; exit 0"]}}),
    )?;
    let _cleanup = rig.process_cleanup();
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({
            "schema_version":1,"graph_id":"command-gate","nodes":[
                {"id":"check","kind":"task","access":"read_only","prompt":"Run the configured check.","route":{"adapter":"command","name":"gate-check"}},
                {"id":"accept","kind":"gate","access":"read_only","depends_on":["check"],
                    "inputs":[{"node":"check","outputs":["verdict"]}],
                    "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]
        }))?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let status = rig.command(&["fabric", "graph", "status", "command-gate", "--json"])?;
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if status["data"]["status"] == "complete" {
            let nodes = status["data"]["checkpoint"]["nodes"]
                .as_array()
                .ok_or("nodes")?;
            assert_eq!(nodes[0]["state"], "succeeded", "{status}");
            assert_eq!(
                nodes[0]["attempts"]
                    .as_array()
                    .ok_or("check attempts")?
                    .len(),
                1
            );
            assert_eq!(nodes[1]["state"], "succeeded", "{status}");
            assert_eq!(
                nodes[1]["attempts"]
                    .as_array()
                    .ok_or("gate attempts")?
                    .len(),
                0
            );
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{status}");
        std::thread::park_timeout(Duration::from_millis(50));
    }
    Ok(())
}

fn attempt_id(dispatched: &Value) -> Result<&str> {
    dispatched["data"]["attempts"][0]["attempt_id"]
        .as_str()
        .or_else(|| dispatched["data"]["lanes"][0]["attempt_id"].as_str())
        .ok_or_else(|| format!("attempt id: {dispatched}").into())
}

#[test]
fn process_tree_cli_is_unavailable_without_a_supervisor_sample() -> Result {
    let rig = TestRig::new()?;
    let out = rig.command(&["process-tree", "--root", "1", "--json"])?;
    assert_eq!(code(&out), 4);
    let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(result["status"], "unavailable");
    assert_eq!(result["error"]["code"], "PROCESS_TREE_UNAVAILABLE");
    assert!(result["data"].is_null());
    for root in ["0", "-1", "2147483648"] {
        assert_eq!(
            code(&rig.command(&["process-tree", "--root", root, "--json"])?),
            2
        );
    }
    Ok(())
}

#[test]
fn command_lane_reads_its_supervisors_process_tree_through_the_cli() -> Result {
    if !available()? {
        return Ok(());
    }
    let binary = env!("CARGO_BIN_EXE_provenant");
    let script = r#"
set -eu
test -n "$PROVENANT_PROCESS_TREE"
/bin/sh -c '/bin/sleep 15 & wait' &
child=$!
# Ready only when a published sample shows the child's own child (the sleep);
# the sampler publishes on its own cadence, so poll that condition, not a delay.
# Only "sample unavailable" (exit 4) is retried; any other CLI failure fails now.
attempt=0
while :; do
    status=0
    "$1" process-tree --root "$$" --json > "$TMPDIR/sample.json" || status=$?
    if test "$status" = 0; then
        /usr/bin/grep -Eq "\"ppid\":$child[,}]" "$TMPDIR/sample.json" && break
    else
        test "$status" = 4
    fi
    attempt=$((attempt + 1))
    test "$attempt" -lt 50
    /bin/sleep 0.2
done
printf '%s\n' "$PROVENANT_PROCESS_TREE"
/bin/cat "$TMPDIR/sample.json"
"#;
    let command = json!({"argv":["/bin/sh","-c",script,"memory",binary], "runtimes":[binary]});
    let mut rig = rig(json!({"memory":command.clone(), "memory-foreign":command}))?;
    let _cleanup = rig.process_cleanup();
    for (name, foreign) in [("memory", false), ("memory-foreign", true)] {
        rig.env
            .retain(|(key, _)| key != provenant_supervisor::process_tree::ENV);
        if foreign {
            rig.env.push((
                provenant_supervisor::process_tree::ENV.into(),
                "/foreign/process-tree.json".into(),
            ));
        }
        let submitted = dispatch(&rig, name, 15)?;
        let id = attempt_id(&submitted)?;
        let lane = settled(&rig, id)?;
        assert_eq!(lane["state"], "succeeded", "{lane}");
        let run = submitted["data"]["run_id"].as_str().ok_or("run id")?;
        let output = fs::read_to_string(
            rig.state
                .join("runs")
                .join(run)
                .join("attempts")
                .join(id)
                .join("stdout.log"),
        )?;
        let (snapshot_path, json) = output.split_once('\n').ok_or("snapshot path output")?;
        assert_eq!(
            std::path::Path::new(snapshot_path),
            rig.state
                .join("runs")
                .join(run)
                .join("attempts")
                .join(id)
                .join(provenant_supervisor::process_tree::FILE)
        );
        let value: Value = serde_json::from_str(json.trim())?;
        assert_eq!(value["status"], "ok", "{value}");
        let sample = &value["data"];
        assert_eq!(sample["schema_version"], 2);
        for field in ["swap_used_bytes", "swap_total_bytes"] {
            assert!(sample.get(field).is_some(), "missing {field}: {sample}");
            assert!(sample[field].is_null() || sample[field].as_u64().is_some());
        }
        let root = sample["root_pid"].as_u64().ok_or("root pid")?;
        let processes = sample["processes"].as_array().ok_or("processes")?;
        let child = processes
            .iter()
            .find(|process| process["ppid"] == root)
            .ok_or("child")?;
        assert!(
            processes
                .iter()
                .any(|process| process["ppid"] == child["pid"]),
            "{value}"
        );
        // A short-lived helper (grep, sleep) can be sampled as an unreaped
        // zombie with no resident pages, so only the live lane shell must
        // report memory; every process must still carry a byte count.
        assert!(
            processes
                .iter()
                .all(|process| process["rss_bytes"].as_u64().is_some()),
            "{value}"
        );
        assert!(
            child["rss_bytes"].as_u64().is_some_and(|rss| rss > 0),
            "{value}"
        );
    }
    Ok(())
}

#[test]
fn command_pass_fail_truncation_and_spawn_failure_settle_without_models() -> Result {
    if !available()? {
        return Ok(());
    }
    let rig = rig(json!({
        "pass":{"argv":["/bin/sh","-c","printf 'hello\\n'; printf 'diagnostic\\n' >&2; test ! -w .git/config; ! touch forbidden;"]},
        "fail":{"argv":["/bin/sh","-c","printf 'failed\\n'; exit 7"]},
        "tail":{"argv":["/bin/sh","-c","/usr/bin/yes x | /usr/bin/head -c 2000000; printf '\\nlast-output\\n'"]},
        "missing":{"argv":["/missing/command"]}
    }))?;
    let _cleanup = rig.process_cleanup();
    for (name, expected, verdict) in [
        ("pass", "succeeded", "PASS"),
        ("fail", "failed", "FAIL"),
        ("tail", "succeeded", "PASS"),
        ("missing", "rejected", ""),
    ] {
        let submitted = dispatch(&rig, name, 10)?;
        let id = attempt_id(&submitted)?;
        let lane = settled(&rig, id)?;
        assert_eq!(lane["state"], expected, "{lane}");
        if name == "missing" {
            assert!(lane.to_string().contains("/missing/command"), "{lane}");
        }
        assert!(lane["applied_route"]["model"].is_null(), "{lane}");
        if !verdict.is_empty() {
            let run = submitted["data"]["run_id"].as_str().ok_or("run id")?;
            let result = fs::read_to_string(
                rig.state
                    .join("runs")
                    .join(run)
                    .join("attempts")
                    .join(id)
                    .join("result.md"),
            )?;
            assert!(result.contains(&format!("VERDICT: {verdict}")), "{result}");
            if name == "tail" {
                assert!(result.contains("last-output"), "{result}");
                assert!(result.contains("retained tail"), "{result}");
            }
        }
    }
    assert!(!rig.project.join("forbidden").exists());
    Ok(())
}

#[test]
fn command_dispatch_launches_when_the_store_fails_fast() -> Result {
    if !available()? {
        return Ok(());
    }
    let rig = rig(json!({"pass":{"argv":["/bin/sh","-c","printf 'hello\\n'"]}}))?;
    let _cleanup = rig.process_cleanup();
    let original = fs::read_to_string(rig.state.join("config.toml"))?;
    let mut config: toml_edit::DocumentMut = original.parse()?;
    config["domains"]["fabric"]["limits"]["store_busy_timeout_ms"] = toml_edit::value(0);
    fs::write(rig.state.join("config.toml"), config.to_string())?;
    let loaded = rig.command(&["config", "show", "--json"])?;
    assert_eq!(code(&loaded), 0, "{loaded:?}");
    let loaded: Value = serde_json::from_slice(&loaded.stdout.bytes)?;
    assert_eq!(
        loaded["data"]["domains"]["fabric"]["limits"]["store_busy_timeout_ms"], 0,
        "{loaded}"
    );
    let submitted = dispatch(&rig, "pass", 10)?;
    // With no busy timeout, the polling reads below fail whenever the lane's
    // supervisor holds the store. Only the dispatch needs the fail-fast store.
    fs::write(rig.state.join("config.toml"), original)?;
    let lane = settled(&rig, attempt_id(&submitted)?)?;
    assert_eq!(
        lane["state"], "succeeded",
        "settlement reason: {}; lane: {lane}",
        lane["settlement"]["reason"]
    );
    Ok(())
}

#[test]
fn command_custom_shebang_runtime_keeps_credentials_and_writes_denied() -> Result {
    use std::os::unix::fs::PermissionsExt;

    if !available()? {
        return Ok(());
    }
    let rig = rig(json!({}))?;
    let _cleanup = rig.process_cleanup();
    let runtime = rig.root.join("custom-runtime/bin/interpreter");
    fs::create_dir_all(runtime.parent().ok_or("runtime parent")?)?;
    // A natively linked shim is signed by the linker; a copied platform
    // binary can be killed by the kernel even after ad hoc re-signing.
    let source = runtime.with_file_name("interpreter.c");
    fs::write(
        &source,
        "#include <unistd.h>\nint main(int argc, char **argv) {\n    (void)argc;\n    argv[0] = \"sh\";\n    execv(\"/bin/sh\", argv);\n    return 127;\n}\n",
    )?;
    let Ok(built) = std::process::Command::new("cc")
        .arg("-o")
        .arg(&runtime)
        .arg(&source)
        .output()
    else {
        return Ok(());
    };
    assert!(built.status.success(), "{built:?}");
    fs::remove_file(&source)?;
    fs::write(runtime.with_file_name("neighbour"), "private runtime data")?;
    let script = rig.project.join("check-script");
    fs::write(
        &script,
        format!(
            r#"#!{}
set -eu
for path in ".config/gh/fixture-secret" "Library/Application Support/GitHub CLI/fixture-secret" "Library/Keychains/fixture-secret" ".config/gcloud/fixture-secret" ".config/cursor/fixture-secret" ".config/opencode/fixture-secret" ".local/share/opencode/fixture-secret" ".aws/fixture-secret" ".ssh/fixture-secret" ".codex/fixture-secret" ".claude/fixture-secret" ".cargo/credentials" ".cargo/credentials.toml"; do
    if /bin/cat "$HOME/$path" >/dev/null 2>&1; then
        printf 'credential readable: %s\n' "$path"
        exit 1
    fi
done
if /bin/cat "{}/neighbour" >/dev/null 2>&1; then exit 2; fi
if /usr/bin/touch "{}/new-file" 2>/dev/null; then exit 3; fi
if /usr/bin/touch forbidden 2>/dev/null; then exit 4; fi
printf 'custom interpreter ran; credentials and writes denied\n'
"#,
            runtime.display(),
            runtime.parent().unwrap().display(),
            runtime.parent().unwrap().display()
        ),
    )?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
    for directory in [
        ".config/gh",
        "Library/Application Support/GitHub CLI",
        "Library/Keychains",
        ".config/gcloud",
        ".config/cursor",
        ".config/opencode",
        ".local/share/opencode",
        ".aws",
        ".ssh",
        ".codex",
        ".claude",
    ] {
        fs::create_dir_all(rig.home.join(directory))?;
        fs::write(
            rig.home.join(directory).join("fixture-secret"),
            "fixture credential",
        )?;
    }
    fs::create_dir_all(rig.home.join(".cargo"))?;
    for file in ["credentials", "credentials.toml"] {
        fs::write(rig.home.join(".cargo").join(file), "fixture credential")?;
    }
    configure_commands(
        &rig,
        json!({
            "no-runtime": {"argv": [script]},
            "granted": {"argv": [script], "runtimes": [runtime]}
        }),
    )?;
    // The same script cannot launch without its non-system interpreter grant.
    let submitted = dispatch(&rig, "no-runtime", 10)?;
    let lane = settled(&rig, attempt_id(&submitted)?)?;
    assert_eq!(lane["state"], "failed", "{lane}");
    let submitted = dispatch(&rig, "granted", 10)?;
    let id = attempt_id(&submitted)?;
    let lane = settled(&rig, id)?;
    assert_eq!(lane["state"], "succeeded", "{lane}");
    // Network is on unless the task turns it off.
    assert_eq!(
        submitted["data"]["lanes"][0]["network"], "open",
        "{submitted}"
    );
    let run = submitted["data"]["run_id"].as_str().ok_or("run id")?;
    let result = fs::read_to_string(
        rig.state
            .join("runs")
            .join(run)
            .join("attempts")
            .join(id)
            .join("result.md"),
    )?;
    assert!(
        result.contains("custom interpreter ran; credentials and writes denied"),
        "{result}"
    );
    assert!(result.contains("VERDICT: PASS"), "{result}");
    assert!(!rig.project.join("forbidden").exists());
    assert!(!runtime.with_file_name("new-file").exists());
    Ok(())
}

#[test]
fn command_timeout_cleans_descendants_and_running_replay_keeps_one_attempt() -> Result {
    if !available()? {
        return Ok(());
    }
    let rig = rig(
        json!({"slow":{"argv":["/bin/sh","-c","/usr/bin/perl -MPOSIX -e 'POSIX::setsid(); sleep 30' & child=$!; printf 'child=%s\\n' \"$child\"; wait"]}}),
    )?;
    let _cleanup = rig.process_cleanup();
    let first = dispatch(&rig, "slow", 2)?;
    let second = dispatch(&rig, "slow", 2)?;
    let id = attempt_id(&first)?;
    assert_eq!(id, attempt_id(&second)?);
    let lane = settled(&rig, id)?;
    assert_eq!(lane["state"], "timed_out", "{lane}");
    let out = rig.command(&["output", id, "--part", "result", "--json"])?;
    let result = String::from_utf8_lossy(&out.stdout.bytes);
    assert!(result.contains("VERDICT: FAIL"), "{result}");
    // Custody publishes success only after the recorded descendant census settles.
    let run = first["data"]["run_id"].as_str().ok_or("run id")?;
    let exit: Value = serde_json::from_slice(&fs::read(
        rig.state
            .join("runs")
            .join(run)
            .join("attempts")
            .join(id)
            .join("process-exit.json"),
    )?)?;
    assert_eq!(
        exit["settlement_evidence"], "darwin_census_and_custody_lease",
        "{exit}"
    );
    assert_eq!(exit["unresolved_cleanup"], false, "{exit}");
    let result = fs::read_to_string(
        rig.state
            .join("runs")
            .join(run)
            .join("attempts")
            .join(id)
            .join("result.md"),
    )?;
    let child: u32 = result
        .lines()
        .find_map(|line| line.strip_prefix("| child="))
        .ok_or("detached child pid")?
        .parse()?;
    let alive = rig.execute(
        std::path::Path::new("/bin/kill"),
        &["-0", &child.to_string()],
        None,
    )?;
    assert_ne!(
        code(&alive),
        0,
        "detached child {child} survived settlement"
    );
    Ok(())
}

#[test]
fn command_network_is_per_task_through_manifest_and_single_task_route() -> Result {
    if !available()? {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port().to_string();
    let reach = json!({"argv":["/usr/bin/nc","-z","-w","2","127.0.0.1",port]});
    let rig = rig(json!({
        "mf-off": reach, "mf-on": reach, "m-default": reach, "m-on": reach, "m-off": reach,
        "r-default": reach, "r-on": reach, "r-off": reach
    }))?;
    let _cleanup = rig.process_cleanup();
    for (name, via, network, state, reported) in [
        ("m-default", Via::Manifest(None), None, "succeeded", "open"),
        ("m-on", Via::Manifest(None), Some("on"), "succeeded", "open"),
        (
            "m-off",
            Via::Manifest(None),
            Some("off"),
            "failed",
            "provider",
        ),
        (
            "mf-off",
            Via::Manifest(Some("off")),
            None,
            "failed",
            "provider",
        ),
        (
            "mf-on",
            Via::Manifest(Some("off")),
            Some("on"),
            "succeeded",
            "open",
        ),
        ("r-default", Via::Route, None, "succeeded", "open"),
        ("r-on", Via::Route, Some("on"), "succeeded", "open"),
        ("r-off", Via::Route, Some("off"), "failed", "provider"),
    ] {
        let submitted = dispatch_with(&rig, name, 10, via, network)?;
        let lane = settled(&rig, attempt_id(&submitted)?)?;
        assert_eq!(lane["state"], state, "{name}: {lane}");
        assert_eq!(
            submitted["data"]["lanes"][0]["network"], reported,
            "{name}: {submitted}"
        );
    }
    Ok(())
}

#[test]
fn explain_for_model_and_route_previews_without_creating_attempts() -> Result {
    for (selector, expected) in [("--model", "codex/gpt-6-luna"), ("--route", "junior")] {
        let rig = TestRig::new()?;
        let prompt = rig.root.join("prompt.md");
        std::fs::write(&prompt, "Preview this task")?;
        let output = rig.execute(
            Path::new(env!("CARGO_BIN_EXE_provenant")),
            &[
                "run",
                selector,
                expected,
                "--prompt-file",
                prompt.to_str().ok_or("prompt path")?,
                "--mode",
                "read-only",
                "--deliver",
                "none",
                "--explain",
            ],
            None,
        )?;

        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let text = String::from_utf8(output.stdout.bytes)?;
        assert!(text.contains(expected), "{text}");
        assert!(
            !rig.state.join("runs").exists(),
            "explain created run state for {selector}: {text}"
        );
    }
    // Gemini Flash runs writer lanes by default with no host override (#2646).
    let rig = TestRig::new()?;
    let prompt = rig.root.join("prompt-flash.md");
    std::fs::write(&prompt, "Implement feature")?;
    let output = rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[
            "run",
            "--model",
            "agy/gemini-3.8-flash",
            "--branch",
            "feature-test",
            "--base",
            "origin/main",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--explain",
        ],
        None,
    )?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let text = String::from_utf8(output.stdout.bytes)?;
    assert!(text.contains("agy/gemini-3.8-flash"), "{text}");
    assert!(
        !rig.state.join("runs").exists(),
        "explain created run state: {text}"
    );
    Ok(())
}

#[test]
fn disk_report_json_and_human_output_leave_missing_state_absent() -> support::Result {
    let rig = TestRig::new()?.without_state()?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "initial"], None)?;
    assert_eq!(code(&commit), 0);
    let before = std::fs::read(rig.project.join(".git/index")).ok();
    let json = rig.command(&["disk", "report", "--json"])?;
    assert_eq!(
        code(&json),
        0,
        "{}",
        String::from_utf8_lossy(&json.stderr.bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&json.stdout.bytes)?;
    assert_eq!(value["command"], "disk report");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["data"]["schema_version"], 1);
    assert_eq!(value["data"]["errors"], serde_json::json!([]));
    assert_eq!(value["data"]["totals"]["worktree"]["count"], 1);
    assert!(!rig.state.exists());
    let human = rig.command(&["disk", "report"])?;
    assert_eq!(code(&human), 0);
    assert!(String::from_utf8(human.stdout.bytes)?.contains("class=worktree"));
    assert!(!rig.state.exists());
    assert_eq!(std::fs::read(rig.project.join(".git/index")).ok(), before);
    rig.assert_legacy_unchanged()?;
    Ok(())
}

#[test]
fn preset_reader_uses_writer_checkout_from_another_checkout() -> Result {
    preset_reader_checkout_case(false)
}

#[test]
fn preset_reader_honours_explicit_source_from_another_checkout() -> Result {
    preset_reader_checkout_case(true)
}

fn preset_reader_checkout_case(explicit: bool) -> Result {
    use provenant_core::Clock;
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    struct CompletedWriter {
        head: String,
    }
    impl AttemptOwner for CompletedWriter {
        type Error = provenant_os::fs::FsError;
        fn actor(&self) -> String {
            provenant_core::ActorId::from_bytes([34; 16]).to_string()
        }
        fn bind_writer_worktree(
            &mut self,
            path: &str,
        ) -> std::result::Result<Option<graph::WriterWorktreeBinding>, Self::Error> {
            let metadata = provenant_os::fs::Directory::open_anchor(Path::new(path))?.metadata()?;
            Ok(Some(graph::WriterWorktreeBinding {
                extra: [(
                    "source_directory_identity".into(),
                    json!({"device":metadata.device,"inode":metadata.inode}),
                )]
                .into(),
                worktree: path.into(),
                head: self.head.clone(),
            }))
        }
        fn observe_or_start(
            &mut self,
            key: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
            if key.node_id == "writer" {
                Ok(graph::AttemptObservation::Terminal {
                    attempt_ref: None,
                    outcome: graph::AttemptOutcome::Succeeded,
                    launched: true,
                    result: None,
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: Some(self.head.clone()),
                })
            } else {
                Ok(graph::AttemptObservation::Pending { attempt_ref: None })
            }
        }
    }

    let mut rig = TestRig::new()?;
    configure_commands(
        &rig,
        json!({"source-check":{"argv":["/bin/sh","-c","cat subject; printf '\\nVERDICT: PASS\\n'"]}}),
    )?;
    let _cleanup = rig.process_cleanup();
    let product = std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))?;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = product.clone().into_os_string();
        }
    }
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    fs::write(rig.project.join("subject"), "writer checkout")?;
    for args in [
        vec!["add", "subject"],
        vec!["commit", "-qm", "writer source"],
    ] {
        let out = rig.execute(&git, &args, None)?;
        assert_eq!(
            code(&out),
            0,
            "{}",
            String::from_utf8_lossy(&out.stderr.bytes)
        );
    }
    let out = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    let head = String::from_utf8(out.stdout.bytes)?.trim().to_owned();
    let writer = rig.project.join(".worktrees/writer");
    fs::create_dir_all(writer.parent().ok_or("worktree parent")?)?;
    let out = rig.execute(
        &git,
        &[
            "worktree",
            "add",
            "-qb",
            "writer",
            writer.to_str().ok_or("writer path")?,
            &head,
        ],
        None,
    )?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stderr.bytes)
    );
    fs::write(rig.project.join("subject"), "invoking checkout")?;
    for args in [
        vec!["add", "subject"],
        vec!["commit", "-qm", "invoking source"],
    ] {
        let out = rig.execute(&git, &args, None)?;
        assert_eq!(
            code(&out),
            0,
            "{}",
            String::from_utf8_lossy(&out.stderr.bytes)
        );
    }
    let presets = rig.project.join(".provenant/presets");
    fs::create_dir_all(&presets)?;
    fs::write(
        presets.join("bound-reader.toml"),
        r#"
stages = [
  { id = "writer", role = "write" },
  { id = "reader", role = "read", source = { type = "node", node = "writer" } },
]
[parameters]
worktree = { type = "string", default = "" }
subject = { type = "table", default = { type = "commit", commit = "HEAD" } }
[roles.write]
access = "worktree_write"
worktree = "${worktree}"
source = "${subject}"
tier = { adapter = "fixture", model = "fixture/model" }
[roles.read]
access = "read_only"
worktree = "${worktree}"
routes = [{ adapter = "command", name = "source-check" }]
"#,
    )?;
    let prompt = rig.project.join("brief");
    fs::write(&prompt, "Inspect the writer source")?;
    let worktree_setting = format!(
        "parameters.worktree={}",
        serde_json::to_string(writer.to_str().ok_or("writer path")?)?
    );
    let explicit_setting = "parameters.subject.commit=\"HEAD\"";
    let mut args = vec![
        "run",
        "--preset",
        "bound-reader",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
        "--set",
        &worktree_setting,
        "--plan",
        "off",
    ];
    if explicit {
        args.extend(["--set", explicit_setting]);
    }
    let mut explain_args = args.to_vec();
    explain_args.extend(["--explain", "--json"]);
    let explain = rig.command(&explain_args)?;
    assert_eq!(
        code(&explain),
        0,
        "{}",
        String::from_utf8_lossy(&explain.stdout.bytes)
    );
    let explanation: Value = serde_json::from_slice(&explain.stdout.bytes)?;
    let document: graph::GraphDocument =
        serde_json::from_value(explanation["data"]["graph"].clone())?;
    let invoking_head = {
        let out = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
        String::from_utf8(out.stdout.bytes)?.trim().to_owned()
    };
    let expected_head = if explicit { &invoking_head } else { &head };
    assert_eq!(
        explanation["data"]["graph"]["nodes"][0]["source"]["commit"],
        *expected_head
    );
    assert_eq!(
        explanation["data"]["graph"]["nodes"][0]["worktree"],
        writer.to_str().ok_or("writer path")?
    );
    let project: provenant_core::ProjectId = {
        let identity = rig.command(&["fabric", "identity", "--json"])?;
        assert_eq!(code(&identity), 0);
        serde_json::from_slice::<Value>(&identity.stdout.bytes)?["data"]["project"]
            .as_str()
            .ok_or("project id")?
            .parse()?
    };
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let state = provenant_core::StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 20)?;
    {
        let mut journal =
            graph::FileJournal::open(&state, project, &document.graph_id, budget, &cancel, &clock)?;
        journal.bind(graph::GraphExecution {
            extra: Default::default(),
            directory: rig.project.clone(),
            document: document.clone(),
            review_base: None,
            integration_base: None,
        })?;
        let mut owner = CompletedWriter {
            head: expected_head.clone(),
        };
        let config = provenant_fabric_runtime::config::GraphConfig::default();
        let mut result = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &config.limits,
            &config.budget,
        )?;
        if result
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "reader")
            .is_none_or(|node| node.attempts.is_empty())
        {
            result = graph::advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &config.limits,
                &config.budget,
            )?;
        }
        assert!(
            result
                .checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "reader")
                .is_some_and(|node| !node.attempts.is_empty())
        );
    }
    if !available()? {
        return Ok(());
    }
    let mut run_args = args.to_vec();
    run_args.extend(["--wait", "20s", "--json"]);
    let launched = rig.command(&run_args)?;
    assert_eq!(
        code(&launched),
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&launched.stdout.bytes),
        String::from_utf8_lossy(&launched.stderr.bytes)
    );
    let status = rig.command(&[
        "fabric",
        "graph",
        "status",
        &document.graph_id,
        "--full",
        "--json",
    ])?;
    let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
    let reader = status["data"]["checkpoint"]["nodes"]
        .as_array()
        .ok_or("nodes")?
        .iter()
        .find(|node| node["id"] == "reader")
        .ok_or("reader")?;
    assert_eq!(reader["state"], "succeeded", "{status}");
    let excerpt = reader["outputs"]["result"]["excerpt"]
        .as_str()
        .ok_or("result excerpt")?;
    let (expected, other) = if explicit {
        ("invoking checkout", "writer checkout")
    } else {
        ("writer checkout", "invoking checkout")
    };
    assert!(excerpt.contains(expected), "{excerpt}");
    assert!(!excerpt.contains(other), "{excerpt}");
    Ok(())
}
