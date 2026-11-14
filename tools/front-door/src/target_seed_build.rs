//! Build from a pinned remote-tracking commit in a disposable host checkout.
use provenant_core::Clock;
use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use provenant_run::target_seed::{self, Manifest};
use std::{
    error::Error,
    ffi::OsString,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = make_directories_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn make_directories_writable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
        )?;
        for entry in fs::read_dir(path)? {
            make_directories_writable(&entry?.path())?;
        }
    }
    Ok(())
}

fn run(cwd: &Path, argv: &[OsString], overrides: &[(OsString, OsString)]) -> Result<Vec<u8>> {
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            !key.to_string_lossy().starts_with("GIT_")
                && !overrides.iter().any(|(name, _)| name == key)
        })
        .collect();
    env.extend_from_slice(overrides);
    let mut options = ExecOptions::new(argv, cwd, Duration::from_secs(4 * 60 * 60));
    options.env = Some(&env);
    options.process_group = true;
    let result = SystemExecutor::new(1)?.execute(options)?;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.stdout.truncated
        || result.stdout.read_failed
    {
        return Err(io::Error::other(format!(
            "command failed: {argv:?}: {}",
            String::from_utf8_lossy(&result.stderr.bytes)
        ))
        .into());
    }
    Ok(result.stdout.bytes)
}
fn git(cwd: &Path, args: &[OsString]) -> Result<Vec<u8>> {
    let executable =
        provenant_os::git::git_executable().ok_or_else(|| io::Error::other("git unavailable"))?;
    let mut argv: Vec<OsString> = vec![
        executable.into_os_string(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
    ];
    argv.extend_from_slice(args);
    run(
        cwd,
        &argv,
        &[
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ],
    )
}

// A clone advertises local branches and tags, not remote-tracking refs.
// Fetch the pinned object explicitly before checkout.
fn checkout_commit(project: &Path, checkout: &Path, commit: &str) -> Result<()> {
    git(
        project,
        &[
            "clone".into(),
            "--no-local".into(),
            "--no-hardlinks".into(),
            "--no-checkout".into(),
            "--".into(),
            project.as_os_str().into(),
            checkout.as_os_str().into(),
        ],
    )?;
    git(
        checkout,
        &[
            "fetch".into(),
            "--no-tags".into(),
            "--".into(),
            project.as_os_str().into(),
            commit.into(),
        ],
    )?;
    git(
        checkout,
        &["checkout".into(), "--detach".into(), commit.into()],
    )?;
    Ok(())
}

/// The pinned remote-tracking commit every seed build starts from.
fn origin_main(host_state: &Path, project: &Path) -> Result<String> {
    use provenant_confinement::{
        repo::GitRefName,
        utility::{UtilityBudget, UtilityRuntime},
        worktrees::RepoFacts,
    };
    let executable =
        provenant_os::git::git_executable().ok_or_else(|| io::Error::other("git unavailable"))?;
    let utility = UtilityRuntime::read_only(executable, host_state)?;
    let executor = SystemExecutor::new(1)?;
    let budget = UtilityBudget::bounded(Duration::from_secs(4 * 60 * 60))?;
    let cancel = provenant_core::CancelToken::new();
    let repo = utility.open_read_repo(&executor, project, budget, &cancel)?;
    let facts = RepoFacts::default();
    let reference = GitRefName::remote_tracking("origin/main")?;
    Ok(facts
        .resolve_read_commit(&repo, &executor, &utility, budget, reference, &cancel)?
        .to_string())
}

/// What a refresh did.
#[derive(Debug, PartialEq, Eq)]
pub enum Refresh {
    /// This project has no selected Cargo workspace; nothing to seed.
    NotCargo,
    /// The newest complete generation already builds this `origin/main`.
    Current,
    /// Another host seed build holds the build lock; nothing was started.
    Busy,
    /// A new generation was published.
    Built(String),
    /// The last build of this commit (named here) failed for this project; it
    /// is not retried until `origin/main` moves.
    BackedOff(String),
}

/// Rebuild the seed when `origin/main` has moved past the newest complete
/// generation, at most one build at a time. Call after a fetch that may have
/// advanced `origin/main`; it does nothing when the seed is current or a build
/// is running, and never builds from a lane checkout (the build uses a scratch
/// clone of the pinned `origin/main`).
// The detached seed worker (`internal tick-seed`) calls this once per project
// after the daily collection's fetch. The harness-free worktree driver
// includes this module without that caller.
#[allow(dead_code)]
pub fn refresh(state: &Path, project: &Path) -> Result<Refresh> {
    refuse_in_lane()?;
    if target_seed::cargo_build_directory(project, state)?.is_none() {
        return Ok(Refresh::NotCargo);
    }
    let commit = origin_main(state, project)?;
    refresh_with(state, &marker_name(project), &commit, |stage| {
        build_into(stage, project, state, &commit)
    })
}

/// Raised under the build lock when another refresher published this commit
/// while we waited for it.
#[derive(Debug)]
struct AlreadyCurrent;
impl std::fmt::Display for AlreadyCurrent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("seed already current")
    }
}
impl Error for AlreadyCurrent {}

