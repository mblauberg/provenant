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
            "graph_id":id, "nodes":[{"id":"writer", "use":"task", "brief":"reply OK",
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

fn finish_graph_command(mut pending: PendingGraphCommand, phase: &str) -> Result<Value> {
    let exited = provenant_test_support::wait_for(
        "graph submission after stop",
        Duration::from_secs(4),
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

pub(super) fn graph_run_refuses_low_disk_writers() -> Result {
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "low-disk", "--json"])?;
    let (roots, _) = writer_context(&rig)?;
    let settings = provenant_config::read(&roots)?;
    assert_eq!(settings.gc.free_floor_bytes(rig.state.as_path())?, 1);
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='18446744073709551615B'\n[knobs]\n'policy.LAUNCH_DISK'='refuse'\n",
    )?;
    let document = rig.root.join("low-disk.json");
    fs::write(
        &document,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2, "graph_id":"low-disk", "nodes":[{
                "id":"writer", "prompt":"hello", "access":"worktree_write",
                "worktree":rig.project.join(".worktrees/low-disk"),
                "write_scope":["."], "route":{"adapter":"codex","model":"gpt-6-luna"}
            }]
        }))?,
    )?;
    let output = command(
        &rig,
        &[
            "run",
            "--file",
            document.to_str().ok_or("document")?,
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 1, "{value}");
    assert_eq!(value["status"], "failed", "{value}");
    assert_eq!(value["data"]["outcome"], "failed", "{value}");
    let node = &value["data"]["checkpoint"]["nodes"][0];
    assert_eq!(node["state"], "blocked", "{value}");
    assert_eq!(node["advance_error_code"], "launch_refused", "{value}");
    assert!(node["attempts"][0]["attempt_ref"].is_null(), "{value}");
    let message = node["advance_error"].as_str().ok_or("message")?;
    assert!(
        message.contains("available") && message.contains("floor 18446744073709551615"),
        "{message}"
    );
    assert!(message.contains("provenant gc"), "{message}");
    assert!(!rig.state.join("attempts").exists());
    fs::write(
        &document,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2, "graph_id":"low-disk-reader", "nodes":[{
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
        reader_node["advance_error_code"], "preparation_refused",
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

fn wait_graph_gate(
    pending: &mut PendingGraphCommand,
    marker: &Path,
    name: &str,
) -> Result {
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

pub(super) fn graph_stop_cancels_live_delivery_fetch_and_seed_preparation() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if !writer_backend_available()? {
        return Ok(());
    }
    for phase in ["delivery", "seed"] {
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
        if phase == "delivery" {
            let wrapper = bin.join("git");
            fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\ncase \" $* \" in *' fetch '*) if [ -f '{}/hold' ]; then : > '{}/entered'; exec /bin/sleep 30; fi;; esac\nexec '{}' \"$@\"\n",
                    gate.display(),
                    gate.display(),
                    git_exe.display()
                ),
            )?;
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
            rig.env.retain(|(key, _)| key != "PROVENANT_GIT");
            rig.env
                .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
        } else {
            rig.env.push((
                "PROVENANT_FIXTURE_HOLD_SEED".into(),
                gate.clone().into_os_string(),
            ));
        }
        let mut first = spawn_graph_command(
            &rig,
            "preparation-first",
            &worktree,
            if phase == "delivery" { "push" } else { "none" },
        )?;
        wait_graph_gate(
            &mut first,
            &gate.join("entered"),
            "graph preparation entered",
        )?;
        let before = std::time::Instant::now();
        ok(&rig, &["stop", "preparation-first", "--json"])?;
        ok(
            &rig,
            &["wait", "preparation-first", "--timeout", "3s", "--json"],
        )?;
        let first = finish_graph_command(first, phase)?;
        assert!(
            first["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"].is_string(),
            "{phase}: {first}"
        );
        let stopped = ok(&rig, &["status", "preparation-first", "--json"])?;
        assert!(
            before.elapsed() < Duration::from_secs(4),
            "{phase}: {stopped}"
        );
        assert_eq!(
            stopped["data"]["rows"][0]["state"], "cancelled",
            "{phase}: {stopped}"
        );
        fs::remove_file(gate.join("hold"))?;
        fs::remove_file(gate.join("entered"))?;
        let next_gate = rig.root.join("next-seed-gate");
        fs::create_dir(&next_gate)?;
        fs::write(next_gate.join("hold"), b"hold")?;
        rig.env
            .retain(|(key, _)| key != "PROVENANT_FIXTURE_HOLD_SEED");
        rig.env.push((
            "PROVENANT_FIXTURE_HOLD_SEED".into(),
            next_gate.clone().into_os_string(),
        ));
        let mut next = spawn_graph_command(&rig, "preparation-next", &worktree, "none")?;
        wait_graph_gate(
            &mut next,
            &next_gate.join("entered"),
            "next graph acquired the writer and entered seeding",
        )?;
        ok(&rig, &["stop", "preparation-next", "--json"])?;
        ok(
            &rig,
            &["wait", "preparation-next", "--timeout", "3s", "--json"],
        )?;
        let next = finish_graph_command(next, phase)?;
        assert!(
            next["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"].is_string(),
            "{phase}: {next}"
        );
        let stopped = ok(&rig, &["status", "preparation-next", "--json"])?;
        assert_eq!(
            stopped["data"]["rows"][0]["state"], "cancelled",
            "{phase}: {stopped}"
        );
        fs::remove_file(next_gate.join("hold"))?;
    }
    Ok(())
}
