//! Host Git conformance with a linked worktree and a local bare remote.
use super::delivery::*;
use super::*;
use provenant_core::GitOid;
use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use std::{ffi::OsString, path::Path, time::Duration};

struct PrepareFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    environment: Vec<(OsString, OsString)>,
    writer: WriterRequest,
    request: WriterDeliveryRequest,
}
impl PrepareFixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&home)?;
        let fixture = Self {
            _temp: temp,
            root: root.clone(),
            environment: vec![
                ("HOME".into(), home.into_os_string()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ],
            writer: WriterRequest {
                project,
                worktree: root.join("lane"),
            },
            request: WriterDeliveryRequest {
                remote: "origin".into(),
                branch: "published/lane".into(),
                open_pr: false,
            },
        };
        fixture.git(&["init", "-q"])?;
        fixture.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Initial",
        ])?;
        let remote = root.join("remote.git");
        fixture.git(&["init", "--bare", "-q", remote.to_str().ok_or("remote")?])?;
        fixture.git(&["remote", "add", "origin", remote.to_str().ok_or("remote")?])?;
        fixture.git(&[
            "worktree",
            "add",
            "-qb",
            "lane",
            fixture.writer.worktree.to_str().ok_or("lane")?,
        ])?;
        Ok(fixture)
    }
    fn git(&self, args: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let git = provenant_os::git::git_executable().ok_or("git executable")?;
        let argv: Vec<OsString> = std::iter::once(git.into_os_string())
            .chain(args.iter().map(OsString::from))
            .collect();
        let mut options = ExecOptions::new(&argv, &self.writer.project, Duration::from_secs(5));
        options.env = Some(&self.environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(())
    }
    fn executable(&self, path: &Path, script: &str) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    fn prepare(&self) -> Result<HostDeliveryContext> {
        HostDeliveryContext::new(self.environment.clone()).prepare(&self.writer, &self.request)
    }
    fn lane_git(&self, args: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut command = vec!["-C", self.writer.worktree.to_str().ok_or("lane")?];
        command.extend_from_slice(args);
        self.git(&command)
    }
    fn commit(
        &self,
        path: &Path,
        message: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        self.git(&[
            "-C",
            path.to_str().ok_or("commit path")?,
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            message,
        ])
    }
    fn terminal(
        &self,
        write: impl FnOnce() -> std::result::Result<(), Box<dyn std::error::Error>>,
    ) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime};
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::fs::Directory;
        use provenant_supervisor::custody::{RecoveryState, WriterBinding};
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;

        let state = self.root.join("state");
        let attempt_path = self.root.join("attempt");
        std::fs::create_dir(&state)?;
        std::fs::create_dir(&attempt_path)?;
        for path in [&state, &attempt_path, &self.root.join("home")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let attempt = Directory::open_anchor(&attempt_path)?;
        let utility = UtilityRuntime::new(
            provenant_os::git::git_executable().ok_or("git executable")?,
            self.root.join("home"),
        )?;
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
        let repo = utility.open_repo(&executor, &self.writer.project, budget, &cancel)?;
        let worktree = provenant_confinement::verify_worktree(&repo, &self.writer.worktree)?;
        let head = provenant_confinement::run_utility(
            &executor,
            GitRead::RevParseHead,
            worktree.repo(),
            &utility,
            budget,
            &cancel,
        )?;
        assert_eq!(head.termination, Termination::Exit(0));
        let base = GitOid::parse(std::str::from_utf8(&head.stdout.bytes)?.trim())?;
        let launch = writer::WriterLaunch {
            worktree,
            branch: provenant_confinement::repo::GitRefName::new("refs/heads/lane")?,
            base,
            custody: None,
        };
        let scope = provenant_supervisor::custody::Scope {
            generation: 1,
            ..tests::exit().scope
        };
        launch.snapshot(&state, &attempt, scope)?;
        let binding = WriterBinding::from_verified(
            launch.worktree.repo().root_dir(),
            launch.worktree.repo().private_dir(),
            launch.branch.as_str(),
            base,
        )?;
        let clock = provenant_os::clock::SystemClock::new();
        let recovery = provenant_supervisor::fixture::acquire_writer_with_binding(
            Directory::open_anchor(&state)?,
            scope,
            binding,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &cancel,
            &clock,
        )?
        .into_recovery()?;
        assert_eq!(recovery.observe(), RecoveryState::Absent);
        write()?;
        let review = launch.terminal(&state, &attempt, &recovery, "origin")?;
        let archive = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        let manifest: serde_json::Value = serde_json::from_slice(
            &archive.read_bounded(OsStr::new("manifest.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(manifest["review_required"], review);
        let terminal: writer::WriterTerminal = serde_json::from_slice(
            &attempt.read_bounded(OsStr::new("writer-terminal.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(terminal.protected_paths.is_empty(), !review);
        Ok(review)
    }
}

#[test]
fn terminal_ignores_protected_changes_present_at_lane_start()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    std::fs::create_dir(fixture.writer.worktree.join(".github"))?;
    std::fs::write(
        fixture.writer.worktree.join(".github/ci.yml"),
        "PR policy\n",
    )?;
    fixture.lane_git(&["add", ".github"])?;
    fixture.commit(&fixture.writer.worktree, "Existing PR policy")?;
    assert!(!fixture.terminal(|| {
        std::fs::write(
            fixture.writer.worktree.join("source.txt"),
            "writer source\n",
        )?;
        fixture.lane_git(&["add", "source.txt"])?;
        fixture.commit(&fixture.writer.worktree, "Writer source")
    })?);
    Ok(())
}

#[test]
fn terminal_falls_back_when_delivery_target_cannot_be_resolved()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for target in ["missing_head", "dangling_head", "unrelated"] {
        for protected in [false, true] {
            let fixture = PrepareFixture::new()?;
            if target != "missing_head" {
                fixture.git(&[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ])?;
            }
            if target == "unrelated" {
                fixture.git(&["checkout", "--orphan", "unrelated"])?;
                fixture.commit(&fixture.writer.project, "Unrelated target")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
            }
            let review = fixture.terminal(|| {
                let path = if protected { "AGENTS.md" } else { "source.txt" };
                std::fs::write(fixture.writer.worktree.join(path), "writer change\n")?;
                fixture.lane_git(&["add", path])?;
                fixture.commit(&fixture.writer.worktree, "Writer change")
            })?;
            assert_eq!(review, protected, "{target}");
        }
    }
    Ok(())
}

#[test]
fn terminal_exempts_merged_upstream_protected_changes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for inherited in ["none", "addition", "deletion"] {
        for protected in [false, true] {
            let fixture = PrepareFixture::new()?;
            if inherited == "addition" {
                std::fs::create_dir(fixture.writer.worktree.join(".github"))?;
                std::fs::write(
                    fixture.writer.worktree.join(".github/pr.yml"),
                    "PR policy\n",
                )?;
                fixture.lane_git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.worktree, "Existing PR addition")?;
            } else if inherited == "deletion" {
                std::fs::create_dir(fixture.writer.project.join(".github"))?;
                std::fs::write(
                    fixture.writer.project.join(".github/old.yml"),
                    "old policy\n",
                )?;
                fixture.git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.project, "Shared policy")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
                fixture.lane_git(&["merge", "--ff-only", "origin/main"])?;
                fixture.lane_git(&["rm", ".github/old.yml"])?;
                fixture.commit(&fixture.writer.worktree, "Existing PR deletion")?;
            }
            let review = fixture.terminal(|| {
                std::fs::create_dir_all(fixture.writer.project.join(".github"))?;
                std::fs::write(
                    fixture.writer.project.join(".github/ci.yml"),
                    "main policy\n",
                )?;
                fixture.git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.project, "Upstream policy")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
                fixture.git(&[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ])?;
                std::fs::write(
                    fixture.writer.worktree.join("source.txt"),
                    "writer source\n",
                )?;
                fixture.lane_git(&["add", "source.txt"])?;
                fixture.commit(&fixture.writer.worktree, "Writer source")?;
                fixture.lane_git(&[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "merge",
                    "--no-edit",
                    "origin/main",
                ])?;
                if protected {
                    std::fs::write(
                        fixture.writer.worktree.join(".github/ci.yml"),
                        "writer policy\n",
                    )?;
                    fixture.lane_git(&["add", ".github"])?;
                    fixture.commit(&fixture.writer.worktree, "Writer policy")?;
                }
                Ok(())
            })?;
            assert_eq!(review, protected, "{inherited}");
        }
    }
    Ok(())
}