/// The back-off marker file for one project, so one project's broken build
/// neither blocks nor clears another's.
fn marker_name(project: &Path) -> String {
    use std::hash::{Hash, Hasher};
    // A toolchain change may rename every marker, which only ends a back-off.
    let mut hasher = std::hash::DefaultHasher::new();
    project.hash(&mut hasher);
    format!("last-failed-{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

/// The commit whose build last failed for this project, so a broken
/// `origin/main` is not rebuilt every tick. A new commit differs and so clears
/// the back-off.
fn failed_commit(state: &Path, marker: &str) -> Option<String> {
    fs::read_to_string(target_seed::store_path(state).join(marker))
        .ok()
        .map(|text| text.trim().to_owned())
}

fn refresh_with(
    state: &Path,
    marker: &str,
    head: &str,
    build: impl FnOnce(&Path) -> Result<Manifest>,
) -> Result<Refresh> {
    refresh_racing(state, marker, head, || {}, build)
}

/// `between` runs after the unlocked staleness check and before the build
/// lock: the window another refresher can publish in.
fn refresh_racing(
    state: &Path,
    marker: &str,
    head: &str,
    between: impl FnOnce(),
    build: impl FnOnce(&Path) -> Result<Manifest>,
) -> Result<Refresh> {
    if newest_generation_commit(state).as_deref() == Some(head) {
        return Ok(Refresh::Current);
    }
    if failed_commit(state, marker).as_deref() == Some(head) {
        return Ok(Refresh::BackedOff(head.to_owned()));
    }
    // Another refresher may publish this commit between the check above and
    // our taking the build lock, so look again once we hold it.
    between();
    let result = publish(state, |stage| {
        if newest_generation_commit(state).as_deref() == Some(head) {
            return Err(AlreadyCurrent.into());
        }
        build(stage)
    });
    match result {
        Ok(id) => {
            let _ = fs::remove_file(target_seed::store_path(state).join(marker));
            Ok(Refresh::Built(id))
        }
        Err(error) if error.is::<AlreadyCurrent>() => Ok(Refresh::Current),
        Err(error)
            if matches!(
                error.downcast_ref::<provenant_os::fs::FsError>(),
                Some(provenant_os::fs::FsError::LockTimeout { .. })
            ) =>
        {
            Ok(Refresh::Busy)
        }
        Err(error) => {
            let _ = fs::write(
                target_seed::store_path(state).join(marker),
                format!("{head}\n"),
            );
            Err(error)
        }
    }
}

/// The commit the newest complete generation was built from.
fn newest_generation_commit(state: &Path) -> Option<String> {
    let generations = target_seed::store_path(state).join("generations");
    let mut names: Vec<_> = fs::read_dir(generations)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .collect();
    names.sort();
    names.into_iter().rev().find_map(|path| {
        (fs::read(path.join("complete")).ok()? == b"complete\n")
            .then(|| fs::read(path.join("manifest.json")).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
            .map(|manifest| manifest.commit)
    })
}

fn refuse_in_lane() -> Result<()> {
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() {
        return Err(io::Error::other("target seeds can only be built on the host").into());
    }
    Ok(())
}

pub fn build(state: &Path, project: &Path) -> Result<String> {
    refuse_in_lane()?;
    let commit = origin_main(state, project)?;
    publish(state, |stage| build_into(stage, project, state, &commit))
}

fn build_into(stage: &Path, project: &Path, host_state: &Path, commit: &str) -> Result<Manifest> {
    let checkout = stage.join("checkout");
    checkout_commit(project, &checkout, commit)?;
    let worktree = target_seed::cargo_build_directory(&checkout, host_state)?
        .ok_or_else(|| io::Error::other("project has no selected Cargo workspace"))?;
    let target = stage.join("target");
    fs::create_dir(&target)?;
    let (rustc_vv, host_triple) = target_seed::rustc_metadata(&worktree)?;
    let mut flags = vec![
        "cargo build --locked --all-targets".to_owned(),
        "cargo test --locked --no-run".to_owned(),
    ];
    flags.extend(
        [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_BUILD_RUSTFLAGS",
            "CARGO_BUILD_TARGET",
        ]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|v| format!("{key}={v}"))),
    );
    for args in [
        &["build", "--locked", "--all-targets"][..],
        &["test", "--locked", "--no-run"][..],
    ] {
        let argv: Vec<OsString> = std::iter::once(OsString::from("cargo"))
            .chain(args.iter().map(OsString::from))
            .collect();
        run(
            &worktree,
            &argv,
            &[
                ("CARGO_TARGET_DIR".into(), target.as_os_str().into()),
                ("CARGO_BUILD_BUILD_DIR".into(), target.as_os_str().into()),
            ],
        )?;
    }
    fs::remove_dir_all(checkout)?;
    Ok(Manifest {
        rustc_vv,
        host_triple,
        commit: commit.to_owned(),
        profile: "dev".into(),
        features: vec!["default".into()],
        flags,
    })
}

fn publish(state: &Path, build: impl FnOnce(&Path) -> Result<Manifest>) -> Result<String> {
    let store_lock = target_seed::lock_store(state)?;
    let _build_lock = target_seed::lock_build(state)?;
    let store = target_seed::store_path(state);
    let random = provenant_os::fs::temporary_name()?;
    let stage = Staging(store.join(format!(".staging-{}", random.to_string_lossy())));
    fs::create_dir(&stage.0)?;
    drop(store_lock);
    let manifest = build(&stage.0)?;
    fs::write(
        stage.0.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    make_read_only(&stage.0.join("target"))
        .map_err(|error| io::Error::new(error.kind(), format!("freeze generation: {error}")))?;
    make_read_only(&stage.0.join("manifest.json"))?;
    sync_tree(&stage.0)?;
    let id = format!(
        "{:020}-{}",
        provenant_os::clock::SystemClock.wall_now()?.as_micros(),
        random.to_string_lossy()
    );
    let _lock = target_seed::lock_store(state)
        .map_err(|error| io::Error::other(format!("lock publication: {error}")))?;
    fs::rename(&stage.0, store.join("generations").join(&id))
        .map_err(|error| io::Error::new(error.kind(), format!("publish generation: {error}")))?;
    let generation = store.join("generations").join(&id);
    let marker = generation.join("complete");
    fs::write(&marker, b"complete\n")?;
    make_read_only(&marker)?;
    provenant_os::fs::durable_sync_file(&fs::File::open(&marker)?)?;
    let mode = fs::metadata(&generation)?.permissions().mode();
    fs::set_permissions(&generation, fs::Permissions::from_mode(mode & !0o222))?;
    sync_directory(&generation)?;
    sync_directory(&store.join("generations"))?;
    sync_directory(&store)?;
    Ok(id)
}

// Never follow symlinks, which may point outside the staged tree.
fn sync_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            sync_tree(&entry?.path())?;
        }
        return sync_directory(path);
    }
    if !metadata.is_file() {
        return Err(io::Error::other("unsupported entry in target seed"));
    }
    provenant_os::fs::durable_sync_file(&fs::File::open(path)?)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn make_read_only(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            make_read_only(&entry?.path())?;
        }
    }
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(metadata.permissions().mode() & !0o222),
    )
}

