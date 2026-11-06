//! Host observations used by the existing GC composition.
use crate::worktrees::Error;
use provenant_confinement::worktrees::WorktreeContext;
use provenant_core::{ActorId, CancelToken, Clock, GitOid, Roots, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    exec::{ExecOptions, ExecOutcome, Executor, StreamPolicy, Termination},
    fs::{Directory, LockMode, Metadata, PublishMode, entry_names},
};
use provenant_run::{AttemptState, ExecutionContext, LaneQuery, RunStore};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

fn host_git(
    repo: &Path,
    executor: &dyn Executor,
    args: &[&str],
    timeout: Duration,
) -> Result<ExecOutcome, Error> {
    host_git_with_stderr(repo, executor, args, timeout, false)
}

fn host_git_with_stderr(
    repo: &Path,
    executor: &dyn Executor,
    args: &[&str],
    timeout: Duration,
    capture_stderr: bool,
) -> Result<ExecOutcome, Error> {
    let git = provenant_os::git::git_executable().ok_or(Error::GitUnavailable)?;
    let environment: Vec<(OsString, OsString)> = [
        "HOME",
        "SSH_AUTH_SOCK",
        "GH_CONFIG_DIR",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value)))
    .chain([
        (
            "PATH".into(),
            "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin:/opt/local/bin".into(),
        ),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_CONFIG_COUNT".into(), "0".into()),
        ("LANG".into(), "C".into()),
    ])
    .collect();

    let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
            .chain(
                [
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "core.sshCommand=/usr/bin/ssh -oBatchMode=yes -oConnectTimeout=5 -oConnectionAttempts=1",
                    "-c",
                    "protocol.allow=never",
                    "-c",
                    "protocol.file.allow=always",
                    "-c",
                    "protocol.https.allow=always",
                    "-c",
                    "protocol.http.allow=always",
                    "-c",
                    "protocol.ssh.allow=always",
                    "-c",
                    "maintenance.auto=false",
                    "-c",
                    "gc.auto=0",
                ]
                .into_iter()
                .chain(args.iter().copied())
                .map(OsString::from),
            )
            .collect();
    let mut options = ExecOptions::new(&argv, repo, timeout);
    options.env = Some(&environment);
    options.stdin = Some(b"");
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = if capture_stderr {
        StreamPolicy::Capture { limit: 64 * 1024 }
    } else {
        StreamPolicy::Discard
    };
    options.stop_on_output_limit = true;
    let output = executor.execute(options)?;
    if output.timed_out
        || output.cancelled
        || output.stdout.truncated
        || output.stderr.truncated
        || output.drain_truncated
        || output.overflow_stopped
        || output.stdout.sink_lost_bytes > 0
    {
        return Err(Error::GcObservation("host git operation incomplete"));
    }
    Ok(output)
}

/// Refresh only the writer's tracking ref. An absent remote branch is normal.
pub(crate) fn refresh_writer_tracking_ref(
    context: &WorktreeContext<'_>,
    branch: &str,
) -> Result<(), Error> {
    refresh_branch(context.repo.root_path(), context.executor, branch)
        .map_err(|_| Error::GcObservation("cannot refresh writer tracking ref"))
}

fn refresh_branch(repo: &Path, executor: &dyn Executor, branch: &str) -> Result<(), Error> {
    provenant_confinement::repo::GitRefName::branch(branch)
        .map_err(|_| Error::GcObservation("invalid writer branch"))?;
    if branch == "HEAD" {
        return Err(Error::GcObservation("invalid writer branch"));
    }
    let remote = host_git(
        repo,
        executor,
        &["config", "--get", "remote.origin.url"],
        Duration::from_secs(5),
    )?;
    if remote.termination == Termination::Exit(1) {
        return Ok(());
    }
    if remote.termination != Termination::Exit(0) {
        return Err(Error::GcObservation("cannot read origin"));
    }
    let source = format!("refs/heads/{branch}");
    let refspec = format!("+{source}:refs/remotes/origin/{branch}");
    let fetched = host_git_with_stderr(
        repo,
        executor,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--refmap=",
            "origin",
            &refspec,
        ],
        Duration::from_secs(15),
        true,
    )?;
    if fetched.termination != Termination::Exit(0) {
        let missing = format!("fatal: couldn't find remote ref {source}\n");
        if fetched.stderr.bytes == missing.as_bytes() {
            return Ok(());
        }
        return Err(Error::GcObservation("cannot fetch writer branch"));
    }
    Ok(())
}

