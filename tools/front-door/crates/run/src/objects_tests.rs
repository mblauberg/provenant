//! Behavioural tests of private lane Git and Git's receive quarantine.
#![allow(clippy::disallowed_methods)]
use super::verify_graph;
use provenant_config::LaneHistory;
use provenant_confinement::{UtilityBudget, UtilityRuntime, VerifiedWorktree};
use provenant_core::{AttemptId, CancelToken, GitOid};
use provenant_os::{exec::SystemExecutor, fs::Directory};
use std::{
    error::Error,
    ffi::OsStr,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
type TestResult<T = ()> = std::result::Result<T, Box<dyn Error>>;

#[test]
fn private_view_binding_refuses_replaced_directories_and_foreign_paths() -> TestResult {
    for component in ["", "git-common", "git-objects"] {
        let mut fixture = Fixture::new("sha1")?;
        let (attempt, scratch) = fixture.overlay()?;
        let target = scratch.join(component);
        fs::rename(&target, fixture.root.join("retained-view"))?;
        fs::create_dir(&target)?;
        if component.is_empty() {
            fs::create_dir(target.join("git-common"))?;
            fs::create_dir(target.join("git-objects"))?;
        }
        assert!(super::bound_source(&attempt).is_err(), "{component}");
        fixture.assert_not_published();
    }
    let mut fixture = Fixture::new("sha1")?;
    let (attempt, _) = fixture.overlay()?;
    let other_attempt = fixture.root.join("other-attempt");
    fs::create_dir_all(other_attempt.join("tmp/candidate-0/git-common"))?;
    fs::create_dir(other_attempt.join("tmp/candidate-0/git-objects"))?;
    assert!(
        super::bind(
            &Directory::open_anchor(&other_attempt)?,
            &attempt.current_path()?.join("tmp/candidate-0")
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn private_view_refuses_incomplete_identity_versions() -> TestResult {
    for version in [1, 2, 3] {
        let temp = tempfile::tempdir()?;
        let path = fs::canonicalize(temp.path())?;
        fs::create_dir_all(path.join("tmp/candidate-0/git-common"))?;
        fs::create_dir(path.join("tmp/candidate-0/git-objects"))?;
        let attempt = Directory::open_anchor(&path)?;
        crate::artifacts::seal(
            &attempt,
            "writer-objects.json",
            &crate::db::bytes(
                &serde_json::json!({ "schema_version": version, "candidate": "candidate-0" }),
            )?,
            crate::RECORD_LIMIT,
        )?;
        assert!(super::bound_source(&attempt).is_err(), "version {version}");
    }
    Ok(())
}

#[test]
fn private_view_refuses_shared_objects_as_an_import_source() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let info = fixture.common_objects().join("info");
    let sentinel = info.join("commit-graph");
    fs::write(&sentinel, b"host-owned accelerator")?;
    assert!(
        super::settle(
            &fixture.git,
            fixture.worktree.repo(),
            Some(&Directory::open_anchor(&fixture.common_objects())?),
            fixture.head
        )
        .is_err()
    );
    assert_eq!(fs::read(&sentinel)?, b"host-owned accelerator");
    fixture.assert_not_published();
    Ok(())
}

#[test]
fn private_view_restarts_import_and_publication_in_shallow_repositories() -> TestResult {
    for format in ["sha1", "sha256"] {
        let mut fixture = Fixture::new(format)?;
        fs::write(
            fixture.project.join(".git/shallow"),
            format!("{}\n", fixture.base),
        )?;
        let (attempt, _) = fixture.overlay()?;
        for checkpoint in 0..4 {
            // Reopen from the durable binding at each restart, including after
            // import and publication removed the attempt's temporary ref.
            let source = super::bound_source(&attempt)?.ok_or("private view")?;
            let head = super::private_tip(&source, "refs/heads/lane")?;
            super::settle(
                &fixture.git,
                fixture.worktree.repo(),
                Some(&source.objects),
                head,
            )?;
            if checkpoint >= 2 {
                super::publish_branch(
                    &fixture.git,
                    fixture.worktree.repo(),
                    super::BranchPublication {
                        branch: "refs/heads/lane",
                        base: fixture.base,
                        launch_ref: None,
                        head,
                        attempt: fixture.import_attempt,
                        history: LaneHistory::default(),
                    },
                )?;
            }
            let expected = if checkpoint >= 2 { head } else { fixture.base };
            assert_eq!(
                super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
                expected
            );
            let staging = fixture.project.join(".git");
            assert!(
                !fs::read_dir(staging)?.any(|entry| entry.is_ok_and(|entry| entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b"tmp-provenant-import-")))
            );
        }
    }
    Ok(())
}

#[test]
fn private_view_recovers_after_import_and_publication_acknowledgement_failures() -> TestResult {
    for format in ["sha1", "sha256"] {
        let mut fixture = Fixture::new(format)?;
        let (attempt, _) = fixture.overlay()?;
        let wrapper = fixture.root.join("interrupted-git");
        // Return failure after Git successfully receives the objects. Settlement
        // has not recorded import success, but the staging ref now retains them.
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n'{}' \"$@\" || exit $?\nfor arg do\ncase \"$arg\" in push) exit 1;; esac\ndone\n",
                fixture.git.display()
            ),
        )?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        let source = super::bound_source(&attempt)?.ok_or("private view")?;
        assert!(
            super::settle(
                &wrapper,
                fixture.worktree.repo(),
                Some(&source.objects),
                fixture.head
            )
            .is_err()
        );
        let stage = super::import_pin(fixture.import_attempt, fixture.head);
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), &stage)?,
            fixture.head
        );
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
            fixture.base
        );
        drop(source);
        let source = super::bound_source(&attempt)?.ok_or("reopened view")?;
        super::settle(
            &fixture.git,
            fixture.worktree.repo(),
            Some(&source.objects),
            fixture.head,
        )?;
        // Refuse only cleanup: the branch CAS has succeeded when this fails.
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nfor arg do\ncase \"$arg\" in -d) exit 1;; esac\ndone\nexec '{}' \"$@\"\n",
                fixture.git.display()
            ),
        )?;
        assert!(
            super::publish_branch(
                &wrapper,
                fixture.worktree.repo(),
                super::BranchPublication {
                    branch: "refs/heads/lane",
                    base: fixture.base,
                    launch_ref: None,
                    head: fixture.head,
                    attempt: fixture.import_attempt,
                    history: LaneHistory::default(),
                },
            )
            .is_err()
        );
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
            fixture.head
        );
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), &stage)?,
            fixture.head
        );
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head: fixture.head,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
        )?;
        assert!(super::resolve_ref(&fixture.git, fixture.worktree.repo(), &stage).is_err());
    }
    Ok(())
}

#[test]
fn foreign_alternates_are_refused_without_importing_foreign_objects() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let foreign = fixture.root.join("foreign-objects");
    fs::rename(&fixture.source, &foreign)?;
    fs::create_dir_all(fixture.source.join("info"))?;
    fs::write(
        fixture.source.join("info/alternates"),
        format!("{}\n", foreign.display()),
    )?;
    assert!(matches!(
        fixture.import(),
        Err(crate::RunError::Validation(
            "lane object alternates are untrusted"
        ))
    ));
    fixture.assert_not_published();
    Ok(())
}

