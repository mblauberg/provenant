
struct SourceViewFixture {
    _temp: tempfile::TempDir,
    base: std::path::PathBuf,
    project: std::path::PathBuf,
    git: std::path::PathBuf,
    roots: provenant_core::Roots,
    home: std::path::PathBuf,
    assets: std::path::PathBuf,
    project_id: provenant_core::ProjectId,
    cancel: provenant_core::CancelToken,
    counter: std::cell::Cell<u8>,
}

impl SourceViewFixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let state = base.join("state");
        let project = base.join("repo");
        let assets = base.join("assets");
        let git_home = base.join("git-home");
        for path in [&home, &state, &project, &assets, &git_home] {
            std::fs::create_dir(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let executor = provenant_os::exec::SystemExecutor::new(1)?;
        let git = init_fixture_repository(&executor, &project, &git_home)?;
        let roots = provenant_core::RootInputs {
            state_dir: Some(state),
            ..provenant_core::RootInputs::new(home.clone(), project.clone())
        }
        .resolve(&provenant_os::fs::SystemPathResolver)?;
        let utility = provenant_confinement::UtilityRuntime::new(&git, &git_home)?;
        let cancel = provenant_core::CancelToken::new();
        let repo = utility.open_repo(
            &executor,
            &project,
            provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(10))?,
            &cancel,
        )?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(repo.common_git_dir())?,
        )?;
        Ok(Self {
            _temp: temp,
            base,
            project,
            git,
            roots,
            home,
            assets,
            project_id: binding.project_id(),
            cancel,
            counter: std::cell::Cell::new(0),
        })
    }

    fn commit(&self, cwd: &Path, name: &str, body: &str) -> Result<String, Box<dyn std::error::Error>> {
        std::fs::write(cwd.join(name), body)?;
        provenant_os::fixture::run_git(cwd, &["add", name])?;
        provenant_os::fixture::run_git(cwd, &["commit", "-qm", name])?;
        Ok(run::resolve_git_base(&self.git, cwd, "HEAD")?)
    }

    fn snapshot(
        &self,
        source: &Path,
        head: &str,
    ) -> Result<PathBuf, super::source_tree::SourceViewError> {
        use provenant_core::{AttemptId, OperationId, RunId, WallTime};
        use std::os::unix::fs::PermissionsExt;
        let n = self.counter.get() + 1;
        self.counter.set(n);
        let registry = Registry {
            roots: Some(&self.roots),
            home: Some(&self.home),
            assets: Some(&self.assets),
            cwd: Some(&self.project),
            state_root: Some(self.roots.state.as_path()),
            project_id: Some(self.project_id),
            host: None,
            cancel: Some(&self.cancel),
            repo_facts: Default::default(),
        };
        let mut task = super::super::tests::trace_run(
            50,
            AttemptId::from_bytes([51; 16]),
            WallTime::from_micros(100).unwrap(),
        )
        .unwrap()
        .request
        .tasks
        .remove(0);
        task.head = Some(head.to_owned());
        task.base = Some(head.to_owned());
        let temporary = self.base.join(format!("tmp-{n}"));
        std::fs::create_dir(&temporary).unwrap();
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths = run::LaunchPaths {
            attempt_id: AttemptId::from_bytes([52; 16]),
            operation_id: OperationId::parse("source-view").unwrap(),
            task_index: 0,
            lane_depth: 1,
            run_id: RunId::from_bytes([53; 16]),
            deadline_unix_ms: u64::MAX,
            scratch: self.base.join("scratch"),
            provider_state: self.base.join("provider-state"),
            temporary,
            temporary_alias: None,
        };
        super::source_tree::snapshot_source(&registry, &task, &paths, source, &self.git)
    }
}

