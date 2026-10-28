//! Native Git evidence for the host's private object-store import boundary.
#![allow(clippy::disallowed_methods)]

use super::{import_objects, verify_graph};
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
    symlink(attempt.join("retained"), &scratch)?;
    assert!(super::bound_source(&held).is_err());
    Ok(())
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    git: PathBuf,
    project: PathBuf,
    lane: PathBuf,
    source: PathBuf,
    staging: PathBuf,
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
        Ok(Self {
            _temp: temp,
            root,
            home,
            git: executable,
            project,
            lane,
            source,
            staging,
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
            .env(
                "GIT_INDEX_FILE",
                self.worktree.repo().private_git_dir().join("index"),
            )
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
        import_objects(
            &self.git,
            self.worktree.repo(),
            &Directory::open_anchor(&self.source)?,
            &Directory::open_anchor(&self.staging)?,
            &self.staging,
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
        git(
            &self.git,
            &self.home,
            &self.lane,
            Some(&self.source),
            &["repack", "-ad"],
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
            &["show", "HEAD:file"],
        )?;
        assert_eq!(output.stdout, b"private lane blob\n");
        verify_graph(
            &fixture.git,
            fixture.worktree.repo().private_git_dir(),
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
fn altered_private_pack_refuses_before_publication() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let pack = fixture.pack()?;
    let mut bytes = fs::read(&pack)?;
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    fs::set_permissions(&pack, fs::Permissions::from_mode(0o600))?;
    fs::write(pack, bytes)?;
    assert!(fixture.import().is_err());
    fixture.assert_not_published();
    Ok(())
}

#[test]
fn import_preserves_only_the_trusted_host_shallow_boundary() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    fixture.import()?;
    fs::write(
        fixture.project.join(".git/shallow"),
        format!("{}\n", fixture.head),
    )?;
    fs::remove_file(object_path(&fixture.common_objects(), fixture.base))?;
    // The snapshot has the lane commit but its parent is legitimately absent.
    fs::remove_dir_all(&fixture.staging)?;
    fs::create_dir(&fixture.staging)?;
    fixture.import()?;
    fs::remove_file(fixture.project.join(".git/shallow"))?;
    fs::remove_dir_all(&fixture.staging)?;
    fs::create_dir(&fixture.staging)?;
    fs::write(
        fixture.source.join("info/shallow"),
        format!("{}\n", fixture.head),
    )?;
    assert!(fixture.import().is_err());
    Ok(())
}

#[test]
fn private_pack_indices_are_rebuilt_from_verified_pack_contents() -> TestResult {
    for format in ["sha1", "sha256"] {
        let fixture = Fixture::new(format)?;
        let pack = fixture.pack()?;
        let index = pack.with_extension("idx");
        fs::set_permissions(&index, fs::Permissions::from_mode(0o600))?;
        fs::write(index, b"forged lane index")?;
        fixture.import()?;
        let output = git(
            &fixture.git,
            &fixture.home,
            &fixture.lane,
            None,
            &["show", "HEAD:file"],
        )?;
        assert_eq!(output.stdout, b"private lane blob\n");
    }
    Ok(())
}

#[test]
fn lane_alternates_are_ignored_and_cannot_supply_missing_reachable_objects() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    fs::write(
        fixture.source.join("info/alternates"),
        "/lane-selected/nonexistent-object-store\n",
    )?;
    fixture.import()?;

    let fixture = Fixture::new("sha1")?;
    let external = fixture.root.join("external-objects");
    let external_blob = object_path(&external, fixture.blob);
    fs::create_dir_all(external_blob.parent().ok_or("external object parent")?)?;
    fs::rename(object_path(&fixture.source, fixture.blob), external_blob)?;
    fs::write(
        fixture.source.join("info/alternates"),
        format!("{}\n", external.display()),
    )?;
    assert!(fixture.import().is_err());
    fixture.assert_not_published();
    Ok(())
}

#[test]
fn symlinked_private_loose_objects_and_packs_refuse() -> TestResult {
    for packed in [false, true] {
        let fixture = Fixture::new("sha1")?;
        let target = if packed {
            fixture.pack()?
        } else {
            object_path(&fixture.source, fixture.blob)
        };
        let external = fixture.root.join("external-object");
        fs::rename(&target, &external)?;
        symlink(external, &target)?;
        assert!(fixture.import().is_err(), "packed={packed}");
        fixture.assert_not_published();
    }
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
fn forged_duplicate_object_cannot_replace_shared_content() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let common = object_path(&fixture.common_objects(), fixture.base_blob);
    let duplicate = object_path(&fixture.source, fixture.base_blob);
    fs::create_dir_all(duplicate.parent().ok_or("duplicate parent")?)?;
    let bytes = fs::read(&common)?;
    fs::copy(object_path(&fixture.source, fixture.blob), duplicate)?;
    assert!(fixture.import().is_err());
    assert_eq!(fs::read(common)?, bytes);
    fixture.assert_not_published();
    Ok(())
}

#[test]
fn object_verification_ignores_unrelated_unfinished_worktree_metadata() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    fs::write(
        fixture
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
        "unfinished cherry-pick\n",
    )?;
    fixture.import()?;
    verify_graph(
        &fixture.git,
        fixture.worktree.repo().private_git_dir(),
        &fixture.lane,
        fixture.head,
    )?;
    Ok(())
}

#[test]
fn hash_verification_refuses_a_corrupt_shared_blob_that_cat_file_accepts() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    fixture.import()?;
    let blob = object_path(&fixture.common_objects(), fixture.blob);
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o600))?;
    fs::write(
        blob,
        fs::read(object_path(&fixture.common_objects(), fixture.base_blob))?,
    )?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.lane,
        None,
        &["cat-file", "-e", &format!("{}^{{commit}}", fixture.head)],
    )?;
    assert!(
        verify_graph(
            &fixture.git,
            fixture.worktree.repo().private_git_dir(),
            &fixture.lane,
            fixture.head,
        )
        .is_err()
    );
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
fn host_preparation_initialises_objects_without_adapter_git_setup() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    fs::write(
        fixture.project.join(".git/refs/heads/lane"),
        format!("{}\n", fixture.base),
    )?;
    let attempt_path = fixture.root.join("custom-attempt");
    let scratch = attempt_path.join("tmp/candidate-0");
    fs::create_dir_all(&scratch)?;
    let attempt = Directory::open_anchor(&attempt_path)?;
    super::prepare_bound(
        &fixture.git,
        fixture.worktree.repo(),
        &attempt,
        &scratch,
        "refs/heads/lane",
        fixture.base,
    )?;
    let source = super::bound_source(&attempt)?.ok_or("custom source")?;
    assert!(source.objects.entries()?.next().is_none());
    assert_eq!(
        super::private_tip(&source, "refs/heads/lane")?,
        fixture.base
    );
    super::settle(
        &fixture.git,
        fixture.worktree.repo(),
        Some(&source.objects),
        fixture.base,
    )?;
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
