//! Host Git conformance with a linked worktree and a local bare remote.
use super::delivery::*;
use super::*;
use provenant_core::GitOid;
use provenant_supervisor::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use std::{ffi::OsString, path::Path, time::Duration};

#[test]
fn host_delivery_fetches_commits_pushes_and_records_upstream()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let project = root.join("project");
    let lane = root.join("lane");
    let remote = root.join("remote.git");
    for path in [&home, &project] {
        std::fs::create_dir(path)?;
    }
    let environment = vec![
        ("HOME".into(), home.into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ];
    let host = HostDeliveryContext::new(environment.clone());
    let executor = SystemExecutor::new(1)?;
    let git =
        |cwd: &Path, args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once("/usr/bin/git")
                .chain(args.iter().copied())
                .map(Into::into)
                .collect();
            let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
            options.env = Some(&environment);
            let output = executor.execute(options)?;
            assert_eq!(
                output.termination,
                Termination::Exit(0),
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
            Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
        };
    git(&project, &["init", "-q"])?;
    git(&project, &["config", "user.name", "Fixture"])?;
    git(
        &project,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(&project, &["commit", "--allow-empty", "-qm", "Initial"])?;
    git(
        &project,
        &["init", "--bare", "-q", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &project,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    // A narrowed fetch refspec that omits the delivered branch, as in a
    // main-only clone, must not defeat delivery.
    git(
        &project,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/base:refs/remotes/origin/base",
        ],
    )?;
    git(&project, &["push", "-q", "origin", "HEAD:refs/heads/base"])?;
    let initial = git(&project, &["rev-parse", "HEAD"])?;
    git(&project, &["update-ref", "-d", "refs/remotes/origin/base"])?;
    git(
        &project,
        &[
            "worktree",
            "add",
            "-qb",
            "lane",
            lane.to_str().ok_or("lane")?,
        ],
    )?;
    let writer = WriterRequest {
        project,
        worktree: lane.clone(),
    };
    let request = WriterDeliveryRequest {
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
    };
    let host = host.prepare(&writer, &request)?;
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/base"])?,
        initial
    );
    // The writer's committed head, rather than any provider's prose, is delivered.
    std::fs::write(lane.join("change"), "committed by the writer")?;
    git(&lane, &["add", "change"])?;
    git(&lane, &["commit", "-qm", "Writer change"])?;
    git(&lane, &["config", "push.followTags", "true"])?;
    git(
        &lane,
        &["tag", "-a", "local-only", "-m", "Do not deliver this tag"],
    )?;
    let hook = writer.project.join(".git/hooks/pre-push");
    std::fs::write(&hook, "#!/bin/sh\nexit 42\n")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))?;
    let head = GitOid::parse(&git(&lane, &["rev-parse", "HEAD"])?)?;
    let head_text = head.to_string();
    let fixture = root.join("store");
    std::fs::create_dir(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&tests::context(&fixture)?, &call)?;
    let mut input = tests::input();
    input.request.tasks[0].writer = Some(writer.clone());
    input.request.tasks[0].deliver = Some(request.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    // A lane that rewrites the stored destination after launch cannot steer
    // the host's push: the destination bound at prepare wins.
    let rewritten = WriterDeliveryRequest {
        branch: "main".into(),
        ..request.clone()
    };
    assert!(matches!(
        deliver_once(&host, &writer, &rewritten, &attempt),
        Err(RunError::Validation(
            "delivery destination changed after launch"
        ))
    ));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/main"])?.is_empty());
    let receipt = deliver_once(&host, &writer, &request, &attempt)?;
    assert_eq!(receipt.head, Some(head));
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered { pull_request: None }
    ));
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/published/lane"])?,
        head_text.as_str()
    );
    assert_eq!(git(&lane, &["config", "branch.lane.remote"])?, "origin");
    assert_eq!(
        git(&lane, &["config", "branch.lane.merge"])?,
        "refs/heads/published/lane"
    );
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    assert!(git(&lane, &["ls-remote", "--tags", "origin"])?.is_empty());
    dispatch_tests::seal_exit(&store, &tests::exit())?;
    let record = store.publish_terminal(id, 1, &tests::publication(), &tests::exit(), &call)?;
    assert_eq!(record.delivery.as_ref(), Some(&receipt));
    assert_eq!(store.record(id, &call)?.head, Some(head));
    // A repeated terminal callback must not push a newer head or repeat PR creation.
    git(&lane, &["commit", "--allow-empty", "-qm", "Later"])?;
    assert_eq!(deliver_once(&host, &writer, &request, &attempt)?, receipt);
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    // A writer that made no commit reports `no_new_commits` and pushes nothing.
    let idle_lane = root.join("idle");
    git(
        &writer.project,
        &[
            "worktree",
            "add",
            "-qb",
            "idle",
            idle_lane.to_str().ok_or("idle")?,
        ],
    )?;
    let idle_writer = WriterRequest {
        project: writer.project.clone(),
        worktree: idle_lane.clone(),
    };
    let idle_request = WriterDeliveryRequest {
        branch: "published/idle".into(),
        ..request.clone()
    };
    let idle_host =
        HostDeliveryContext::new(environment.clone()).prepare(&idle_writer, &idle_request)?;
    let mut idle = tests::input();
    idle.run_id = provenant_core::RunId::from_bytes([17; 16]);
    idle.operation_id = provenant_core::OperationId::parse("idle-delivery")?;
    idle.attempts[0].attempt_id = provenant_core::AttemptId::from_bytes([18; 16]);
    idle.request.tasks[0].writer = Some(idle_writer.clone());
    idle.request.tasks[0].deliver = Some(idle_request.clone());
    store.create(&idle, &call)?;
    let idle_attempt = store.attempt_dir(idle.run_id, idle.attempts[0].attempt_id, false)?;
    let idle_receipt = deliver_once(&idle_host, &idle_writer, &idle_request, &idle_attempt)?;
    assert_eq!(idle_receipt.state, WriterDeliveryState::NoNewCommits);
    assert_eq!(idle_receipt.head, Some(GitOid::parse(&initial)?));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/published/idle"])?.is_empty());
    // An interrupted delivery is recorded during terminal recovery, with no push.
    let mut interrupted = tests::input();
    interrupted.run_id = provenant_core::RunId::from_bytes([14; 16]);
    interrupted.operation_id = provenant_core::OperationId::parse("interrupted-delivery")?;
    interrupted.attempts[0].attempt_id = provenant_core::AttemptId::from_bytes([16; 16]);
    interrupted.request.tasks[0].writer = Some(writer);
    interrupted.request.tasks[0].deliver = Some(WriterDeliveryRequest {
        branch: "interrupted".into(),
        ..request
    });
    store.create(&interrupted, &call)?;
    let id = interrupted.attempts[0].attempt_id;
    let attempt = store.attempt_dir(interrupted.run_id, id, false)?;
    let pending = WriterDeliveryReceipt {
        branch: "interrupted".into(),
        state: WriterDeliveryState::Failed {
            stage: WriterDeliveryStage::Interrupted,
        },
        ..receipt
    };
    artifacts::seal(
        &attempt,
        "delivery-started.json",
        &db::bytes(&pending)?,
        RECORD_LIMIT,
    )?;
    let mut exit = tests::exit();
    exit.scope.run = interrupted.run_id;
    exit.scope.attempt = id;
    dispatch_tests::seal_exit(&store, &exit)?;
    artifacts::seal(
        &attempt,
        "result.md",
        tests::publication().result.as_bytes(),
        RESULT_LIMIT,
    )?;
    let record = store.recover_result_only(id, &tests::publication(), &exit, &call)?;
    assert_eq!(record.delivery, Some(pending));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/interrupted"])?.is_empty());
    Ok(())
}