#[cfg(test)]
mod checkout_tests {
    #[allow(unused_imports)]
    // This module is also included by a harness-free integration target.
    use super::*;

    #[test]
    fn checkout_transfers_remote_tracking_commit_ahead_of_local_branch() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        let project = temp.path().join("project");
        let checkout = temp.path().join("checkout");
        fs::create_dir(&remote).unwrap();
        git(&remote, &["init".into(), "--initial-branch=main".into()]).unwrap();
        fs::write(remote.join("file"), "first").unwrap();
        git(&remote, &["add".into(), "file".into()]).unwrap();
        let commit = |message: &str| {
            provenant_os::fixture::run_git(&remote, &["commit", "-am", message]).unwrap();
        };
        commit("first");
        git(
            temp.path(),
            &[
                "clone".into(),
                "--".into(),
                remote.as_os_str().into(),
                project.as_os_str().into(),
            ],
        )
        .unwrap();
        fs::write(remote.join("file"), "second").unwrap();
        commit("second");
        git(&project, &["fetch".into(), "origin".into()]).unwrap();
        let pinned =
            String::from_utf8(git(&project, &["rev-parse".into(), "origin/main".into()]).unwrap())
                .unwrap();
        let pinned = pinned.trim();
        let local = String::from_utf8(git(&project, &["rev-parse".into(), "main".into()]).unwrap())
            .unwrap();
        assert_ne!(pinned, local.trim());
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        assert_eq!(origin_main(&state, &project).unwrap(), pinned);
        checkout_commit(&project, &checkout, pinned).unwrap();
        assert_eq!(fs::read_to_string(checkout.join("file")).unwrap(), "second");
        let actual =
            String::from_utf8(git(&checkout, &["rev-parse".into(), "HEAD".into()]).unwrap())
                .unwrap();
        assert_eq!(actual.trim(), pinned);
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    // This module is also included by a harness-free integration target.
    use super::*;
    #[test]
    fn failure_never_publishes() {
        let state = tempfile::tempdir().unwrap();
        assert!(
            publish(state.path(), |_| Err(
                io::Error::other("build failed").into()
            ))
            .is_err()
        );
        assert_eq!(
            fs::read_dir(target_seed::store_path(state.path()).join("generations"))
                .unwrap()
                .count(),
            0
        );
    }
    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn manifest(commit: &str) -> Manifest {
        Manifest {
            rustc_vv: "version".into(),
            host_triple: "host".into(),
            commit: commit.into(),
            profile: "dev".into(),
            features: vec![],
            flags: vec![],
        }
    }
    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn built(stage: &Path, commit: &str) -> Result<Manifest> {
        fs::create_dir(stage.join("target"))?;
        Ok(manifest(commit))
    }
    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn freeze(state: &Path) {
        let generations = target_seed::store_path(state).join("generations");
        for entry in fs::read_dir(generations).unwrap() {
            make_directories_writable(&entry.unwrap().path()).unwrap();
        }
    }

