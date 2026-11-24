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

const WRITER_CHILD: &str = "PROVENANT_REMOTELESS_WRITER_CHILD";
const WRITER_LOG: &str = "PROVENANT_REMOTELESS_WRITER_GIT_LOG";
const WRITER_RAN: &str = "PROVENANT_REMOTELESS_WRITER_RAN";

/// Child side of `prepare_writer_git_never_passes_an_empty_remote_to_git`.
/// Builds a fresh remote-less checkout under the recording Git, discards the
/// setup invocations, then drives the production preparation on it.
fn child_prepare_writer_git() -> FixtureResult<()> {
    let log = PathBuf::from(std::env::var_os(WRITER_LOG).ok_or("missing writer Git log")?);
    let ran = PathBuf::from(std::env::var_os(WRITER_RAN).ok_or("missing child marker")?);
    let fixture = RemotelessFixture::new()?;
    let lane = fixture.add_worktree("lane", "lane")?;
    let mut writer = fixture.prepare(&lane)?;
    std::fs::write(&ran, "1")?;
    // Only the production call under test may appear in the log the parent
    // reads; fixture setup uses the same recording executable.
    std::fs::write(&log, "")?;
    assert_eq!(
        crate::writer::terminal_forge_remote(&fixture.state, &fixture.project),
        None,
        "the remote-less fixture must resolve to a genuine absence"
    );
    crate::writer::prepare_writer_git(
        &mut writer,
        &fixture.state,
        &fixture.root,
        None,
        &crate::writer::isolated_lane_access_receipt(),
        &provenant_core::CancelToken::new(),
        &provenant_records::policy::Policy::default(),
        (None, None),
    )?;
    Ok(())
}

/// The direct consumers above pass a literal `None`. This test is the one that
/// resolves the absence from a real remote-less checkout and drives the
/// production `prepare_writer_git`, where the resolved remote reaches the Git
/// path checks and refresh. A re-flatten to `""` would appear in the recorded
/// invocations as an empty argument, `refs/remotes//` or `--remotes=`.
#[test]
fn prepare_writer_git_never_passes_an_empty_remote_to_git() -> FixtureResult<()> {
    if std::env::var_os(WRITER_CHILD).is_some() {
        return child_prepare_writer_git();
    }
    let temp = tempfile::tempdir()?;
    // The writer pins Git by opening each ancestor without following links,
    // so the wrapper must sit under a canonical root (macOS /var is a link).
    let root = temp.path().canonicalize()?;
    let bin = root.join("bin");
    let log = root.join("git.log");
    let ran = root.join("child.ran");
    std::fs::create_dir(&bin)?;
    let wrapper = bin.join("git");
    let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
    // Bracket every argument so an empty one is visible as `<>` in the log;
    // `"$*"` alone would collapse it into an ambiguous run of spaces.
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n{{ printf 'CALL'; for a in \"$@\"; do printf ' <%s>' \"$a\"; done; printf '\\n'; }} >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            git.display()
        ),
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

    // Prime the wrapper and prove it records into the very log we read, then
    // clear the prime so only the child's Git work can appear.
    #[allow(
        clippy::disallowed_methods,
        reason = "the test primes its own recording Git wrapper"
    )]
    let primed = std::process::Command::new(&wrapper)
        .arg("--version")
        .output()?;
    assert!(
        primed.status.success(),
        "the recording wrapper must run Git"
    );
    assert!(
        std::fs::read_to_string(&log)?.contains("<--version>"),
        "the recording wrapper must append every argument to the log"
    );
    std::fs::write(&log, "")?;

    let test_path = provenant_os::fixture::exact_test_path(
        module_path!(),
        "prepare_writer_git_never_passes_an_empty_remote_to_git",
    );
    #[allow(
        clippy::disallowed_methods,
        reason = "the test re-runs itself with a recording Git"
    )]
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", test_path.as_str(), "--nocapture"])
        .env_clear()
        .env("HOME", &root)
        .env("PATH", "/usr/bin:/bin")
        .env(provenant_core::consumer_env::GIT, &wrapper)
        .env(WRITER_CHILD, "1")
        .env(WRITER_LOG, &log)
        .env(WRITER_RAN, &ran)
        .status()?;
    assert!(status.success(), "the recording child writer test failed");
    assert!(
        ran.exists(),
        "the exact child test must have run; a renamed child matches nothing and still exits 0"
    );

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !calls.trim().is_empty(),
        "prepare_writer_git must have invoked the recording Git"
    );
    for needle in ["<>", "refs/remotes//", "--remotes="] {
        assert!(
            !calls.contains(needle),
            "an empty remote reached Git via {needle}: {calls}"
        );
    }
    Ok(())
}
