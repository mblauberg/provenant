//! Host observations used by the existing GC composition.
use crate::worktrees::Error;
use provenant_confinement::worktrees::WorktreeContext;
use provenant_core::{ActorId, CancelToken, Clock, GitOid, Roots, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    exec::{ExecOptions, StreamPolicy, Termination},
    fs::{Directory, FileKind, LockMode, Metadata, PublishMode, entry_names},
};
use provenant_run::{AttemptState, ExecutionContext, LaneQuery, RunStore};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

/// Fetch the remote's actual default branch, independently of local main or a
/// stale origin/HEAD. Only this branch's remote-tracking ref is updated.
pub(crate) fn fetch_default(
    context: &WorktreeContext<'_>,
) -> Result<Option<(String, GitOid)>, Error> {
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
    let run = |args: &[&str], timeout: Duration| -> Result<_, Error> {
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
        let mut options = ExecOptions::new(&argv, context.repo.root_path(), timeout);
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let output = context.executor.execute(options)?;
        if output.timed_out
            || output.cancelled
            || output.stdout.truncated
            || output.drain_truncated
            || output.overflow_stopped
            || output.stdout.sink_lost_bytes > 0
        {
            return Err(Error::GcObservation("upstream fetch incomplete"));
        }
        Ok(output)
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
    let tip = run(
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        Duration::from_secs(5),
    )?;
    if tip.termination != Termination::Exit(0) {
        return Err(Error::GcObservation("cannot pin fetched upstream"));
    }
    let oid = std::str::from_utf8(&tip.stdout.bytes)
        .ok()
        .and_then(|text| GitOid::parse(text.trim()).ok())
        .ok_or(Error::GcObservation("invalid fetched upstream"))?;
    Ok(Some((reference, oid)))
}

/// Unknown stores or records protect every candidate. Include child stores so
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
    children: Option<(Metadata, Vec<OsString>)>,
}

impl LaneSnapshot {
    pub(crate) fn get(&self, path: &Path) -> Option<&bool> {
        self.is_current().then(|| self.states.get(path)).flatten()
    }

    pub(crate) fn no_live_lanes(&self, path: &Path) -> bool {
        self.is_current() && self.states.get(path).is_none_or(|terminal| *terminal)
    }

    /// Reuse the lane scan only while every observed store and its child-store
    /// membership remain unchanged. A changed snapshot never becomes valid again.
    pub(crate) fn is_current(&self) -> bool {
        if self.stale.get() {
            return false;
        }
        let current = || -> Result<bool, Box<dyn std::error::Error>> {
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = provenant_run::Request {
                clock: &clock,
                budget: WaitBudget::cli(clock.mono_now(), 1)?,
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
                let children = child_membership(&observed.root)?;
                match (&observed.children, children) {
                    (None, None) => {}
                    (Some((before, names)), Some((after, fresh)))
                        if before.same_identity(&after) && *names == fresh => {}
                    _ => return Ok(false),
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

fn child_membership(
    root: &Directory,
) -> Result<Option<(Metadata, Vec<OsString>)>, provenant_os::fs::FsError> {
    let children = match root.open_verified_directory(OsStr::new("children")) {
        Ok(children) => children,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut names = entry_names(&children, 10_000)?;
    names.sort();
    Ok(Some((children.metadata()?, names)))
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

/// Lane states under `roots` and its child stores, which bind with the same
/// `home` and `assets` as their parent.
pub(crate) fn lane_states(
    roots: &Roots,
    home: &Path,
    assets: &Path,
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
    scan_terminal(&context, (home, assets), &mut snapshot, 0)?;
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
    (home, assets): (&Path, &Path),
    snapshot: &mut LaneSnapshot,
    depth: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if depth >= 16 {
        return Err("child store depth".into());
    }
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 5)?,
        cancel: &cancel,
    };
    let root = Directory::open_anchor(context.state_root.as_path())?;
    let database_before = optional_metadata(&root, OsStr::new("run.sqlite3"))?;
    let store = RunStore::open_disk_read_only(context, &call)?;
    let version = store
        .as_ref()
        .map(|store| store.data_version(&call))
        .transpose()?;
    let database = optional_metadata(&root, OsStr::new("run.sqlite3"))?;
    if !same_optional_identity(database_before, database) {
        return Err("run store changed while opening snapshot".into());
    }
    let children_before = child_membership(&root)?;
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
        if depth > 0 || root.metadata_at(OsStr::new("run.sqlite3"), false).is_ok() {
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
    let children = match root.open_verified_directory(OsStr::new("children")) {
        Ok(children) => children,
        Err(error) if error.is_not_found() => {
            snapshot.stores.push(StoreSnapshot {
                path: context.state_root.as_path().to_owned(),
                root,
                database,
                store,
                version,
                children: children_before,
            });
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    for name in entry_names(&children, 10_000)? {
        if children.metadata_at(&name, false)?.kind != FileKind::Directory {
            return Err("unknown child store".into());
        }
        children.open_verified_directory(&name)?;
        let mut child = context.clone();
        child.state_root = crate::roots::child(
            home,
            assets,
            context.state_root.as_path().join("children").join(name),
        )?
        .state;
        scan_terminal(&child, (home, assets), snapshot, depth + 1)?;
    }
    snapshot.stores.push(StoreSnapshot {
        path: context.state_root.as_path().to_owned(),
        root,
        database,
        store,
        version,
        children: children_before,
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
    let detail = format!(
        "low_disk: {free} bytes available, threshold {floor} bytes; run `provenant gc`"
    );
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
    let backoff = provenant_fabric_store::ThreadWait;
    let request = provenant_fabric_store::Request {
        clock: &clock,
        budget,
        cancel: &cancel,
        backoff: &backoff,
    };
    let store = provenant_fabric_store::Store::open(&roots.state, &request)
        .map_err(|_| Error::GcObservation("cannot open Fabric for disk alert"))?;
    for project in projects {
        let event = provenant_fabric_store::EventRecord {
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
