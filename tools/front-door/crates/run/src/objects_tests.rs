//! Behavioural tests of private lane Git and Git's receive quarantine.
#![allow(clippy::disallowed_methods)]
use super::verify_graph;
use provenant_confinement::{UtilityBudget, UtilityRuntime, VerifiedWorktree};
use provenant_core::{CancelToken, GitOid};
use provenant_os::{exec::SystemExecutor, fs::Directory};
use std::{
    error::Error,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
type TestResult<T = ()> = std::result::Result<T, Box<dyn Error>>;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    git: PathBuf,
    project: PathBuf,
    lane: PathBuf,
    source: PathBuf,
    worktree: VerifiedWorktree,
    base: GitOid,
    head: GitOid,
    base_blob: GitOid,
    blob: GitOid,
}

fn git(
    executable: &Path,
    home: &Path,
    cwd: &Path,
    objects: Option<&Path>,
    args: &[&str],
) -> TestResult<Output> {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .current_dir(cwd);
    if let Some(objects) = objects {
        command.env("GIT_OBJECT_DIRECTORY", objects);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "Git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output)
}

fn oid(output: Output) -> TestResult<GitOid> {
    Ok(GitOid::parse(std::str::from_utf8(&output.stdout)?.trim())?)
}

fn object_path(directory: &Path, oid: GitOid) -> PathBuf {
    let oid = oid.to_string();
    directory.join(&oid[..2]).join(&oid[2..])
}

impl Fixture {
    fn new(format: &str) -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let project = root.join("project");
        let lane = root.join("lane");
        let source = root.join("private-objects");
        let staging = root.join("host-staging");
        for path in [&home, &project, &source, &staging] {
            fs::create_dir(path)?;
        }
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        let executable = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let host_git = |args: &[&str]| git(&executable, &home, &project, None, args);
        host_git(&[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ])?;
        fs::write(project.join("file"), "trusted base blob\n")?;
        host_git(&["add", "file"])?;
        host_git(&["commit", "-qm", "base"])?;
        let base = oid(host_git(&["rev-parse", "HEAD"])?)?;
        let base_blob = oid(host_git(&["rev-parse", "HEAD:file"])?)?;
        host_git(&[
            "worktree",
            "add",
            "-qb",
            "lane",
            lane.to_str().ok_or("lane path is not UTF-8")?,
        ])?;
        fs::create_dir(source.join("info"))?;
        fs::write(
            source.join("info/alternates"),
            format!("{}\n", project.join(".git/objects").display()),
        )?;
        let runtime = UtilityRuntime::new(&executable, &home)?;
        let executor = SystemExecutor::new(1)?;
        let repo = runtime.open_repo(
            &executor,
            &project,
            UtilityBudget::bounded(Duration::from_secs(10))?,
            &CancelToken::new(),
        )?;
        let worktree = provenant_confinement::verify_worktree(&repo, &lane)?;
        fs::write(lane.join("file"), "private lane blob\n")?;
        let private_git = |args: &[&str]| git(&executable, &home, &lane, Some(&source), args);
        private_git(&["add", "file"])?;
        private_git(&["commit", "-qm", "lane commit"])?;
        let head = oid(private_git(&["rev-parse", "HEAD"])?)?;
        let blob = oid(private_git(&["rev-parse", "HEAD:file"])?)?;
        host_git(&["update-ref", "refs/heads/lane", &base.to_string()])?;
        Ok(Self {
            _temp: temp,
            root,
            home,
            git: executable,
            project,
            lane,
            source,
            worktree,
            base,
            head,
            base_blob,
            blob,
        })
    }

    fn common_objects(&self) -> PathBuf {
        self.project.join(".git/objects")
    }

    fn overlay(&mut self) -> TestResult<(Directory, PathBuf)> {
        // The original fixture wrote a private object tip to the shared ref.
        // Restore the shared base before preparing the production ref overlay.
        fs::write(
            self.project.join(".git/refs/heads/lane"),
            format!("{}\n", self.base),
        )?;
        let attempt = self.root.join("attempt");
        let scratch = attempt.join("tmp/candidate-0");
        fs::create_dir_all(&scratch)?;
        let private_objects = scratch.join("git-objects");
        fs::rename(&self.source, &private_objects)?;
        self.source = private_objects;
        super::prepare_refs(
            &self.git,
            self.worktree.repo(),
            &Directory::open_anchor(&scratch)?,
            "refs/heads/lane",
            self.base,
        )?;
        let attempt = Directory::open_anchor(&attempt)?;
        super::bind(&attempt, &scratch)?;
        let source = super::bound_source(&attempt)?.ok_or("bound overlay")?;
        assert_eq!(super::private_tip(&source, "refs/heads/lane")?, self.base);
        fs::write(
            scratch.join("git-common/refs/heads/lane"),
            format!("{}\n", self.head),
        )?;
        Ok((attempt, scratch))
    }

    fn git_overlay(&self, scratch: &Path, args: &[&str]) -> TestResult<Output> {
        let output = Command::new(&self.git)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_DIR", scratch.join("git-common"))
            .env("GIT_COMMON_DIR", scratch.join("git-common"))
            .env("GIT_WORK_TREE", &self.lane)
            .env("GIT_OBJECT_DIRECTORY", &self.source)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
            ])
            .args(args)
            .current_dir(&self.lane)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "overlay Git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(output)
    }

    fn import(&self) -> crate::Result<()> {
        super::settle(
            &self.git,
            self.worktree.repo(),
            Some(&Directory::open_anchor(&self.source)?),
            self.head,
        )
    }

    fn replace_private(&self, object: GitOid, donor: GitOid) -> TestResult {
        let target = object_path(&self.source, object);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        fs::write(
            target,
            fs::read(object_path(&self.common_objects(), donor))?,
        )?;
        Ok(())
    }

    fn pack(&self) -> TestResult<PathBuf> {
        fs::write(
            self.project.join(".git/refs/heads/lane"),
            format!("{}\n", self.head),
        )?;
        git(
            &self.git,
            &self.home,
            &self.lane,
            Some(&self.source),
            &["repack", "-ad"],
        )?;
        fs::write(
            self.project.join(".git/refs/heads/lane"),
            format!("{}\n", self.base),
        )?;
        let packs = fs::read_dir(self.source.join("pack"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        packs
            .into_iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "pack")
            })
            .ok_or_else(|| "private repack produced no pack".into())
    }

    fn assert_not_published(&self) {
        assert!(!object_path(&self.common_objects(), self.head).exists());
        assert!(!object_path(&self.common_objects(), self.blob).exists());
        assert!(
            fs::read_dir(self.common_objects().join("pack"))
                .is_ok_and(|mut entries| entries.next().is_none()),
            "unverified pack was published"
        );
    }
}