#[test]
fn symlinked_pack_directory_is_refused_without_importing_objects() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let pack = fixture.pack()?;
    let packs = pack.parent().ok_or("pack parent")?;
    let foreign = fixture.root.join("foreign-packs");
    fs::rename(packs, &foreign)?;
    symlink(&foreign, packs)?;
    assert!(matches!(
        fixture.import(),
        Err(crate::RunError::Validation(
            "lane object store contains a symlink or special file"
        ))
    ));
    fixture.assert_not_published();
    Ok(())
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
    git: PathBuf,
    project: PathBuf,
    lane: PathBuf,
    source: PathBuf,
    worktree: VerifiedWorktree,
    import_attempt: AttemptId,
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
    let mut command = provenant_os::fixture::git_command_at(executable);
    command
        .env_clear()
        .envs(provenant_os::fixture::fixture_git_settings())
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
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
        let state = root.join("state");
        for path in [&home, &project, &source, &staging] {
            fs::create_dir(path)?;
        }
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        provenant_os::fixture::private_state_dir(&state)?;
        let executable =
            provenant_os::git::resolve_git_executable(None).ok_or("Git unavailable")?;
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
            format!(
                "{}\n",
                super::quote_alternate(&project.join(".git/objects"))
            ),
        )?;
        let runtime = UtilityRuntime::new(&executable, &home, &state)?;
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
            state,
            git: executable,
            project,
            lane,
            source,
            worktree,
            import_attempt: AttemptId::from_bytes([0; 16]),
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
        git(
            &self.git,
            &self.home,
            &self.lane,
            None,
            &["read-tree", &self.base.to_string()],
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
        self.git_overlay(&scratch, &["read-tree", &self.head.to_string()])?;
        Ok((attempt, scratch))
    }

    fn git_overlay(&self, scratch: &Path, args: &[&str]) -> TestResult<Output> {
        let output = self.git_overlay_raw(scratch, args)?;
        if !output.status.success() {
            return Err(format!(
                "overlay Git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(output)
    }

    fn git_overlay_raw(&self, scratch: &Path, args: &[&str]) -> TestResult<Output> {
        Ok(provenant_os::fixture::git_command_at(&self.git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_DIR", scratch.join("git-common"))
            .env("GIT_COMMON_DIR", scratch.join("git-common"))
            .env("GIT_WORK_TREE", &self.lane)
            .env("GIT_OBJECT_DIRECTORY", &self.source)
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(&self.lane)
            .output()?)
    }

    fn prepare_attempt(&self, name: &str, base: GitOid) -> TestResult<(Directory, PathBuf)> {
        let attempt_path = self.root.join(name);
        let scratch = attempt_path.join("tmp/candidate-0");
        fs::create_dir_all(&scratch)?;
        let attempt = Directory::open_anchor(&attempt_path)?;
        super::prepare_bound(
            &self.git,
            self.worktree.repo(),
            &attempt,
            &scratch,
            "refs/heads/lane",
            base,
        )?;
        Ok((attempt, scratch))
    }

    fn git_attempt(&self, scratch: &Path, args: &[&str]) -> TestResult<Output> {
        let common = scratch.join("git-common");
        let mut command = provenant_os::fixture::git_command_at(&self.git);
        command
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", scratch.join("gitconfig"))
            .env("GIT_DIR", &common)
            .env("GIT_COMMON_DIR", &common)
            .env("GIT_INDEX_FILE", common.join("index"))
            .env("GIT_WORK_TREE", &self.lane)
            .env("GIT_OBJECT_DIRECTORY", scratch.join("git-objects"))
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(&self.lane);
        let output = command.output()?;
        if !output.status.success() {
            return Err(format!(
                "attempt Git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(output)
    }

    // A private commit leaves a cache-tree in the linked worktree index whose
    // objects live only in `source`. Host GC reads every worktree index, so
    // restore this finished fixture's index to a tree the host actually owns.
    fn collect_shared_objects(&self) -> TestResult {
        git(
            &self.git,
            &self.home,
            &self.lane,
            None,
            &["read-tree", &self.base.to_string()],
        )?;
        git(
            &self.git,
            &self.home,
            &self.project,
            None,
            &["reflog", "expire", "--expire=now", "--all"],
        )?;
        git(
            &self.git,
            &self.home,
            &self.project,
            None,
            &["gc", "--prune=now"],
        )?;
        Ok(())
    }

    fn import(&self) -> crate::Result<()> {
        super::settle(
            &self.git,
            self.worktree.repo(),
            Some(&Directory::open_anchor(&self.source)?),
            self.head,
        )
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
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
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
// Spec: docs/specs/run-launch.md#admission-and-custody
fn shared_object_snapshot_bounds_empty_files_and_directories() -> TestResult {
    let source = tempfile::tempdir()?;
    fs::write(source.path().join("first"), b"")?;
    fs::write(source.path().join("second"), b"")?;
    fs::create_dir(source.path().join("empty"))?;
    let source = Directory::open_anchor(source.path())?;
    assert!(matches!(
        super::object_bytes_with_entries(&source, 0, &mut 2),
        Err(crate::RunError::ObjectImportLimit(_))
    ));
    assert_eq!(super::object_bytes_with_entries(&source, 0, &mut 3)?, 0);
    let refused = tempfile::tempdir()?;
    let refused = Directory::open_anchor(refused.path())?;
    assert!(matches!(
        super::snapshot_source(&source, &refused, &mut 0, &mut 2, 0),
        Err(crate::RunError::ObjectImportLimit(_))
    ));
    let copied = tempfile::tempdir()?;
    let copied = Directory::open_anchor(copied.path())?;
    super::snapshot_source(&source, &copied, &mut 0, &mut 3, 0)?;
    assert_eq!(copied.open_read(OsStr::new("first"))?.metadata()?.size, 0);
    assert_eq!(copied.open_read(OsStr::new("second"))?.metadata()?.size, 0);
    copied.open_verified_directory(OsStr::new("empty"))?;
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn per_attempt_git_views_keep_identity_and_configuration_private() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let (_, first) = fixture.prepare_attempt("private-attempt-1", fixture.base)?;
    let (_, second) = fixture.prepare_attempt("private-attempt-2", fixture.base)?;
    let first_common = first.join("git-common");
    let second_common = second.join("git-common");
    assert_ne!(first_common, second_common);
    assert_ne!(first_common.join("index"), second_common.join("index"));
    assert_ne!(
        first_common.join("refs/heads/lane"),
        second_common.join("refs/heads/lane")
    );

    fs::write(
        first.join("gitconfig"),
        b"[credential]\n\thelper = fixture-attempt-one\n[user]\n\tname = Attempt One\n",
    )?;
    fs::write(
        second.join("gitconfig"),
        b"[credential]\n\thelper = fixture-attempt-two\n[user]\n\tname = Attempt Two\n",
    )?;
    assert_eq!(
        fixture
            .git_attempt(
                &first,
                &["config", "--global", "--get", "credential.helper"]
            )?
            .stdout,
        b"fixture-attempt-one\n"
    );
    assert_eq!(
        fixture
            .git_attempt(
                &second,
                &["config", "--global", "--get", "credential.helper"]
            )?
            .stdout,
        b"fixture-attempt-two\n"
    );
    assert_eq!(
        fixture
            .git_attempt(&first, &["config", "--global", "--get", "user.name"])?
            .stdout,
        b"Attempt One\n"
    );
    assert_eq!(
        fixture
            .git_attempt(&second, &["config", "--global", "--get", "user.name"])?
            .stdout,
        b"Attempt Two\n"
    );

    fs::write(
        first_common.join("config"),
        b"[credential]\n\thelper = private-one\n",
    )?;
    let second_config = fs::read(second_common.join("config"))?;
    assert_eq!(fs::read(second_common.join("config"))?, second_config);
    assert_eq!(
        fs::read(first_common.join("config"))?,
        b"[credential]\n\thelper = private-one\n"
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn plain_commit_excludes_a_sibling_attempts_staged_path() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let (_, lane_scratch) = fixture.prepare_attempt("commit-attempt", fixture.base)?;
    let (_, sibling_scratch) = fixture.prepare_attempt("staged-sibling", fixture.base)?;

    fs::write(
        fixture.lane.join("sibling-staged"),
        "sibling staged bytes\n",
    )?;
    fixture.git_attempt(&sibling_scratch, &["add", "sibling-staged"])?;
    let sibling_index = fs::read(sibling_scratch.join("git-common/index"))?;

    fs::write(fixture.lane.join("lane-owned"), "lane owned bytes\n")?;
    fixture.git_attempt(&lane_scratch, &["add", "lane-owned"])?;
    // No pathspec: the index itself must define the lane's commit boundary.
    fixture.git_attempt(&lane_scratch, &["commit", "-qm", "lane commit"])?;
    let lane_head = oid(fixture.git_attempt(&lane_scratch, &["rev-parse", "HEAD"])?)?;
    let changed = fixture.git_attempt(
        &lane_scratch,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            &lane_head.to_string(),
        ],
    )?;
    assert_eq!(changed.stdout, b"lane-owned\n");
    assert_eq!(
        fixture
            .git_attempt(&sibling_scratch, &["show", ":sibling-staged"])?
            .stdout,
        b"sibling staged bytes\n"
    );
    assert_eq!(
        fs::read(sibling_scratch.join("git-common/index"))?,
        sibling_index
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn launch_at_an_advanced_tip_preserves_a_live_siblings_staged_index() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let (_, sibling_scratch) = fixture.prepare_attempt("live-sibling", fixture.base)?;
    fs::write(fixture.lane.join("sibling-staged"), "live staged bytes\n")?;
    fixture.git_attempt(&sibling_scratch, &["add", "sibling-staged"])?;
    let sibling_index = fs::read(sibling_scratch.join("git-common/index"))?;

    let advanced = child_of(&fixture, fixture.base, "branch advanced")?;
    update_ref(&fixture, "refs/heads/lane", advanced)?;
    let (_, launched_scratch) = fixture.prepare_attempt("new-launch", advanced)?;
    let launched = Directory::open_anchor(&fixture.root.join("new-launch"))?;
    let source = super::bound_source(&launched)?.ok_or("launched private view")?;
    assert_eq!(super::private_tip(&source, "refs/heads/lane")?, advanced);
    assert_eq!(
        oid(fixture.git_attempt(&launched_scratch, &["write-tree"])?)?,
        oid(git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["rev-parse", &format!("{advanced}^{{tree}}")]
        )?)?
    );
    assert_eq!(
        fs::read(sibling_scratch.join("git-common/index"))?,
        sibling_index
    );
    assert_eq!(
        fixture
            .git_attempt(&sibling_scratch, &["show", ":sibling-staged"])?
            .stdout,
        b"live staged bytes\n"
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn private_commits_survive_a_losing_host_settlement_cas() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let (first_attempt, first_scratch) = fixture.prepare_attempt("first-writer", fixture.base)?;
    let (second_attempt, second_scratch) =
        fixture.prepare_attempt("second-writer", fixture.base)?;

    fs::write(fixture.lane.join("first-writer-file"), "first commit\n")?;
    fixture.git_attempt(&first_scratch, &["add", "first-writer-file"])?;
    fixture.git_attempt(&first_scratch, &["commit", "-qm", "first writer"])?;
    let first_source = super::bound_source(&first_attempt)?.ok_or("first private source")?;
    let first_head = super::private_tip(&first_source, "refs/heads/lane")?;

    fs::write(fixture.lane.join("second-writer-file"), "second commit\n")?;
    fixture.git_attempt(&second_scratch, &["add", "second-writer-file"])?;
    fixture.git_attempt(&second_scratch, &["commit", "-qm", "second writer"])?;
    let second_source = super::bound_source(&second_attempt)?.ok_or("second private source")?;
    let second_head = super::private_tip(&second_source, "refs/heads/lane")?;
    assert_ne!(first_head, second_head);

    super::settle(
        &fixture.git,
        fixture.worktree.repo(),
        Some(&first_source.objects),
        first_head,
    )?;
    super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: first_head,
            attempt: AttemptId::from_bytes([1; 16]),
            history: LaneHistory::default(),
        },
    )?;
    super::settle(
        &fixture.git,
        fixture.worktree.repo(),
        Some(&second_source.objects),
        second_head,
    )?;
    // The host branch moved after the second writer prepared at the same base,
    // so its settlement CAS loses. The default rebase setting publishes the
    // sibling histories as a host merge: both private commits stay reachable.
    let published = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: second_head,
            attempt: AttemptId::from_bytes([2; 16]),
            history: LaneHistory::default(),
        },
    )?;
    assert_eq!(published.previous, first_head);
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        published.tip
    );
    let parents = git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "rev-list",
            "--parents",
            "-n",
            "1",
            &published.tip.to_string(),
        ],
    )?;
    let parents = String::from_utf8_lossy(&parents.stdout);
    assert!(parents.contains(&first_head.to_string()), "{parents}");
    assert!(parents.contains(&second_head.to_string()), "{parents}");
    assert_eq!(
        super::private_tip(&second_source, "refs/heads/lane")?,
        second_head
    );
    assert_eq!(
        fixture
            .git_attempt(
                &second_scratch,
                &["show", &format!("{second_head}:second-writer-file")]
            )?
            .stdout,
        b"second commit\n"
    );
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
    for format in ["sha1", "sha256"] {
        for corrupt_commit in [false, true] {
            let fixture = Fixture::new(format)?;
            let (target, donor) = if corrupt_commit {
                (fixture.head, fixture.base)
            } else {
                (fixture.blob, fixture.base_blob)
            };
            // A lane shell can replace its own loose objects. A valid zlib
            // object under the wrong digest must still fail host receipt.
            let target_path = object_path(&fixture.source, target);
            let donor_path = object_path(&fixture.project.join(".git/objects"), donor);
            let output = Command::new("/bin/sh")
                .args([
                    "-c",
                    "chmod u+w \"$2\" && cp \"$1\" \"$2\"",
                    "corrupt-object",
                ])
                .arg(&donor_path)
                .arg(&target_path)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .output()?;
            assert!(output.status.success(), "{output:?}");
            assert_eq!(fs::read(&target_path)?, fs::read(&donor_path)?);
            assert!(
                fixture.import().is_err(),
                "format={format}, corrupt_commit={corrupt_commit}"
            );
            fixture.assert_not_published();
        }
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
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: fixture.head,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
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
fn shared_branch_publication_compares_base_merges_divergence_and_is_idempotent() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    fixture.overlay()?;
    fixture.import()?;
    for _ in 0..2 {
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head: fixture.head,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
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
    let published = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: fixture.head,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?;
    assert_ne!(published.tip, divergent);
    let parents = git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "rev-list",
            "--parents",
            "-n",
            "1",
            &published.tip.to_string(),
        ],
    )?;
    let parents = String::from_utf8_lossy(&parents.stdout);
    assert!(parents.contains(&fixture.head.to_string()), "{parents}");
    assert!(parents.contains(&divergent.to_string()), "{parents}");
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        published.tip
    );

    // A lane may still configure the old retain-and-refuse outcome. It is a
    // configured stop for its own branch, typed and final for settlement.
    update_ref(&fixture, "refs/heads/lane", divergent)?;
    let refuse = LaneHistory {
        rebase: provenant_config::Setting {
            value: provenant_config::Rebase::Refuse,
            source: provenant_records::LimitSource::Project,
        },
        ..LaneHistory::default()
    };
    let refusal = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: fixture.head,
            attempt: fixture.import_attempt,
            history: refuse,
        },
    )
    .expect_err("a forked lane history is refused when configured so");
    // Typed and final: settlement retains the private history at once instead
    // of retrying a refusal no retry can change.
    assert!(matches!(
        refusal,
        crate::RunError::PublicationDiverged { ref setting }
            if setting.contains("lanes.history.rebase = refuse")
                && setting.contains("source: project")
    ));
    assert!(refusal.import_refusal_is_final() && refusal.is_publication_refusal());
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
fn a_divergent_publication_that_conflicts_names_the_files_and_retains_the_branch() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let commit_with = |contents: &str, message: &str| -> TestResult<GitOid> {
        fs::write(fixture.project.join("conflict.txt"), contents)?;
        git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["add", "conflict.txt"],
        )?;
        let tree = oid(git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["write-tree"],
        )?)?;
        oid(git(
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
                message,
            ],
        )?)
    };
    let host = commit_with("host side\n", "host update")?;
    let lane = commit_with("lane side\n", "lane update")?;
    update_ref(&fixture, "refs/heads/lane", host)?;
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: lane,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )
    .expect_err("a real conflict stays retained");
    assert!(matches!(
        error,
        crate::RunError::PublicationConflict {
            ref conflicting_files,
            ref setting,
        } if conflicting_files
            .iter()
            .any(|file| file.as_str() == "conflict.txt")
            && setting.contains("lanes.history.rebase = allow")
    ));
    assert!(error.import_refusal_is_final() && error.is_publication_refusal());
    let message = error.to_string();
    assert!(message.contains("conflict.txt"), "{message}");
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        host
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-settlement.md#settlement
fn the_unrooted_decision_judges_the_lane_head_before_any_host_merge() -> TestResult {
    use provenant_config::{HistoryEffect, Setting};
    use provenant_records::LimitSource;
    let fixture = Fixture::new("sha1")?;
    // The host branch was rewritten onto a root the lane never saw, so a merge
    // of it would descend the branch while the lane head descends neither it
    // nor the launch base.
    let root = commit_of(
        &fixture,
        tree_with(&fixture, &[("root.txt", "unrelated root\n")])?,
        None,
        "unrelated root",
    )?;
    let host = commit_of(
        &fixture,
        tree_with(&fixture, &[("host.txt", "host side\n")])?,
        Some(root),
        "host rewrite",
    )?;
    let lane = commit_of(
        &fixture,
        tree_with(&fixture, &[("lane.txt", "lane side\n")])?,
        Some(root),
        "lane commit",
    )?;
    update_ref(&fixture, "refs/heads/lane", host)?;
    let history = LaneHistory {
        unrooted_tip: Setting {
            value: HistoryEffect::Refuse,
            source: LimitSource::Host,
        },
        ..LaneHistory::default()
    };
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: lane,
            attempt: fixture.import_attempt,
            history,
        },
    )
    .expect_err("the lane head descends none of the known history");
    assert!(matches!(
        error,
        crate::RunError::LaneTipUnrooted(ref cause)
            if cause.tip == lane && cause.base == fixture.base
    ));
    // The merge never ran: the branch stays where the host left it.
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        host
    );
    Ok(())
}

