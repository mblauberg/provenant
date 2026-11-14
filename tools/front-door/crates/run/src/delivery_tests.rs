//! Host Git conformance with a linked worktree and a local bare remote.
use super::delivery::*;
use super::*;
use provenant_core::{CancelToken, GitOid, ProjectId};
use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use provenant_os::fs::Directory;
use std::{ffi::OsString, path::Path, time::Duration};

pub(super) struct PrepareFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    environment: Vec<(OsString, OsString)>,
    pub(super) writer: WriterRequest,
    request: WriterDeliveryRequest,
}

pub(super) fn registered_writer_project_id(
    project: &Path,
    home: &Path,
    checkout: &Path,
    state: &Path,
) -> std::result::Result<ProjectId, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(home)?;
    std::fs::create_dir_all(checkout)?;
    let mut inputs = provenant_core::RootInputs::new(home.to_path_buf(), checkout.to_path_buf());
    inputs.state_dir = Some(state.to_path_buf());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::Occupancy,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    let clock = provenant_os::clock::SystemClock;
    let cancel = CancelToken::new();
    let workers = Free;
    Ok(
        provenant_confinement::worktrees::with_context_using_workers(
            &roots,
            home,
            project,
            &clock,
            &cancel,
            &workers,
            |context| Ok(context.binding.project_id()),
        )?,
    )
}

pub(super) fn writer_custody_extra(project_id: ProjectId) -> provenant_records::ExtraFields {
    [("custody_project".into(), serde_json::json!(project_id))].into()
}

// Medium: real filesystem locking with the remote process edge supplied as a callback.

// Medium: a real contended fetch lock with an exhausted wait budget.

// Medium: an external Git client holds a real tracking-ref lock.

/// A project with a bare `origin` whose branches the test writes as packed
/// refs, so a case-insensitive host can still hold both spellings.
struct OriginFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    project: std::path::PathBuf,
    home: std::path::PathBuf,
    remote: std::path::PathBuf,
    git: std::path::PathBuf,
}

impl OriginFixture {
    fn run_in(
        &self,
        directory: &Path,
        args: &[&str],
    ) -> std::result::Result<String, Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command_at(&self.git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_DIR", directory)
            .env("GIT_WORK_TREE", &self.project)
            .current_dir(&self.project)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn local(&self, args: &[&str]) -> std::result::Result<String, Box<dyn std::error::Error>> {
        self.run_in(&self.project.join(".git"), args)
    }

    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let fixture = Self {
            project: root.join("project"),
            home: root.join("home"),
            remote: root.join("remote.git"),
            git: provenant_os::git::git_executable().ok_or("git unavailable")?,
            root,
            _temp: temp,
        };
        std::fs::create_dir(&fixture.project)?;
        std::fs::create_dir(&fixture.home)?;
        fixture.local(&["init", "-q"])?;
        fixture.commit("initial")?;
        fixture.run_in(&fixture.remote, &["init", "-q"])?;
        fixture.run_in(&fixture.remote, &["config", "--local", "core.bare", "true"])?;
        let remote = fixture.remote.to_str().ok_or("remote path")?;
        fixture.local(&["remote", "add", "origin", remote])?;
        fixture.local(&["push", "-q", "origin", "HEAD:refs/heads/main"])?;
        fixture.local(&["fetch", "-q", "origin"])?;
        fixture.local(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ])?;
        Ok(fixture)
    }

    fn commit(&self, message: &str) -> std::result::Result<String, Box<dyn std::error::Error>> {
        self.local(&["commit", "--allow-empty", "-qm", message])?;
        self.local(&["rev-parse", "HEAD"])
    }

    /// Advertise exactly these branches, with `default` as the remote HEAD.
    fn advertise(
        &self,
        branches: &[(&str, &str)],
        default: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let _ = self.run_in(&self.remote, &["update-ref", "-d", "refs/heads/main"]);
        // The header promises byte order, which Git relies on for lookups.
        let mut branches = branches.to_vec();
        branches.sort_by_key(|(name, _)| name.as_bytes().to_vec());
        let mut packed = String::from("# pack-refs with: sorted\n");
        for (name, oid) in branches {
            packed.push_str(&format!("{oid} refs/heads/{name}\n"));
        }
        std::fs::write(self.remote.join("packed-refs"), packed)?;
        self.run_in(
            &self.remote,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{default}")],
        )?;
        Ok(())
    }

    fn refresh(&self, git: Option<&Path>) -> crate::Result<()> {
        let mut environment: Vec<(OsString, OsString)> = vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), self.home.as_os_str().to_owned()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ];
        if let Some(git) = git {
            environment.push(("PROVENANT_GIT".into(), git.as_os_str().to_owned()));
        }
        let custody_project = registered_writer_project_id(
            &self.project,
            &self.home,
            &self.root.join("checkout"),
            &self.root.join("state"),
        )
        .map_err(|error| crate::RunError::Io(std::io::Error::other(error.to_string())))?;
        let writer = WriterRequest {
            extra: writer_custody_extra(custody_project),
            project: self.project.clone(),
            worktree: self.project.clone(),
        };
        HostDeliveryContext::new(environment)
            .refresh_origin_with(&writer, &self.project.join(".git"))?;
        Ok(())
    }
}

