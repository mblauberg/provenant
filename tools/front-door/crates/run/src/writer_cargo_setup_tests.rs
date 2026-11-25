use super::*;

type FixtureResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct WriterFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    state: PathBuf,
    git_dir: PathBuf,
    git: PathBuf,
    custody_project: ProjectId,
    source: String,
}

impl WriterFixture {
    fn new() -> FixtureResult<Self> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let home = root.join("home");
        let state = root.join("state");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&home)?;
        provenant_os::fixture::private_state_dir(&state)?;
        let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
        let git_dir = project.join(".git");
        let run = |args: &[&str]| -> FixtureResult<String> {
            let output = provenant_os::fixture::git_command_at(&git)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_DIR", &git_dir)
                .env("GIT_WORK_TREE", &project)
                .current_dir(&project)
                .args(args)
                .output()?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
            }
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        run(&["init", "-q"])?;
        std::fs::write(project.join("old name.txt"), "tracked")?;
        run(&["add", "old name.txt"])?;
        run(&["commit", "--allow-empty", "-qm", "initial"])?;
        let mut roots = provenant_core::RootInputs::new(home, project.clone());
        roots.state_dir = Some(state.clone());
        let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
        let custody_project = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(git_dir.clone())?,
        )?
        .project_id();
        let source = run(&["rev-parse", "HEAD"])?;
        Ok(Self {
            _temp: temp,
            root,
            project,
            state,
            git_dir,
            git,
            custody_project,
            source,
        })
    }

    fn run_git(&self, args: &[&str]) -> FixtureResult<String> {
        let output = provenant_os::fixture::git_command_at(&self.git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.project)
            .current_dir(&self.project)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn lane_git(&self, lane: &Path, args: &[&str]) -> FixtureResult<()> {
        let output = provenant_os::fixture::git_command_at(&self.git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(lane)
            .args(args)
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned().into())
        }
    }

    fn add_worktree(&self, branch: &str, name: &str) -> FixtureResult<PathBuf> {
        let lane = self.root.join(name);
        self.run_git(&[
            "worktree",
            "add",
            "-qb",
            branch,
            lane.to_str().ok_or("lane path")?,
        ])?;
        Ok(lane)
    }

    fn request(&self, lane: &Path) -> WriterRequest {
        WriterRequest {
            extra: Default::default(),
            custody_project: Some(self.custody_project),
            project: self.project.clone(),
            worktree: lane.to_path_buf(),
        }
    }

    fn prepare(&self, lane: &Path) -> FixtureResult<WriterLaunch> {
        Ok(prepare_writer_with_facts(
            &self.request(lane),
            &self.state,
            self.custody_project,
            &worktrees::RepoFacts::default(),
        )?)
    }

    fn prepare_git(&self, writer: &mut WriterLaunch) -> FixtureResult<()> {
        prepare_writer_git(
            writer,
            &self.state,
            &self.root,
            None,
            &isolated_lane_access_receipt(),
            &CancelToken::new(),
            &provenant_records::policy::Policy::default(),
            (None, None),
        )?;
        Ok(())
    }
}

fn clean_environment_child(test_filter: &str) -> FixtureResult<bool> {
    const CHILD: &str = "PROVENANT_WRITER_CONFIG_FIXTURE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return Ok(false);
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    #[allow(
        clippy::disallowed_methods,
        reason = "the test re-runs itself in a clean home"
    )]
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([test_filter, "--nocapture"])
        .current_dir(temp.path())
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(CHILD, "1")
        .status()?;
    assert!(status.success());
    Ok(true)
}

#[test]
fn terminal_forge_remote_follows_the_checkout_not_a_compiled_origin() {
    let fixture = WriterFixture::new().unwrap();
    fixture
        .run_git(&[
            "remote",
            "add",
            "upstream",
            fixture.root.join("upstream.git").to_str().unwrap(),
        ])
        .unwrap();
    assert_eq!(
        terminal_forge_remote(&fixture.state, &fixture.project),
        Some(provenant_config::RemoteName::new("upstream").unwrap())
    );
}

/// A checkout without a remote yields no name at all. Flattening that
/// absence to `""` is the defect class that crashed `git status` on Git
/// 2.53 and named `refs/remotes//`.
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
#[test]
fn terminal_forge_remote_reports_an_absent_remote_as_none() {
    let fixture = WriterFixture::new().unwrap();
    assert_eq!(
        terminal_forge_remote(&fixture.state, &fixture.project),
        None
    );
}

fn oid(digit: char) -> GitOid {
    GitOid::parse(&digit.to_string().repeat(40)).unwrap()
}

fn refs(entries: &[(&str, char)]) -> std::collections::BTreeMap<String, GitOid> {
    entries
        .iter()
        .map(|(name, digit)| ((*name).to_owned(), oid(*digit)))
        .collect()
}