#[test]
fn a_lane_tip_that_shares_no_history_with_the_host_branch_is_refused_as_unrelated() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    // The host branch is an unrelated root; the lane descends only its launch
    // base. Git refuses this merge, and the refusal is typed, not a crash.
    let host = commit_of(
        &fixture,
        tree_with(&fixture, &[("host.txt", "host side\n")])?,
        None,
        "unrelated host tip",
    )?;
    let lane = child_of(&fixture, fixture.base, "lane commit")?;
    update_ref(&fixture, "refs/heads/lane", host)?;
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: lane,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )
    .expect_err("unrelated histories cannot be merged");
    assert!(matches!(
        error,
        crate::RunError::PublicationUnrelated { ref setting }
            if setting.contains("lanes.history.rebase = allow")
    ));
    assert!(error.import_refusal_is_final() && error.is_publication_refusal());
    let message = error.to_string();
    assert!(message.contains("invent a common ancestor"), "{message}");
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        host
    );
    Ok(())
}

#[test]
fn a_configured_merge_driver_refuses_the_host_merge_before_it_runs() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let marker = fixture.root.join("merge-driver-ran");
    let script = fixture.root.join("merge-driver.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "config",
            "merge.provenant.driver",
            script.to_str().ok_or("driver path is not UTF-8")?,
        ],
    )?;
    let attributes = ".gitattributes";
    let host = {
        let files = [(attributes, "* merge=provenant\n"), ("file", "host side\n")];
        commit_of(
            &fixture,
            tree_with(&fixture, &files)?,
            Some(fixture.base),
            "host update",
        )?
    };
    let lane = {
        let files = [(attributes, "* merge=provenant\n"), ("file", "lane side\n")];
        commit_of(
            &fixture,
            tree_with(&fixture, &files)?,
            Some(fixture.base),
            "lane update",
        )?
    };
    update_ref(&fixture, "refs/heads/lane", host)?;
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: lane,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )
    .expect_err("a configured merge driver is refused");
    assert!(matches!(
        error,
        crate::RunError::PublicationMergeDriver { ref setting }
            if setting.contains("lanes.history.rebase = allow")
    ));
    assert!(!marker.exists(), "the guard runs before any driver");
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        host
    );
    // The driver, not the trees, is what refused: without it the same pair is
    // a real conflict, which keeps the private history for the same reason.
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["config", "--unset", "merge.provenant.driver"],
    )?;
    let conflict = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: lane,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )
    .expect_err("the same trees conflict without the driver");
    assert!(matches!(
        conflict,
        crate::RunError::PublicationConflict { ref conflicting_files, .. }
            if conflicting_files.iter().any(|file| file.as_str() == "file")
    ));
    assert!(!marker.exists(), "no driver ran in either attempt");
    Ok(())
}

#[test]
fn a_branch_rewound_behind_its_launch_base_is_refused_not_fast_forwarded() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    // The lane launched from `launch`, and the branch has since been rewound to
    // its parent. Publishing would reinstate the history the host removed.
    let launch = child_of(&fixture, fixture.base, "launch base")?;
    update_ref(&fixture, "refs/heads/lane", fixture.base)?;
    let head = child_of(&fixture, launch, "lane commit")?;
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: launch,
            launch_ref: None,
            head,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )
    .expect_err("a branch behind its launch base is not fast-forwarded");
    assert!(matches!(
        error,
        crate::RunError::PublicationRewound { branch_tip, base }
            if branch_tip == fixture.base && base == launch
    ));
    assert!(error.import_refusal_is_final() && error.is_publication_refusal());
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        fixture.base
    );
    Ok(())
}

#[test]
fn a_head_the_branch_already_contains_never_advances_to_a_tip_someone_else_reached() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (_attempt, _scratch) = fixture.overlay()?;
    fixture.import()?;
    // Publish the lane head, then let the host advance the branch past it, so
    // the next publication finds the head already contained and changes nothing.
    publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    let ahead = child_of(&fixture, fixture.head, "host advance")?;
    update_ref(&fixture, "refs/heads/lane", ahead)?;
    let published = publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    assert_eq!(published.previous, ahead);
    assert_eq!(published.tip, ahead);
    assert!(!published.merged);
    assert!(
        !published.ends_at_head(fixture.head),
        "a contained head must not move this worktree to a tip someone else advanced"
    );
    assert!(published.ends_at_head(ahead));
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        ahead
    );
    Ok(())
}

#[test]
fn a_rewound_branch_is_judged_before_an_unrooted_tip() -> TestResult {
    use provenant_config::{HistoryEffect, Setting};
    use provenant_records::LimitSource;
    let fixture = Fixture::new("sha1")?;
    // The branch was rewound to the launch base's parent while the lane tip
    // descends nothing. The rewound branch decides the publication, so the
    // unrooted setting never turns it into its own refusal.
    let launch = child_of(&fixture, fixture.base, "launch base")?;
    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    let orphan = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["commit-tree", &tree.to_string(), "-m", "orphan"],
    )?)?;
    update_ref(&fixture, "refs/heads/lane", fixture.base)?;
    let history = LaneHistory {
        unrooted_tip: Setting {
            value: HistoryEffect::Refuse,
            source: LimitSource::Host,
        },
        ..LaneHistory::default()
    };
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: launch,
            launch_ref: None,
            head: orphan,
            attempt: fixture.import_attempt,
            history,
        },
    )
    .expect_err("a branch behind its launch base is not fast-forwarded");
    assert!(matches!(
        error,
        crate::RunError::PublicationRewound { branch_tip, base }
            if branch_tip == fixture.base && base == launch
    ));
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        fixture.base
    );
    Ok(())
}

#[test]
fn an_unrooted_tip_warns_or_stays_quiet_and_the_host_merge_still_publishes() -> TestResult {
    use provenant_config::{HistoryEffect, Setting};
    use provenant_records::LimitSource;
    for value in [HistoryEffect::Off, HistoryEffect::Warn] {
        let fixture = Fixture::new("sha1")?;
        // The host branch and the lane tip share a root the lane never launched
        // from, so the tip is unrooted and the two histories still merge
        // cleanly.
        let root = commit_of(
            &fixture,
            tree_with(&fixture, &[("root.txt", "shared root\n")])?,
            None,
            "unrelated root",
        )?;
        let host = commit_of(
            &fixture,
            tree_with(&fixture, &[("host.txt", "host side\n")])?,
            Some(root),
            "host update",
        )?;
        let lane = commit_of(
            &fixture,
            tree_with(&fixture, &[("lane.txt", "lane side\n")])?,
            Some(root),
            "lane update",
        )?;
        update_ref(&fixture, "refs/heads/lane", host)?;
        let history = LaneHistory {
            unrooted_tip: Setting {
                value,
                source: LimitSource::Host,
            },
            ..LaneHistory::default()
        };
        let published = super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head: lane,
                attempt: fixture.import_attempt,
                history,
            },
        )
        .map_err(|error| -> Box<dyn Error> { format!("{value:?}: {error}").into() })?;
        assert!(published.merged, "{value:?}");
        assert_ne!(published.tip, lane, "{value:?}");
        assert_eq!(published.previous, host, "{value:?}");
        assert!(published.ends_at_head(lane), "{value:?}");
        assert!(
            super::is_ancestor(&fixture.git, fixture.worktree.repo(), lane, published.tip),
            "{value:?}"
        );
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
            published.tip
        );
    }
    Ok(())
}

#[test]
fn the_host_branch_roots_a_tip_only_when_the_tip_contains_it() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let root = commit_of(
        &fixture,
        tree_with(&fixture, &[("root.txt", "root\n")])?,
        None,
        "unrelated root",
    )?;
    let host = commit_of(
        &fixture,
        tree_with(&fixture, &[("host.txt", "host\n")])?,
        Some(root),
        "host update",
    )?;
    let head = commit_of(
        &fixture,
        tree_with(&fixture, &[("lane.txt", "lane\n")])?,
        Some(host),
        "lane commit",
    )?;
    // The launch base is unrelated and the repository has no remote default or
    // remote-tracking branch, so the host branch is the only candidate root and
    // it counts only when the caller says the tip already contains it.
    assert_eq!(
        super::known_ancestor(
            &fixture.git,
            fixture.worktree.repo(),
            fixture.base,
            None,
            head,
            "refs/heads/lane",
            None,
        )?,
        None
    );
    assert_eq!(
        super::known_ancestor(
            &fixture.git,
            fixture.worktree.repo(),
            fixture.base,
            None,
            head,
            "refs/heads/lane",
            Some(host),
        )?,
        Some(super::Ancestor::Known {
            reference: "refs/heads/lane".to_owned(),
            oid: host,
        })
    );
    Ok(())
}

#[test]
fn a_merge_driver_outside_the_repository_config_file_still_refuses_the_host_merge() -> TestResult {
    // The driver is reachable only through an included file, so a guard that
    // reads the repository config alone would run the merge and its driver.
    for conditional in [false, true] {
        let fixture = Fixture::new("sha1")?;
        let included = fixture.root.join("merge-driver.cfg");
        fs::write(&included, "[merge \"provenant\"]\n\tdriver = /bin/false\n")?;
        let included = included.to_str().ok_or("driver path is not UTF-8")?;
        let key = if conditional {
            format!("includeIf.gitdir:{}/.path", fixture.project.display())
        } else {
            "include.path".to_owned()
        };
        git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["config", "--local", key.as_str(), included],
        )?;
        let host = child_of(&fixture, fixture.base, "host update")?;
        let lane = child_of(&fixture, fixture.base, "lane update")?;
        update_ref(&fixture, "refs/heads/lane", host)?;
        let error = super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head: lane,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
        )
        .expect_err("a driver in an included file is still configured");
        assert!(
            matches!(
                error,
                crate::RunError::PublicationMergeDriver { ref setting }
                    if setting.contains("lanes.history.rebase = allow")
            ),
            "conditional {conditional}: {error}"
        );
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
            host
        );
    }
    Ok(())
}

