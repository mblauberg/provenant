//! Host observations used by the existing GC composition.
use crate::worktrees::Error;
use provenant_confinement::worktrees::WorktreeContext;
use provenant_core::{ActorId, CancelToken, Clock, GitOid, Roots, WaitBudget};
use provenant_run::{AttemptState, ExecutionContext, LaneQuery, RunStore};
use provenant_supervisor::{
    clock::SystemClock,
    exec::{ExecOptions, StreamPolicy, Termination},
    fs::{Directory, FileKind, LockMode, PublishMode, entry_names},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::PathBuf,
    time::Duration,
};

/// Fetch the remote's actual default branch, independently of local main or a
/// stale origin/HEAD. Only this branch's remote-tracking ref is updated.
pub(crate) fn fetch_default(
    context: &WorktreeContext<'_>,
) -> Result<Option<(String, GitOid)>, Error> {
    let git = crate::roots::git_binary().ok_or(Error::GitUnavailable)?;
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
    let run = |args: &[&str]| -> Result<_, Error> {
        let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
            .chain(
                [
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "core.sshCommand=/usr/bin/ssh -oBatchMode=yes",
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
        let mut options =
            ExecOptions::new(&argv, context.repo.root_path(), Duration::from_secs(60));
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
    let remote = run(&["config", "--get", "remote.origin.url"])?;
    if remote.termination == Termination::Exit(1) {
        return Ok(None);
    }
    if remote.termination != Termination::Exit(0) {
        return Err(Error::GcObservation("cannot read origin"));
    }
    let advertised = run(&["ls-remote", "--symref", "origin", "HEAD"])?;
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
    let fetched = run(&[
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--refmap=",
        "origin",
        &refspec,
    ])?;
    if fetched.termination != Termination::Exit(0) {
        return Err(Error::GcObservation("cannot fetch upstream default branch"));
    }
    let tip = run(&["rev-parse", "--verify", &format!("{reference}^{{commit}}")])?;
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
pub(crate) fn terminal_worktrees(
    roots: &Roots,
    project: provenant_core::ProjectId,
) -> BTreeSet<PathBuf> {
    lane_states(roots, project)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(path, terminal)| terminal.then_some(path))
        .collect()
}

pub(crate) fn no_live_lanes(
    roots: &Roots,
    project: provenant_core::ProjectId,
    path: &std::path::Path,
) -> bool {
    lane_states(roots, project)
        .is_ok_and(|states| states.get(path).is_none_or(|terminal| *terminal))
}

fn lane_states(
    roots: &Roots,
    project: provenant_core::ProjectId,
) -> Result<BTreeMap<PathBuf, bool>, Box<dyn std::error::Error>> {
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: project,
        actor_id: ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let mut states = BTreeMap::new();
    scan_terminal(&context, &mut states, 0)?;
    Ok(states)
}

fn scan_terminal(
    context: &ExecutionContext,
    states: &mut BTreeMap<PathBuf, bool>,
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
    let store = RunStore::open_disk_read_only(context, &call)?;
    if let Some(store) = store {
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
                    let terminal = matches!(attempt.state, AttemptState::Terminal { .. });
                    *states.entry(path).or_insert(true) &= terminal;
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
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for name in entry_names(&children, 10_000)? {
        if children.metadata_at(&name, false)?.kind != FileKind::Directory {
            return Err("unknown child store".into());
        }
        children.open_verified_directory(&name)?;
        let mut child = context.clone();
        let mut inputs =
            provenant_core::RootInputs::new(crate::roots::home()?, crate::roots::product_root()?);
        inputs.state_dir = Some(context.state_root.as_path().join("children").join(name));
        child.state_root = inputs.resolve(&provenant_supervisor::fs::LocalFs)?.state;
        scan_terminal(&child, states, depth + 1)?;
    }
    Ok(())
}

/// Serialise the dip latch with the event. A recovered reading rearms it;
/// dry runs never change the latch or emit an event.
pub(crate) fn disk_alert(
    roots: &Roots,
    projects: &[provenant_core::ProjectId],
    floor: Option<&str>,
    dry_run: bool,
) -> Result<Vec<String>, Error> {
    let Some(floor) = floor else {
        return Ok(Vec::new());
    };
    let floor = provenant_config::parse_cache_size(floor)
        .map_err(|_| Error::GcObservation("invalid alert threshold"))?;
    let root = Directory::open_anchor(roots.state.as_path())?;
    let free = root
        .available_bytes()
        .map_err(|_| Error::GcObservation("cannot read available disk space"))?;
    let detail = format!("low_disk: {free} bytes available, threshold {floor}");
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
