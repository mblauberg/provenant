//! Named commands use real lane custody, capture, settlement and graph gates.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::{fs, time::Duration};
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

/// Submit a single command node through the graph CLI.
fn dispatch(rig: &TestRig, name: &str, timeout: u64) -> Result<Value> {
    dispatch_with(rig, name, timeout, None, true)
}

fn dispatch_with(
    rig: &TestRig,
    name: &str,
    timeout: u64,
    network: Option<&str>,
    wait: bool,
) -> Result<Value> {
    let document = rig.root.join(format!("{name}.json"));
    let mut graph = json!({"schema_version":2,"graph_id":name,"nodes":[{
        "id":"command", "use":"task", "brief":"Ignore this prompt: it cannot change command argv.",
        "access":"read_only", "route":{"adapter":"command","name":name},
        "attempt":{"provider_timeout":timeout}
    }]});
    if let Some(network) = network {
        graph["network"] = json!(network);
    }
    fs::write(&document, serde_json::to_vec(&graph)?)?;
    let mut args = vec![
        "run",
        "--file",
        document.to_str().ok_or("document")?,
        "--operation-id",
        name,
        "--json",
    ];
    if wait {
        args.push("--wait=30s");
    }
    let out = rig.command(&args)?;
    let value: Value = serde_json::from_slice(&out.stdout.bytes)?;
    let expected = if wait && matches!(name, "fail" | "missing" | "m-off") {
        1
    } else {
        0
    };
    assert_eq!(code(&out), expected, "{value}");
    Ok(value)
}