impl PrepareFixture {
    pub(super) fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&home)?;
        let mut fixture = Self {
            _temp: temp,
            root: root.clone(),
            environment: vec![
                ("HOME".into(), home.into_os_string()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ]
            .into_iter()
            .chain(provenant_os::fixture::fixture_git_settings())
            .collect(),
            writer: WriterRequest {
                extra: Default::default(),
                project,
                worktree: root.join("lane"),
            },
            request: WriterDeliveryRequest {
                extra: Default::default(),
                mode: Default::default(),
                remote: "origin".into(),
                branch: "published/lane".into(),
                open_pr: false,
                fast_forward_only: false,
            },
        };
        fixture.git(&["init", "-q"])?;
        fixture.git(&["commit", "--allow-empty", "-qm", "Initial"])?;
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
        let custody_project = registered_writer_project_id(
            &fixture.writer.project,
            &root.join("home"),
            &root.join("checkout"),
            &root.join("state"),
        )?;
        fixture.writer.extra = writer_custody_extra(custody_project);
        Ok(fixture)
    }
    pub(super) fn git(&self, args: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
    pub(crate) fn lane_git(
        &self,
        args: &[&str],
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut command = vec!["-C", self.writer.worktree.to_str().ok_or("lane")?];
        command.extend_from_slice(args);
        self.git(&command)
    }
    pub(crate) fn commit(
        &self,
        path: &Path,
        message: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        self.git(&[
            "-C",
            path.to_str().ok_or("commit path")?,
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
        self.terminal_via("origin", None, write)
    }
    fn terminal_via(
        &self,
        remote: &str,
        base_ref: Option<&str>,
        write: impl FnOnce() -> std::result::Result<(), Box<dyn std::error::Error>>,
    ) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        self.terminal_via_inner(remote, base_ref, false, false, false, write)
    }
    fn terminal_via_inner(
        &self,
        _remote: &str,
        _base_ref: Option<&str>,
        resume_after_publish: bool,
        advance_after_publish: bool,
        budget_stop: bool,
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
        // A lane fixture may already have built the attempt and its scratch tree.
        for path in [&state, &attempt_path] {
            provenant_os::fixture::private_state_dir(path)?;
        }
        std::fs::set_permissions(
            self.root.join("home"),
            std::fs::Permissions::from_mode(0o700),
        )?;
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
            facts: Default::default(),
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
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0)?,
            provenant_core::MonoTime::from_micros(0),
        );
        let mut recovery = provenant_supervisor::fixture::acquire_writer_with_binding(
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
        let scratch = attempt_path.join("tmp/candidate-0");
        std::fs::create_dir_all(&scratch)?;
        let git_executable = provenant_os::git::git_executable().ok_or("git executable")?;
        let tip = crate::objects::resolve_ref(
            &git_executable,
            launch.worktree.repo(),
            launch.branch.as_str(),
        )?;
        if crate::objects::bound_source(&attempt)?.is_none() {
            crate::objects::prepare_bound(
                &git_executable,
                launch.worktree.repo(),
                &attempt,
                &scratch,
                launch.branch.as_str(),
                base,
            )?;
            std::fs::write(
                scratch.join("git-common/refs/heads/lane"),
                format!("{tip}\n"),
            )?;
        }
        recovery.prepare_settlement()?;
        let mut settlement =
            crate::settlement::Guard::claim(&attempt, scope, &clock)?.ok_or("settlement owner")?;
        if resume_after_publish {
            let source = crate::objects::bound_source(&attempt)?.ok_or("private Git source")?;
            let tip = crate::objects::private_tip(&source, launch.branch.as_str())?;
            settlement.set_head(tip)?;
            settlement.set_base(base)?;
            crate::objects::settle(
                &git_executable,
                launch.worktree.repo(),
                Some(&source.objects),
                tip,
            )?;
            settlement.advance(crate::settlement::Phase::Publish)?;
            crate::objects::publish_branch(
                &git_executable,
                launch.worktree.repo(),
                launch.branch.as_str(),
                base,
                tip,
            )?;
            if advance_after_publish {
                std::fs::write(self.writer.worktree.join("AGENTS.md"), "later writer\n")?;
                self.lane_git(&["add", "AGENTS.md"])?;
                self.commit(&self.writer.worktree, "later writer")?;
            }
            // Drop the process owner at the crash boundary before the phase
            // checkpoint. A fresh driver must safely replay publication.
            drop(settlement);
            clock.wait_until(provenant_core::MonoTime::from_micros(
                clock.mono_now().as_micros() + 120_000_000,
            ))?;
            // The claim is a zero-wait try_lock, and a sibling test's forked
            // child can briefly share the dropped lock's open file description
            // until it execs. Retry for about five seconds.
            let mut resumed = None;
            let (_, observation) = provenant_test_support::wait_for(
                "resumed settlement owner",
                Duration::from_secs(5),
                || {
                    let result = crate::settlement::Guard::claim_observed(&attempt, scope, &clock);
                    let retryable = matches!(
                        &result,
                        Ok((
                            None,
                            Some(
                                crate::settlement::ClaimRefusal::DriverLease
                                    | crate::settlement::ClaimRefusal::JournalLock
                            )
                        ))
                    );
                    let observation = match &result {
                        Ok((None, reason)) => format!("claim refused: {reason:?}"),
                        Ok((Some(_), _)) => "claimed".to_owned(),
                        Err(error) => format!("claim error: {error}"),
                    };
                    resumed = Some(result.map(|(driver, _)| driver));
                    (retryable, observation)
                },
                |(retryable, _)| !retryable,
            )?;
            settlement = resumed
                .ok_or("no claim attempted")??
                .ok_or_else(|| format!("resumed owner: {observation}"))?;
        }
        launch.terminal(&state, &attempt, &recovery, &mut settlement, budget_stop)?;
        let archive = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        let manifest: serde_json::Value = serde_json::from_slice(
            &archive.read_bounded(OsStr::new("manifest.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(manifest["disposition"], "writer_settled");
        let terminal: writer::WriterTerminal = serde_json::from_slice(
            &attempt.read_bounded(OsStr::new("writer-terminal.json"), RECORD_LIMIT)?,
        )?;
        if advance_after_publish {
            assert_eq!(terminal.head, tip);
        }
        Ok(false)
    }
}

fn stopped_merge_is_restored(
    budget_stop: bool,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime};
    use provenant_core::CancelToken;
    let fixture = PrepareFixture::new()?;
    std::fs::write(fixture.writer.worktree.join("conflict.txt"), "ours\n")?;
    fixture.lane_git(&["add", "conflict.txt"])?;
    fixture.commit(&fixture.writer.worktree, "ours")?;
    fixture.git(&["update-ref", "refs/remotes/origin/main", "refs/heads/lane"])?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    std::fs::write(fixture.writer.project.join("conflict.txt"), "theirs\n")?;
    fixture.git(&["add", "conflict.txt"])?;
    fixture.commit(&fixture.writer.project, "theirs")?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let output = provenant_os::fixture::git_command_at(&git)
        .env_clear()
        .envs(fixture.environment.iter().map(|(key, value)| (key, value)))
        .current_dir(&fixture.writer.project)
        .args(["rev-parse", "HEAD"])
        .output()?;
    assert!(output.status.success());
    let theirs = String::from_utf8(output.stdout)?.trim().to_owned();
    let prepare_private =
        || -> std::result::Result<std::path::PathBuf, Box<dyn std::error::Error>> {
            let attempt_path = fixture.root.join("attempt");
            let scratch = attempt_path.join("tmp/candidate-0");
            std::fs::create_dir_all(&scratch)?;
            let utility = UtilityRuntime::new(&git, fixture.root.join("home"))?;
            let executor = SystemExecutor::new(1)?;
            let cancel = CancelToken::new();
            let repo = utility.open_repo(
                &executor,
                &fixture.writer.project,
                UtilityBudget::bounded(Duration::from_secs(5))?,
                &cancel,
            )?;
            let worktree = provenant_confinement::verify_worktree(&repo, &fixture.writer.worktree)?;
            let base = crate::objects::resolve_ref(&git, worktree.repo(), "refs/heads/lane")?;
            crate::objects::prepare_bound(
                &git,
                worktree.repo(),
                &Directory::open_anchor(&attempt_path)?,
                &scratch,
                "refs/heads/lane",
                base,
            )?;
            Ok(scratch)
        };
    fixture.terminal_via_inner("origin", None, false, false, budget_stop, || {
        let scratch = prepare_private()?;
        let output = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(fixture.environment.iter().map(|(key, value)| (key, value)))
            .env("GIT_DIR", scratch.join("git-common"))
            .env("GIT_COMMON_DIR", scratch.join("git-common"))
            .env("GIT_OBJECT_DIRECTORY", scratch.join("git-objects"))
            .env("GIT_WORK_TREE", &fixture.writer.worktree)
            .current_dir(&fixture.writer.worktree)
            .args(["merge", "--no-edit", &theirs])
            .output()?;
        assert!(!output.status.success(), "fixture merge must conflict");
        Ok(())
    })?;
    assert!(fixture.root.join("attempt/merge-restored.json").is_file());
    if budget_stop {
        let note: serde_json::Value = serde_json::from_slice(&std::fs::read(
            fixture.root.join("attempt/budget-commit.json"),
        )?)?;
        assert!(
            note["note"]
                .as_str()
                .ok_or("note")?
                .contains("in-progress merge preserved")
        );
        let head = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(fixture.environment.iter().map(|(k, v)| (k, v)))
            .current_dir(&fixture.writer.worktree)
            .args(["log", "-1", "--pretty=%s"])
            .output()?;
        assert!(head.status.success());
        assert_eq!(head.stdout, b"ours\n");
        assert_eq!(
            std::fs::read_to_string(
                fixture
                    .writer
                    .project
                    .join(".git/worktrees/lane/MERGE_HEAD")
            )?
            .trim(),
            theirs
        );
    }
    let output = provenant_os::fixture::git_command_at(&git)
        .env_clear()
        .envs(fixture.environment.iter().map(|(key, value)| (key, value)))
        .current_dir(&fixture.writer.worktree)
        .args(["ls-files", "--unmerged"])
        .output()?;
    assert!(output.status.success());
    assert!(!output.stdout.is_empty());
    assert!(
        std::fs::read_to_string(fixture.writer.worktree.join("conflict.txt"))?.contains("<<<<<<<")
    );
    // A fresh writer starts from the restored index and finishes the merge.
    std::fs::rename(fixture.root.join("state"), fixture.root.join("first-state"))?;
    std::fs::rename(
        fixture.root.join("attempt"),
        fixture.root.join("first-attempt"),
    )?;
    fixture.terminal(|| {
        let scratch = prepare_private()?;
        let run = |args: &[&str]| -> std::result::Result<std::process::Output, Box<dyn std::error::Error>> {
            Ok(provenant_os::fixture::git_command_at(&git)
                .env_clear()
                .envs(fixture.environment.iter().map(|(key, value)| (key, value)))
                .env("GIT_DIR", scratch.join("git-common"))
                .env("GIT_COMMON_DIR", scratch.join("git-common"))
                .env("GIT_OBJECT_DIRECTORY", scratch.join("git-objects"))
                .env("GIT_WORK_TREE", &fixture.writer.worktree)
                .current_dir(&fixture.writer.worktree)
                .args(args)
                .output()?)
        };
        let unmerged = run(&["ls-files", "--unmerged"])?;
        assert!(unmerged.status.success() && !unmerged.stdout.is_empty());
        std::fs::write(fixture.writer.worktree.join("conflict.txt"), "resolved\n")?;
        assert!(run(&["add", "conflict.txt"])?.status.success());
        let commit = run(&["commit", "--no-edit"])?;
        assert!(commit.status.success(), "{}", String::from_utf8_lossy(&commit.stderr));
        Ok(())
    })?;
    let merge_head = fixture
        .writer
        .project
        .join(".git/worktrees/lane/MERGE_HEAD");
    assert!(!merge_head.exists());
    Ok(())
}

/// A lane that commits a clean merge in its private Git view, as a lane which
/// merged the default branch and pushed does. Returns the fixture, a runner for
/// private-view Git, and the host's Git environment.
fn lane_with_committed_merge() -> std::result::Result<
    (PrepareFixture, std::path::PathBuf, std::path::PathBuf),
    Box<dyn std::error::Error>,
> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime};
    use std::os::unix::fs::PermissionsExt;
    let fixture = PrepareFixture::new()?;
    fixture.git(&["update-ref", "refs/remotes/origin/main", "refs/heads/lane"])?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    std::fs::write(fixture.writer.project.join("theirs.txt"), "theirs\n")?;
    fixture.git(&["add", "theirs.txt"])?;
    fixture.commit(&fixture.writer.project, "theirs")?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let attempt_path = fixture.root.join("attempt");
    let scratch = attempt_path.join("tmp/candidate-0");
    provenant_os::fixture::private_state_dir(&scratch)?;
    // The utility runtime refuses a home that group or others can reach.
    std::fs::set_permissions(
        fixture.root.join("home"),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let utility = UtilityRuntime::new(&git, fixture.root.join("home"))?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let repo = utility.open_repo(
        &executor,
        &fixture.writer.project,
        UtilityBudget::bounded(Duration::from_secs(5))?,
        &cancel,
    )?;
    let worktree = provenant_confinement::verify_worktree(&repo, &fixture.writer.worktree)?;
    let base = crate::objects::resolve_ref(&git, worktree.repo(), "refs/heads/lane")?;
    crate::objects::prepare_bound(
        &git,
        worktree.repo(),
        &Directory::open_anchor(&attempt_path)?,
        &scratch,
        "refs/heads/lane",
        base,
    )?;
    Ok((fixture, scratch, git))
}

fn private_git(
    fixture: &PrepareFixture,
    scratch: &Path,
    git: &Path,
    args: &[&str],
) -> std::result::Result<String, Box<dyn std::error::Error>> {
    let output = provenant_os::fixture::git_command_at(git)
        .env_clear()
        .envs(fixture.environment.iter().map(|(key, value)| (key, value)))
        .env("GIT_DIR", scratch.join("git-common"))
        .env("GIT_COMMON_DIR", scratch.join("git-common"))
        .env("GIT_OBJECT_DIRECTORY", scratch.join("git-objects"))
        .env("GIT_WORK_TREE", &fixture.writer.worktree)
        .current_dir(&fixture.writer.worktree)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn worktree_git(
    fixture: &PrepareFixture,
    git: &Path,
    args: &[&str],
) -> std::result::Result<String, Box<dyn std::error::Error>> {
    let output = provenant_os::fixture::git_command_at(git)
        .env_clear()
        .envs(fixture.environment.iter().map(|(key, value)| (key, value)))
        .current_dir(&fixture.writer.worktree)
        .args(args)
        .output()?;
    assert!(output.status.success());
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// A lane which merged the default branch and committed leaves HEAD, index and
/// files at the same commit once settlement publishes it.
#[test]
fn committed_merge_leaves_the_bound_worktree_clean_at_the_lane_tip()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (fixture, scratch, git) = lane_with_committed_merge()?;
    let theirs = {
        let output = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(fixture.environment.iter().map(|(k, v)| (k, v)))
            .current_dir(&fixture.writer.project)
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        String::from_utf8(output.stdout)?.trim().to_owned()
    };
    fixture.terminal(|| {
        std::fs::write(fixture.writer.worktree.join("ours.txt"), "ours\n")?;
        private_git(&fixture, &scratch, &git, &["add", "ours.txt"])?;
        private_git(&fixture, &scratch, &git, &["commit", "-qm", "ours"])?;
        private_git(&fixture, &scratch, &git, &["merge", "--no-edit", &theirs])?;
        Ok(())
    })?;
    assert_eq!(
        worktree_git(&fixture, &git, &["status", "--porcelain"])?,
        ""
    );
    let head = worktree_git(&fixture, &git, &["rev-parse", "HEAD"])?;
    let lane = worktree_git(&fixture, &git, &["rev-parse", "refs/heads/lane"])?;
    assert_eq!(head, lane);
    assert_ne!(head, theirs);
    Ok(())
}

/// A lane which merged the default branch, pushed its branch and then ended
/// (as a `blocked` handoff does: settlement runs the same way whatever the
/// handoff says) leaves the files, index and HEAD at the pushed commit.
#[test]
fn pushed_lane_leaves_the_bound_worktree_clean_at_the_pushed_commit()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (fixture, scratch, git) = lane_with_committed_merge()?;
    let theirs_tip = {
        let output = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(fixture.environment.iter().map(|(k, v)| (k, v)))
            .current_dir(&fixture.writer.project)
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        String::from_utf8(output.stdout)?.trim().to_owned()
    };
    let pushed = std::cell::RefCell::new(String::new());
    fixture.terminal(|| {
        std::fs::write(fixture.writer.worktree.join("ours.txt"), "ours\n")?;
        private_git(&fixture, &scratch, &git, &["add", "ours.txt"])?;
        private_git(&fixture, &scratch, &git, &["commit", "-qm", "ours"])?;
        private_git(
            &fixture,
            &scratch,
            &git,
            &["merge", "--no-edit", &theirs_tip],
        )?;
        private_git(
            &fixture,
            &scratch,
            &git,
            &["push", "-q", "origin", "HEAD:refs/heads/lane"],
        )?;
        *pushed.borrow_mut() = private_git(&fixture, &scratch, &git, &["rev-parse", "HEAD"])?;
        Ok(())
    })?;
    let pushed = pushed.into_inner();
    assert_eq!(
        worktree_git(&fixture, &git, &["status", "--porcelain"])?,
        ""
    );
    assert_eq!(
        worktree_git(&fixture, &git, &["rev-parse", "HEAD"])?,
        pushed
    );
    assert_eq!(
        worktree_git(&fixture, &git, &["rev-parse", "refs/heads/lane"])?,
        pushed
    );
    assert_eq!(
        worktree_git(&fixture, &git, &["ls-remote", "origin", "refs/heads/lane"])?
            .split_whitespace()
            .next(),
        Some(pushed.as_str())
    );
    Ok(())
}

/// When the lane's commits cannot be published, the bound worktree is back at
/// the commit the host holds, clean, and the settlement note names the lane's
/// commit and branch.
#[test]
fn unpublished_lane_leaves_the_worktree_clean_and_names_the_commit_and_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (fixture, scratch, git) = lane_with_committed_merge()?;
    let wt = &fixture.writer.worktree;
    let base = worktree_git(&fixture, &git, &["rev-parse", "HEAD"])?;
    std::fs::write(wt.join("ours.txt"), "ours\n")?;
    private_git(&fixture, &scratch, &git, &["add", "ours.txt"])?;
    private_git(&fixture, &scratch, &git, &["commit", "-qm", "ours"])?;
    let tip = private_git(&fixture, &scratch, &git, &["rev-parse", "HEAD"])?;
    assert_ne!(tip, base);
    // The files carry the lane's commit while the host still holds the base.
    assert_eq!(
        worktree_git(&fixture, &git, &["status", "--porcelain"])?,
        "?? ours.txt"
    );
    let utility = provenant_confinement::UtilityRuntime::new(&git, fixture.root.join("home"))?;
    let executor = SystemExecutor::new(1)?;
    let repo = utility.open_repo(
        &executor,
        &fixture.writer.project,
        provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
        &CancelToken::new(),
    )?;
    let launch = writer::WriterLaunch {
        worktree: provenant_confinement::verify_worktree(&repo, wt)?,
        branch: provenant_confinement::repo::GitRefName::new("refs/heads/lane")?,
        base: GitOid::parse(&base)?,
        custody: None,
        facts: Default::default(),
    };
    let note = launch.unpublish(&Directory::open_anchor(&fixture.root.join("attempt"))?);
    assert!(note.contains(&tip), "{note}");
    assert!(note.contains("refs/heads/lane"), "{note}");
    assert_eq!(
        worktree_git(&fixture, &git, &["status", "--porcelain"])?,
        ""
    );
    assert_eq!(worktree_git(&fixture, &git, &["rev-parse", "HEAD"])?, base);
    assert!(!wt.join("ours.txt").exists());
    Ok(())
}