#[test]
fn a_merge_driver_in_the_worktree_config_refuses_the_host_merge() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["config", "--local", "extensions.worktreeConfig", "true"],
    )?;
    fs::write(
        fixture.project.join(".git/config.worktree"),
        "[merge \"provenant\"]\n\tdriver = /bin/false\n",
    )?;
    let host = child_of(&fixture, fixture.base, "host update")?;
    let lane = child_of(&fixture, fixture.base, "lane update")?;
    update_ref(&fixture, "refs/heads/lane", host)?;
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: lane,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )
    .expect_err("a driver in the worktree config is still configured");
    assert!(matches!(
        error,
        crate::RunError::PublicationMergeDriver { .. }
    ));
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        host
    );
    Ok(())
}

#[test]
fn same_head_import_pins_survive_independent_publication_cleanup() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let first = AttemptId::from_bytes([31; 16]);
    let second = AttemptId::from_bytes([32; 16]);
    let source = Directory::open_anchor(&fixture.source)?;
    for attempt in [first, second] {
        super::settle_many(
            &fixture.git,
            fixture.worktree.repo(),
            Some(&source),
            fixture.head,
            &[],
            None,
            super::ImportScope {
                attempt,
                custody: None,
            },
        )?;
    }

    let first_pin = super::import_pin(first, fixture.head);
    let second_pin = super::import_pin(second, fixture.head);
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), &first_pin)?,
        fixture.head
    );
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), &second_pin)?,
        fixture.head
    );

    super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: fixture.head,
            attempt: first,
            history: LaneHistory::default(),
        },
    )?;
    assert!(super::resolve_ref(&fixture.git, fixture.worktree.repo(), &first_pin).is_err());
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), &second_pin)?,
        fixture.head,
        "publishing the first attempt must retain the sibling's same-head pin"
    );

    super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: fixture.head,
            attempt: second,
            history: LaneHistory::default(),
        },
    )?;
    assert!(super::resolve_ref(&fixture.git, fixture.worktree.repo(), &second_pin).is_err());
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
fn cancellation_kills_a_blocking_import_promptly() -> TestResult {
    use provenant_core::Clock;
    let fixture = Fixture::new("sha1")?;
    let clock = provenant_os::clock::SystemClock::new();
    let wrapper = fixture.root.join("blocking-git");
    let entered = fixture.root.join("entered");
    let fifo = fixture.root.join("import.fifo");
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nfor arg do\ncase \"$arg\" in push) mkfifo {} || exit 91; : > {}; exec /bin/cat {};; esac\ndone\nexec {} \"$@\"\n",
            quote(&fifo),
            quote(&entered),
            quote(&fifo),
            quote(&fixture.git)
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
                        fixture.import_attempt,
                        fixture.head,
                        Some(&cancel),
                        None,
                    )
                },
            )
        });
        let seen = provenant_test_support::wait_for(
            "import to start",
            Duration::from_secs(3),
            || entered.exists(),
            |seen| *seen,
        )
        .is_ok();
        let cancelled_at = clock.mono_now();
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
        assert!(
            clock
                .mono_now()
                .as_micros()
                .saturating_sub(cancelled_at.as_micros())
                < 10_000_000,
            "cancellation exceeded ten seconds"
        );
        assert!(seen, "never reached import");
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

fn blob_name(bytes: &[u8]) -> TestResult<[u8; 20]> {
    let mut object = format!("blob {}\0", bytes.len()).into_bytes();
    object.extend_from_slice(bytes);
    sha1_bytes(&object)
}

