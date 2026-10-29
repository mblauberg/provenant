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

fn dispatch(rig: &TestRig, name: &str, timeout: u64) -> Result<Value> {
    let manifest = rig.root.join(format!("{name}.json"));
    let task = json!({
        "prompt":"Ignore this prompt: it cannot change command argv.",
        "route":{"kind":"adapter","provider":"command","name":name}
    });
    fs::write(
        &manifest,
        serde_json::to_vec(&json!({"schema_version":1,"tasks":[task]}))?,
    )?;
    let timeout = timeout.to_string();
    // Adapter routes exist only in manifests, which refuse --network: command
    // lanes always run at the adapter's provider-only default.
    let output = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest")?,
        "--operation-id",
        name,
        "--provider-timeout",
        &timeout,
        "--json",
    ])?;
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
    let cleanup = rig.process_cleanup();
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
    cleanup.disarm();
    Ok(())
}

fn attempt_id(dispatched: &Value) -> Result<&str> {
    dispatched["data"]["attempts"][0]["attempt_id"]
        .as_str()
        .or_else(|| dispatched["data"]["lanes"][0]["attempt_id"].as_str())
        .ok_or_else(|| format!("attempt id: {dispatched}").into())
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
    let cleanup = rig.process_cleanup();
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
    cleanup.disarm();
    Ok(())
}

#[test]
fn command_custom_shebang_runtime_keeps_credentials_and_writes_denied() -> Result {
    use std::os::unix::fs::PermissionsExt;

    if !available()? {
        return Ok(());
    }
    let rig = rig(json!({}))?;
    let cleanup = rig.process_cleanup();
    let runtime = rig.root.join("custom-runtime/bin/interpreter");
    fs::create_dir_all(runtime.parent().ok_or("runtime parent")?)?;
    rig.copy_executable(std::path::Path::new("/bin/sh"), &runtime)?;
    // A copied platform binary is killed by the kernel until it is re-signed.
    if std::path::Path::new("/usr/bin/codesign").exists() {
        let signed = std::process::Command::new("/usr/bin/codesign")
            .args(["-s", "-", "--force"])
            .arg(&runtime)
            .output()?;
        assert!(signed.status.success(), "{signed:?}");
    }
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
    // Command lanes never get open network.
    assert_ne!(lane["network"], "open", "{lane}");
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
    cleanup.disarm();
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
    let cleanup = rig.process_cleanup();
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
    cleanup.disarm();
    Ok(())
}