fn primary(head_ref: &str, head: char, entries: &[(&str, char)]) -> crate::objects::PrimaryRefs {
    crate::objects::PrimaryRefs {
        head_ref: Some(head_ref.to_owned()),
        head: Some(oid(head)),
        refs: refs(entries),
    }
}

const MAIN: &str = "refs/heads/main";
const ORIGIN_MAIN: &str = "refs/remotes/origin/main";

// Spec: docs/specs/run-settlement.md#stray-lane-work
#[test]
fn a_branch_that_did_not_exist_at_launch_is_stray_work() {
    let launch = refs(&[(MAIN, 'a'), (ORIGIN_MAIN, 'a')]);
    let worktree = Path::new("/repo/.worktrees/lane");
    let on_new = primary(
        "refs/heads/stray",
        'a',
        &[(MAIN, 'a'), ("refs/heads/stray", 'a'), (ORIGIN_MAIN, 'a')],
    );
    let plan = StrayPlan::new(MAIN, Some(MAIN), &launch, &on_new);
    assert_eq!(plan.new_branch.as_deref(), Some("stray"));
    let found = plan
        .report("/repo", worktree, &[])
        .expect("sitting on a new branch is a finding with no commits at all");
    for fragment in [
        "/repo",
        "branch stray",
        "did not exist at launch",
        "/repo/.worktrees/lane",
    ] {
        assert!(found.note.contains(fragment), "{fragment}: {}", found.note);
    }
    // A new branch left behind with nothing on it, and not checked out, is no work.
    let left = primary(
        MAIN,
        'a',
        &[(MAIN, 'a'), ("refs/heads/stray", 'a'), (ORIGIN_MAIN, 'a')],
    );
    let plan = StrayPlan::new(MAIN, Some(MAIN), &launch, &left);
    assert_eq!(plan.new_branch, None);
    assert_eq!(plan.report("/repo", worktree, &[]), None);
    // The branch the primary sat on at launch is never new, even when unborn.
    let unborn = primary(MAIN, 'a', &[]);
    assert_eq!(
        StrayPlan::new(
            MAIN,
            Some(MAIN),
            &std::collections::BTreeMap::new(),
            &unborn
        )
        .new_branch,
        None
    );
}

// Spec: docs/specs/run-settlement.md#stray-lane-work
#[test]
fn commits_on_new_or_advanced_branches_are_candidates_even_after_switching_back() {
    let launch = refs(&[(MAIN, 'a'), (ORIGIN_MAIN, 'a')]);
    let switched_back = primary(
        MAIN,
        'a',
        &[(MAIN, 'a'), ("refs/heads/stray", 'b'), (ORIGIN_MAIN, 'a')],
    );
    let plan = StrayPlan::new(MAIN, Some(MAIN), &launch, &switched_back);
    assert_eq!(plan.new_branch, None);
    assert!(
        plan.tips.contains(&oid('b')),
        "the new branch tip is asked about"
    );
    assert!(
        !plan.excluded.contains(&oid('b')),
        "nothing accounts for it"
    );
    let found = plan
        .report("/repo", Path::new("/w"), &[oid('b')])
        .expect("commits outside every known ref are a finding");
    assert!(found.note.contains("1 commit(s)"), "{}", found.note);
    assert!(found.note.contains("stray"), "{}", found.note);
    let advanced = primary(MAIN, 'c', &[(MAIN, 'c'), (ORIGIN_MAIN, 'a')]);
    let plan = StrayPlan::new(MAIN, Some(MAIN), &launch, &advanced);
    assert!(plan.tips.contains(&oid('c')));
    assert!(plan.tip_names.iter().any(|name| name == "main"));
}

// Spec: docs/specs/run-settlement.md#stray-lane-work
#[test]
fn a_pull_fast_forward_is_not_stray_work() {
    let launch = refs(&[(MAIN, 'a'), (ORIGIN_MAIN, 'a')]);
    // The fetch moved origin/main and the merge carried main to it.
    let pulled = primary(
        MAIN,
        'b',
        &[
            (MAIN, 'b'),
            (ORIGIN_MAIN, 'b'),
            ("refs/remotes/origin/new", 'c'),
        ],
    );
    let plan = StrayPlan::new(MAIN, Some(MAIN), &launch, &pulled);
    assert_eq!(plan.new_branch, None);
    assert!(
        plan.excluded.contains(&oid('b')),
        "the current tip of a remote-tracking ref that existed at launch accounts for it"
    );
    assert!(
        !plan.excluded.contains(&oid('c')),
        "a remote-tracking ref created after launch accounts for nothing"
    );
    assert!(
        !plan.tips.contains(&oid('c')),
        "remote-tracking refs are never the lane's work"
    );
}