#[test]
fn host_prepared_private_repository_reads_shared_base_and_imports_new_commit() -> TestResult {
    for format in ["sha1", "sha256"] {
        let mut fixture = Fixture::new(format)?;
        let attempt = fixture.root.join("prepared-attempt");
        let scratch = attempt.join("tmp/candidate-0");
        fs::create_dir_all(&scratch)?;
        let attempt = Directory::open_anchor(&attempt)?;
        super::prepare_bound(
            &fixture.git,
            fixture.worktree.repo(),
            &attempt,
            &scratch,
            "refs/heads/lane",
            fixture.base,
        )?;
        fixture.source = scratch.join("git-objects");
        fixture.git_overlay(&scratch, &["cat-file", "-e", &fixture.base.to_string()])?;
        fs::write(fixture.lane.join("file"), "host-prepared private commit\n")?;
        fixture.git_overlay(&scratch, &["add", "file"])?;
        fixture.git_overlay(&scratch, &["commit", "-qm", "host-prepared private commit"])?;
        let source = super::bound_source(&attempt)?.ok_or("bound private Git")?;
        let head = super::private_tip(&source, "refs/heads/lane")?;
        assert_ne!(head, fixture.base);
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
            fixture.base
        );
        super::settle(
            &fixture.git,
            fixture.worktree.repo(),
            Some(&source.objects),
            head,
        )?;
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            "refs/heads/lane",
            fixture.base,
            head,
        )?;
        let content = git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["show", "lane:file"],
        )?;
        assert_eq!(content.stdout, b"host-prepared private commit\n");
    }
    Ok(())
}