fn attempt_id(submitted: &Value) -> Result<&str> {
    submitted["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
        .as_str()
        .ok_or_else(|| format!("attempt reference: {submitted}").into())
}

fn attempt_dir(rig: &TestRig, reference: &str) -> Result<std::path::PathBuf> {
    let (_, attempt) = reference.rsplit_once('/').ok_or("attempt reference")?;
    fs::read_dir(rig.state.join("runs"))?
        .flatten()
        .map(|run| run.path().join("attempts").join(attempt))
        .find(|path| path.is_dir())
        .ok_or_else(|| format!("attempt directory: {reference}").into())
}

fn settled(rig: &TestRig, reference: &str) -> Result<Value> {
    let (_, attempt) = reference.rsplit_once('/').ok_or("attempt reference")?;
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let identity =
        provenant_cli::fabric::Context::open(roots.clone(), &rig.home, &rig.project)?.identity;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state,
        project_id: identity.project,
        actor_id: identity.inbox.actor,
        read_prompts: true,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    let record = store.record(attempt.parse()?, &call)?;
    let request = store.request(record.run_id, &call)?;
    let task = request
        .tasks
        .iter()
        .find(|task| task.task_id == record.task_id)
        .ok_or("task")?;
    let mut lane = serde_json::to_value(provenant_cli::run::project_lane(
        &record,
        &[],
        rig.state.as_path(),
        None,
        false,
    ))?;
    lane["network"] = json!(task.access.network(task.writer.is_some()));
    Ok(lane)
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
            "schema_version":2,"graph_id":"command-gate","nodes":[
                {"id":"check","use":"task","access":"read_only","brief":"Run the configured check.","route":{"adapter":"command","name":"gate-check"}},
                {"id":"accept","kind":"gate","access":"read_only","depends_on":["check"],
                 "inputs":[{"node":"check","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]
        }))?,
    )?;
    let out = rig.command(&["run", "--file", "graph.json", "--wait=30s", "--json"])?;
    let status: Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(code(&out), 0, "{status}");
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
    Ok(())
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
        let output = fs::read_to_string(attempt_dir(&rig, id)?.join("stdout.log"))?;
        let (snapshot_path, json) = output.split_once('\n').ok_or("snapshot path output")?;
        assert_eq!(
            std::path::Path::new(snapshot_path),
            attempt_dir(&rig, id)?.join(provenant_supervisor::process_tree::FILE)
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
        // A batch whose every lane is refused exits non-zero, with its receipt.
        let submitted = dispatch(&rig, name, 10)?;
        let id = attempt_id(&submitted)?;
        let lane = settled(&rig, id)?;
        assert_eq!(lane["state"], expected, "{lane}");
        if name == "missing" {
            assert_eq!(lane["terminal_cause"], "command_unavailable", "{lane}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.ends_with("; executable /missing/command")),
                "{lane}"
            );
        }
        assert!(lane["applied_route"]["model"].is_null(), "{lane}");
        if !verdict.is_empty() {
            let result = fs::read_to_string(attempt_dir(&rig, id)?.join("result.md"))?;
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
    let submitted = dispatch_with(&rig, "pass", 10, None, false)?;
    let id = attempt_id(&submitted)?;
    let exit = attempt_dir(&rig, id)?.join("process-exit.json");
    // Admission in the guardian can outlast dispatch. Polling lanes
    // can hold the store during that fail-fast admission, so wait for the
    // guardian's exit evidence without opening the store first.
    fs::write(rig.state.join("config.toml"), original)?;
    provenant_test_support::wait_for(
        "guardian exit evidence",
        Duration::from_secs(20),
        || exit.exists(),
        |ready| *ready,
    )?;
    let evidence = fs::read_to_string(exit)?;
    let waited = rig.command(&["status", "pass", "--wait=30s", "--until", "all", "--json"])?;
    assert_eq!(code(&waited), 0, "{waited:?}");
    let lane = settled(&rig, id)?;
    assert_eq!(
        lane["state"], "succeeded",
        "settlement reason: {}; exit: {evidence}; lane: {lane}",
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
if /usr/bin/touch forbidden 2>/dev/null; then exit 4; fi
printf 'custom interpreter ran; credentials and writes denied\n'
"#,
            runtime.display()
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
            "granted": {"argv": [script], "runtimes": [runtime]}
        }),
    )?;
    let submitted = dispatch(&rig, "granted", 10)?;
    let id = attempt_id(&submitted)?;
    let lane = settled(&rig, id)?;
    assert_eq!(lane["state"], "succeeded", "{lane}");
    // Network is on unless the task turns it off.
    assert_eq!(lane["network"], "open", "{submitted}");
    let result = fs::read_to_string(attempt_dir(&rig, id)?.join("result.md"))?;
    assert!(
        result.contains("custom interpreter ran; credentials and writes denied"),
        "{result}"
    );
    assert!(result.contains("VERDICT: PASS"), "{result}");
    assert!(!rig.project.join("forbidden").exists());
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
    let first = dispatch_with(&rig, "slow", 5, None, false)?;
    let id = attempt_id(&first)?;
    let running = settled(&rig, id)?;
    assert!(
        matches!(running["state"].as_str(), Some("starting" | "running")),
        "{running}"
    );
    let second = dispatch_with(&rig, "slow", 5, None, false)?;
    assert_eq!(id, attempt_id(&second)?);
    let waited = rig.command(&["status", "slow", "--wait=30s", "--until", "all", "--json"])?;
    assert_eq!(code(&waited), 0, "{waited:?}");
    let lane = settled(&rig, id)?;
    assert_eq!(lane["state"], "timed_out", "{lane}");
    let out = rig.command(&["output", id, "--part", "result", "--json"])?;
    let result = String::from_utf8_lossy(&out.stdout.bytes);
    assert!(result.contains("VERDICT: FAIL"), "{result}");
    // Custody publishes success only after the recorded descendant census settles.
    let exit: Value =
        serde_json::from_slice(&fs::read(attempt_dir(&rig, id)?.join("process-exit.json"))?)?;
    assert_eq!(
        exit["settlement_evidence"], "darwin_census_and_custody_lease",
        "{exit}"
    );
    assert_eq!(exit["unresolved_cleanup"], false, "{exit}");
    let result = fs::read_to_string(attempt_dir(&rig, id)?.join("result.md"))?;
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
fn command_network_is_graph_wide() -> Result {
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
    // Graphs currently expose one network setting, not per-task manifest overrides.
    for (name, network, state, reported) in [
        ("m-default", None, "succeeded", "open"),
        ("m-on", Some("on"), "succeeded", "open"),
        ("m-off", Some("off"), "failed", "provider"),
    ] {
        let submitted = dispatch_with(&rig, name, 10, network, true)?;
        let lane = settled(&rig, attempt_id(&submitted)?)?;
        assert_eq!(lane["state"], state, "{name}: {lane}");
        assert_eq!(lane["network"], reported, "{name}: {lane}");
    }
    Ok(())
}

#[test]
fn explain_for_tier_previews_without_creating_attempts() -> Result {
    for tier in ["junior", "senior"] {
        let rig = TestRig::new()?;
        let output = rig.command(&[
            "run",
            "Preview this task",
            "--tier",
            tier,
            "--explain",
            "--json",
        ])?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(
            result["data"]["document"]["nodes"][0]["route"]["alias"], tier,
            "{result}"
        );
        assert!(
            !rig.state.join("runs").exists(),
            "explain created run state: {result}"
        );
        assert!(
            !rig.state.join("graphs").exists(),
            "explain created graph state: {result}"
        );
    }
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