fn git_oid(name: [u8; 20]) -> TestResult<GitOid> {
    let hex = name
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(GitOid::parse(&hex)?)
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

#[derive(Clone, Copy, Debug)]
enum IncomingPack {
    Malformed,
    Duplicate,
    Oversized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnpackStatus {
    Accepted,
    Refused,
}

impl IncomingPack {
    fn build(self) -> TestResult<(Vec<u8>, GitOid, Vec<GitOid>)> {
        match self {
            Self::Malformed => {
                let first = b"first entry";
                let second = b"second entry";
                let first_name = blob_name(first)?;
                let second_name = blob_name(second)?;
                let entries = [
                    (first.as_slice(), first_name),
                    (second.as_slice(), second_name),
                ];
                let (mut pack, _) = pack_pair(&entries)?;
                // Corrupt the non-first entry's DEFLATE checksum, then fix the
                // outer digest so Git checks the object stream itself.
                let end = pack.len();
                pack[end - 21] ^= 0xff;
                let digest = sha1_bytes(&pack[..end - 20])?;
                pack[end - 20..].copy_from_slice(&digest);
                Ok((
                    pack,
                    git_oid(first_name)?,
                    vec![git_oid(first_name)?, git_oid(second_name)?],
                ))
            }
            Self::Duplicate => {
                let bytes = b"duplicate blob\n";
                let name = blob_name(bytes)?;
                let (pack, _) = pack_pair(&[(bytes, name), (bytes, name)])?;
                Ok((pack, git_oid(name)?, vec![git_oid(name)?]))
            }
            Self::Oversized => {
                let bytes = vec![b'x'; 4096];
                let name = blob_name(&bytes)?;
                let (pack, _) = pack_pair(&[(&bytes, name)])?;
                Ok((pack, git_oid(name)?, vec![git_oid(name)?]))
            }
        }
    }

    fn receiver_cap(self, pack: &[u8], request: &[u8]) -> TestResult<u64> {
        let pack_bytes = u64::try_from(pack.len())?;
        let request_bytes = u64::try_from(request.len())?;
        match self {
            Self::Oversized => pack_bytes.checked_sub(1).ok_or_else(|| "empty pack".into()),
            Self::Malformed | Self::Duplicate => request_bytes
                .checked_add(1)
                .ok_or_else(|| "request size overflow".into()),
        }
    }
}

/// Refused incoming packs stay quarantined: no new shared object, pack, or
/// import ref may be published, regardless of which typed boundary rejects it.
#[test]
fn refused_received_packs_never_publish_objects_or_refs() -> TestResult {
    assert_eq!(unpack_status(b"000eunpack ok\n")?, UnpackStatus::Accepted);
    assert!(
        unpack_status(b"0000").is_err(),
        "missing unpack status refused"
    );
    for case in [
        (IncomingPack::Malformed, UnpackStatus::Refused),
        (IncomingPack::Duplicate, UnpackStatus::Refused),
        (IncomingPack::Oversized, UnpackStatus::Refused),
    ] {
        let fixture = Fixture::new("sha1")?;
        let (pack, tip, incoming_objects) = case.0.build()?;
        let attack_pin = super::import_pin(AttemptId::from_bytes([0xa7; 16]), tip);
        let request = receive_request(&pack, tip, &attack_pin);
        let cap = case.0.receiver_cap(&pack, &request)?;
        let output = receive_pack(&fixture, &request, cap)?;
        assert_eq!(unpack_status(&output.stdout)?, case.1, "{case:?}");
        fixture.assert_not_published();
        assert!(
            !fixture.project.join(".git").join(&attack_pin).exists(),
            "{:?} published its import ref",
            case.0
        );
        for incoming_object in incoming_objects {
            assert!(
                !object_path(&fixture.common_objects(), incoming_object).exists(),
                "{:?} published incoming object {incoming_object}",
                case.0
            );
        }
    }
    Ok(())
}

fn unpack_status(response: &[u8]) -> TestResult<UnpackStatus> {
    let mut offset: usize = 0;
    while offset < response.len() {
        let header_end = offset.checked_add(4).ok_or("pkt-line offset overflow")?;
        let header = response
            .get(offset..header_end)
            .ok_or("truncated pkt-line header")?;
        let length = usize::from_str_radix(std::str::from_utf8(header)?, 16)?;
        match length {
            0 => break,
            1 | 2 => offset = header_end,
            3 => return Err("invalid pkt-line length".into()),
            _ => {
                if length < 4 {
                    return Err("invalid pkt-line length".into());
                }
                let packet_end = offset.checked_add(length).ok_or("pkt-line size overflow")?;
                let payload = response
                    .get(header_end..packet_end)
                    .ok_or("truncated pkt-line payload")?;
                if let Some(status) = payload.strip_prefix(b"unpack ") {
                    let status = status
                        .strip_suffix(b"\n")
                        .ok_or("invalid unpack status line")?;
                    if status.is_empty() {
                        return Err("empty unpack status".into());
                    }
                    return Ok(if status == b"ok" {
                        UnpackStatus::Accepted
                    } else {
                        UnpackStatus::Refused
                    });
                }
                offset = packet_end;
            }
        }
    }
    Err("missing unpack status".into())
}

fn receive_request(pack: &[u8], tip: GitOid, reference: &str) -> Vec<u8> {
    let command = format!("{} {} {reference}\0report-status\n", "0".repeat(40), tip);
    let mut request = format!("{:04x}", command.len() + 4).into_bytes();
    request.extend_from_slice(command.as_bytes());
    request.extend_from_slice(b"0000");
    request.extend_from_slice(pack);
    request
}

fn receive_pack(fixture: &Fixture, request: &[u8], cap: u64) -> TestResult<Output> {
    use std::{io::Write, process::Stdio};
    let mut child = provenant_os::fixture::git_command_at(&fixture.git)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "")
        .args(super::receiver_args(cap))
        .arg("--stateless-rpc")
        .arg(fixture.project.join(".git"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("receive input")?
        .write_all(request)?;
    Ok(child.wait_with_output()?)
}

#[test]
fn unmerged_private_index_without_merge_metadata_is_not_publishable() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (attempt, scratch) = fixture.overlay()?;
    let input = format!(
        "0 {}\tfile\n100644 {} 1\tfile\n100644 {} 2\tfile\n100644 {} 3\tfile\n",
        "0".repeat(40),
        fixture.base_blob,
        fixture.base_blob,
        fixture.blob
    );
    super::execute_bounded(
        &fixture.git,
        &scratch.join("git-common"),
        &fixture.lane,
        &super::args(&["update-index", "--index-info"]),
        None,
        super::Execution {
            input: Some(input.as_bytes()),
            ..Default::default()
        },
    )?;
    let host_index = fixture.worktree.repo().private_git_dir().join("index");
    let before = fs::read(&host_index)?;
    let source = super::bound_source(&attempt)?.ok_or("private source")?;
    assert!(super::merge_objects(&source, fixture.worktree.repo()).is_err());
    assert_eq!(fs::read(host_index)?, before);
    assert!(
        !fixture
            .git_overlay(&scratch, &["ls-files", "--unmerged"])?
            .stdout
            .is_empty()
    );
    Ok(())
}

#[test]
fn bounded_native_ref_lock_is_retried_until_git_publishes() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let repo = fixture.worktree.repo();
    let target = "refs/provenant/retry-lock";
    let lock = repo.common_git_dir().join(format!("{target}.lock"));
    fs::create_dir_all(lock.parent().ok_or("ref lock parent")?)?;
    fs::write(&lock, b"another Git mutation owns this lock")?;

    let marker = fixture.root.join("git-attempts");
    let wrapper = fixture.root.join("counting-git");
    let shell_quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf x >> {}\nexec {} \"$@\"\n",
            shell_quote(&marker),
            shell_quote(&fixture.git),
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;

    let release_lock = lock.clone();
    let wait_for_attempt = marker.clone();
    let releaser = std::thread::spawn(move || -> std::io::Result<()> {
        provenant_test_support::wait_for(
            "native Git lock retries",
            Duration::from_secs(2),
            || {
                fs::metadata(&wait_for_attempt)
                    .map(|metadata| metadata.len())
                    .unwrap_or_default()
            },
            |attempts| *attempts >= 2,
        )
        .map_err(std::io::Error::other)?;
        std::thread::sleep(Duration::from_millis(100));
        fs::remove_file(release_lock)
    });

    let result = super::execute_bounded(
        &wrapper,
        repo.common_git_dir(),
        repo.root_path(),
        &super::args(&["update-ref", target, &fixture.base.to_string()]),
        None,
        super::Execution {
            timeout: Duration::from_secs(5),
            ..Default::default()
        },
    );
    releaser
        .join()
        .map_err(|_| std::io::Error::other("native lock release thread panicked"))??;
    result?;

    assert!(
        fs::read(&marker)?.len() >= 2,
        "the host Git command should be retried after its real ref-lock collision"
    );
    assert_eq!(
        super::resolve_ref(&fixture.git, repo, target)?,
        fixture.base
    );
    Ok(())
}

#[test]
fn buffered_ref_retries_require_one_atomic_delete_transaction() {
    let valid = concat!(
        "start\n",
        "delete refs/provenant/import-extra/attempt/object\n",
        "prepare\n",
        "commit\n",
    )
    .as_bytes();
    let multiple = concat!(
        "start\n",
        "delete refs/provenant/import-extra/attempt/object\n",
        "prepare\n",
        "commit\n",
        "start\n",
        "delete refs/provenant/import-extra/attempt/object\n",
        "prepare\n",
        "commit\n",
    )
    .as_bytes();
    let mutating_prefix = concat!(
        "start\n",
        "delete refs/provenant/import-extra/attempt/object\n",
        "commit\n",
        "start\n",
        "delete refs/provenant/import-extra/attempt/object\n",
        "prepare\n",
        "commit\n",
    )
    .as_bytes();
    assert!(super::is_atomic_ref_transaction(valid));
    assert!(!super::is_atomic_ref_transaction(multiple));
    assert!(!super::is_atomic_ref_transaction(mutating_prefix));
}

#[test]
// Spec: docs/specs/run.md#lane-git-and-import
fn clear_import_refs_removes_every_attempt_pin_in_one_transaction() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let repo = fixture.worktree.repo();
    let sibling = AttemptId::from_bytes([0x5a; 16]);
    let oid = fixture.base.to_string();
    let own: Vec<String> = (1..=3)
        .map(|index| {
            format!(
                "refs/provenant/import-extra/{}/object-{index}",
                fixture.import_attempt
            )
        })
        .collect();
    let foreign = format!("refs/provenant/import-extra/{sibling}/object");
    for reference in own.iter().chain(std::iter::once(&foreign)) {
        super::command(
            &fixture.git,
            repo.common_git_dir(),
            &fixture.lane,
            &super::args(&["update-ref", reference, &oid]),
            None,
        )?;
    }
    let marker = fixture.root.join("update-ref-attempts");
    let wrapper = fixture.root.join("counting-git");
    let shell_quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    // The executor places `--git-dir` and `-c` options before the subcommand,
    // so count `update-ref` wherever it appears rather than assuming `$1`.
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nfor arg do\n  if [ \"$arg\" = update-ref ]; then printf x >> {}; fi\ndone\nexec {} \"$@\"\n",
            shell_quote(&marker),
            shell_quote(&fixture.git),
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;

    super::clear_import_refs(&wrapper, repo, fixture.import_attempt)?;

    assert_eq!(
        fs::read(&marker)?.len(),
        1,
        "an attempt's pins must be deleted in one batched update-ref process"
    );
    for reference in &own {
        assert!(
            super::command(
                &fixture.git,
                repo.common_git_dir(),
                &fixture.lane,
                &super::args(&["for-each-ref", reference]),
                None,
            )?
            .is_empty(),
            "{reference} survived cleanup"
        );
    }
    assert!(
        !super::command(
            &fixture.git,
            repo.common_git_dir(),
            &fixture.lane,
            &super::args(&["for-each-ref", &foreign]),
            None,
        )?
        .is_empty(),
        "another attempt's pin was removed"
    );
    Ok(())
}

#[test]
fn stopped_merge_retains_private_state_without_adoption_by_a_new_attempt() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    fs::write(fixture.project.join("file"), "conflicting main change\n")?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["add", "file"],
    )?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["commit", "-qm", "main change"],
    )?;
    let merge_head = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", "HEAD"],
    )?)?;
    let (attempt, scratch) = fixture.overlay()?;
    let merge =
        fixture.git_overlay_raw(&scratch, &["merge", "--no-edit", &merge_head.to_string()])?;
    assert!(!merge.status.success(), "fixture merge must conflict");
    fs::write(fixture.lane.join("staged.txt"), "private staged content\n")?;
    fixture.git_overlay(&scratch, &["add", "staged.txt"])?;
    let staged_blob = oid(fixture.git_overlay(&scratch, &["rev-parse", ":staged.txt"])?)?;
    let metadata = scratch.join("git-common/MERGE_HEAD");
    let parent = fs::read(&metadata)?;
    assert_eq!(std::str::from_utf8(&parent)?.trim(), merge_head.to_string());
    let stages = fixture
        .git_overlay(&scratch, &["ls-files", "--unmerged", "-z"])?
        .stdout;
    assert!(!stages.is_empty());
    let working = fs::read(fixture.lane.join("file"))?;
    assert!(working.windows(7).any(|bytes| bytes == b"<<<<<<<"));
    let host_index = fixture.worktree.repo().private_git_dir().join("index");
    let source = super::bound_source(&attempt)?.ok_or("private source")?;
    let objects = super::merge_objects(&source, fixture.worktree.repo())?;
    let private_blob = object_path(&fixture.source, staged_blob);
    let original_blob = fs::read(&private_blob)?;
    fs::set_permissions(&private_blob, fs::Permissions::from_mode(0o600))?;
    fs::write(&private_blob, b"corrupt object")?;
    assert!(
        super::settle_many(
            &fixture.git,
            fixture.worktree.repo(),
            Some(&source.objects),
            fixture.head,
            &objects,
            None,
            super::ImportScope {
                attempt: fixture.import_attempt,
                custody: None,
            },
        )
        .is_err()
    );
    assert!(
        super::command(
            &fixture.git,
            fixture.worktree.repo().common_git_dir(),
            &fixture.lane,
            &super::args(&["cat-file", "-e", &format!("{staged_blob}^{{blob}}")]),
            None,
        )
        .is_err()
    );
    fs::write(&private_blob, original_blob)?;
    super::settle_many(
        &fixture.git,
        fixture.worktree.repo(),
        Some(&source.objects),
        fixture.head,
        &objects,
        None,
        super::ImportScope {
            attempt: fixture.import_attempt,
            custody: None,
        },
    )?;
    super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: fixture.head,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?;
    super::sync_native_index(fixture.worktree.repo(), fixture.base);
    let import_attempt = provenant_core::AttemptId::from_bytes([0; 16]);
    super::clear_import_refs(&fixture.git, fixture.worktree.repo(), import_attempt)?;
    assert!(
        super::command(
            &fixture.git,
            fixture.worktree.repo().common_git_dir(),
            &fixture.lane,
            &super::args(&[
                "for-each-ref",
                &format!("refs/provenant/import-extra/{import_attempt}")
            ]),
            None
        )?
        .is_empty()
    );
    super::command(
        &fixture.git,
        fixture.worktree.repo().common_git_dir(),
        &fixture.lane,
        &super::args(&["cat-file", "-e", &format!("{staged_blob}^{{blob}}")]),
        None,
    )?;
    assert!(host_index.is_file());
    assert!(
        !fixture
            .worktree
            .repo()
            .private_git_dir()
            .join("MERGE_HEAD")
            .exists()
    );
    assert!(
        super::command(
            &fixture.git,
            fixture.worktree.repo().private_git_dir(),
            &fixture.lane,
            &super::args(&["ls-files", "--unmerged", "-z"]),
            None
        )?
        .is_empty()
    );
    assert_eq!(fs::read(&metadata)?, parent);
    assert_eq!(
        fixture
            .git_overlay(&scratch, &["ls-files", "--unmerged", "-z"])?
            .stdout,
        stages
    );
    assert_eq!(fs::read(fixture.lane.join("file"))?, working);
    let native_before = fs::read(&host_index)?;
    let next_attempt_path = fixture.root.join("continuation");
    let next_scratch = next_attempt_path.join("tmp/candidate-0");
    fs::create_dir_all(&next_scratch)?;
    let next_attempt = Directory::open_anchor(&next_attempt_path)?;
    super::prepare_bound(
        &fixture.git,
        fixture.worktree.repo(),
        &next_attempt,
        &next_scratch,
        "refs/heads/lane",
        fixture.head,
    )?;
    assert!(
        fixture
            .git_overlay(&next_scratch, &["ls-files", "--unmerged", "-z"])?
            .stdout
            .is_empty()
    );
    assert!(!next_scratch.join("git-common/MERGE_HEAD").exists());
    assert_eq!(fs::read(&host_index)?, native_before);
    assert_eq!(fs::read(&metadata)?, parent);
    assert!(
        !fixture
            .worktree
            .repo()
            .private_git_dir()
            .join("MERGE_HEAD")
            .exists()
    );
    assert_eq!(
        super::command(
            &fixture.git,
            fixture.worktree.repo().private_git_dir(),
            &fixture.lane,
            &super::args(&["ls-files", "--unmerged", "-z"]),
            None
        )?,
        Vec::<u8>::new(),
    );
    Ok(())
}

#[test]
fn native_index_advance_keeps_private_flags_and_exposes_worktree_edits() -> TestResult {
    for options in [
        vec!["--skip-worktree"],
        vec!["--assume-unchanged"],
        vec!["--skip-worktree", "--assume-unchanged"],
    ] {
        let mut fixture = Fixture::new("sha1")?;
        let (_attempt, scratch) = fixture.overlay()?;
        for flag in &options {
            fixture.git_overlay(&scratch, &["update-index", flag, "file"])?;
        }
        let hidden = fixture.git_overlay(&scratch, &["ls-files", "-v"])?;
        let expected = match options.as_slice() {
            ["--skip-worktree"] => b"S file\n".as_slice(),
            ["--assume-unchanged"] => b"h file\n".as_slice(),
            _ => b"s file\n".as_slice(),
        };
        assert_eq!(hidden.stdout, expected);
        fixture.import()?;
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head: fixture.head,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
        )?;
        super::sync_native_index(fixture.worktree.repo(), fixture.base);
        assert_eq!(
            fixture.git_overlay(&scratch, &["ls-files", "-v"])?.stdout,
            expected
        );
        fs::write(fixture.lane.join("file"), "hidden uncommitted edit\n")?;
        let flags = git(
            &fixture.git,
            &fixture.home,
            &fixture.lane,
            None,
            &["ls-files", "-v"],
        )?;
        assert_eq!(flags.stdout, b"H file\n");
        let status = git(
            &fixture.git,
            &fixture.home,
            &fixture.lane,
            None,
            &["status", "--porcelain"],
        )?;
        assert_eq!(status.stdout, b" M file\n");
    }
    Ok(())
}

fn publish_tip(
    fixture: &Fixture,
    head: GitOid,
    attempt: AttemptId,
) -> TestResult<super::Published> {
    Ok(super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head,
            attempt,
            history: LaneHistory::default(),
        },
    )?)
}