// Spec: docs/specs/run-settlement.md#stray-lane-work
#[test]
fn a_bound_branch_commit_turns_a_primary_move_into_a_warning() {
    const WRITER: &str = "refs/heads/chore/writer";
    let launch = refs(&[(MAIN, 'a'), (WRITER, 'a'), (ORIGIN_MAIN, 'a')]);
    // Another process detaches the primary at a commit no launch ref reaches,
    // while the lane commits on the branch it was bound to (#3943).
    let moved = primary(
        "refs/heads/main",
        'b',
        &[(MAIN, 'b'), (WRITER, 'c'), (ORIGIN_MAIN, 'a')],
    );
    let plan = StrayPlan::new(WRITER, Some(MAIN), &launch, &moved);
    assert!(plan.bound_branch_advanced);
    let found = plan
        .report("/repo", Path::new("/repo/.worktrees/writer"), &[oid('b')])
        .expect("an unaccounted primary move is still observed");
    // Warn leaves the Succeeded outcome untouched and emits a typed warning;
    // only Review holds the lane and its dependants for the chair.
    let note = match found.disposition() {
        StrayDisposition::Warn { note } => note,
        StrayDisposition::Review { note } => {
            panic!("the lane committed on its bound branch: {note}")
        }
    };
    assert!(
        note.contains(&oid('b').to_string()),
        "the warning names the new primary HEAD: {note}"
    );
    assert!(note.contains("(main)"), "it names the HEAD's ref: {note}");

    // With no bound-branch commits the same move is still held for review: the
    // work may be in the primary checkout (#3611).
    let stalled = primary(
        "refs/heads/main",
        'b',
        &[(MAIN, 'b'), (WRITER, 'a'), (ORIGIN_MAIN, 'a')],
    );
    let plan = StrayPlan::new(WRITER, Some(MAIN), &launch, &stalled);
    assert!(!plan.bound_branch_advanced);
    let found = plan
        .report("/repo", Path::new("/repo/.worktrees/writer"), &[oid('b')])
        .expect("an unaccounted primary move is a finding");
    assert!(
        matches!(found.disposition(), StrayDisposition::Review { note } if note.contains("stray work")),
        "a lane whose bound branch stayed at base is still held"
    );
}