    #[test]
    fn refresh_builds_only_when_origin_main_has_advanced() {
        let state = tempfile::tempdir().unwrap();
        let builds = std::cell::Cell::new(0);
        let run = |head: &str| {
            refresh_with(state.path(), "p", head, |stage| {
                builds.set(builds.get() + 1);
                built(stage, head)
            })
            .unwrap()
        };
        assert!(matches!(run("aaa"), Refresh::Built(_)));
        assert_eq!(run("aaa"), Refresh::Current);
        assert_eq!(builds.get(), 1, "a current seed is not rebuilt");
        // An incomplete newer generation does not count as current.
        let generations = target_seed::store_path(state.path()).join("generations");
        fs::create_dir(generations.join("99999999999999999999-partial")).unwrap();
        assert_eq!(
            newest_generation_commit(state.path()).as_deref(),
            Some("aaa")
        );
        assert!(matches!(run("bbb"), Refresh::Built(_)));
        assert_eq!(run("bbb"), Refresh::Current);
        assert_eq!(builds.get(), 2);
        freeze(state.path());
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // A real deadline bounds the fork-race retry.
    fn refresh_runs_one_build_at_a_time_and_failure_publishes_nothing() {
        let state = tempfile::tempdir().unwrap();
        {
            // Another host build holds the build lock: no second build starts.
            target_seed::lock_store(state.path()).map(drop).unwrap();
            let _running = target_seed::lock_build(state.path()).unwrap();
            let outcome = refresh_with(state.path(), "p", "aaa", |_| panic!("must not build"));
            assert_eq!(outcome.unwrap(), Refresh::Busy);
        }
        // A sibling test thread can fork while the lock above is released and
        // briefly hold a copy of its descriptor, so retry for at most 2 s.
        let retry_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let failed = loop {
            let attempt = refresh_with(state.path(), "p", "aaa", |_| {
                Err(io::Error::other("build failed").into())
            });
            if !matches!(attempt, Ok(Refresh::Busy)) || std::time::Instant::now() >= retry_deadline
            {
                break attempt;
            }
            std::thread::yield_now();
        };
        assert!(failed.is_err(), "{failed:?}");
        assert_eq!(newest_generation_commit(state.path()), None);
        // The failed commit is backed off; a newer commit builds again.
        assert_eq!(
            refresh_with(state.path(), "p", "aaa", |_| panic!(
                "must not rebuild a failed commit"
            ))
            .unwrap(),
            Refresh::BackedOff("aaa".into())
        );
        assert!(matches!(
            refresh_with(state.path(), "p", "bbb", |stage| built(stage, "bbb")).unwrap(),
            Refresh::Built(_)
        ));
        assert_eq!(
            failed_commit(state.path(), "p"),
            None,
            "success clears the back-off"
        );
        freeze(state.path());
    }

    #[test]
    fn one_projects_failure_never_clears_another_projects_back_off() {
        let state = tempfile::tempdir().unwrap();
        let (a, b) = (marker_name(Path::new("/a")), marker_name(Path::new("/b")));
        assert_ne!(a, b);
        let fail = |marker: &str, head: &str| {
            refresh_with(state.path(), marker, head, |_| {
                Err(io::Error::other("build failed").into())
            })
            .unwrap_err()
        };
        fail(&a, "aaa");
        // B is not backed off by A's failure, and B's success leaves A's marker.
        assert!(matches!(
            refresh_with(state.path(), &b, "bbb", |stage| built(stage, "bbb")).unwrap(),
            Refresh::Built(_)
        ));
        assert_eq!(failed_commit(state.path(), &a).as_deref(), Some("aaa"));
        assert_eq!(failed_commit(state.path(), &b), None);
        assert_eq!(
            refresh_with(state.path(), &a, "aaa", |_| panic!("backed off")).unwrap(),
            Refresh::BackedOff("aaa".into())
        );
        freeze(state.path());
    }

    #[test]
    fn refresh_rechecks_staleness_after_taking_the_build_lock() {
        let state = tempfile::tempdir().unwrap();
        let outcome = refresh_racing(
            state.path(),
            "p",
            "aaa",
            || {
                publish(state.path(), |stage| built(stage, "aaa")).unwrap();
            },
            |_| panic!("the racing publisher already built this commit"),
        )
        .unwrap();
        assert_eq!(outcome, Refresh::Current);
        assert_eq!(failed_commit(state.path(), "p"), None);
        freeze(state.path());
    }

    #[test]
    fn success_publishes_read_only_generation() {
        let state = tempfile::tempdir().unwrap();
        let id = publish(state.path(), |stage| {
            fs::create_dir(stage.join("target"))?;
            fs::write(stage.join("target/artifact"), b"built")?;
            Ok(Manifest {
                rustc_vv: "version".into(),
                host_triple: "host".into(),
                commit: "commit".into(),
                profile: "dev".into(),
                features: vec![],
                flags: vec![],
            })
        })
        .unwrap();
        let generation = target_seed::store_path(state.path())
            .join("generations")
            .join(id);
        for path in [
            generation.clone(),
            generation.join("manifest.json"),
            generation.join("target/artifact"),
        ] {
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o222, 0);
        }
        make_directories_writable(&generation).unwrap();
    }
}
