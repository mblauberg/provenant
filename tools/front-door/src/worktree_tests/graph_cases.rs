use super::*;

struct PendingGraphCommand {
    child: provenant_supervisor::fixture::ChildGuard,
    stdout: PathBuf,
    stderr: PathBuf,
}

fn spawn_graph_command(
    rig: &TestRig,
    id: &str,
    worktree: &std::path::Path,
    delivery: &str,
) -> Result<PendingGraphCommand> {
    let file = rig.root.join(format!("{id}.json"));
    fs::write(
        &file,
        serde_json::to_vec(&serde_json::json!({
            "graph_id":id, "nodes":[{"id":"writer", "use":"task", "prompt":"reply OK",
                "access":"worktree_write", "worktree":worktree, "delivery":delivery,
                "route":{"adapter":"codex", "model":"codex/gpt-6-luna", "account":"preparation"}}]
        }))?,
    )?;
    let stdout = rig.root.join(format!("{id}.stdout"));
    let stderr = rig.root.join(format!("{id}.stderr"));
    let child = rig.with_process_spawn_lock(|| {
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .arg("--state-dir")
            .arg(&rig.state)
            .args(["run", "--file"])
            .arg(&file)
            .arg("--json")
            .current_dir(&rig.project)
            .env_clear()
            .envs(rig.env.iter().cloned())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(fs::File::create(&stdout)?))
            .stderr(std::process::Stdio::from(fs::File::create(&stderr)?));
        Ok(command.spawn()?)
    })?;
    Ok(PendingGraphCommand {
        child: provenant_supervisor::fixture::ChildGuard::new(child),
        stdout,
        stderr,
    })
}

/// Failure ceiling for a stopped graph to settle. Stop is observed by waiting
/// on the graph's own terminal state, so this bounds a missed cancellation, not
/// the latency of a correct one. It stays below the fixture's held fetch.
const GRAPH_STOP_CEILING: Duration = Duration::from_secs(20);

fn finish_graph_command(mut pending: PendingGraphCommand, phase: &str) -> Result<Value> {
    let exited = provenant_test_support::wait_for(
        "graph submission after stop",
        GRAPH_STOP_CEILING,
        || pending.child.try_wait(),
        |status| matches!(status, Ok(Some(_)) | Err(_)),
    )?;
    let status = exited?.ok_or("graph submission did not exit")?;
    let stdout = fs::read(&pending.stdout)?;
    let stderr = fs::read(&pending.stderr)?;
    assert!(
        status.success(),
        "{phase}: stdout={} stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let mut records = serde_json::Deserializer::from_slice(&stdout)
        .into_iter::<Value>()
        .collect::<serde_json::Result<Vec<_>>>()?;
    records
        .pop()
        .ok_or_else(|| "graph command emitted no JSON result".into())
}

/// A read-only node whose only route is unavailable is refused at preparation,
/// before any provider starts, and still keeps its attempt reference.
///
/// This case previously also pinned a writer's low-disk refusal. That refusal
/// lived in the lane build admission wrapper, which #3790 deleted with lane
/// builds themselves; the surviving disk decision is admission's, pinned by
/// `crates/run/src/dispatch_disk_admission_tests.rs`.
pub(super) fn graph_run_refuses_unavailable_reader_route() -> Result {
    let rig = repository()?;
    let document = rig.root.join("reader-unavailable.json");
    fs::write(
        &document,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2, "graph_id":"reader-unavailable", "nodes":[{
                "id":"reader", "prompt":"hello", "access":"read_only",
                "route":{"adapter":"fixture-unavailable","model":"one"}
            }]
        }))?,
    )?;
    let reader = command(
        &rig,
        &[
            "run",
            "--file",
            document.to_str().ok_or("document")?,
            "--json",
        ],
    )?;
    let reader_value = json(&reader)?;
    assert_eq!(code(&reader), 1, "{reader_value}");
    assert_eq!(reader_value["status"], "failed", "{reader_value}");
    let reader_node = &reader_value["data"]["checkpoint"]["nodes"][0];
    assert_eq!(reader_node["state"], "failed", "{reader_value}");
    assert_eq!(
        reader_node["attempts"][0]["outcome"],
        serde_json::json!({"preparation_refused":"route_resolution_refused"}),
        "{reader_value}"
    );
    assert_eq!(
        reader_node["advance_code"], "preparation_refused",
        "{reader_value}"
    );
    assert_eq!(
        reader_node["attempts"][0]["launched"], false,
        "{reader_value}"
    );
    assert!(
        reader_node["attempts"][0]["attempt_ref"].as_str().is_some(),
        "{reader_value}"
    );
    assert!(rig.state.join("attempts").exists());
    Ok(())
}