#[test]
fn private_loose_objects_are_verified_and_imported_for_both_hash_formats() -> TestResult {
    for format in ["sha1", "sha256"] {
        let fixture = Fixture::new(format)?;
        fixture.assert_not_published();
        fixture.import()?;
        let output = git(
            &fixture.git,
            &fixture.home,
            &fixture.lane,
            None,
            &["show", &format!("{}:file", fixture.head)],
        )?;
        assert_eq!(output.stdout, b"private lane blob\n");
        verify_graph(
            &fixture.git,
            fixture.worktree.repo().common_git_dir(),
            &fixture.lane,
            fixture.head,
        )?;
    }
    Ok(())
}

#[test]
fn private_loose_blob_and_commit_hash_mismatches_refuse_before_publication() -> TestResult {
    for corrupt_commit in [false, true] {
        let fixture = Fixture::new("sha1")?;
        let (target, donor) = if corrupt_commit {
            (fixture.head, fixture.base)
        } else {
            (fixture.blob, fixture.base_blob)
        };
        fixture.replace_private(target, donor)?;
        assert!(fixture.import().is_err(), "corrupt_commit={corrupt_commit}");
        fixture.assert_not_published();
    }
    Ok(())
}

#[test]
fn corrupt_private_pack_cannot_contaminate_shared_objects() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let pack = fixture.pack()?;
    let mut bytes = fs::read(&pack)?;
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    fs::set_permissions(&pack, fs::Permissions::from_mode(0o600))?;
    fs::write(pack, bytes)?;
    match fixture.import() {
        Err(_) => fixture.assert_not_published(),
        Ok(()) => {
            git(
                &fixture.git,
                &fixture.home,
                &fixture.project,
                None,
                &["fsck", "--full"],
            )?;
            assert_eq!(
                git(
                    &fixture.git,
                    &fixture.home,
                    &fixture.project,
                    None,
                    &["show", &format!("{}:file", fixture.head)]
                )?
                .stdout,
                b"private lane blob\n"
            );
        }
    }
    Ok(())
}

#[test]
fn missing_reachable_private_object_refuses_before_publication() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    fs::remove_file(object_path(&fixture.source, fixture.blob))?;
    assert!(fixture.import().is_err());
    fixture.assert_not_published();
    Ok(())
}

#[test]
fn private_ref_overlay_keeps_host_and_sibling_fetch_usable_until_verified_publication() -> TestResult
{
    let mut fixture = Fixture::new("sha1")?;
    let (attempt, scratch) = fixture.overlay()?;
    fixture.git_overlay(
        &scratch,
        &["commit", "--allow-empty", "-qm", "private overlay commit"],
    )?;
    fixture.head = oid(fixture.git_overlay(&scratch, &["rev-parse", "HEAD"])?)?;
    let source = super::bound_source(&attempt)?.ok_or("bound overlay")?;
    assert_eq!(
        super::private_tip(&source, "refs/heads/lane")?,
        fixture.head
    );
    let shared_tip = || -> TestResult<GitOid> {
        oid(git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["rev-parse", "refs/heads/lane"],
        )?)
    };
    assert_eq!(shared_tip()?, fixture.base);
    fixture.assert_not_published();

    let remote = fixture.root.join("remote");
    fs::create_dir(&remote)?;
    git(
        &fixture.git,
        &fixture.home,
        &remote,
        None,
        &["init", "-q", "-b", "main"],
    )?;
    git(
        &fixture.git,
        &fixture.home,
        &remote,
        None,
        &["commit", "--allow-empty", "-qm", "remote advancement"],
    )?;
    let sibling = fixture.root.join("sibling");
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "worktree",
            "add",
            "-qb",
            "sibling",
            sibling.to_str().ok_or("sibling path")?,
            "main",
        ],
    )?;
    for cwd in [&fixture.project, &sibling] {
        git(
            &fixture.git,
            &fixture.home,
            cwd,
            None,
            &[
                "-c",
                "protocol.file.allow=always",
                "fetch",
                "--no-tags",
                "--refmap=",
                remote.to_str().ok_or("remote path")?,
                "main",
            ],
        )?;
    }
    assert_eq!(shared_tip()?, fixture.base);
    fixture.import()?;
    assert_eq!(shared_tip()?, fixture.base);
    super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        "refs/heads/lane",
        fixture.base,
        fixture.head,
    )?;
    assert_eq!(shared_tip()?, fixture.head);
    Ok(())
}