/// A sibling lane's commit changing `file`, built through the native index and
/// worktree, which are restored so the test controls where the index stands.
fn sibling_commit(fixture: &Fixture, parent: GitOid, content: &str) -> TestResult<GitOid> {
    let native = |args: &[&str]| git(&fixture.git, &fixture.home, &fixture.lane, None, args);
    let index = fixture.worktree.repo().private_git_dir().join("index");
    let saved_index = fs::read(&index)?;
    let saved_file = fs::read(fixture.lane.join("file"))?;
    fs::write(fixture.lane.join("file"), content)?;
    native(&["add", "file"])?;
    let tree = String::from_utf8(native(&["write-tree"])?.stdout)?;
    let commit = oid(native(&[
        "commit-tree",
        tree.trim(),
        "-p",
        &parent.to_string(),
        "-m",
        "sibling lane",
    ])?)?;
    fs::write(&index, saved_index)?;
    fs::write(fixture.lane.join("file"), saved_file)?;
    Ok(commit)
}

fn assert_native_index_follows(fixture: &Fixture, tip: GitOid, content: &[u8]) -> TestResult {
    let native = |args: &[&str]| git(&fixture.git, &fixture.home, &fixture.lane, None, args);
    assert_eq!(
        native(&["diff", "--cached", "--name-only"])?.stdout,
        Vec::<u8>::new(),
        "the native index must match the published tip"
    );
    assert_eq!(native(&["show", ":file"])?.stdout, content);
    assert_eq!(
        super::followed_native_index(fixture.worktree.repo())?,
        Some(tip)
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn a_native_index_changed_since_the_record_is_left_alone_instead_of_merged_from_a_stale_commit()
-> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (_attempt, _scratch) = fixture.overlay()?;
    fixture.import()?;
    super::seed_native_index(fixture.worktree.repo(), fixture.base)?;
    let first = publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    assert_eq!(
        super::sync_native_index(fixture.worktree.repo(), first.previous),
        super::IndexSync::Synced
    );
    // A person restages `file` in the shared worktree after the index followed
    // the first tip; the record still describes the index Provenant left.
    let native = |args: &[&str]| git(&fixture.git, &fixture.home, &fixture.lane, None, args);
    fs::write(fixture.lane.join("file"), "person staged blob\n")?;
    native(&["add", "file"])?;
    let index = fixture.worktree.repo().private_git_dir().join("index");
    let person_index = fs::read(&index)?;
    let second_head = sibling_commit(&fixture, fixture.head, "second lane blob\n")?;
    publish_tip(&fixture, second_head, AttemptId::from_bytes([1; 16]))?;
    let super::IndexSync::OutOfSync(report) =
        super::sync_native_index(fixture.worktree.repo(), fixture.base)
    else {
        return Err("a changed index must not be merged from the recorded commit".into());
    };
    assert_eq!(report.followed, Some(fixture.head));
    assert_eq!(report.target, Some(second_head));
    assert_eq!(report.paths, vec!["file".to_owned()]);
    assert_eq!(fs::read(&index)?, person_index);
    assert_eq!(
        super::followed_native_index(fixture.worktree.repo())?,
        Some(fixture.head)
    );
    // The advertised remedy repairs the index and the record together.
    let remedy = std::process::Command::new("sh")
        .arg("-c")
        .arg(super::NATIVE_INDEX_REMEDY)
        .current_dir(&fixture.lane)
        .env("HOME", &fixture.home)
        .status()?;
    assert!(remedy.success());
    assert_eq!(
        super::sync_native_index(fixture.worktree.repo(), fixture.base),
        super::IndexSync::Synced
    );
    assert_native_index_follows(&fixture, second_head, b"second lane blob\n")
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn native_index_record_and_inspection_failures_are_typed_results_that_change_nothing() -> TestResult
{
    let mut fixture = Fixture::new("sha1")?;
    let (_attempt, _scratch) = fixture.overlay()?;
    fixture.import()?;
    super::seed_native_index(fixture.worktree.repo(), fixture.base)?;
    publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    let private = fixture.worktree.repo().private_git_dir().to_path_buf();
    let record = private.join("provenant-native-index.json");
    let index = private.join("index");
    let good_record = fs::read(&record)?;
    let good_index = fs::read(&index)?;
    // A corrupt record is unknown: nothing advances and nothing is rewritten.
    fs::write(&record, "{ not json")?;
    let corrupt = super::sync_native_index(fixture.worktree.repo(), fixture.base);
    assert!(
        matches!(corrupt, super::IndexSync::OutOfSync(_)),
        "{corrupt:?}"
    );
    assert_eq!(fs::read(&record)?, b"{ not json");
    assert_eq!(fs::read(&index)?, good_index);
    // A missing index is unknown as well.
    fs::write(&record, &good_record)?;
    fs::remove_file(&index)?;
    let missing = super::sync_native_index(fixture.worktree.repo(), fixture.base);
    assert!(
        matches!(missing, super::IndexSync::OutOfSync(_)),
        "{missing:?}"
    );
    assert_eq!(fs::read(&record)?, good_record);
    assert!(!index.exists());
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn sequential_publications_touching_one_path_advance_the_native_index_from_the_recorded_commit()
-> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (_attempt, _scratch) = fixture.overlay()?;
    fixture.import()?;
    super::seed_native_index(fixture.worktree.repo(), fixture.base)?;
    let first = publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    assert_eq!(first.previous, fixture.base);
    assert_eq!(
        super::sync_native_index(fixture.worktree.repo(), first.previous),
        super::IndexSync::Synced
    );
    let second_head = sibling_commit(&fixture, fixture.head, "second lane blob\n")?;
    let second = publish_tip(&fixture, second_head, AttemptId::from_bytes([1; 16]))?;
    assert_eq!(second.previous, fixture.head);
    // The launch base is stale after the first publication; the record is not.
    assert_eq!(
        super::sync_native_index(fixture.worktree.repo(), fixture.base),
        super::IndexSync::Synced
    );
    assert_native_index_follows(&fixture, second_head, b"second lane blob\n")
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn a_publication_that_crashed_before_the_index_advance_resumes_from_the_recorded_commit()
-> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (_attempt, _scratch) = fixture.overlay()?;
    fixture.import()?;
    super::seed_native_index(fixture.worktree.repo(), fixture.base)?;
    let first = publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    super::sync_native_index(fixture.worktree.repo(), first.previous);
    // B publishes on top of A and dies before advancing the index.
    let second_head = sibling_commit(&fixture, fixture.head, "second lane blob\n")?;
    let b = AttemptId::from_bytes([1; 16]);
    publish_tip(&fixture, second_head, b)?;
    // On resume the branch is already at B's tip, so the replaced tip is gone
    // and the launch base is the only fallback. The record still drives it.
    let resumed = publish_tip(&fixture, second_head, b)?;
    assert_eq!(resumed.previous, second_head);
    assert_eq!(
        super::sync_native_index(fixture.worktree.repo(), fixture.base),
        super::IndexSync::Synced
    );
    assert_native_index_follows(&fixture, second_head, b"second lane blob\n")
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn a_sibling_advancing_past_a_crashed_publication_leaves_nothing_for_it_to_revert() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (_attempt, _scratch) = fixture.overlay()?;
    fixture.import()?;
    super::seed_native_index(fixture.worktree.repo(), fixture.base)?;
    // A's compare-and-swap lands, then A dies before advancing the index.
    publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    // B publishes on A's tip and advances the index; it started at A's tip,
    // but the index never followed A, so B's advance must start from the record.
    let second_head = sibling_commit(&fixture, fixture.head, "second lane blob\n")?;
    let b = publish_tip(&fixture, second_head, AttemptId::from_bytes([1; 16]))?;
    assert_eq!(b.previous, fixture.head);
    assert_eq!(
        super::sync_native_index(fixture.worktree.repo(), b.previous),
        super::IndexSync::Synced
    );
    assert_native_index_follows(&fixture, second_head, b"second lane blob\n")?;
    // A resumes: its publication is already contained, and there is nothing
    // left to advance or to revert.
    let index = fixture.worktree.repo().private_git_dir().join("index");
    let before = fs::read(&index)?;
    publish_tip(&fixture, fixture.head, fixture.import_attempt)?;
    assert_eq!(
        super::sync_native_index(fixture.worktree.repo(), fixture.base),
        super::IndexSync::Synced
    );
    assert_eq!(fs::read(&index)?, before);
    assert_native_index_follows(&fixture, second_head, b"second lane blob\n")
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn native_index_advance_preserves_staged_snapshots_on_conflicts_and_disjoint_updates() -> TestResult
{
    for (path, advances, busy) in [
        ("file", false, false),
        ("native-only", true, false),
        ("file", false, true),
    ] {
        let mut fixture = Fixture::new("sha1")?;
        let (_attempt, scratch) = fixture.overlay()?;
        fs::write(fixture.lane.join(path), "native staged snapshot\n")?;
        git(
            &fixture.git,
            &fixture.home,
            &fixture.lane,
            None,
            &["add", path],
        )?;
        fs::write(fixture.lane.join(path), "working version after staging\n")?;
        let index = fixture.worktree.repo().private_git_dir().join("index");
        let staged_index = fs::read(&index)?;
        let private_index = fs::read(scratch.join("git-common/index"))?;
        fixture.import()?;
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head: fixture.head,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
        )?;
        let lock = fixture.worktree.repo().private_git_dir().join("index.lock");
        if busy {
            fs::write(&lock, "another Git writer owns this lock\n")?;
        }
        super::seed_native_index(fixture.worktree.repo(), fixture.base)?;
        let synced = super::sync_native_index(fixture.worktree.repo(), fixture.base);
        assert_eq!(matches!(synced, super::IndexSync::Synced), advances);
        if !advances {
            // A refused merge leaves the index alone, never silently diverges:
            // it reports the paths the branch moved and keeps the record.
            let super::IndexSync::OutOfSync(report) = synced else {
                return Err("a refused merge reports out-of-sync".into());
            };
            assert_eq!(report.followed, Some(fixture.base));
            assert_eq!(report.target, Some(fixture.head));
            assert_eq!(report.paths, vec!["file".to_owned()]);
            assert_eq!(
                super::followed_native_index(fixture.worktree.repo())?,
                Some(fixture.base)
            );
            assert_eq!(fs::read(&index)?, staged_index);
        }
        if busy {
            assert_eq!(fs::read(&lock)?, b"another Git writer owns this lock\n");
        }
        assert_eq!(fs::read(scratch.join("git-common/index"))?, private_index);
        assert_eq!(
            git(
                &fixture.git,
                &fixture.home,
                &fixture.lane,
                None,
                &["show", &format!(":{path}")]
            )?
            .stdout,
            b"native staged snapshot\n"
        );
        assert_eq!(
            fixture.git_overlay(&scratch, &["show", ":file"])?.stdout,
            b"private lane blob\n"
        );
        assert_eq!(
            fs::read(fixture.lane.join(path))?,
            b"working version after staging\n"
        );
        if advances {
            assert_eq!(
                git(
                    &fixture.git,
                    &fixture.home,
                    &fixture.lane,
                    None,
                    &["show", ":file"]
                )?
                .stdout,
                b"private lane blob\n"
            );
        }
    }
    Ok(())
}

#[test]
fn publication_rechecks_already_published_branch_before_removing_import_ref() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let (attempt, _) = fixture.overlay()?;
    let source = super::bound_source(&attempt)?.ok_or("private view")?;
    super::settle(
        &fixture.git,
        fixture.worktree.repo(),
        Some(&source.objects),
        fixture.head,
    )?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["update-ref", "refs/heads/lane", &fixture.head.to_string()],
    )?;
    let wrapper = fixture.root.join("moving-git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n'{}' \"$@\" || exit $?\nfor arg do\n  if [ \"$arg\" = merge-base ]; then '{}' --git-dir '{}' update-ref refs/heads/lane '{}'; break; fi\ndone\n",
            fixture.git.display(),
            fixture.git.display(),
            fixture.project.join(".git").display(),
            fixture.base
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
    assert!(
        super::publish_branch(
            &wrapper,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: fixture.base,
                launch_ref: None,
                head: fixture.head,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
        )
        .is_err()
    );
    assert_eq!(
        super::resolve_ref(
            &fixture.git,
            fixture.worktree.repo(),
            &super::import_pin(fixture.import_attempt, fixture.head)
        )?,
        fixture.head
    );
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        fixture.base
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-settlement.md#settlement
fn unrooted_history_warns_by_default_and_each_configured_value_decides() -> TestResult {
    use provenant_config::{HistoryEffect, Setting};
    use provenant_records::LimitSource;
    let fixture = Fixture::new("sha1")?;
    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    let prior = child_of(&fixture, fixture.base, "unique prior branch work")?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["update-ref", "refs/heads/lane", &prior.to_string()],
    )?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "update-ref",
            "refs/provenant/lane-base/test",
            &prior.to_string(),
        ],
    )?;
    fs::write(fixture.project.join("file"), "different orphan contents\n")?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["add", "file"],
    )?;
    let orphan_tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["write-tree"],
    )?)?;
    assert_ne!(tree, orphan_tree);
    let orphan = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["commit-tree", &orphan_tree.to_string(), "-m", "orphan"],
    )?)?;
    super::settle(
        &fixture.git,
        fixture.worktree.repo(),
        Some(&Directory::open_anchor(&fixture.source)?),
        orphan,
    )?;
    let history = |value| LaneHistory {
        unrooted_tip: Setting {
            value,
            source: LimitSource::Project,
        },
        ..LaneHistory::default()
    };
    let refusal = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: prior,
            launch_ref: None,
            head: orphan,
            attempt: fixture.import_attempt,
            history: history(HistoryEffect::Refuse),
        },
    );
    assert!(matches!(
        refusal,
        Err(crate::RunError::LaneTipUnrooted(ref cause))
            if cause.tip == orphan
                && cause.base == prior
                && cause.launch_ref.is_none()
                && cause.setting.contains("lanes.history.unrooted_tip = refuse")
                && cause.setting.contains("source: project")
    ));
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        prior
    );
    for value in [HistoryEffect::Warn, HistoryEffect::Off] {
        // Each value is judged from the same prior tip; otherwise the previous
        // iteration's published tip would count as the known host lane branch.
        update_ref(&fixture, "refs/heads/lane", prior)?;
        let ancestor = super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: prior,
                launch_ref: None,
                head: orphan,
                attempt: fixture.import_attempt,
                history: history(value),
            },
        )?
        .ancestor;
        assert_eq!(ancestor, super::Ancestor::Unrooted);
        assert_eq!(
            super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
            orphan
        );
    }
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["update-ref", "-d", "refs/provenant/lane-base/test"],
    )?;
    fixture.collect_shared_objects()?;
    assert_eq!(
        super::resolve_ref(
            &fixture.git,
            fixture.worktree.repo(),
            &format!("refs/provenant/recovery/{prior}")
        )?,
        prior
    );
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["cat-file", "-e", &format!("{prior}^{{commit}}")],
    )?;
    Ok(())
}

