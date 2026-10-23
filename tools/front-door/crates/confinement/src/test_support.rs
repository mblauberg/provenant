use crate::repo::VerifiedRepo;
use crate::utility::{
    UtilityBudget, UtilityError, UtilityRead, UtilityRuntime, environment, exec_options,
    run_utility,
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
        let temp = tempfile::tempdir_in("/tmp")?;
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        fs::create_dir(&root)?;
        fs::create_dir(&home)?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        let executor = SystemExecutor::new(4)?;
        let env = vec![
            ("HOME".into(), home.as_os_str().to_owned()),
            ("TMPDIR".into(), temp.path().as_os_str().to_owned()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_COUNT".into(), "0".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_PAGER".into(), "cat".into()),
            ("PAGER".into(), "cat".into()),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        run_fixture_git(
            &executor,
            &git,
            temp.path(),
            &env,
            &[
                "init",
                "--quiet",
                root.to_str().ok_or("non-UTF8 test root")?,
            ],
        )?;
        run_fixture_git(
            &executor,
            &git,
            temp.path(),
            &env,
            &[
                "-C",
                root.to_str().ok_or("non-UTF8 test root")?,
                "config",
                "user.name",
                "Fixture",
            ],
        )?;
        run_fixture_git(
            &executor,
            &git,
            temp.path(),
            &env,
            &[
                "-C",
                root.to_str().ok_or("non-UTF8 test root")?,
                "config",
                "user.email",
                "fixture@example.invalid",
            ],
        )?;
        run_fixture_git(
            &executor,
            &git,
            temp.path(),
            &env,
            &[
                "-C",
                root.to_str().ok_or("non-UTF8 test root")?,
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        )?;
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
        let env = environment(&self.runtime, &self.repo);
        run_fixture_git(&self.executor, &self.git, self.repo.root_path(), &env, args)
    }

    pub(crate) fn budget(&self) -> Result<UtilityBudget, UtilityError> {
        UtilityBudget::new(
            Duration::from_secs(10),
            Duration::from_millis(250),
            Duration::from_secs(2),
            8 * 1024 * 1024,
            8 * 1024 * 1024,
        )
    }

    pub(crate) fn head(&self) -> Result<GitOid, Box<dyn Error>> {
        let outcome = run_utility(
            &self.executor,
            UtilityRead::Git(crate::utility::GitRead::RevParseHead),
            &self.repo,
            &self.runtime,
            self.budget()?,
            &CancelToken::new(),
        )?;
        let text = std::str::from_utf8(&outcome.stdout.bytes)?;
        Ok(GitOid::parse(text.trim())?)
    }
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