#[test]
fn prepare_labels_unsupported_ssh_command() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture
        .environment
        .push(("GIT_SSH_COMMAND".into(), "ssh -v".into()));
    let reason = fixture
        .prepare()
        .expect_err("unsupported SSH command")
        .to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (environment): GIT_SSH_COMMAND is not supported; use host SSH config"
    );
    Ok(())
}

#[test]
fn prepare_labels_environment_path_failures() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = PrepareFixture::new()?;
    for name in ["HOME", "GH_CONFIG_DIR"] {
        for (path, detail) in [
            (fixture.root.join("missing/config"), "not found"),
            (
                fixture.writer.worktree.clone(),
                "inside the writer worktree",
            ),
        ] {
            fixture.environment.retain(|(key, _)| key != name);
            fixture
                .environment
                .insert(0, (name.into(), path.into_os_string()));
            let reason = fixture
                .prepare()
                .expect_err("invalid host path")
                .to_string();
            assert_eq!(
                reason,
                format!("host writer delivery failed at Prepare (environment): {name}: {detail}")
            );
        }
        fixture.environment.retain(|(key, _)| key != name);
    }
    Ok(())
}

#[test]
fn prepare_labels_unavailable_gh_executable() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = PrepareFixture::new()?;
    fixture.request.open_pr = true;
    fixture.environment.retain(|(key, _)| key != "PATH");
    let reason = fixture.prepare().expect_err("PATH is unset").to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (gh executable): gh is unavailable: PATH is unset"
    );
    fixture.environment.push(("PATH".into(), "".into()));
    let reason = fixture
        .prepare()
        .expect_err("gh is unavailable")
        .to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (gh executable): gh is unavailable on PATH"
    );
    Ok(())
}