#[test]
// Spec: docs/specs/run-settlement.md#settlement
fn publication_pins_symbolic_recovery_ref_through_collection() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let prior = child_of(&fixture, fixture.base, "displaced work")?;
    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    let orphan = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["commit-tree", &tree.to_string(), "-m", "orphan"],
    )?)?;
    let recovery = format!("refs/provenant/recovery/{prior}");
    let target = "refs/heads/recovery-target";
    let pin = "refs/provenant/lane-base/test";
    for reference in ["refs/heads/lane", target, pin] {
        update_ref(&fixture, reference, prior)?;
    }
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["symbolic-ref", &recovery, target],
    )?;
    super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: prior,
            launch_ref: None,
            head: orphan,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?;
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), target)?,
        prior
    );
    update_ref(&fixture, target, orphan)?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["update-ref", "-d", pin],
    )?;
    fixture.collect_shared_objects()?;
    let refs = git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["for-each-ref", "--format=%(symref)", &recovery],
    )?;
    assert_eq!(refs.stdout, b"\n");
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), &recovery)?,
        prior
    );
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["cat-file", "-e", &format!("{prior}^{{commit}}")],
    )?;
    Ok(())
}

#[test]
// Spec: docs/specs/run-settlement.md#settlement
fn publication_refuses_conflicting_recovery_ref_atomically() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let prior = child_of(&fixture, fixture.base, "displaced work")?;
    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    let orphan = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["commit-tree", &tree.to_string(), "-m", "orphan"],
    )?)?;
    let recovery = format!("refs/provenant/recovery/{prior}");
    update_ref(&fixture, "refs/heads/lane", prior)?;
    update_ref(&fixture, &recovery, fixture.base)?;
    assert!(
        super::publish_branch(
            &fixture.git,
            fixture.worktree.repo(),
            super::BranchPublication {
                branch: "refs/heads/lane",
                base: prior,
                launch_ref: None,
                head: orphan,
                attempt: fixture.import_attempt,
                history: LaneHistory::default(),
            },
        )
        .is_err()
    );
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        prior
    );
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), &recovery)?,
        fixture.base
    );
    Ok(())
}

// Child of `parent` with the base tree, so a rebase needs no checkout.
fn child_of(fixture: &Fixture, parent: GitOid, message: &str) -> TestResult<GitOid> {
    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "commit-tree",
            &tree.to_string(),
            "-p",
            &parent.to_string(),
            "-m",
            message,
        ],
    )?)
}

// The base tree with `files` written over it, staged through the host index so
// a test never checks out.
fn tree_with(fixture: &Fixture, files: &[(&str, &str)]) -> TestResult<GitOid> {
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["read-tree", &fixture.base.to_string()],
    )?;
    for &(path, contents) in files {
        fs::write(fixture.project.join(path), contents)?;
        git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["add", "--", path],
        )?;
    }
    oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["write-tree"],
    )?)
}

fn commit_of(
    fixture: &Fixture,
    tree: GitOid,
    parent: Option<GitOid>,
    message: &str,
) -> TestResult<GitOid> {
    let tree = tree.to_string();
    let parent = parent.map(|parent| parent.to_string());
    let mut args = vec!["commit-tree", tree.as_str()];
    if let Some(parent) = parent.as_deref() {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", message]);
    oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &args,
    )?)
}

fn update_ref(fixture: &Fixture, reference: &str, value: GitOid) -> TestResult {
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["update-ref", reference, &value.to_string()],
    )?;
    Ok(())
}

#[test]
fn unrooted_tip_refuses_only_under_its_refuse_setting() -> TestResult {
    use provenant_config::{HistoryEffect, Setting};
    use provenant_records::LimitSource;
    let fixture = Fixture::new("sha1")?;
    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    let orphan = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["commit-tree", &tree.to_string(), "-m", "orphan"],
    )?)?;
    let history = LaneHistory {
        unrooted_tip: Setting {
            value: HistoryEffect::Refuse,
            source: LimitSource::Host,
        },
        ..LaneHistory::default()
    };
    let error = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: Some("integration"),
            head: orphan,
            attempt: fixture.import_attempt,
            history,
        },
    )
    .expect_err("an unrooted tip is refused");
    assert!(error.import_refusal_is_final());
    let message = error.to_string();
    assert!(message.contains(&orphan.to_string()), "{message}");
    assert!(message.contains(&fixture.base.to_string()), "{message}");
    assert!(message.contains("launch ref integration"), "{message}");
    assert!(
        message.contains("lanes.history.unrooted_tip = refuse (source: host)"),
        "{message}"
    );
    // A lost branch CAS depends on a branch that can move, so a retry can
    // change its answer: it must stay retryable.
    assert!(!super::invalid().import_refusal_is_final());
    Ok(())
}

#[test]
fn successful_git_import_with_graft_hint_keeps_result_and_other_stderr_refuses() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let wrapper = fixture.root.join("git-with-hint");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' 'hint: Support for <GIT_DIR>/info/grafts is deprecated' >&2\nexec '{}' \"$@\"\n",
            fixture.git.display()
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
    super::require_commit(
        &wrapper,
        fixture.worktree.repo().common_git_dir(),
        fixture.worktree.repo().root_path(),
        fixture.base,
        None,
        None,
    )?;
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' 'error: corrupt object' >&2\nexec '{}' \"$@\"\n",
            fixture.git.display()
        ),
    )?;
    assert!(
        super::require_commit(
            &wrapper,
            fixture.worktree.repo().common_git_dir(),
            fixture.worktree.repo().root_path(),
            fixture.base,
            None,
            None,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn git_advice_is_stripped_from_stderr() {
    let stderr = b"hint: Support for <GIT_DIR>/info/grafts is deprecated\nhint: and will be removed in a future Git version.\n";
    assert!(super::only_git_advice(stderr));
    assert!(super::strip_git_advice(stderr).is_empty());

    let mixed = b"hint: advice line\nwarning: non-advice warning\nhint: another hint\n";
    assert!(!super::only_git_advice(mixed));
    assert_eq!(
        super::strip_git_advice(mixed),
        b"warning: non-advice warning"
    );
}

#[test]
fn every_publication_refusal_is_final_for_settlement() {
    let oid = GitOid::Sha1([7; 20]);
    let refusals = [
        crate::RunError::PublicationDiverged {
            setting: "lanes.history.rebase".into(),
        },
        crate::RunError::PublicationConflict {
            conflicting_files: Vec::new(),
            setting: "lanes.history.rebase".into(),
        },
        crate::RunError::PublicationUnrelated {
            setting: "lanes.history.rebase".into(),
        },
        crate::RunError::PublicationMergeDriver {
            setting: "lanes.history.rebase".into(),
        },
        crate::RunError::PublicationRewound {
            branch_tip: oid,
            base: oid,
        },
        crate::RunError::LaneTipUnrooted(Box::new(crate::LaneTipRefusal {
            tip: oid,
            base: oid,
            launch_ref: None,
            setting: "lanes.history.unrooted_tip".into(),
        })),
    ];
    for error in refusals {
        assert!(error.is_publication_refusal(), "{error}");
        // One definition: settlement seals a publication refusal at once
        // instead of retrying a branch its own retry cannot change, and retains
        // the private Git for explicit host recovery.
        assert!(error.import_refusal_is_final(), "{error}");
    }
    // A lost branch CAS depends on a branch that can move, so it stays
    // retryable and is never a publication refusal.
    assert!(!super::invalid().import_refusal_is_final());
    assert!(!super::invalid().is_publication_refusal());
}

#[test]
fn publication_accepts_a_rebase_onto_a_pushed_non_default_branch() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let lane_base = child_of(&fixture, fixture.base, "lane start")?;
    let feature = child_of(&fixture, fixture.base, "pushed feature branch")?;
    let rebased = child_of(&fixture, feature, "rebased lane")?;
    update_ref(&fixture, "refs/heads/lane", lane_base)?;
    update_ref(&fixture, "refs/remotes/origin/feature", feature)?;
    let ancestor = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: lane_base,
            launch_ref: None,
            head: rebased,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?
    .ancestor;
    assert_eq!(
        ancestor,
        super::Ancestor::Known {
            reference: "refs/remotes/origin/feature".into(),
            oid: feature
        }
    );
    let warning = ancestor
        .warning("refs/heads/lane", lane_base, rebased)
        .ok_or("a rebase warns")?;
    assert!(warning.contains("refs/remotes/origin/feature"), "{warning}");
    assert!(warning.contains(&lane_base.to_string()), "{warning}");
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        rebased
    );
    let reflog = git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["reflog", "show", "--format=%gs", "refs/heads/lane"],
    )?;
    assert!(
        String::from_utf8_lossy(&reflog.stdout)
            .contains("descends from refs/remotes/origin/feature"),
        "{}",
        String::from_utf8_lossy(&reflog.stdout)
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-settlement.md#settlement
fn publication_accepts_a_rebase_onto_the_launch_ref() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let lane_base = child_of(&fixture, fixture.base, "lane start")?;
    let integration = child_of(&fixture, fixture.base, "integration moved on")?;
    let rebased = child_of(&fixture, integration, "rebased lane")?;
    update_ref(&fixture, "refs/heads/lane", lane_base)?;
    update_ref(&fixture, "refs/heads/integration", integration)?;
    let ancestor = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: lane_base,
            launch_ref: Some("integration"),
            head: rebased,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?
    .ancestor;
    assert_eq!(
        ancestor,
        super::Ancestor::Known {
            reference: "integration".into(),
            oid: integration
        }
    );
    // Without the launch ref the same tip is unrooted: the refuse setting
    // stops it, while the default warns and publishes it with recovery for
    // displaced work.
    update_ref(&fixture, "refs/heads/lane", lane_base)?;
    let refuse = LaneHistory {
        unrooted_tip: provenant_config::Setting {
            value: provenant_config::HistoryEffect::Refuse,
            source: provenant_records::LimitSource::Project,
        },
        ..LaneHistory::default()
    };
    let refusal = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: lane_base,
            launch_ref: None,
            head: rebased,
            attempt: fixture.import_attempt,
            history: refuse,
        },
    );
    assert!(matches!(
        refusal,
        Err(crate::RunError::LaneTipUnrooted(ref cause))
            if cause.tip == rebased && cause.base == lane_base && cause.launch_ref.is_none()
    ));
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        lane_base
    );
    let ancestor = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: lane_base,
            launch_ref: None,
            head: rebased,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?
    .ancestor;
    assert_eq!(ancestor, super::Ancestor::Unrooted);
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        rebased
    );
    Ok(())
}