#[test]
fn source_view_builds_from_a_linked_worktree_behind_the_subject()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = SourceViewFixture::new()?;
    let first = fixture.commit(&fixture.project, "one.txt", "first\n")?;
    let linked = fixture.base.join("linked");
    provenant_os::fixture::run_git(
        &fixture.project,
        &["worktree", "add", "--detach", "-q", linked.to_str().ok_or("path")?, &first],
    )?;
    // The subject exists only in the shared object store, and as a branch
    // another worktree owns; the worktree's HEAD stays behind. Remote-tracking
    // refs resolve through the remote's advertisement, which needs a remote.
    let second = fixture.commit(&fixture.project, "two.txt", "second\n")?;
    provenant_os::fixture::run_git(&fixture.project, &["reset", "-q", "--hard", &first])?;
    provenant_os::fixture::run_git(
        &fixture.project,
        &["update-ref", "refs/heads/feature", &second],
    )?;
    let by_sha = fixture.snapshot(&linked, &second)?;
    assert_eq!(std::fs::read(by_sha.join("two.txt"))?, b"second\n");
    assert_eq!(std::fs::read(by_sha.join("one.txt"))?, b"first\n");
    let resolved = run::resolve_git_base(&fixture.git, &linked, "feature")?;
    assert_eq!(resolved, second);
    let by_ref = fixture.snapshot(&linked, &resolved)?;
    assert_eq!(std::fs::read(by_ref.join("two.txt"))?, b"second\n");
    // The worktree itself is untouched.
    assert!(!linked.join("two.txt").exists());
    Ok(())
}

#[test]
fn source_view_names_the_failing_step_and_cause() -> Result<(), Box<dyn std::error::Error>> {
    use super::source_tree::{SourceCause, SourceStep};
    let fixture = SourceViewFixture::new()?;
    fixture.commit(&fixture.project, "one.txt", "secret contents\n")?;
    let missing = "1".repeat(40);
    let error = fixture
        .snapshot(&fixture.project, &missing)
        .expect_err("an absent commit has no tree");
    assert_eq!(error.step, SourceStep::TreeListing);
    assert!(matches!(error.cause, SourceCause::GitExit(_)), "{error}");
    let refusal = PrepareError::from(error).into_refusal();
    assert_eq!(
        refusal.terminal_cause,
        Some(provenant_records::run::TerminalCause::SourceViewRefused)
    );
    let note = refusal.terminal_note.ok_or("note")?;
    assert!(note.contains("tree listing") && note.contains("Git exited with status"));
    assert!(!note.contains("secret"));
    assert!(
        !provenant_records::run::TerminalCause::SourceViewRefused
            .detail()
            .contains("route and account configuration")
    );
    Ok(())
}

#[test]
fn source_view_of_twenty_thousand_files_uses_batched_reads_within_budget()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = SourceViewFixture::new()?;
    for shard in 0..100 {
        let directory = fixture.project.join(format!("d{shard}"));
        std::fs::create_dir(&directory)?;
        for file in 0..200 {
            std::fs::write(directory.join(format!("f{file}.txt")), format!("{shard}/{file}\n"))?;
        }
    }
    provenant_os::fixture::run_git(&fixture.project, &["add", "-A"])?;
    provenant_os::fixture::run_git(&fixture.project, &["commit", "-qm", "many"])?;
    let head = run::resolve_git_base(&fixture.git, &fixture.project, "HEAD")?;
    use provenant_core::Clock;
    let clock = provenant_os::clock::SystemClock::new();
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 60)?;
    let view = fixture.snapshot(&fixture.project, &head)?;
    assert!(budget.remaining(clock.mono_now()).as_micros() > 0);
    assert_eq!(std::fs::read(view.join("d99/f199.txt"))?, b"99/199\n");
    Ok(())
}

#[test]
fn source_view_batch_reader_decodes_in_order_and_names_missing_objects() {
    let a: provenant_core::GitOid = "a".repeat(40).parse().unwrap();
    let b: provenant_core::GitOid = "b".repeat(40).parse().unwrap();
    let stream = format!("{a} blob 3\nabc\n{b} blob 0\n\n");
    let parsed = super::source_tree::parse_blob_batch(&[a, b], stream.as_bytes()).unwrap();
    assert_eq!(parsed, [b"abc".to_vec(), Vec::new()]);
    let missing = format!("{a} missing\n");
    let error = super::source_tree::parse_blob_batch(&[a], missing.as_bytes()).unwrap_err();
    assert_eq!(error.step, super::source_tree::SourceStep::ObjectRead);
    assert!(super::source_tree::parse_blob_batch(&[a], b"garbage").is_err());
}