// Spec: docs/specs/run-settlement.md#stray-lane-work
#[test]
fn git_separates_a_pull_and_lane_commits_from_work_left_in_the_primary_checkout()
-> FixtureResult<()> {
    if clean_environment_child(stringify!(
        git_separates_a_pull_and_lane_commits_from_work_left_in_the_primary_checkout
    ))? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    let git_dir = root.join("metadata/project.git");
    let caller = root.join("caller");
    let writer = project.join(".worktrees/writer");
    let origin = root.join("origin.git");
    let other = root.join("other");
    let home = root.join("home");
    let state = root.join("state");
    for path in [
        &project,
        git_dir.parent().ok_or("metadata parent")?,
        writer.parent().ok_or("writer parent")?,
        &home,
    ] {
        std::fs::create_dir_all(path)?;
    }
    provenant_os::fixture::private_state_dir(&state)?;
    let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
    let run_git = |cwd: &Path, args: &[&str]| -> FixtureResult<String> {
        let output = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(cwd)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    let path_text = |path: &Path| -> FixtureResult<String> {
        Ok(path.to_str().ok_or("fixture path")?.to_owned())
    };
    run_git(
        &root,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            &path_text(&git_dir)?,
            &path_text(&project)?,
        ],
    )?;
    run_git(&project, &["config", "user.name", "Fixture"])?;
    run_git(
        &project,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    run_git(&project, &["commit", "--allow-empty", "-qm", "initial"])?;
    let main = run_git(&project, &["symbolic-ref", "--short", "HEAD"])?;
    run_git(&root, &["init", "-q", "--bare", &path_text(&origin)?])?;
    run_git(&project, &["remote", "add", "origin", &path_text(&origin)?])?;
    run_git(&project, &["push", "-q", "origin", &main])?;
    run_git(&project, &["fetch", "-q", "origin"])?;
    for (branch, path) in [("chore/caller", &caller), ("chore/writer", &writer)] {
        run_git(
            &project,
            &["worktree", "add", "-qb", branch, &path_text(path)?],
        )?;
    }
    let mut roots = provenant_core::RootInputs::new(home.clone(), project.clone());
    roots.state_dir = Some(state.clone());
    let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = UtilityRuntime::read_only(&git, &state)?;
    let executor = SystemExecutor::new(1)?;
    let discovered =
        utility.project_location(&executor, &project, budget()?, &CancelToken::new())?;
    let binding = provenant_config::bind_project(&roots, discovered.location)?;
    let request = resolve_writer(&state, &caller, Path::new(".worktrees/writer"))?;
    let launch = prepare_writer(&request, &state, binding.project_id())?;

    let (checkout, at_launch) = launch.primary_state(&git)?.ok_or("primary state")?;
    assert_eq!(checkout, project.display().to_string());
    let (launch_head, launch_refs) = (at_launch.head_ref, at_launch.refs);
    let origin_main = format!("refs/remotes/origin/{main}");
    assert!(launch_refs.contains_key(&origin_main), "{launch_refs:?}");
    let assess = || -> FixtureResult<Option<StrayWork>> {
        Ok(launch.assess_primary(&git, launch_head.as_deref(), &launch_refs)?)
    };
    assert_eq!(assess()?, None, "nothing has happened yet");

    // A commit in the bound worktree is the lane doing its job.
    run_git(&writer, &["commit", "--allow-empty", "-qm", "lane work"])?;
    assert_eq!(assess()?, None, "the bound branch is accounted for");

    // The chair pulls: origin/main and main both move, and nothing is stray.
    run_git(
        &root,
        &["clone", "-q", &path_text(&origin)?, &path_text(&other)?],
    )?;
    run_git(
        &other,
        &[
            "-c",
            "user.name=Other",
            "-c",
            "user.email=other@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "upstream",
        ],
    )?;
    run_git(&other, &["push", "-q", "origin", &main])?;
    run_git(&project, &["fetch", "-q", "origin"])?;
    run_git(
        &project,
        &["merge", "-q", "--ff-only", &format!("origin/{main}")],
    )?;
    assert_eq!(
        assess()?,
        None,
        "a pull fast-forward is not the lane's work"
    );

    // A commit made directly in the primary checkout is observed. The lane has
    // committed on its bound branch above, so settlement warns rather than
    // failing: the move cannot be attributed to the lane (#3943).
    run_git(&project, &["commit", "--allow-empty", "-qm", "stray"])?;
    let found = assess()?.ok_or("a primary commit is a finding")?;
    assert!(found.note.contains("1 commit(s)"), "{}", found.note);
    assert!(found.note.contains(&main), "{}", found.note);
    assert!(found.bound_branch_advanced);
    assert!(
        matches!(found.disposition(), StrayDisposition::Warn { .. }),
        "the bound branch advanced, so the primary move is a warning"
    );
    run_git(
        &project,
        &["reset", "-q", "--hard", &format!("origin/{main}")],
    )?;
    assert_eq!(assess()?, None, "back on the fetched tip");

    // A branch the primary sits on that did not exist at launch is.
    run_git(&project, &["checkout", "-qb", "stray"])?;
    let found = assess()?.ok_or("a new branch is a finding")?;
    assert!(found.note.contains("branch stray"), "{}", found.note);

    // Commits on it are found even after switching back.
    run_git(
        &project,
        &["commit", "--allow-empty", "-qm", "stray on branch"],
    )?;
    run_git(&project, &["checkout", "-q", &main])?;
    let found = assess()?.ok_or("commits on a new branch are a finding")?;
    assert!(found.note.contains("1 commit(s)"), "{}", found.note);
    assert!(found.note.contains("stray"), "{}", found.note);
    Ok(())
}

// Spec: docs/specs/run-git.md#project-layout-ownership
#[test]
fn source_and_writer_resolution_use_the_registered_primary_with_separate_git_dir()
-> FixtureResult<()> {
    if clean_environment_child(stringify!(
        source_and_writer_resolution_use_the_registered_primary_with_separate_git_dir
    ))? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    let git_dir = root.join("unrelated/metadata/project.git");
    let caller = root.join("caller");
    let writer = project.join(".worktrees/writer");
    let home = root.join("home");
    let state = root.join("state");
    for path in [
        &project,
        git_dir.parent().ok_or("metadata parent")?,
        writer.parent().ok_or("writer parent")?,
        &home,
    ] {
        std::fs::create_dir_all(path)?;
    }
    provenant_os::fixture::private_state_dir(&state)?;
    let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
    let run_git = |cwd: &Path, args: &[&str]| -> FixtureResult<String> {
        let output = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(cwd)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    run_git(
        &root,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            git_dir.to_str().ok_or("git dir path")?,
            project.to_str().ok_or("project path")?,
        ],
    )?;
    run_git(&project, &["config", "user.name", "Fixture"])?;
    run_git(
        &project,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    run_git(&project, &["commit", "--allow-empty", "-qm", "initial"])?;
    run_git(
        &project,
        &[
            "worktree",
            "add",
            "-qb",
            "chore/caller",
            caller.to_str().ok_or("caller path")?,
        ],
    )?;
    run_git(
        &project,
        &[
            "worktree",
            "add",
            "-qb",
            "chore/writer",
            writer.to_str().ok_or("writer path")?,
        ],
    )?;

    let mut roots = provenant_core::RootInputs::new(home, project.clone());
    roots.state_dir = Some(state.clone());
    let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = UtilityRuntime::read_only(&git, &state)?;
    let executor = SystemExecutor::new(1)?;
    let discovered =
        utility.project_location(&executor, &project, budget()?, &CancelToken::new())?;
    let binding = provenant_config::bind_project(&roots, discovered.location)?;

    let request = resolve_writer(&state, &caller, Path::new(".worktrees/writer"))?;
    assert_eq!(request.project, caller);
    assert_eq!(request.worktree, writer);
    assert_eq!(
        resolve_reader_source(&state, &caller, Path::new(".worktrees/writer"))?,
        writer
    );
    assert_eq!(binding.common_dir(), git_dir);
    let launch = prepare_writer(&request, &state, binding.project_id())?;
    assert_eq!(launch.project_root(), Some(project.as_path()));
    assert_eq!(launch.managed_root(), project);

    let primary_bin = project.join("tools");
    let linked_bin = caller.join("tools");
    let safe_bin = root.join("safe-tools");
    for path in [&primary_bin, &linked_bin, &safe_bin] {
        std::fs::create_dir_all(path)?;
    }
    let path = std::env::join_paths([&primary_bin, &linked_bin, &safe_bin])?;
    let excluded = launch
        .lane_path_roots()
        .into_iter()
        .chain(std::iter::once(launch.worktree.path()))
        .collect::<Vec<_>>();
    let filtered = crate::FilteredLanePath::from_path_excluding_roots(&path, &excluded)?;
    assert_eq!(
        std::env::split_paths(filtered.path()).collect::<Vec<_>>(),
        vec![safe_bin]
    );
    Ok(())
}

// Spec: docs/specs/run-git.md#project-layout-ownership
#[test]
fn bare_backed_writer_uses_common_layout_and_verified_linked_executor() -> FixtureResult<()> {
    if clean_environment_child(stringify!(
        bare_backed_writer_uses_common_layout_and_verified_linked_executor
    ))? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let seed = root.join("seed");
    let common = root.join("metadata/project.git");
    let source = root.join("source");
    let writer_path = common.join(".worktrees/lane");
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(&seed)?;
    std::fs::create_dir_all(common.parent().ok_or("common parent")?)?;
    std::fs::create_dir(&home)?;
    provenant_os::fixture::private_state_dir(&state)?;
    let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
    let run_git = |cwd: &Path, args: &[&str]| -> FixtureResult<String> {
        let output = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(cwd)
            .args(args)
            .output()?;
        if output.status.success() {
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned().into())
        }
    };
    run_git(
        &root,
        &[
            "init",
            "-q",
            "--initial-branch=main",
            seed.to_str().ok_or("seed path")?,
        ],
    )?;
    run_git(&seed, &["config", "user.name", "Fixture"])?;
    run_git(&seed, &["config", "user.email", "fixture@example.invalid"])?;
    std::fs::write(seed.join("README.md"), "bare layout\n")?;
    run_git(&seed, &["add", "README.md"])?;
    run_git(&seed, &["commit", "-qm", "initial"])?;
    run_git(
        &root,
        &[
            "clone",
            "--bare",
            seed.to_str().ok_or("seed path")?,
            common.to_str().ok_or("common path")?,
        ],
    )?;
    run_git(
        &root,
        &[
            "--git-dir",
            common.to_str().ok_or("common path")?,
            "worktree",
            "add",
            source.to_str().ok_or("source path")?,
            "main",
        ],
    )?;
    std::fs::create_dir_all(writer_path.parent().ok_or("writer parent")?)?;
    run_git(
        &root,
        &[
            "--git-dir",
            common.to_str().ok_or("common path")?,
            "worktree",
            "add",
            "-b",
            "lane",
            writer_path.to_str().ok_or("writer path")?,
        ],
    )?;

    let mut roots = provenant_core::RootInputs::new(home, source.clone());
    roots.state_dir = Some(state.clone());
    let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(common.clone())?,
    )?;
    let request = resolve_writer(&state, &source, Path::new(".worktrees/lane"))?;
    assert_eq!(request.project, source);
    assert_eq!(request.worktree, writer_path);
    assert_eq!(
        resolve_reader_source(&state, &source, Path::new("."))?,
        source
    );
    let launch = prepare_writer(&request, &state, binding.project_id())?;
    assert_eq!(launch.project_root(), None);
    assert_eq!(launch.managed_root(), common);
    assert_eq!(launch.lane_path_roots(), vec![source.as_path()]);
    Ok(())
}