/// Fetch the remote's actual default branch, independently of local main or a
/// stale origin/HEAD. Only this branch's remote-tracking ref is updated.
pub(crate) fn fetch_default(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<Option<(String, GitOid)>, Error> {
    let run = |args: &[&str], timeout: Duration| {
        host_git(context.repo.root_path(), context.executor, args, timeout)
    };
    let remote = run(
        &["config", "--get", "remote.origin.url"],
        Duration::from_secs(5),
    )?;
    if remote.termination == Termination::Exit(1) {
        return Ok(None);
    }
    if remote.termination != Termination::Exit(0) {
        return Err(Error::GcObservation("cannot read origin"));
    }
    let advertised = run(
        &["ls-remote", "--symref", "origin", "HEAD"],
        Duration::from_secs(60),
    )?;
    if advertised.termination != Termination::Exit(0) {
        return Err(Error::GcObservation(
            "cannot observe upstream default branch",
        ));
    }
    let text = std::str::from_utf8(&advertised.stdout.bytes)
        .map_err(|_| Error::GcObservation("invalid upstream default branch"))?;
    let branch = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .ok_or(Error::GcObservation("upstream has no default branch"))?;
    provenant_confinement::repo::GitRefName::branch(branch)
        .map_err(|_| Error::GcObservation("invalid upstream default branch"))?;
    let reference = format!("refs/remotes/origin/{branch}");
    let refspec = format!("+refs/heads/{branch}:{reference}");
    let fetched = run(
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--refmap=",
            "origin",
            &refspec,
        ],
        Duration::from_secs(60),
    )?;
    if fetched.termination != Termination::Exit(0) {
        return Err(Error::GcObservation("cannot fetch upstream default branch"));
    }
    let name = provenant_confinement::repo::GitRefName::remote_tracking(format!("origin/{branch}"))
        .map_err(|_| Error::GcObservation("invalid fetched upstream ref"))?;
    let oid = context.facts.resolve_commit(context, name, cancel)?;
    Ok(Some((reference, oid)))
}

/// Unknown stores or records protect every candidate. Observe the host store so
/// a nested lane is subject to the same terminal-state rule as its parent.
pub(crate) struct LaneSnapshot {
    states: BTreeMap<PathBuf, bool>,
    terminal_times: BTreeMap<PathBuf, i64>,
    stores: Vec<StoreSnapshot>,
    stale: std::cell::Cell<bool>,
}

pub(crate) fn refresh_if_stale<T>(
    snapshot: &RefCell<Option<T>>,
    rescan_failed: &Cell<bool>,
    is_current: impl FnOnce(&T) -> bool,
    rescan: impl FnOnce() -> Option<T>,
) {
    if rescan_failed.get() {
        return;
    }
    if snapshot.borrow().as_ref().is_some_and(is_current) {
        return;
    }
    let refreshed = rescan();
    if refreshed.is_none() {
        rescan_failed.set(true);
    }
    *snapshot.borrow_mut() = refreshed;
}

struct StoreSnapshot {
    path: PathBuf,
    root: Directory,
    database: Option<Metadata>,
    store: Option<RunStore>,
    version: Option<u64>,
}

impl LaneSnapshot {
    pub(crate) fn get(&self, path: &Path) -> Option<&bool> {
        self.is_current().then(|| self.states.get(path)).flatten()
    }

    pub(crate) fn no_live_lanes(&self, path: &Path) -> bool {
        self.is_current() && self.states.get(path).is_none_or(|terminal| *terminal)
    }

