use crate::repo::VerifiedRepo;
use crate::utility::{
    GitRead, UtilityBudget, UtilityError, UtilityRuntime, exec_options, run_utility,
};
use provenant_core::{CancelToken, GitOid};
use provenant_supervisor::exec::{ExecOutcome, Executor, SystemExecutor, Termination};
use provenant_supervisor::fs::{LocalFs, SafeFs};
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn tempdir() -> io::Result<tempfile::TempDir> {
    provenant_supervisor::fixture::tempdir(".tmp")
}

pub(crate) struct Fixture {
    pub(crate) _temp: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) git: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) executor: SystemExecutor,
    pub(crate) runtime: UtilityRuntime,
    pub(crate) repo: VerifiedRepo,
}

impl Fixture {
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        let git = [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/usr/bin/git",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Git fixture is unavailable"))?;
        let git = fs::canonicalize(git)?;
        let temp = tempdir()?;
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        fs::create_dir(&root)?;
        fs::create_dir(&home)?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        let executor = SystemExecutor::new(4)?;
        let env = fixture_env(&home, temp.path());
        let root_path = root.to_str().ok_or("non-UTF8 test root")?;
        for args in [
            vec!["init", "--quiet", root_path],
            vec!["-C", root_path, "config", "user.name", "Fixture"],
            vec![
                "-C",
                root_path,
                "config",
                "user.email",
                "fixture@example.invalid",
            ],
            vec!["-C", root_path, "commit", "--allow-empty", "-m", "fixture"],
        ] {
            run_fixture_git(&executor, &git, temp.path(), &env, &args)?;
        }
        let fs: Arc<dyn SafeFs> = Arc::new(LocalFs);
        let runtime = UtilityRuntime::new(fs.clone(), &git, &home)?;
        let probe_budget = UtilityBudget::bounded(Duration::from_secs(10))?;
        let repo = runtime.open_repo(&executor, &root, probe_budget, &CancelToken::new())?;
        Ok(Self {
            _temp: temp,
            root,
            git,
            home,
            executor,
            runtime,
            repo,
        })
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<ExecOutcome, Box<dyn Error>> {
        let env = fixture_env(&self.home, self._temp.path());
        run_fixture_git(&self.executor, &self.git, self.repo.root_path(), &env, args)
    }

    pub(crate) fn binding(&self) -> Result<provenant_config::ProjectBinding, Box<dyn Error>> {
        let state = self._temp.path().join("state");
        fs::create_dir_all(&state)?;
        let mut input = provenant_core::RootInputs::new(self.home.clone(), self.root.clone());
        input.state_dir = Some(state);
        let roots = input.resolve(&LocalFs)?;
        Ok(provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(self.repo.common_git_dir())?,
            &LocalFs,
        )?)
    }

    pub(crate) fn budget(&self) -> Result<UtilityBudget, UtilityError> {
        UtilityBudget::bounded(Duration::from_secs(10))
    }

    pub(crate) fn head(&self) -> Result<GitOid, Box<dyn Error>> {
        let outcome = run_utility(
            &self.executor,
            GitRead::RevParseHead,
            &self.repo,
            &self.runtime,
            self.budget()?,
            &CancelToken::new(),
        )?;
        let text = std::str::from_utf8(&outcome.stdout.bytes)?;
        Ok(GitOid::parse(text.trim())?)
    }
}

pub(crate) struct PoisonedNestedRepo {
    pub(crate) path: PathBuf,
    pub(crate) marker: PathBuf,
}

impl PoisonedNestedRepo {
    pub(crate) fn arm(&self) -> Result<(), Box<dyn Error>> {
        let tracked = self.path.join("tracked.txt");
        let index_timestamp = fs::metadata(self.path.join(".git/index"))?.modified()?;
        fs::write(&tracked, b"changed!\n")?;
        fs::File::options()
            .write(true)
            .open(&tracked)?
            .set_times(fs::FileTimes::new().set_modified(index_timestamp))?;
        Ok(())
    }
}

pub(crate) fn fixture_env(
    home: &Path,
    temp: &Path,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vec![
        ("HOME".into(), home.as_os_str().to_owned()),
        ("TMPDIR".into(), temp.as_os_str().to_owned()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_COUNT".into(), "0".into()),
        ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
        ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_PAGER".into(), "cat".into()),
        ("PAGER".into(), "cat".into()),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
    ]
}

pub(crate) fn add_poisoned_gitlink(
    fixture: &Fixture,
    parent: &Path,
    gitlink_path: &str,
    marker: &Path,
) -> Result<PoisonedNestedRepo, Box<dyn Error>> {
    let path = parent.join(gitlink_path);
    fs::create_dir_all(&path)?;
    let env = fixture_env(&fixture.home, fixture._temp.path());
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Nested"],
        vec!["config", "user.email", "nested@example.invalid"],
    ] {
        run_fixture_git(&fixture.executor, &fixture.git, &path, &env, &args)?;
    }
    let tracked = path.join("tracked.txt");
    fs::write(&tracked, b"baseline\n")?;
    run_fixture_git(
        &fixture.executor,
        &fixture.git,
        &path,
        &env,
        &["add", "--", "tracked.txt"],
    )?;
    run_fixture_git(
        &fixture.executor,
        &fixture.git,
        &path,
        &env,
        &["commit", "-m", "nested fixture"],
    )?;

    let clean_filter = format!("touch '{}' && cat", marker.display());
    run_fixture_git(
        &fixture.executor,
        &fixture.git,
        &path,
        &env,
        &["config", "filter.poison.clean", clean_filter.as_str()],
    )?;
    fs::write(path.join(".git/info/attributes"), b"* filter=poison\n")?;

    let head = run_fixture_git(
        &fixture.executor,
        &fixture.git,
        &path,
        &env,
        &["rev-parse", "HEAD"],
    )?;
    let oid = std::str::from_utf8(&head.stdout.bytes)?.trim();
    let cacheinfo = format!("160000,{oid},{gitlink_path}");
    run_fixture_git(
        &fixture.executor,
        &fixture.git,
        parent,
        &env,
        &["update-index", "--add", "--cacheinfo", cacheinfo.as_str()],
    )?;
    let nested = PoisonedNestedRepo {
        path,
        marker: marker.to_path_buf(),
    };
    nested.arm()?;
    Ok(nested)
}

pub(crate) fn assert_raw_git_runs_poison_filter(
    fixture: &Fixture,
    nested: &PoisonedNestedRepo,
) -> Result<(), Box<dyn Error>> {
    let env = fixture_env(&fixture.home, fixture._temp.path());
    run_fixture_git(
        &fixture.executor,
        &fixture.git,
        &nested.path,
        &env,
        &["status", "--porcelain=v2", "--", "tracked.txt"],
    )?;
    if !nested.marker.is_file() {
        return Err(io::Error::other("raw Git did not run the poisoned clean filter").into());
    }
    fs::remove_file(&nested.marker)?;
    Ok(())
}

pub(crate) fn run_fixture_git(
    executor: &SystemExecutor,
    git: &Path,
    cwd: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    args: &[&str],
) -> Result<ExecOutcome, Box<dyn Error>> {
    let mut argv = vec![git.as_os_str().to_owned()];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    let options = exec_options(
        &argv,
        cwd,
        UtilityBudget::bounded(Duration::from_secs(10))?,
        env,
        None,
        None,
    );
    let outcome = executor.execute(options)?;
    if outcome.termination != Termination::Exit(0) || outcome.timed_out || outcome.cancelled {
        return Err(io::Error::other("bounded Git fixture setup failed").into());
    }
    Ok(outcome)
}