#[test]
fn writer_source_binding_rejects_unrelated_lineage() -> FixtureResult<()> {
    if clean_environment_child(stringify!(writer_source_binding_rejects_unrelated_lineage))? {
        return Ok(());
    }
    let fixture = WriterFixture::new()?;
    let lane = fixture.add_worktree("chore/source", "lane")?;
    let mut writer = fixture.prepare(&lane)?;
    let source = GitOid::parse(&fixture.source)?;
    writer.require_source(source, &provenant_records::policy::Policy::default())?;

    let tree = fixture.run_git(&["rev-parse", "HEAD^{tree}"])?;
    let unrelated =
        GitOid::parse(&fixture.run_git(&["commit-tree", &tree, "-m", "unrelated root"])?)?;
    assert!(matches!(
        writer.require_source(unrelated, &provenant_records::policy::Policy::default()),
        Err(RunError::WriterSourceMismatch { expected, actual })
            if expected == unrelated && actual == source
    ));

    let child = GitOid::parse(&fixture.run_git(&[
        "commit-tree",
        &tree,
        "-p",
        &fixture.source,
        "-m",
        "child commit",
    ])?)?;
    writer.base = child;
    let utility = runtime(&fixture.state)?;
    writer.require_continuation_source(
        utility.git_program_path(),
        source,
        child,
        &provenant_records::policy::Policy::default(),
    )?;
    assert!(matches!(
        writer.require_continuation_source(utility.git_program_path(), unrelated, child, &provenant_records::policy::Policy::default()),
        Err(RunError::WriterSourceMismatch { expected, actual })
            if expected == unrelated && actual == child
    ));
    assert!(matches!(
        writer.require_continuation_source(utility.git_program_path(), source, source, &provenant_records::policy::Policy::default()),
        Err(RunError::WriterSourceMismatch { expected, actual })
            if expected == source && actual == child
    ));
    Ok(())
}