#[test]
fn prepare_labels_git_executable_inside_worktree()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let git = fixture.writer.worktree.join("git");
    fixture.executable(&git, "#!/bin/sh\nexit 0\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), git.into_os_string()));
    let reason = fixture
        .prepare()
        .expect_err("unsafe git executable")
        .to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (git executable): inside the writer worktree"
    );
    Ok(())
}

#[test]
fn prepare_accepts_absolute_shell_helper_outside_path()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let helper = fixture.root.join("gh");
    fixture.executable(&helper, "#!/bin/sh\nexit 0\n")?;
    fixture.git(&[
        "config",
        "credential.helper",
        &format!("!{} auth git-credential", helper.display()),
    ])?;
    fixture.prepare()?;
    Ok(())
}

#[test]
fn prepare_failures_name_step_and_bound_git_stderr()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for (pattern, step) in [
        ("remote get-url", "remote URL"),
        ("symbolic-ref --quiet", "HEAD"),
        ("rev-parse --absolute-git-dir", "git dir"),
        ("config --get-regexp", "credential helper"),
        ("fetch --no-tags", "fetch"),
    ] {
        let mut fixture = PrepareFixture::new()?;
        let git = fixture.root.join("git");
        let real_git = provenant_os::git::git_executable().ok_or("git executable")?;
        fixture.executable(&git, &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'{pattern}'*)\nprintf '%s' 'fixture git failure: private-token https://user:password@example.invalid/ {}' >&2\nexit 2;;\nesac\nexec '{}' \"$@\"\n", "é".repeat(5000), real_git.display()))?;
        fixture
            .environment
            .push(("PROVENANT_GIT".into(), git.into_os_string()));
        fixture
            .environment
            .push(("GH_TOKEN".into(), "private-token".into()));
        let reason = fixture.prepare().expect_err("git must fail").to_string();
        assert!(reason.contains("Prepare"), "{reason}");
        assert!(reason.contains(step), "{reason}");
        assert!(reason.contains("fixture git failure"), "{reason}");
        assert!(!reason.contains("private-token"), "{reason}");
        assert!(!reason.contains("user:password"), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
        assert!(reason.len() < 1400, "diagnostic has {} bytes", reason.len());
    }
    Ok(())
}

#[test]
fn prepare_refuses_unavailable_and_unsafe_credential_helpers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let unavailable = fixture.root.join("missing/gh");
    let not_executable = fixture.root.join("not-executable");
    std::fs::write(&not_executable, "not executable")?;
    let lane_helper = fixture.writer.worktree.join("gh");
    fixture.executable(&lane_helper, "#!/bin/sh\nexit 0\n")?;
    let link = fixture.root.join("linked-gh");
    std::os::unix::fs::symlink(&lane_helper, &link)?;
    let opaque = fixture.root.join("gh;echo");
    fixture.executable(&opaque, "#!/bin/sh\nexit 0\n")?;
    for (helper, detail) in [
        (&unavailable, "gh: not found"),
        (&not_executable, "not-executable"),
        (&fixture.root, "unavailable"),
        (&lane_helper, "inside the writer worktree"),
        (&link, "inside the writer worktree"),
        (&opaque, "opaque shell credential helper"),
    ] {
        fixture.git(&[
            "config",
            "credential.helper",
            &format!("!{} auth git-credential", helper.display()),
        ])?;
        let reason = fixture.prepare().expect_err("unsafe helper").to_string();
        assert!(reason.contains("Prepare (credential helper)"), "{reason}");
        assert!(reason.contains(detail), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
    }
    Ok(())
}

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
    let git_executable = provenant_os::git::git_executable().ok_or("git executable")?;
    let git =
        |cwd: &Path, args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once(git_executable.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
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