#[test]
fn private_tip_ignores_lane_configuration_and_refuses_symlinked_refs() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (attempt, scratch) = fixture.overlay()?;
    let marker = fixture.root.join("host-callback-executed");
    fs::write(
        scratch.join("git-common/config"),
        format!(
            "[core]\nfsmonitor = \"/bin/sh -c 'touch {}'\"\n[invalid configuration\n",
            marker.display()
        ),
    )?;
    let source = super::bound_source(&attempt)?.ok_or("bound overlay")?;
    assert_eq!(
        super::private_tip(&source, "refs/heads/lane")?,
        fixture.head
    );
    fixture.import()?;
    assert!(!marker.exists());
    let private_ref = scratch.join("git-common/refs/heads/lane");
    fs::remove_file(&private_ref)?;
    symlink(fixture.project.join(".git/refs/heads/lane"), private_ref)?;
    assert!(super::private_tip(&source, "refs/heads/lane").is_err());
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn private_tip_resolves_a_packed_selected_ref_and_rejects_ambiguous_records() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (attempt, scratch) = fixture.overlay()?;
    fs::remove_file(scratch.join("git-common/refs/heads/lane"))?;
    let packed = scratch.join("git-common/packed-refs");
    let entry = format!("{} refs/heads/lane\n", fixture.head);
    fs::write(&packed, format!("# pack-refs with: sorted\n{entry}"))?;
    let source = super::bound_source(&attempt)?.ok_or("bound overlay")?;
    assert_eq!(
        super::private_tip(&source, "refs/heads/lane")?,
        fixture.head
    );
    fs::write(&packed, format!("{entry}{entry}"))?;
    assert!(super::private_tip(&source, "refs/heads/lane").is_err());
    fs::remove_file(&packed)?;
    let external = fixture.root.join("external-packed-refs");
    fs::write(&external, entry)?;
    symlink(external, packed)?;
    assert!(super::private_tip(&source, "refs/heads/lane").is_err());
    Ok(())
}

#[test]
fn shared_branch_publication_compares_base_and_is_idempotent_for_the_same_head() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    fixture.overlay()?;
    fixture.import()?;
    for _ in 0..2 {
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            "refs/heads/lane",
            fixture.base,
            fixture.head,
        )?;
    }
    let published = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", "refs/heads/lane"],
    )?)?;
    assert_eq!(published, fixture.head);

    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    let divergent = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "commit-tree",
            &tree.to_string(),
            "-p",
            &fixture.base.to_string(),
            "-m",
            "competing host branch update",
        ],
    )?)?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "update-ref",
            "refs/heads/lane",
            &divergent.to_string(),
            &fixture.head.to_string(),
        ],
    )?;
    assert!(
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            "refs/heads/lane",
            fixture.base,
            fixture.head,
        )
        .is_err()
    );
    let actual = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", "refs/heads/lane"],
    )?)?;
    assert_eq!(actual, divergent);
    Ok(())
}

#[test]
fn object_source_binding_is_scoped_and_refuses_symlinked_candidates() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let attempt = root.join("attempt");
    let scratch = attempt.join("tmp/candidate-3");
    fs::create_dir_all(scratch.join("git-objects"))?;
    fs::create_dir_all(scratch.join("git-common"))?;
    let held = Directory::open_anchor(&attempt)?;
    assert!(super::bound_source(&held)?.is_none());
    assert!(super::object_binding(&held)?.is_none());
    assert!(super::bind(&held, &attempt.join("tmp/../escape")).is_err());
    super::bind(&held, &scratch)?;
    super::bind(&held, &scratch)?;
    assert!(super::bind(&held, &attempt.join("tmp/candidate-4")).is_err());
    let source = super::bound_source(&held)?.ok_or("source binding")?;
    assert!(
        source
            .objects
            .metadata()?
            .same_identity(&Directory::open_anchor(&scratch.join("git-objects"))?.metadata()?)
    );
    fs::rename(&scratch, attempt.join("retained"))?;
    assert!(
        !super::object_binding(&held)?.is_none(),
        "policy detection must not open reclaimed scratch"
    );
    symlink(attempt.join("retained"), &scratch)?;
    assert!(super::bound_source(&held).is_err());
    Ok(())
}