#[test]
fn retry_repins_a_descendant_head_and_refuses_an_unrelated_one() -> FixtureResult<()> {
    if clean_environment_child(stringify!(
        retry_repins_a_descendant_head_and_refuses_an_unrelated_one
    ))? {
        return Ok(());
    }
    let fixture = WriterFixture::new()?;
    let lane = fixture.add_worktree("chore/repin", "lane")?;
    let mut writer = fixture.prepare(&lane)?;
    let source = GitOid::parse(&fixture.source)?;
    let utility = runtime(&fixture.state)?;
    let git = utility.git_program_path();

    assert_eq!(
        writer.repin_head(git, source)?,
        RepinOutcome::Unchanged { head: source }
    );

    // A commit authored in the worktree descends from the pin.
    let tree = fixture.run_git(&["rev-parse", "HEAD^{tree}"])?;
    let child = GitOid::parse(&fixture.run_git(&[
        "commit-tree",
        &tree,
        "-p",
        &fixture.source,
        "-m",
        "writer own commit",
    ])?)?;
    writer.base = child;
    let outcome = writer.repin_head(git, source)?;
    assert_eq!(
        outcome,
        RepinOutcome::Repinned {
            pinned: source,
            head: child
        }
    );
    let warning = outcome.warning().expect("warning");
    assert!(warning.contains(&source.to_string()), "{warning}");
    assert!(warning.contains(&child.to_string()), "{warning}");

    // An unrelated head must be refused, naming both ids.
    let unrelated = GitOid::parse(&fixture.run_git(&["commit-tree", &tree, "-m", "unrelated"])?)?;
    writer.base = unrelated;
    let outcome = writer.repin_head(git, source)?;
    assert_eq!(
        outcome,
        RepinOutcome::NotDescendant {
            pinned: source,
            head: unrelated
        }
    );
    let refusal = outcome.refusal().expect("refusal");
    assert!(refusal.contains(&source.to_string()), "{refusal}");
    assert!(refusal.contains(&unrelated.to_string()), "{refusal}");
    Ok(())
}

#[test]
fn wip_refuses_branch_changed_at_the_same_head() -> FixtureResult<()> {
    if clean_environment_child(stringify!(wip_refuses_branch_changed_at_the_same_head))? {
        return Ok(());
    }
    let fixture = WriterFixture::new()?;
    let lane = fixture.add_worktree("chore/original", "lane")?;
    let writer = fixture.prepare(&lane)?;
    std::fs::write(lane.join("retained.txt"), "draft")?;
    let paths = writer.uncommitted_paths(&fixture.git)?;
    fixture.lane_git(&lane, &["checkout", "-qb", "chore/other"])?;
    let error = writer
        .commit_wip(&fixture.git, "WIP: test", &paths)
        .expect_err("branch change must refuse");
    assert!(error.to_string().contains("branch changed"), "{error}");
    assert_eq!(std::fs::read_to_string(lane.join("retained.txt"))?, "draft");
    let head = provenant_os::fixture::git_command_at(&fixture.git)
        .current_dir(&lane)
        .args(["rev-parse", "HEAD"])
        .output()?;
    assert!(head.status.success());
    assert_eq!(String::from_utf8(head.stdout)?.trim(), fixture.source);
    Ok(())
}

