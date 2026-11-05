//! End-to-end cloud verification with an isolated fake CLI.
mod support;
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt};
use support::{Result, TestRig, code};

fn command(rig: &TestRig, args: &[&str]) -> Result<provenant_os::exec::ExecOutcome> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor};
    let mut argv = vec![
        std::ffi::OsString::from(env!("CARGO_BIN_EXE_provenant")),
        "--state-dir".into(),
        rig.state.clone().into_os_string(),
    ];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    let executor = SystemExecutor::new(1)?;
    let mut options = ExecOptions::new(&argv, &rig.project, std::time::Duration::from_secs(45));
    options.env = Some(&rig.env);
    options.process_group = true;
    let result = executor.execute(options)?;
    if result.timed_out || result.drain_truncated {
        return Err(format!("fixture command {args:?} timed out or had incomplete output").into());
    }
    Ok(result)
}

#[test]
fn cloud_job_is_recovered_and_visible_after_restart() -> Result {
    let mut rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "seed"], None)?;
    assert_eq!(code(&commit), 0, "{commit:?}");
    let bin = rig.root.join("bin");
    fs::create_dir_all(&bin)?;
    let codex = bin.join("codex");
    fs::write(
        &codex,
        r##"#!/usr/bin/python3
import json, os, re, sys
path = os.path.join(os.environ['TMPDIR'], 'fake-cloud.json')
args = sys.argv[1:]
if args[:2] == ['cloud', 'exec']:
    count_path = path + '.count'
    count = int(open(count_path).read()) if os.path.exists(count_path) else 0
    with open(count_path, 'w') as out: out.write(str(count + 1))
    prompt = sys.stdin.read()
    job = re.search(r'job_id "([^"]+)"', prompt).group(1)
    nonce = re.search(r'nonce "([^"]+)"', prompt).group(1)
    commit = re.search(r'test commit ([0-9a-f]+)', prompt).group(1)
    result = dict(job_id=job, nonce=nonce, commit=commit, exit_status=0, tests_run=['fixture'], tests_failed=[], log_location='fixture-log', tracked_files_changed=False, observed_model=None)
    with open(path, 'w') as out: json.dump(result, out)
    print('{"id":"fake-cloud-1"}')
elif args[:2] == ['cloud', 'status']:
    print('{"status":"completed"}' if os.path.exists(path + '.finish') else '{"status":"running"}')
elif args[:2] == ['cloud', 'diff']:
    with open(path) as src: result = json.load(src)
    print('diff --git a/.provenant-cloud-result.json b/.provenant-cloud-result.json')
    print('new file mode 100644\n--- /dev/null\n+++ b/.provenant-cloud-result.json\n@@ -0,0 +1 @@')
    print('+' + json.dumps(result))
else:
    sys.exit(2)
"##,
    )?;
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let config = rig.state.join("config.toml");
    let prior = fs::read_to_string(&config)?;
    fs::write(
        &config,
        format!(
            "{prior}\n[domains.lanes.cloud_targets.verify]\nprovider = 'codex'\nenvironment = 'fake-env'\nmodel = 'fixture-model'\nrepository = 'example/repo'\nbranch = 'main'\ndeadline_seconds = 3600\n"
        ),
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "run the fixture test")?;
    let missing = command(
        &rig,
        &[
            "run",
            "--placement",
            "cloud:verify",
            "--cloud-environment",
            "",
            "--operation-id",
            "missing-env",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--json",
        ],
    )?;
    assert_ne!(code(&missing), 0);
    let missing: Value = serde_json::from_slice(&missing.stdout.bytes)?;
    let diagnostic = missing["error"]["message"]
        .as_str()
        .ok_or("environment diagnostic")?;
    assert!(
        diagnostic.contains("verify")
            && diagnostic.contains("environment")
            && diagnostic.contains("repair")
    );
    assert!(!rig.root.join("fake-cloud.json.count").exists());
    let run = command(
        &rig,
        &[
            "run",
            "--placement",
            "cloud:verify",
            "--operation-id",
            "cloud-fixture",
            "--wait",
            "1s",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--json",
        ],
    )?;
    assert_eq!(code(&run), 0, "{run:?}");
    let run: Value = serde_json::from_slice(&run.stdout.bytes)?;
    let id = run["data"]["lanes"][0]["attempt_id"]
        .as_str()
        .ok_or("attempt id")?
        .to_owned();
    assert_eq!(run["data"]["cloud_jobs"][0]["remote_id"], "fake-cloud-1");
    let replay = command(
        &rig,
        &[
            "run",
            "--placement",
            "cloud:verify",
            "--operation-id",
            "cloud-fixture",
            "--wait",
            "1s",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--json",
        ],
    )?;
    assert_eq!(code(&replay), 0, "{replay:?}");
    assert_eq!(
        fs::read_to_string(rig.root.join("fake-cloud.json.count"))?,
        "1"
    );
    let cancel = command(&rig, &["cancel", &id, "--wait", "1s", "--json"])?;
    assert_eq!(code(&cancel), 0, "{cancel:?}");
    let cancel: Value = serde_json::from_slice(&cancel.stdout.bytes)?;
    assert_eq!(
        cancel["data"]["receipts"][0]["cloud"]["state"]["state"],
        "cancellation_unconfirmed"
    );
    fs::write(rig.root.join("fake-cloud.json.finish"), "done")?;
    let reconcile = command(&rig, &["reconcile", "--id", &id, "--json"])?;
    assert_eq!(code(&reconcile), 0, "{reconcile:?}");
    let lanes = command(&rig, &["lanes", "--id", &id, "--json"])?;
    assert_eq!(code(&lanes), 0, "{lanes:?}");
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    assert_eq!(lanes["data"]["cloud_jobs"][0]["state"]["state"], "finished");
    assert_eq!(lanes["data"]["cloud_jobs"][0]["passed"], true);
    assert_eq!(lanes["data"]["lanes"][0]["state"], "cloud_passed");
    let filtered = command(
        &rig,
        &["lanes", "--id", &id, "--state", "cloud_passed", "--json"],
    )?;
    assert_eq!(code(&filtered), 0, "{filtered:?}");
    let filtered: Value = serde_json::from_slice(&filtered.stdout.bytes)?;
    assert_eq!(filtered["data"]["lanes"][0]["state"], "cloud_passed");
    let waited = command(&rig, &["lanes", "--id", &id, "--wait", "1s", "--json"])?;
    assert_eq!(code(&waited), 0, "{waited:?}");
    let waited: Value = serde_json::from_slice(&waited.stdout.bytes)?;
    assert_eq!(waited["data"]["condition_met"], true);
    let status_wait = command(
        &rig,
        &[
            "status",
            "--wait=1s",
            "--target",
            &format!("lane:{id}"),
            "--json",
        ],
    )?;
    assert_eq!(code(&status_wait), 0, "{status_wait:?}");
    let unseen = command(&rig, &["lanes", "--wait", "1s", "--json"])?;
    assert_eq!(code(&unseen), 0, "{unseen:?}");
    let unseen: Value = serde_json::from_slice(&unseen.stdout.bytes)?;
    assert_eq!(unseen["data"]["condition_met"], true);
    let again = command(&rig, &["lanes", "--wait", "1s", "--json"])?;
    let again: Value = serde_json::from_slice(&again.stdout.bytes)?;
    assert!(
        again["data"]["lanes"]
            .as_array()
            .is_some_and(|lanes| lanes.is_empty())
    );

    let status = command(&rig, &["status", &id, "--json"])?;
    assert_eq!(code(&status), 0, "{status:?}");
    let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
    assert_eq!(
        status["data"]["cloud_jobs"][0]["observed_model_status"],
        "unknown"
    );
    assert_eq!(status["data"]["lanes"][0]["cloud_state"], "cloud_passed");
    let output = command(&rig, &["output", &id, "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let output: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        output["data"]["cloud"]["state"]["result"]["log_location"],
        "fixture-log"
    );
    let project = status["data"]["lanes"][0]["project_id"]
        .as_str()
        .ok_or("project id")?;
    let prior = fs::read_to_string(&config)?;
    fs::write(
        &config,
        format!("{prior}\n[domains.lanes.projects.\"{project}\"]\ncloud_target = 'verify'\n"),
    )?;
    let configured = command(
        &rig,
        &[
            "run",
            "--operation-id",
            "configured-cloud",
            "--wait",
            "1s",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--json",
        ],
    )?;
    assert_eq!(code(&configured), 0, "{configured:?}");
    let configured: Value = serde_json::from_slice(&configured.stdout.bytes)?;
    assert_eq!(configured["data"]["cloud_jobs"][0]["target"], "verify");
    assert_eq!(
        fs::read_to_string(rig.root.join("fake-cloud.json.count"))?,
        "2"
    );
    Ok(())
}