    /// Reuse the lane scan only while the observed host store
    /// remains unchanged. A changed snapshot never becomes valid again.
    pub(crate) fn is_current(&self) -> bool {
        if self.stale.get() {
            return false;
        }
        let current = || -> Result<bool, Box<dyn std::error::Error>> {
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = provenant_run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            for observed in &self.stores {
                let reopened = Directory::open_anchor(&observed.path)?;
                if !observed
                    .root
                    .metadata()?
                    .same_identity(&reopened.metadata()?)
                {
                    return Ok(false);
                }
                let database = optional_metadata(&observed.root, OsStr::new("run.sqlite3"))?;
                if !same_optional_identity(observed.database, database) {
                    return Ok(false);
                }
                if let Some(store) = &observed.store
                    && Some(store.data_version(&call)?) != observed.version
                {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let current = current().unwrap_or(false);
        self.stale.set(!current);
        current
    }
}

fn optional_metadata(
    root: &Directory,
    name: &OsStr,
) -> Result<Option<Metadata>, provenant_os::fs::FsError> {
    match root.metadata_at(name, false) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
}

fn same_optional_identity(before: Option<Metadata>, after: Option<Metadata>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => before.same_identity(&after),
        _ => false,
    }
}

/// A terminal attempt authorises only the worktree instance that existed
/// when it finished. For linked worktrees, the `.git` file is written when
/// Git creates the administrative gitdir link and remains stable afterward.
fn worktree_created_at_micros(path: &Path) -> Option<i128> {
    let modified = std::fs::metadata(path.join(".git")).ok()?.modified().ok()?;
    let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(i128::from(since_epoch.as_secs()) * 1_000_000 + i128::from(since_epoch.subsec_micros()))
}

fn terminal_after_worktree_creation(finished_at: i64, created_at: Option<i128>) -> bool {
    created_at.is_some_and(|created| i128::from(finished_at) > created)
}

/// Lane states under `roots`, which bind with the same
/// `home` and `assets` as their parent.
pub(crate) fn lane_states(
    roots: &Roots,
    _home: &Path,
    _assets: &Path,
    project: provenant_core::ProjectId,
) -> Result<LaneSnapshot, Box<dyn std::error::Error>> {
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: project,
        actor_id: ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let mut snapshot = LaneSnapshot {
        states: BTreeMap::new(),
        terminal_times: BTreeMap::new(),
        stores: Vec::new(),
        stale: std::cell::Cell::new(false),
    };
    scan_terminal(&context, roots.state.as_path(), &mut snapshot)?;
    for (path, terminal) in &mut snapshot.states {
        if *terminal {
            *terminal = snapshot.terminal_times.get(path).is_some_and(|finished| {
                terminal_after_worktree_creation(*finished, worktree_created_at_micros(path))
            });
        }
    }
    Ok(snapshot)
}

fn scan_terminal(
    context: &ExecutionContext,
    host_state: &Path,
    snapshot: &mut LaneSnapshot,
) -> Result<(), Box<dyn std::error::Error>> {
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let database_before = optional_metadata(&root, OsStr::new("run.sqlite3"))?;
    let store = RunStore::open_disk_read_only(context, &call)?
        .map(|store| store.with_writer_registry(host_state));
    let version = store
        .as_ref()
        .map(|store| store.data_version(&call))
        .transpose()?;
    let database = optional_metadata(&root, OsStr::new("run.sqlite3"))?;
    if !same_optional_identity(database_before, database) {
        return Err("run store changed while opening snapshot".into());
    }
    if let Some(store) = &store {
        let mut query = LaneQuery {
            targets: Vec::new(),
            all: true,
            limit: 100,
            after: 0,
            newest_first: false,
            owner: None,
            unseen_after: None,
        };
        loop {
            let page = store.lanes(&query, &call)?;
            for attempt in page.lanes {
                let request = store.request(attempt.run_id, &call)?;
                let task = request
                    .tasks
                    .iter()
                    .find(|task| task.task_id == attempt.task_id)
                    .ok_or("unknown lane task")?;
                if let Some(writer) = &task.writer {
                    if !writer.worktree.is_absolute() {
                        return Err("unknown writer path".into());
                    }
                    let path = writer.worktree.clone();
                    match &attempt.state {
                        AttemptState::Terminal { finished_at, .. } => {
                            let newest = snapshot
                                .terminal_times
                                .entry(path.clone())
                                .or_insert(i64::MIN);
                            *newest = (*newest).max(finished_at.as_micros());
                            snapshot.states.entry(path).or_insert(true);
                        }
                        _ => *snapshot.states.entry(path).or_insert(true) = false,
                    }
                }
            }
            match page.next {
                Some(next) => query.after = next,
                None => break,
            }
        }
    } else {
        // No store and no run records means no submitted lanes. Any partial
        // store, retained records or missing child database is unknown.
        if root.metadata_at(OsStr::new("run.sqlite3"), false).is_ok() {
            return Err("unobservable run store".into());
        }
        match root.open_verified_directory(OsStr::new("runs")) {
            Ok(runs) if !entry_names(&runs, 10_000)?.is_empty() => {
                return Err("unindexed run records".into());
            }
            Ok(_) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    snapshot.stores.push(StoreSnapshot {
        path: context.state_root.as_path().to_owned(),
        root,
        database,
        store,
        version,
    });
    Ok(())
}

/// Serialise the dip latch with the event. A recovered reading rearms it;
/// dry runs never change the latch or emit an event.
pub(crate) fn disk_alert(
    roots: &Roots,
    projects: &[provenant_core::ProjectId],
    floor: u64,
    dry_run: bool,
) -> Result<Vec<String>, Error> {
    let root = Directory::open_anchor(roots.state.as_path())?;
    let free = root
        .available_bytes()
        .map_err(|_| Error::GcObservation("cannot read available disk space"))?;
    let detail =
        format!("low_disk: {free} bytes available, threshold {floor} bytes; run `provenant gc`");
    if dry_run {
        return Ok(if free < floor {
            vec![detail]
        } else {
            Vec::new()
        });
    }
    match root.mkdir(OsStr::new("gc"), 0o700) {
        Ok(()) => {}
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let gc = root.open_verified_directory(OsStr::new("gc"))?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Budget)?;
    let _lock = gc.lock(
        OsStr::new("alert.lock"),
        LockMode::Exclusive,
        budget,
        &cancel,
        &clock,
    )?;
    let latch = OsStr::new("low-disk.json");
    let latched = match gc.metadata_at(latch, false) {
        Ok(_) => true,
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.into()),
    };
    if free >= floor {
        if latched {
            gc.unlink(latch)?;
            gc.sync()?;
        }
        return Ok(Vec::new());
    }
    if latched {
        return Ok(Vec::new());
    }
    if projects.is_empty() {
        return Ok(vec![detail]);
    }
    let request = provenant_fabric_store::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = provenant_fabric_store::Store::open(&roots.state, &request)
        .map_err(|_| Error::GcObservation("cannot open Fabric for disk alert"))?;
    for project in projects {
        let event = provenant_fabric_store::EventRecord {
            extra: Default::default(),
            header: provenant_records::RecordHeader::V1,
            project: *project,
            actor: None,
            kind: provenant_fabric_store::EventKind::Note,
            entity: Some("gc.low_disk".into()),
            detail: detail.clone(),
            at: clock.wall_now().map_err(|_| Error::Budget)?,
        };
        store
            .note(&event, None, &request)
            .map_err(|_| Error::GcObservation("cannot record Fabric disk alert"))?;
    }
    gc.atomic_write(latch, ".low-disk-", b"true\n", PublishMode::NoReplace)?;
    Ok(vec![detail])
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::*;

    #[test]
    fn fetched_default_is_resolved_by_repo_facts() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let repo = temp.path().join("repo");
        let remote = temp.path().join("remote.git");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&state)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        std::fs::create_dir_all(&repo)?;
        provenant_os::fixture::run_git(
            temp.path(),
            &["init", "--bare", remote.to_str().ok_or("remote")?],
        )?;
        for args in [
            vec!["init", "-q"],
            vec!["commit", "--allow-empty", "-qm", "base"],
            vec!["remote", "add", "origin", remote.to_str().ok_or("remote")?],
            vec!["push", "origin", "HEAD:refs/heads/main"],
        ] {
            provenant_os::fixture::run_git(&repo, &args)?;
        }
        provenant_os::fixture::run_git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), repo.clone());
        inputs.state_dir = Some(state);
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let result = provenant_confinement::worktrees::with_context(
            &roots,
            &home,
            &repo,
            &clock,
            &cancel,
            |context| Ok(fetch_default(context, &cancel)),
        )??
        .ok_or("missing upstream")?;
        assert_eq!(result.0, "refs/remotes/origin/main");
        let actual = provenant_os::fixture::run_git(&repo, &["rev-parse", "HEAD"])?;
        assert_eq!(
            result.1,
            GitOid::parse(std::str::from_utf8(&actual)?.trim())?
        );
        Ok(())
    }

    #[test]
    fn writer_tracking_refresh_handles_remote_branch_absence_and_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::SystemExecutor;
        struct TestExecutor {
            inner: SystemExecutor,
            home: PathBuf,
        }
        impl Executor for TestExecutor {
            fn execute(
                &self,
                options: ExecOptions<'_>,
            ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
                let mut env = options.env.unwrap_or_default().to_vec();
                env.retain(|(key, _)| key != "HOME" && !key.to_string_lossy().starts_with("GIT_"));
                env.push(("HOME".into(), self.home.as_os_str().to_owned()));
                env.extend(provenant_os::fixture::fixture_git_settings());
                let mut adjusted = ExecOptions::new(options.argv, options.cwd, options.timeout);
                adjusted.env = Some(&env);
                adjusted.stdin = options.stdin;
                adjusted.stdout = options.stdout;
                adjusted.stderr = options.stderr;
                adjusted.stop_on_output_limit = options.stop_on_output_limit;
                self.inner.execute(adjusted)
            }
        }
        let temp = tempfile::tempdir()?;
        let executor = TestExecutor {
            inner: SystemExecutor::new(1)?,
            home: temp.path().into(),
        };
        let bare = temp.path().join("remote.git");
        let repo = temp.path().join("repo");
        let git = |cwd: &Path, args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let out = host_git(cwd, &executor, args, Duration::from_secs(10))?;
            if out.termination != Termination::Exit(0) {
                return Err(format!("git failed: {args:?}").into());
            }
            Ok(String::from_utf8(out.stdout.bytes)?.trim().to_owned())
        };
        git(
            temp.path(),
            &["init", "--bare", bare.to_str().ok_or("bare path")?],
        )?;
        git(temp.path(), &["init", repo.to_str().ok_or("repo path")?])?;
        git(&repo, &["commit", "--allow-empty", "-m", "test"])?;
        git(
            &repo,
            &[
                "push",
                bare.to_str().ok_or("bare path")?,
                "HEAD:refs/heads/other/secret",
            ],
        )?;
        refresh_branch(&repo, &executor, "lane/test")?;
        for invalid in ["a*b", "../x", "-x", "HEAD"] {
            assert!(
                refresh_branch(&repo, &executor, invalid).is_err(),
                "{invalid}"
            );
        }
        git(
            &repo,
            &["remote", "add", "origin", bare.to_str().ok_or("bare path")?],
        )?;
        git(&repo, &["push", "origin", "HEAD:refs/heads/lane/test"])?;
        let expected = git(&repo, &["rev-parse", "HEAD"])?;
        git(
            &repo,
            &["update-ref", "-d", "refs/remotes/origin/lane/test"],
        )?;
        refresh_branch(&repo, &executor, "lane/test")?;
        assert_eq!(
            git(&repo, &["rev-parse", "refs/remotes/origin/lane/test"])?,
            expected
        );
        let other = host_git(
            &repo,
            &executor,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                "refs/remotes/origin/other/secret",
            ],
            Duration::from_secs(10),
        )?;
        assert_ne!(other.termination, Termination::Exit(0));
        refresh_branch(&repo, &executor, "missing")?;
        git(
            &repo,
            &[
                "remote",
                "set-url",
                "origin",
                temp.path()
                    .join("absent.git")
                    .to_str()
                    .ok_or("absent path")?,
            ],
        )?;
        assert!(refresh_branch(&repo, &executor, "lane/test").is_err());
        Ok(())
    }

    #[test]
    fn terminal_age_must_be_newer_than_worktree_creation() {
        assert!(!terminal_after_worktree_creation(99, Some(100)));
        assert!(!terminal_after_worktree_creation(100, Some(100)));
        assert!(terminal_after_worktree_creation(101, Some(100)));
        assert!(!terminal_after_worktree_creation(101, None));
    }

    #[test]
    fn stale_snapshot_is_rescanned_once() {
        let snapshot = RefCell::new(Some(1));
        let failed = Cell::new(false);
        let rescans = Cell::new(0);
        refresh_if_stale(
            &snapshot,
            &failed,
            |_| false,
            || {
                rescans.set(rescans.get() + 1);
                Some(2)
            },
        );
        assert_eq!(*snapshot.borrow(), Some(2));
        assert_eq!(rescans.get(), 1);
        refresh_if_stale(
            &snapshot,
            &failed,
            |_| true,
            || {
                rescans.set(rescans.get() + 1);
                Some(3)
            },
        );
        assert_eq!(rescans.get(), 1);
    }

    #[test]
    fn failed_rescan_is_not_retried_during_the_sweep() {
        let snapshot = RefCell::new(Some(1));
        let failed = Cell::new(false);
        let rescans = Cell::new(0);
        for _ in 0..2 {
            refresh_if_stale(
                &snapshot,
                &failed,
                |_| false,
                || {
                    rescans.set(rescans.get() + 1);
                    None::<i32>
                },
            );
        }
        assert!(snapshot.borrow().is_none());
        assert!(failed.get());
        assert_eq!(rescans.get(), 1);
    }
}