#[test]
fn writer_reports_uncommitted_paths_independent_of_git_output_order() -> FixtureResult<()> {
    if clean_environment_child(stringify!(
        writer_reports_uncommitted_paths_independent_of_git_output_order
    ))? {
        return Ok(());
    }
    let fixture = WriterFixture::new()?;
    let lane = fixture.add_worktree("chore/paths", "lane")?;
    let writer = fixture.prepare(&lane)?;
    let utility = runtime(&fixture.state)?;
    assert!(
        writer
            .uncommitted_paths(utility.git_program_path())?
            .is_empty()
    );

    fixture.lane_git(&lane, &["mv", "old name.txt", "renamed.txt"])?;
    std::fs::write(lane.join("café.txt"), "untracked")?;
    let observed: std::collections::BTreeSet<_> = writer
        .uncommitted_paths(utility.git_program_path())?
        .into_iter()
        .collect();
    let expected = ["renamed.txt".to_owned(), "café.txt".to_owned()]
        .into_iter()
        .collect();
    assert_eq!(observed, expected);
    Ok(())
}

#[test]
fn writer_git_preparation_restores_packed_branch_and_upstream() -> FixtureResult<()> {
    if clean_environment_child(stringify!(
        writer_git_preparation_restores_packed_branch_and_upstream
    ))? {
        return Ok(());
    }
    let fixture = WriterFixture::new()?;
    let branch = "refs/heads/chore/x";
    let lane = fixture.add_worktree("chore/x", "lane")?;
    let mut writer = fixture.prepare(&lane)?;
    let expected = fixture.run_git(&["rev-parse", branch])?;
    fixture.run_git(&["pack-refs", "--all"])?;
    let ref_parent = fixture.git_dir.join("refs/heads/chore");
    if ref_parent.exists() {
        std::fs::remove_dir(&ref_parent)?;
    }
    assert!(!fixture.git_dir.join(branch).exists());
    assert!(!ref_parent.exists());
    fixture.run_git(&[
        "remote",
        "add",
        "origin",
        fixture.project.to_str().ok_or("project path")?,
    ])?;

    fixture.prepare_git(&mut writer)?;

    assert_eq!(fixture.run_git(&["rev-parse", branch])?, expected);
    assert_eq!(
        std::fs::read_to_string(fixture.git_dir.join(branch))?.trim(),
        expected
    );
    assert_eq!(
        fixture.run_git(&["config", "--local", "--get", "branch.chore/x.remote"])?,
        "origin"
    );
    assert_eq!(
        fixture.run_git(&["config", "--local", "--get", "branch.chore/x.merge"])?,
        branch
    );
    Ok(())
}

#[test]
fn writer_git_preparation_refuses_symlinked_reflog_paths() -> FixtureResult<()> {
    if clean_environment_child(stringify!(
        writer_git_preparation_refuses_symlinked_reflog_paths
    ))? {
        return Ok(());
    }
    let fixture = WriterFixture::new()?;
    // The tracked reflog path now follows the resolved forge remote, so
    // declare the sole remote that makes that path origin's.
    // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    fixture.run_git(&[
        "remote",
        "add",
        "origin",
        fixture.project.to_str().ok_or("project path")?,
    ])?;
    let lane = fixture.add_worktree("chore/x", "lane")?;
    let mut writer = fixture.prepare(&lane)?;
    let target = fixture.root.join("reflog-target");
    std::fs::write(&target, b"untouched")?;
    let reflog = fixture.git_dir.join("logs/refs/remotes/origin/chore/x");
    std::fs::create_dir_all(reflog.parent().ok_or("reflog parent")?)?;
    std::os::unix::fs::symlink(&target, &reflog)?;
    let failure = fixture
        .prepare_git(&mut writer)
        .err()
        .ok_or("leaf symlink refusal missing")?;
    assert!(matches!(
        failure.downcast_ref::<RunError>(),
        Some(RunError::OperationIo { path: Some(path), .. }) if path == &reflog
    ));
    assert_eq!(std::fs::read(&target)?, b"untouched");

    std::fs::remove_file(&reflog)?;
    let parent = reflog.parent().ok_or("reflog parent")?;
    std::fs::remove_dir(parent)?;
    std::os::unix::fs::symlink(&fixture.root, parent)?;
    let failure = fixture
        .prepare_git(&mut writer)
        .err()
        .ok_or("parent symlink refusal missing")?;
    assert!(matches!(
        failure.downcast_ref::<RunError>(),
        Some(RunError::OperationIo { path: Some(path), .. }) if path == parent
    ));
    assert_eq!(std::fs::read(&target)?, b"untouched");
    Ok(())
}