#[test]
fn shared_duplicate_object_is_never_replaced_or_chmodded() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let common = object_path(&fixture.common_objects(), fixture.base_blob);
    let duplicate = object_path(&fixture.source, fixture.base_blob);
    fs::create_dir_all(duplicate.parent().ok_or("duplicate parent")?)?;
    let bytes = fs::read(&common)?;
    let before = fs::metadata(&common)?;
    fs::write(duplicate, &bytes)?;
    fixture.import()?;
    let after = fs::metadata(&common)?;
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.mode(), after.mode());
    assert_eq!(before.modified()?, after.modified()?);
    assert_eq!(fs::read(common)?, bytes);
    Ok(())
}

#[test]
fn private_git_supports_stash_rebase_merge_and_gc_without_shared_metadata_changes() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (_, scratch) = fixture.overlay()?;
    let common = fixture.project.join(".git");
    let config = fs::read(common.join("config"))?;
    let index = fs::read(fixture.worktree.repo().private_git_dir().join("index"))?;
    fixture.git_overlay(&scratch, &["reset", "--hard", &fixture.base.to_string()])?;
    fs::write(fixture.lane.join("file"), "stash me\n")?;
    fixture.git_overlay(&scratch, &["stash", "push", "-m", "private stash"])?;
    fixture.git_overlay(&scratch, &["stash", "pop"])?;
    fixture.git_overlay(&scratch, &["add", "file"])?;
    fixture.git_overlay(&scratch, &["commit", "-qm", "private change"])?;
    fixture.git_overlay(
        &scratch,
        &["checkout", "-qb", "side", &fixture.base.to_string()],
    )?;
    fs::write(fixture.lane.join("side"), "side\n")?;
    fixture.git_overlay(&scratch, &["add", "side"])?;
    fixture.git_overlay(&scratch, &["commit", "-qm", "side change"])?;
    fixture.git_overlay(&scratch, &["checkout", "lane"])?;
    fixture.git_overlay(&scratch, &["rebase", "side"])?;
    fixture.git_overlay(&scratch, &["merge", "--no-ff", "side", "-m", "merge"])?;
    fixture.git_overlay(&scratch, &["gc"])?;
    assert_eq!(fs::read(common.join("config"))?, config);
    assert_eq!(
        fs::read(fixture.worktree.repo().private_git_dir().join("index"))?,
        index
    );
    assert_eq!(
        oid(git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["rev-parse", "refs/heads/lane"]
        )?)?,
        fixture.base
    );
    fixture.head = oid(fixture.git_overlay(&scratch, &["rev-parse", "HEAD"])?)?;
    fixture.import()?;
    Ok(())
}

#[test]
fn unrelated_corrupt_shared_history_does_not_get_scanned() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let unrelated = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["hash-object", "-w", "--stdin"],
    )?)?;
    let path = object_path(&fixture.common_objects(), unrelated);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    fs::write(path, b"corrupt unrelated object")?;
    fixture.import()?;
    Ok(())
}