fn wait_graph_gate(pending: &mut PendingGraphCommand, marker: &Path, name: &str) -> Result {
    let observed = provenant_test_support::wait_for(
        name,
        Duration::from_secs(20),
        || (marker.exists(), pending.child.try_wait()),
        |(entered, status)| *entered || matches!(status, Ok(Some(_)) | Err(_)),
    );
    let output = || {
        let stdout = fs::read(&pending.stdout).unwrap_or_default();
        let stderr = fs::read(&pending.stderr).unwrap_or_default();
        format!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        )
    };
    match observed {
        Ok((true, _)) => Ok(()),
        Ok((false, status)) => Err(format!(
            "{name}: graph child exited before the fixture gate ({status:?}); {}",
            output(),
        )
        .into()),
        Err(error) => Err(format!(
            "{error}; graph child status: {:?}; {}",
            pending.child.try_wait(),
            output(),
        )
        .into()),
    }
}

pub(super) fn graph_stop_cancels_live_delivery_fetch() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if !writer_backend_available()? {
        return Ok(());
    }
    let phase = "delivery";
    let mut rig = repository()?;
    let _cleanup = rig.process_cleanup();
    let gate = rig.root.join("preparation-gate");
    fs::create_dir(&gate)?;
    fs::write(gate.join("hold"), b"hold")?;
    let git_exe = provenant_os::git::git_executable().ok_or("git")?;
    let remote = rig.root.join("remote.git");
    git(&rig, &["init", "--bare", remote.to_str().ok_or("remote")?])?;
    git(
        &rig,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    git(&rig, &["push", "origin", "HEAD:refs/heads/main"])?;
    ok(
        &rig,
        &[
            "worktree",
            "create",
            "preparation",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let worktree = rig.project.join(".worktrees/preparation");
    let bin = rig.root.join("provider-bin");
    fs::create_dir(&bin)?;
    let codex = bin.join("codex");
    fs::write(
        &codex,
        "#!/bin/sh\ncase \"$1\" in --version) printf 'codex-cli 1.0.0\\n';; login) printf 'Logged in using ChatGPT\\n';; *) printf '{\"type\":\"thread.started\",\"thread_id\":\"fixture-preparation\"}\\n';; esac\n",
    )?;
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700))?;
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\ncase \"$*\" in *token*) printf 'fixture-token\\n';; *) printf 'gh version 2.0.0\\n';; esac\n",
    )?;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let account = rig.root.join("codex");
    let auth = br#"{"tokens":{"account_id":"fixture-preparation"}}"#;
    fs::write(account.join("auth.json"), auth)?;
    let account_key =
        provenant_providers::identity::codex_account(auth).ok_or("Codex fixture key")?;
    support::register_account(&rig, "codex", "preparation", &account_key, &account)?;
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in *' fetch '*) if [ -f '{}/hold' ]; then : > '{}/entered'; exec /bin/sleep 120; fi;; esac\nexec '{}' \"$@\"\n",
            gate.display(),
            gate.display(),
            git_exe.display()
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
    rig.env.retain(|(key, _)| key != "PROVENANT_GIT");
    rig.env
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    let mut first = spawn_graph_command(&rig, "preparation-first", &worktree, "push")?;
    wait_graph_gate(
        &mut first,
        &gate.join("entered"),
        "graph preparation entered",
    )?;
    ok(&rig, &["stop", "preparation-first", "--json"])?;
    ok(
        &rig,
        &["wait", "preparation-first", "--timeout", "20s", "--json"],
    )?;
    let first = finish_graph_command(first, phase)?;
    assert!(
        first["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"].is_string(),
        "{phase}: {first}"
    );
    let stopped = ok(&rig, &["status", "preparation-first", "--json"])?;
    assert_eq!(
        stopped["data"]["rows"][0]["state"], "cancelled",
        "{phase}: {stopped}"
    );
    Ok(())
}
