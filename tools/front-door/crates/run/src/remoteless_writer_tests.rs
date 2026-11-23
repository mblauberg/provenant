//! Regression tests for #3869 outcome 3: a checkout with no Git remote must
//! carry the absence as `None` through every writer Git-path consumer, never
//! as an empty name. Re-flattening any of these `Option`s to `""` fails a test
//! in this file.
//! Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command

use std::{
    error::Error,
    path::{Path, PathBuf},
};

use provenant_core::{GitOid, ProjectId};
use provenant_records::run::WriterRequest;

type FixtureResult<T> = std::result::Result<T, Box<dyn Error>>;

/// A checkout with no remotes at all. This is the remote-less fixture: no
/// `remote` is added, so every resolution through `remote_if_any` yields
/// `None` rather than an empty name.
struct RemotelessFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    state: PathBuf,
    git_dir: PathBuf,
    git: PathBuf,
    custody_project: ProjectId,
}

impl RemotelessFixture {
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
        std::fs::write(project.join("tracked.txt"), "tracked")?;
        run(&["add", "tracked.txt"])?;
        run(&["commit", "--allow-empty", "-qm", "initial"])?;
        // Assert the fixture really is remote-less; a stray remote here would
        // make the whole file stop pinning the absence.
        assert_eq!(run(&["remote"])?, "");
        let mut roots = provenant_core::RootInputs::new(home, project.clone());
        roots.state_dir = Some(state.clone());
        let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
        let custody_project = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(git_dir.clone())?,
        )?
        .project_id();
        Ok(Self {
            _temp: temp,
            root,
            project,
            state,
            git_dir,
            git,
            custody_project,
        })
    }

    fn add_worktree(&self, branch: &str, name: &str) -> FixtureResult<PathBuf> {
        let lane = self.root.join(name);
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
            .args([
                "worktree",
                "add",
                "-qb",
                branch,
                lane.to_str().ok_or("lane path")?,
            ])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(lane)
    }

    fn prepare(&self, lane: &Path) -> FixtureResult<crate::writer::WriterLaunch> {
        let request = WriterRequest {
            extra: Default::default(),
            custody_project: Some(self.custody_project),
            project: self.project.clone(),
            worktree: lane.to_path_buf(),
        };
        Ok(crate::writer::prepare_writer_with_facts(
            &request,
            &self.state,
            self.custody_project,
            &provenant_confinement::worktrees::RepoFacts::default(),
        )?)
    }
}

/// `ahead_of_remote` is the stopped-writer ahead counter. With no remote it
/// must return `None` before constructing any `refs/remotes/<remote>` name or
/// invoking `git rev-list --remotes=<remote>`. A re-flatten to `""` would
/// either run Git against `--remotes=` or build `refs/remotes//<branch>`, so
/// this assertion detects that class of defect at the consumer boundary.
#[test]
fn ahead_of_remote_none_returns_none() -> FixtureResult<()> {
    let fixture = RemotelessFixture::new()?;
    let lane = fixture.add_worktree("lane", "lane")?;
    let writer = fixture.prepare(&lane)?;
    // The head is unused on the None path; a well-formed stand-in keeps the
    // assertion about the remote, not about Git object availability.
    let head = GitOid::parse(&"1".repeat(40))?;
    assert_eq!(writer.ahead_of_remote(&fixture.git, head, None)?, None);
    Ok(())
}

/// `verify_writer_git_paths(.., None)` still checks the local `refs/heads`
/// namespace. Planting a symlink there must be refused: skipping all path
/// checks when the remote is absent would let a hostile local ref through.
#[test]
fn verify_writer_git_paths_none_still_checks_local_heads() -> FixtureResult<()> {
    let temp = tempfile::tempdir()?;
    let common = temp.path();
    let branch = provenant_confinement::repo::GitRefName::new("refs/heads/lane")?;
    std::fs::create_dir_all(common.join("refs/heads"))?;
    std::os::unix::fs::symlink("/etc/passwd", common.join("refs/heads/lane"))?;
    let error = crate::writer::verify_writer_git_paths(common, &branch, None)
        .expect_err("refs/heads must still be checked when there is no remote");
    assert!(
        format!("{error:?}").contains("unsafe file type"),
        "refusal must name the unsafe leaf, got {error:?}"
    );
    Ok(())
}

/// With no remote there is no remote-tracking namespace to verify. A hostile
/// file under `refs/remotes/` is therefore irrelevant and must not be checked.
/// A re-flatten of `None` to `""` would build `refs/remotes/` + `lane` and
/// refuse that file, failing this test.
#[test]
fn verify_writer_git_paths_none_ignores_remote_namespace() -> FixtureResult<()> {
    let temp = tempfile::tempdir()?;
    let common = temp.path();
    let branch = provenant_confinement::repo::GitRefName::new("refs/heads/lane")?;
    std::fs::create_dir_all(common.join("refs/remotes"))?;
    std::os::unix::fs::symlink("/etc/passwd", common.join("refs/remotes/lane"))?;
    crate::writer::verify_writer_git_paths(common, &branch, None)?;
    Ok(())
}