#[test]
fn cancellation_kills_a_blocking_import_within_two_seconds() -> TestResult {
    use std::time::Instant;
    let fixture = Fixture::new("sha1")?;
    let wrapper = fixture.root.join("blocking-git");
    let entered = fixture.root.join("entered");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nfor arg do\ncase \"$arg\" in push) : > '{}'; /bin/sleep 30;; esac\ndone\nexec '{}' \"$@\"\n",
            entered.display(),
            fixture.git.display()
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
    let cancel = CancelToken::new();
    let attempt_path = fixture.root.join("cancel-attempt");
    fs::create_dir(&attempt_path)?;
    let attempt = Directory::open_anchor(&attempt_path)?;
    std::thread::scope(|scope| -> TestResult {
        let worker = scope.spawn(|| {
            crate::settlement::with_cancellation(
                &attempt,
                crate::tests::exit().scope,
                &cancel,
                || {
                    super::settle_cancel(
                        &wrapper,
                        fixture.worktree.repo(),
                        Some(&Directory::open_anchor(&fixture.source)?),
                        fixture.head,
                        Some(&cancel),
                    )
                },
            )
        });
        let start = Instant::now();
        while !entered.exists() && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let seen = entered.exists();
        let cancelled_at = Instant::now();
        let receipt = provenant_supervisor::custody::request_stop(
            attempt.try_clone()?,
            crate::tests::exit().scope,
            provenant_core::ActorId::from_bytes([2; 16]),
            provenant_supervisor::machine::StopReason::ExplicitCancellation,
        )?;
        assert_eq!(
            receipt,
            provenant_supervisor::custody::StopReceipt::Requested
        );
        assert!(worker.join().map_err(|_| "import panicked")?.is_err());
        assert!(seen, "never reached import");
        assert!(cancelled_at.elapsed() < Duration::from_secs(2));
        fixture.assert_not_published();
        Ok(())
    })?;
    Ok(())
}

fn sha1_bytes(bytes: &[u8]) -> TestResult<[u8; 20]> {
    use std::{io::Write, process::Stdio};
    let mut child = Command::new("/usr/bin/shasum")
        .args(["-a", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("digest input")?.write_all(bytes)?;
    let output = child.wait_with_output()?;
    assert!(output.status.success());
    let hex = std::str::from_utf8(&output.stdout)?
        .split_whitespace()
        .next()
        .ok_or("digest")?;
    let mut result = [0; 20];
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)?;
    }
    Ok(result)
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

/// Stored DEFLATE blocks make the pack bytes explicit without a compressor
/// dependency. Every checksum remains valid in the forged pair, so only
/// per-entry object-name verification exposes the wrong content.
fn pack_pair(entries: &[(&[u8], [u8; 20])]) -> TestResult<(Vec<u8>, Vec<u8>)> {
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&u32::try_from(entries.len())?.to_be_bytes());
    let mut index_entries = Vec::new();
    for (bytes, name) in entries {
        let offset = u32::try_from(pack.len())?;
        let start = pack.len();
        let mut size = bytes.len();
        let first = 0x30 | (size & 15) as u8;
        size >>= 4;
        pack.push(first | if size == 0 { 0 } else { 0x80 });
        while size != 0 {
            let value = (size & 127) as u8;
            size >>= 7;
            pack.push(value | if size == 0 { 0 } else { 0x80 });
        }
        pack.extend_from_slice(&[0x78, 0x01, 0x01]);
        let length = u16::try_from(bytes.len())?;
        pack.extend_from_slice(&length.to_le_bytes());
        pack.extend_from_slice(&(!length).to_le_bytes());
        pack.extend_from_slice(bytes);
        let (mut a, mut b) = (1_u32, 0_u32);
        for byte in *bytes {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        pack.extend_from_slice(&((b << 16) | a).to_be_bytes());
        index_entries.push((*name, crc32(&pack[start..]), offset));
    }
    let digest = sha1_bytes(&pack)?;
    pack.extend_from_slice(&digest);
    // Duplicate names deliberately index the first physical copy last, so
    // Git's normal name lookup resolves the correct first copy in that case.
    index_entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| right.2.cmp(&left.2)));
    let mut index = b"\xfftOc\0\0\0\x02".to_vec();
    for first in 0..=u8::MAX {
        let count = index_entries
            .iter()
            .filter(|entry| entry.0[0] <= first)
            .count();
        index.extend_from_slice(&u32::try_from(count)?.to_be_bytes());
    }
    for (name, _, _) in &index_entries {
        index.extend_from_slice(name);
    }
    for (_, crc, _) in &index_entries {
        index.extend_from_slice(&crc.to_be_bytes());
    }
    for (_, _, offset) in &index_entries {
        index.extend_from_slice(&offset.to_be_bytes());
    }
    index.extend_from_slice(&digest);
    index.extend_from_slice(&sha1_bytes(&index)?);
    Ok((pack, index))
}