#[test]
fn publication_from_the_launch_base_reports_no_ancestor_warning() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let tip = child_of(&fixture, fixture.base, "lane work")?;
    let ancestor = super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: fixture.base,
            launch_ref: None,
            head: tip,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?
    .ancestor;
    assert_eq!(ancestor, super::Ancestor::LaunchBase);
    assert_eq!(ancestor.warning("refs/heads/lane", fixture.base, tip), None);
    Ok(())
}

#[test]
fn publication_accepts_a_rebase_onto_the_current_origin_default() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let tree = oid(git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    )?)?;
    let commit = |parent: GitOid, message: &str| -> TestResult<GitOid> {
        oid(git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &[
                "commit-tree",
                &tree.to_string(),
                "-p",
                &parent.to_string(),
                "-m",
                message,
            ],
        )?)
    };
    let lane_base = commit(fixture.base, "lane start")?;
    let origin = commit(fixture.base, "origin update")?;
    let rebased = commit(origin, "rebased lane")?;
    for (reference, value) in [
        ("refs/heads/lane", lane_base),
        ("refs/remotes/origin/main", origin),
    ] {
        git(
            &fixture.git,
            &fixture.home,
            &fixture.project,
            None,
            &["update-ref", reference, &value.to_string()],
        )?;
    }
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    super::publish_branch(
        &fixture.git,
        fixture.worktree.repo(),
        super::BranchPublication {
            branch: "refs/heads/lane",
            base: lane_base,
            launch_ref: None,
            head: rebased,
            attempt: fixture.import_attempt,
            history: LaneHistory::default(),
        },
    )?;
    assert_eq!(
        super::resolve_ref(&fixture.git, fixture.worktree.repo(), "refs/heads/lane")?,
        rebased
    );
    Ok(())
}

#[test]
fn import_sweeps_only_aged_quarantine_and_staging_directories() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let common = fixture.project.join(".git");
    let old_quarantine = common.join("objects/incoming-stale");
    let old_stage = common.join("tmp-provenant-import-stale");
    let legacy_stage = common.join(".provenant-staging.old");
    let fresh_quarantine = common.join("objects/incoming-active");
    let fresh_stage = common.join("tmp-provenant-import-active");
    let unrelated = common.join("tmp-unrelated");
    let old = std::time::SystemTime::now() - Duration::from_secs(48 * 60 * 60);
    for path in [
        &old_quarantine,
        &old_stage,
        &legacy_stage,
        &fresh_quarantine,
        &fresh_stage,
        &unrelated,
    ] {
        fs::create_dir(path)?;
    }
    for path in [&old_quarantine, &old_stage, &legacy_stage, &unrelated] {
        fs::File::open(path)?.set_times(fs::FileTimes::new().set_modified(old))?;
    }
    fixture.import()?;
    for path in [&old_quarantine, &old_stage, &legacy_stage] {
        assert!(!path.exists(), "{}", path.display());
    }
    for path in [&fresh_quarantine, &fresh_stage, &unrelated] {
        assert!(path.exists(), "{}", path.display());
    }
    Ok(())
}

#[test]
fn read_only_preparation_materialises_split_index() -> TestResult {
    for format in ["sha1", "sha256"] {
        let mut fixture = Fixture::new(format)?;
        fs::write(fixture.lane.join("staged"), "staged content")?;
        git(
            &fixture.git,
            &fixture.home,
            &fixture.lane,
            None,
            &["add", "staged"],
        )?;
        git(
            &fixture.git,
            &fixture.home,
            &fixture.lane,
            None,
            &["update-index", "--split-index"],
        )?;
        let runtime = UtilityRuntime::new(&fixture.git, &fixture.home, &fixture.state)?;
        let repo = runtime.open_read_repo(
            &SystemExecutor::new(1)?,
            &fixture.lane,
            UtilityBudget::bounded(Duration::from_secs(10))?,
            &CancelToken::new(),
        )?;
        let host_index = fs::read(repo.private_git_dir().join("index"))?;
        let scratch = fixture.root.join("reader-split");
        fs::create_dir(&scratch)?;
        super::prepare_read_only(&fixture.git, &repo, &scratch, None, Some(fixture.base))?;
        fixture.source = scratch.join("git-objects");
        let status = fixture.git_overlay(&scratch, &["status", "--porcelain"])?;
        assert!(
            String::from_utf8(status.stdout)?.contains("A  staged"),
            "{format}"
        );
        let shared = fixture.git_overlay(&scratch, &["rev-parse", "--shared-index-path"])?;
        assert!(
            String::from_utf8(shared.stdout)?.trim().is_empty(),
            "{format}"
        );
        assert_eq!(fs::read(repo.private_git_dir().join("index"))?, host_index);
    }
    Ok(())
}

#[test]
fn read_only_private_git_fetches_without_changing_linked_checkout() -> TestResult {
    let mut fixture = Fixture::new("sha1")?;
    let remote = fixture.root.join("remote");
    git(
        &fixture.git,
        &fixture.home,
        &fixture.root,
        None,
        &[
            "clone",
            "-q",
            "--no-local",
            fixture.project.to_str().ok_or("project")?,
            remote.to_str().ok_or("remote")?,
        ],
    )?;
    git(
        &fixture.git,
        &fixture.home,
        &fixture.project,
        None,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    fs::write(remote.join("upstream"), "remote-only content\n")?;
    git(
        &fixture.git,
        &fixture.home,
        &remote,
        None,
        &["add", "upstream"],
    )?;
    git(
        &fixture.git,
        &fixture.home,
        &remote,
        None,
        &["commit", "-qm", "upstream"],
    )?;
    let remote_head = oid(git(
        &fixture.git,
        &fixture.home,
        &remote,
        None,
        &["rev-parse", "HEAD"],
    )?)?;
    let runtime = UtilityRuntime::new(&fixture.git, &fixture.home, &fixture.state)?;
    let executor = SystemExecutor::new(1)?;
    let repo = runtime.open_read_repo(
        &executor,
        &fixture.lane,
        UtilityBudget::bounded(Duration::from_secs(10))?,
        &CancelToken::new(),
    )?;
    fn files(root: &Path) -> TestResult<Vec<(PathBuf, Vec<u8>)>> {
        fn walk(root: &Path, path: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) -> TestResult {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    walk(root, &path, out)?;
                } else {
                    out.push((path.strip_prefix(root)?.to_owned(), fs::read(&path)?));
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        walk(root, root, &mut out)?;
        out.sort();
        Ok(out)
    }
    let host_objects = files(&fixture.common_objects())?;
    let host_refs = files(&repo.common_git_dir().join("refs"))?;
    let host_config = fs::read(repo.common_git_dir().join("config"))?;
    let host_head = fs::read(repo.private_git_dir().join("HEAD"))?;
    let host_index = fs::read(repo.private_git_dir().join("index"))?;
    let host_ref = fs::read(repo.common_git_dir().join("refs/heads/lane"))?;
    let checkout = fs::read(fixture.lane.join("file"))?;
    let scratch = fixture.root.join("reader-scratch");
    fs::create_dir(&scratch)?;
    super::prepare_read_only(&fixture.git, &repo, &scratch, None, Some(fixture.base))?;
    fixture.source = scratch.join("git-objects");
    fixture.git_overlay(
        &scratch,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ],
    )?;
    assert_eq!(
        oid(fixture.git_overlay(&scratch, &["rev-parse", "FETCH_HEAD"])?)?,
        remote_head
    );
    assert_eq!(
        oid(fixture.git_overlay(&scratch, &["rev-parse", "refs/remotes/origin/main"])?)?,
        remote_head
    );
    assert_eq!(
        oid(fixture.git_overlay(&scratch, &["rev-parse", "HEAD"])?)?,
        fixture.base
    );
    fixture.git_overlay(&scratch, &["cat-file", "-e", &remote_head.to_string()])?;
    assert!(!object_path(&fixture.common_objects(), remote_head).exists());
    assert_eq!(files(&fixture.common_objects())?, host_objects);
    assert_eq!(files(&repo.common_git_dir().join("refs"))?, host_refs);
    assert_eq!(fs::read(repo.common_git_dir().join("config"))?, host_config);
    assert_eq!(fs::read(repo.private_git_dir().join("HEAD"))?, host_head);
    assert_eq!(fs::read(repo.private_git_dir().join("index"))?, host_index);
    assert_eq!(
        fs::read(repo.common_git_dir().join("refs/heads/lane"))?,
        host_ref
    );
    assert_eq!(fs::read(fixture.lane.join("file"))?, checkout);
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#admission-and-custody
fn host_git_uses_the_pinned_attempt_common_directory() -> TestResult {
    let fixture = Fixture::new("sha1")?;
    let (_, scratch) = fixture.prepare_attempt("attempt-with-planted-commondir", fixture.base)?;
    let common = scratch.join("git-common");
    let rogue = fixture.root.join("rogue-dir");
    fs::create_dir_all(&rogue)?;
    fs::write(common.join("commondir"), format!("{}\n", rogue.display()))?;

    let stdout = super::execute_bounded(
        &fixture.git,
        &common,
        fixture.worktree.repo().root_path(),
        &super::args(&["rev-parse", "--git-common-dir"]),
        None,
        super::Execution {
            common_directory: Some(&common),
            ..Default::default()
        },
    )?;
    let common_reported = std::str::from_utf8(&stdout)?.trim();
    assert_ne!(
        common_reported,
        rogue.to_str().ok_or("rogue path is not UTF-8")?
    );
    assert_eq!(
        Path::new(common_reported).canonicalize()?,
        common.canonicalize()?
    );
    Ok(())
}