/// When the lane's commits cannot be published, its files move back, except
/// any file someone else changed, and they move forward again on publication.
#[test]
fn unpublished_lane_files_move_back_without_touching_foreign_edits()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use crate::objects::worktree::{Stores, converge};
    let (fixture, scratch, git) = lane_with_committed_merge()?;
    let wt = &fixture.writer.worktree;
    std::fs::write(wt.join("base.txt"), "base\n")?;
    fixture.lane_git(&["add", "base.txt"])?;
    fixture.commit(wt, "base file")?;
    let base = worktree_git(&fixture, &git, &["rev-parse", "HEAD"])?;
    // The private view shares the host's refs at launch; move it forward.
    std::fs::write(
        scratch.join("git-common/refs/heads/lane"),
        format!("{base}\n"),
    )?;
    std::fs::write(wt.join("added.txt"), "added\n")?;
    std::fs::write(wt.join("base.txt"), "changed\n")?;
    std::fs::write(wt.join("foreign.txt"), "lane\n")?;
    private_git(&fixture, &scratch, &git, &["add", "-A"])?;
    private_git(&fixture, &scratch, &git, &["commit", "-qm", "lane"])?;
    let tip = private_git(&fixture, &scratch, &git, &["rev-parse", "HEAD"])?;
    // Someone else edits a path the lane also committed.
    std::fs::write(wt.join("foreign.txt"), "someone else\n")?;
    let repo_source =
        crate::objects::bound_source(&Directory::open_anchor(&fixture.root.join("attempt"))?)?
            .ok_or("source")?;
    let (base_oid, tip_oid) = (GitOid::parse(&base)?, GitOid::parse(&tip)?);
    let scratch_git = scratch.join("git-common");
    let objects = scratch.join("git-objects");
    let utility = provenant_confinement::UtilityRuntime::new(&git, fixture.root.join("home"))?;
    let executor = SystemExecutor::new(1)?;
    let repo = utility.open_repo(
        &executor,
        &fixture.writer.project,
        provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
        &CancelToken::new(),
    )?;
    let verified = provenant_confinement::verify_worktree(&repo, wt)?;
    let stores = Stores {
        git_dir: &scratch_git,
        objects: Some(&objects),
    };
    let back = converge(&git, verified.repo(), &stores, tip_oid, base_oid)?;
    assert_eq!(back.moved, 2);
    assert_eq!(back.kept, ["foreign.txt"]);
    assert!(!wt.join("added.txt").exists());
    assert_eq!(std::fs::read_to_string(wt.join("base.txt"))?, "base\n");
    assert_eq!(
        std::fs::read_to_string(wt.join("foreign.txt"))?,
        "someone else\n"
    );
    drop(repo_source);
    Ok(())
}

