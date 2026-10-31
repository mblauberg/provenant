//! Named commands use real lane custody, capture, settlement and graph gates.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::{fs, io::Write, time::Duration};
use support::{Result, TestRig, code};

#[test]
fn macos_ci_requires_command_adapter_confinement_fixtures() {
    let workflow = include_str!("../../../.github/workflows/ci.yml");
    let job = workflow
        .split("  rust-macos-tests:\n")
        .nth(1)
        .expect("macOS test job")
        .split("\n  rust-macos-release:")
        .next()
        .unwrap();
    assert!(job.contains("runs-on: macos-15"));
    assert!(!job.contains("continue-on-error: true"));
    let step = job
        .split("      - name:")
        .find(|step| step.contains("binary(=command_adapter)"))
        .expect("command_adapter must run in the required macOS job");
    assert!(step.contains("PROVENANT_CONFINEMENT_REQUIRED: \"1\""));
    assert!(step.contains("cargo nextest run"));
    assert!(step.contains("--profile ci"));
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
        if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some() {
            return Err(error.into());
        }
        writeln!(std::io::stderr(), "command fixture skipped: {error}")?;
        return Ok(false);
    }
    Ok(true)
}

fn rig(commands: Value) -> Result<TestRig> {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let out = rig.execute(
        &git,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "seed",
        ],
        None,
    )?;
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
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    if "$1" process-tree --root "$$" --json > "$TMPDIR/sample.json"; then
        printf '%s\n' "$PROVENANT_PROCESS_TREE"
        /bin/cat "$TMPDIR/sample.json"
        exit 0
    else
        status=$?
        test "$status" = 4
    fi
    /bin/sleep 0.2
done
exit 90
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
        assert_eq!(sample["schema_version"], 1);
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
        assert!(
            processes
                .iter()
                .all(|process| process["rss_bytes"].as_u64().is_some_and(|rss| rss > 0))
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
    let submitted = dispatch(&rig, "pass", 10)?;
    // With no busy timeout, the polling reads below fail whenever the lane's
    // supervisor holds the store. Only the dispatch needs the fail-fast store.
    fs::write(rig.state.join("config.toml"), original)?;
    let lane = settled(&rig, attempt_id(&submitted)?)?;
    assert_eq!(lane["state"], "succeeded", "{lane}");
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