/// Payloads ported from fix/object-retry-cap-2141. The source index lies about
/// object names. Git may exclude the hidden damaged bytes; the shared graph
/// must either remain untouched or contain exactly the selected valid content.
#[test]
fn wrong_content_duplicate_and_corrupt_non_first_pack_entry_never_enter_shared_store() -> TestResult
{
    for non_first in [false, true] {
        let fixture = Fixture::new("sha1")?;
        let correct = b"private lane blob\n";
        let wrong = b"forged lane bytes\n";
        let mut object = format!("blob {}\0", correct.len()).into_bytes();
        object.extend_from_slice(correct);
        let blob = sha1_bytes(&object)?;
        let first = b"independent first entry\n";
        let mut object = format!("blob {}\0", first.len()).into_bytes();
        object.extend_from_slice(first);
        let first_name = sha1_bytes(&object)?;
        let entries: Vec<(&[u8], [u8; 20])> = if non_first {
            fs::remove_file(object_path(&fixture.source, fixture.blob))?;
            vec![(correct, blob), (wrong, blob)]
        } else {
            vec![(first, first_name), (wrong, blob)]
        };
        let (pack, index) = pack_pair(&entries)?;
        let digest = pack[pack.len() - 20..]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let packs = fixture.source.join("pack");
        fs::create_dir_all(&packs)?;
        fs::write(packs.join(format!("pack-{digest}.pack")), &pack)?;
        fs::write(packs.join(format!("pack-{digest}.idx")), index)?;
        match fixture.import() {
            Err(_) => fixture.assert_not_published(),
            Ok(()) => {
                let content = git(
                    &fixture.git,
                    &fixture.home,
                    &fixture.project,
                    None,
                    &["show", &format!("{}:file", fixture.head)],
                )?;
                assert_eq!(content.stdout, correct);
                git(
                    &fixture.git,
                    &fixture.home,
                    &fixture.project,
                    None,
                    &["fsck", "--full"],
                )?;
            }
        }
        assert!(
            !fixture
                .common_objects()
                .join("pack")
                .join(format!("pack-{digest}.pack"))
                .exists()
        );
    }
    Ok(())
}

/// Exercise receive-pack directly with a malformed incoming pack. Unlike the
/// sender's exclusion of unused entries, this must fail in object quarantine.
#[test]
fn malformed_received_pack_is_quarantined_and_never_publishes_objects_or_refs() -> TestResult {
    use std::{io::Write, process::Stdio};
    let fixture = Fixture::new("sha1")?;
    let entries = [
        (b"first entry".as_slice(), [0; 20]),
        (b"second entry".as_slice(), [0; 20]),
    ];
    let (mut pack, _) = pack_pair(&entries)?;
    // Corrupt the non-first entry's DEFLATE checksum, then fix the outer
    // pack digest. Git must reject the object stream, not just its trailer.
    let end = pack.len();
    pack[end - 21] ^= 0xff;
    let digest = sha1_bytes(&pack[..end - 20])?;
    pack[end - 20..].copy_from_slice(&digest);
    let command = format!(
        "{} {} refs/provenant/import/attack\0report-status\n",
        "0".repeat(40),
        fixture.head
    );
    let mut request = format!("{:04x}", command.len() + 4).into_bytes();
    request.extend_from_slice(command.as_bytes());
    request.extend_from_slice(b"0000");
    request.extend_from_slice(&pack);
    let mut child = Command::new(&fixture.git)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "receive.fsckObjects=true",
            "receive-pack",
            "--stateless-rpc",
        ])
        .arg(fixture.project.join(".git"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("receive input")?
        .write_all(&request)?;
    let output = child.wait_with_output()?;
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        report.contains("unpack ") && !report.contains("unpack ok"),
        "{report}"
    );
    fixture.assert_not_published();
    assert!(
        !fixture
            .project
            .join(".git/refs/provenant/import/attack")
            .exists()
    );
    Ok(())
}