/// A prepared host whose Git prints `bytes` of output for any call containing
/// `pattern`, once prepared. Every other call goes to the real Git. Preparing
/// lists worktrees too, so the fake stays inert until the flag file exists.
fn oversize_host(
    pattern: &str,
    bytes: usize,
) -> std::result::Result<(PrepareFixture, HostDeliveryContext), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let git = fixture.root.join("oversize-git");
    let payload = fixture.root.join("oversize-output");
    let flag = fixture.root.join("oversize-enabled");
    std::fs::write(&payload, vec![b'a'; bytes])?;
    let real_git = provenant_os::git::git_executable().ok_or("git executable")?;
    fixture.executable(
        &git,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'{pattern}'*)\nif [ -e '{}' ]; then cat '{}'; exit 0; fi;;\nesac\nexec '{}' \"$@\"\n",
            flag.display(),
            payload.display(),
            real_git.display()
        ),
    )?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), git.into_os_string()));
    let host = fixture.prepare()?;
    std::fs::write(&flag, b"")?;
    Ok((fixture, host))
}

pub(super) fn registered_landed_writer_fixture(
    fixture: &PrepareFixture,
) -> std::result::Result<ExecutionContext, Box<dyn std::error::Error>> {
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::Occupancy,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    use provenant_core::{CancelToken, Clock, RootInputs};
    use provenant_os::clock::SystemClock;
    let mut execution = tests::context(&fixture.root)?;
    let mut inputs = RootInputs::new(fixture.root.join("home"), fixture.root.join("checkout"));
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    execution.project_id = provenant_confinement::worktrees::with_context(
        &roots,
        &fixture.root.join("home"),
        &fixture.writer.project,
        &clock,
        &cancel,
        |context| {
            let context = provenant_confinement::worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                attempt_id: None,
                operation_id: None,
                branch: "lane".into(),
                base: String::new(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: clock
                    .wall_now()
                    .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput)?,
                origin: provenant_confinement::worktrees::WorktreeOrigin::Adopted,
                label: None,
                reference: None,
            };
            let head = provenant_confinement::run_utility(
                context.executor,
                provenant_confinement::GitRead::RevParseHead,
                context.repo,
                context.utility,
                context.utility_budget,
                &cancel,
            )?;
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                base: String::from_utf8_lossy(&head.stdout.bytes).trim().into(),
                ..owner
            };
            provenant_confinement::worktrees::save_owner(
                &provenant_confinement::repo::verify_worktree(
                    context.repo,
                    &fixture.writer.worktree,
                )?,
                &owner,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    assert_eq!(
        fixture.writer.extra.get("custody_project"),
        Some(&serde_json::json!(execution.project_id))
    );
    Ok(execution)
}

#[derive(Clone, Copy)]
enum LandedWriterFenceCase {
    Terminal,
    Nonterminal,
    WrongScope,
    WrongWriter,
    WrongAttempt,
    MissingQuarantine,
}

fn landed_writer_fence_fixture(
    case: LandedWriterFenceCase,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_os::fs::Directory;
    use provenant_supervisor::custody::{self, WriterBinding};
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    let record = store.mark_starting(
        &store.record(id, &call)?,
        &tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    if !matches!(case, LandedWriterFenceCase::Nonterminal) {
        let mut exit = tests::exit();
        exit.scope = scope;
        tests::seal_observation(&store, &input, &exit)?;
        store.publish_terminal(id, record.revision, &tests::publication(), &exit, &call)?;
    }
    if !matches!(case, LandedWriterFenceCase::MissingQuarantine) {
        let archive = artifacts::subdir(&attempt, std::ffi::OsStr::new("quarantine"), true)?;
        artifacts::seal(
            &archive,
            "manifest.json",
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "scope": scope,
                "kind": "quarantine_manifest",
            }))?,
            RECORD_LIMIT,
        )?;
    }
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        &store.writer_registry,
        execution.project_id,
    )?;
    let binding = WriterBinding::from_verified(
        launch.worktree.repo().root_dir(),
        launch.worktree.repo().private_dir(),
        launch.branch.as_str(),
        launch.base,
    )?;
    let directory = launch.custody.take().ok_or("writer custody")?.0;
    drop(provenant_supervisor::fixture::acquire_writer_with_binding(
        directory,
        scope,
        binding,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let reservation = store
        .writer_registry
        .join("worktrees")
        .join(execution.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(
            &fixture.writer.worktree,
        ))
        .join(scope.attempt.to_string())
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let metadata = attempt.metadata()?;
    value["launch_state"] = "never_spawned".into();
    value["unresolved_cleanup"] = false.into();
    value["census_incomplete"] = false.into();
    value["attempt_device"] = metadata.device.into();
    value["attempt_inode"] = metadata.inode.into();
    match case {
        LandedWriterFenceCase::WrongScope => {
            value["scope"]["generation"] = (scope.generation + 1).into();
        }
        LandedWriterFenceCase::WrongWriter => {
            value["binding"]["branch"] = "refs/heads/other".into();
        }
        LandedWriterFenceCase::WrongAttempt => {
            value["attempt_inode"] = (metadata.inode + 1).into();
        }
        _ => {}
    }
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    let before = std::fs::read(&reservation)?;
    let recovery = custody::acquire_recovery(
        Directory::open_anchor(
            reservation
                .parent()
                .and_then(std::path::Path::parent)
                .ok_or("custody directory")?,
        )?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    let result = store.settle_landed_writer(recovery, &call);
    if matches!(case, LandedWriterFenceCase::Terminal) {
        result?;
        assert!(!reservation.exists());
        assert!(reservation.with_file_name("settlement.json").is_file());
        assert!(artifacts::read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_some());
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
    } else {
        assert!(result.is_err());
        assert_eq!(std::fs::read(&reservation)?, before);
        if !matches!(case, LandedWriterFenceCase::WrongAttempt) {
            assert!(artifacts::read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_none());
        }
    }
    assert!(fixture.writer.worktree.exists());
    Ok(())
}

mod host_commands;
mod integration;
mod prepare;
mod remote_delivery;
mod restart;
mod settlement;