#[test]
fn writer_git_preparation_preserves_existing_ref_locks() -> FixtureResult<()> {
    if clean_environment_child(stringify!(
        writer_git_preparation_preserves_existing_ref_locks
    ))? {
        return Ok(());
    }
    let fixture = WriterFixture::new()?;
    let lane = fixture.add_worktree("chore/x", "lane")?;
    let mut writer = fixture.prepare(&lane)?;
    fixture.run_git(&[
        "remote",
        "add",
        "origin",
        fixture.project.to_str().ok_or("project path")?,
    ])?;
    let tracking_lock = fixture.git_dir.join("refs/remotes/origin/chore/x.lock");
    std::fs::create_dir_all(tracking_lock.parent().ok_or("tracking lock parent")?)?;
    std::fs::write(&tracking_lock, b"held")?;
    fixture.prepare_git(&mut writer)?;
    assert_eq!(std::fs::read(&tracking_lock)?, b"held");

    std::fs::remove_file(&tracking_lock)?;
    fixture.run_git(&["remote", "remove", "origin"])?;
    let branch_lock = fixture.git_dir.join("refs/heads/chore/x.lock");
    std::fs::write(&branch_lock, b"held")?;
    fixture.prepare_git(&mut writer)?;
    assert_eq!(std::fs::read(&branch_lock)?, b"held");
    Ok(())
}

#[test]
fn toolchain_probe_accepts_only_declared_path_entries() {
    use provenant_confinement::permissions::{PathScope, WriteGrant, WriteSource};
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let worktree = tmp.path().join("worktree");
    let scratch = tmp.path().join("scratch");
    let bin = home.join(".cargo/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(home.join(".rustup")).unwrap();
    std::fs::create_dir(&worktree).unwrap();
    std::fs::create_dir_all(scratch.join("bin")).unwrap();
    std::fs::create_dir_all(scratch.join(provenant_os::admission::SHIM_DIRECTORY)).unwrap();
    std::fs::create_dir(scratch.join("other")).unwrap();
    std::fs::write(bin.join("rustup"), b"fixture").unwrap();
    std::os::unix::fs::symlink("rustup", bin.join("rustc")).unwrap();
    let grants = [WriteGrant {
        source: WriteSource::Scratch,
        path: scratch.clone(),
        scope: PathScope::Subtree,
    }];
    let writable_home = WriteGrant {
        source: WriteSource::WriteRoot,
        path: home.join(".rustup"),
        scope: PathScope::Subtree,
    };
    let cases = [
        ("immutable host", vec![bin.clone()], &[][..], Ok(())),
        (
            "scratch shim",
            vec![scratch.join("bin"), bin.clone()],
            &grants[..],
            Ok(()),
        ),
        (
            "scratch cargo shim",
            vec![
                scratch.join(provenant_os::admission::SHIM_DIRECTORY),
                bin.clone(),
            ],
            &grants[..],
            Ok(()),
        ),
        (
            "worktree",
            vec![worktree.clone(), bin.clone()],
            &grants[..],
            Err(codes::PATH_ENTRY_WRITABLE),
        ),
        (
            "other scratch directory",
            vec![scratch.join("other"), bin.clone()],
            &grants[..],
            Err(codes::PATH_ENTRY_WRITABLE),
        ),
    ];
    for (entry, path_entries, path_grants, expected) in cases {
        let path = std::env::join_paths(path_entries).unwrap();
        let env = vec![(OsString::from("PATH"), path)];
        assert_eq!(
            trusted_lane_toolchain_diagnostic(&home, &worktree, &env, path_grants),
            expected,
            "{entry}"
        );
    }
    assert_eq!(
        trusted_lane_toolchain_diagnostic(
            &home,
            &worktree,
            &[(OsString::from("PATH"), bin.as_os_str().to_owned())],
            &[writable_home]
        ),
        Err(codes::RUSTUP_HOME_WRITABLE)
    );
    std::fs::write(scratch.join("bin/rustc"), b"untrusted").unwrap();
    let scratch_path =
        std::env::join_paths([scratch.join("bin").as_path(), bin.as_path()]).unwrap();
    let scratch_env = vec![(OsString::from("PATH"), scratch_path)];
    assert_eq!(
        trusted_lane_toolchain_diagnostic(&home, &worktree, &scratch_env, &grants),
        Err(codes::RUSTC_NOT_RUSTUP_SHIM)
    );
}
