use super::*;
use provenant_core::CancelToken;
use provenant_os::exec::SystemExecutor;
use std::fs;

struct NoPause;
impl Pause for NoPause {
    fn wait(&self, _delay: DurationMicros, _cancel: &CancelToken) {}
}

fn test_env() -> Vec<(OsString, OsString)> {
    let mut env = provenant_os::fixture::git_env();
    if let Some(git) = provenant_os::git::resolve_git_executable(None) {
        env.push((OsString::from("PROVENANT_GIT"), git.into_os_string()));
    }
    env
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = provenant_os::fixture::run_git(cwd, args).expect("git");
    String::from_utf8(out).expect("utf8").trim().to_owned()
}

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    head: provenant_core::GitOid,
}

impl Fixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path().join("lane");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("base.txt"), "base\n")?;
        git(&repo, &["add", "base.txt"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let head = provenant_core::GitOid::parse(&git(&repo, &["rev-parse", "HEAD"]))?;
        Ok(Self {
            _temp: temp,
            repo,
            head,
        })
    }

    fn git_dir(&self) -> PathBuf {
        self.repo.join(".git")
    }

    fn commit(
        &self,
        expected: provenant_core::GitOid,
        retries: u32,
    ) -> std::result::Result<OwnCommit, Box<CommandError>> {
        let paths = [self.repo.join("own.txt")];
        self.commit_input(expected, OwnCommitInput::Paths(&paths), retries)
    }

    fn commit_input(
        &self,
        expected: provenant_core::GitOid,
        input: OwnCommitInput<'_>,
        retries: u32,
    ) -> std::result::Result<OwnCommit, Box<CommandError>> {
        let env = test_env();
        let executor = SystemExecutor::new(4).map_err(|e| {
            Box::new(CommandError::new(
                "EXECUTOR",
                "executor unavailable",
                None,
                e.to_string(),
            ))
        })?;
        let cancel = CancelToken::new();
        commit_own_paths(OwnCommitRequest {
            worktree: &self.repo,
            git_dir: &self.git_dir(),
            ref_name: "refs/heads/main",
            expected,
            input,
            message: "lane: own paths",
            retries,
            env: &env,
            executor: &executor,
            cancel: &cancel,
            pause: &NoPause,
            timeout: Duration::from_secs(30),
        })
        .map_err(Box::new)
    }
}

#[test]
fn commit_own_stages_only_claimed_paths_and_cas_updates_the_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join("own.txt"), "own\n")?;
    fs::write(fixture.repo.join("other.txt"), "other\n")?;
    let committed = fixture
        .commit(fixture.head, 0)
        .map_err(|e| std::io::Error::other(e.message.clone()))?;
    assert_eq!(committed.ref_name, "refs/heads/main");
    assert_eq!(committed.paths, ["own.txt"]);
    let head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    assert_eq!(committed.head.to_string(), head);
    let files = git(
        &fixture.repo,
        &["show", "--pretty=format:", "--name-only", &head],
    );
    assert!(files.contains("own.txt"), "{files}");
    assert!(!files.contains("other.txt"), "{files}");
    assert!(!files.contains("base.txt"), "{files}");
    let subject = git(&fixture.repo, &["log", "-1", "--pretty=format:%s"]);
    assert_eq!(subject, "lane: own paths");
    Ok(())
}

#[test]
fn commit_own_cas_retries_on_race() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    // Move the branch on from `fixture.head`, so a CAS against the stale
    // head loses the race exactly once.
    fs::write(fixture.repo.join("moved.txt"), "moved\n")?;
    git(&fixture.repo, &["add", "moved.txt"]);
    git(&fixture.repo, &["commit", "-qm", "moved"]);
    let current = git(&fixture.repo, &["rev-parse", "HEAD"]);
    fs::write(fixture.repo.join("own.txt"), "own\n")?;
    let committed = fixture
        .commit(fixture.head, 1)
        .map_err(|e| std::io::Error::other(e.message.clone()))?;
    assert_ne!(committed.head.to_string(), current);
    let parent = git(&fixture.repo, &["rev-parse", "HEAD^"]);
    assert_eq!(parent, current);
    Ok(())
}

#[test]
fn commit_own_patch_retries_from_new_head_without_staging_worktree_bytes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join("extra.txt"), "branch change\n")?;
    git(&fixture.repo, &["add", "extra.txt"]);
    git(&fixture.repo, &["commit", "-qm", "branch change"]);
    let current =
        provenant_core::GitOid::parse(&git(&fixture.repo, &["rev-parse", "refs/heads/main"]))?;
    fs::write(fixture.repo.join("base.txt"), "co-writer edit\n")?;
    let patch = b"diff --git a/base.txt b/base.txt\n--- a/base.txt\n+++ b/base.txt\n@@ -1 +1 @@\n-base\n+generated\n";
    let paths = ["base.txt".to_owned()];

    let committed = fixture
        .commit_input(
            fixture.head,
            OwnCommitInput::Patch {
                bytes: patch,
                paths: &paths,
            },
            1,
        )
        .map_err(|error| std::io::Error::other(error.message.clone()))?;
    assert_ne!(committed.head, current);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "HEAD^"]),
        current.to_string()
    );
    assert_eq!(git(&fixture.repo, &["show", "HEAD:base.txt"]), "generated");
    assert_eq!(
        git(&fixture.repo, &["show", "HEAD:extra.txt"]),
        "branch change"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("base.txt"))?,
        "co-writer edit\n"
    );
    assert_eq!(git(&fixture.repo, &["show", ":base.txt"]), "base");
    Ok(())
}

#[test]
fn index_lock_race_retries_then_transient() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join("own.txt"), "own\n")?;
    // Pre-lock the private index for both attempts. The first two
    // `git add` calls fail on the lock and the third exceeds retries.
    for attempt in 0..=1 {
        let tag = format!("{}-{attempt}", std::process::id());
        let lock = crate::worktree_carry::private_index_path(&fixture.git_dir(), &tag);
        let lock = lock.with_extension("index.lock");
        fs::write(lock, "held by another process")?;
    }
    let error = fixture
        .commit(fixture.head, 1)
        .expect_err("must exhaust retries");
    assert_eq!(error.code, "INDEX_LOCK_RETRY_EXHAUSTED");
    assert!(matches!(error.class, ErrorClass::Transient { .. }));
    Ok(())
}
