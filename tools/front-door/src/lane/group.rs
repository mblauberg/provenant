//! The sole provider signaller. The unreaped leader reserves PID and PGID.
use super::{
    files::{self, Notes},
    platform::{self, Identity, Row},
    state::StopReason,
};
use crate::json::{self, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

fn document(path: &Path) -> Option<Value> {
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if f.metadata().ok()?.len() > 1024 * 1024 {
        return None;
    }
    let mut s = String::new();
    f.by_ref()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut s)
        .ok()?;
    json::parse(&s).ok()
}
fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    match v.get(k)? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}
fn integer(v: &Value, k: &str) -> Option<i64> {
    match v.get(k)? {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}
fn custody(row: &Row, path: &Path) -> bool {
    let Ok(f) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    else {
        return false;
    };
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return false;
    }
    if !matches!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EWOULDBLOCK)
    ) {
        return false;
    }
    platform::has_file(row.id.pid, &f)
        && platform::process(row.id.pid).is_some_and(|r| r.id == row.id)
}
fn registered_owner(row: &Row, attempt: &Path) -> bool {
    if row.pgid != row.id.pid {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(attempt.join("tmp/children")) else {
        return false;
    };
    for entry in entries.filter_map(Result::ok).take(4096) {
        if entry.path().extension().is_none_or(|s| s != "json") {
            continue;
        }
        let Some(child) = document(&entry.path()) else {
            continue;
        };
        let Some(dir) = text(&child, "run_dir")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            continue;
        };
        let Some(owner) = document(&dir.join("dispatch-owner.json")) else {
            continue;
        };
        if integer(&owner, "schema_version") != Some(1)
            || !matches!(text(&owner, "kind"), Some("dispatch" | "batch"))
            || integer(&owner, "owner_pid") != Some(row.id.pid as i64)
            || integer(&owner, "owner_pgid") != Some(row.id.pid as i64)
            || text(&owner, "run_token").is_none()
            || text(&owner, "run_token") != text(&child, "run_token")
            || text(&owner, "run_dir").map(Path::new) != Some(dir.as_path())
            || text(&owner, "owner_started_at") != Some(files::canonical_start(row.epoch).as_str())
        {
            continue;
        }
        if custody(row, &dir.join("MANIFEST.md")) {
            return true;
        }
        // S2 supervisors can retain custody after an owner disappears. Only a
        // registered run's locks count; environment tokens grant no authority.
        if let Ok(tasks) = std::fs::read_dir(dir.join("dispatch/tasks")) {
            for task in tasks.filter_map(Result::ok).take(256) {
                if let Ok(attempts) = std::fs::read_dir(task.path()) {
                    for a in attempts.filter_map(Result::ok).take(256) {
                        if custody(row, &a.path().join("supervisor.lock")) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}
fn item(row: &Row) -> String {
    let command: String = row.command.chars().take(80).collect();
    format!(
        "{{\"pid\":{},\"start_time\":{},\"command\":{}}}",
        row.id.pid,
        files::quote(&row.id.start.to_string()),
        files::quote(&command)
    )
}
pub struct ProviderGroup {
    child: Option<Child>,
    pub pid: i32,
    root: Option<Identity>,
    tracked: BTreeMap<Identity, Row>,
    parents: BTreeMap<Identity, Identity>,
    spared: BTreeSet<Identity>,
    owners: BTreeSet<Identity>,
    shared: BTreeSet<Identity>,
    spared_seen: BTreeMap<Identity, Row>,
    reaped: BTreeMap<Identity, Row>,
    pub warnings: Notes,
    attempt: PathBuf,
    spare_host: bool,
    stop_at: Option<Instant>,
    child_grace: Duration,
    root_flush: Duration,
    desc_killed: bool,
    root_killed: bool,
    pub done: bool,
    pub status: Option<(Option<i32>, Option<i32>)>,
    last_sample: Instant,
    #[cfg(target_os = "linux")]
    previous_subreaper: i32,
}
impl ProviderGroup {
    pub fn enable_subreaper() -> io::Result<i32> {
        #[cfg(target_os = "linux")]
        unsafe {
            let mut old = 0;
            if libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut old, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            return Ok(old);
        }
        #[cfg(not(target_os = "linux"))]
        Ok(0)
    }
    pub fn new(
        child: Child,
        attempt: PathBuf,
        spare_host: bool,
        child_grace: u64,
        root_flush: u64,
        previous_subreaper: i32,
    ) -> Self {
        let pid = child.id() as i32;
        let mut group = Self {
            child: Some(child),
            pid,
            root: platform::process(pid).map(|r| r.id),
            tracked: BTreeMap::new(),
            parents: BTreeMap::new(),
            spared: BTreeSet::new(),
            owners: BTreeSet::new(),
            shared: BTreeSet::new(),
            spared_seen: BTreeMap::new(),
            reaped: BTreeMap::new(),
            warnings: Notes::default(),
            attempt,
            spare_host,
            stop_at: None,
            child_grace: Duration::from_millis(child_grace),
            root_flush: Duration::from_millis(root_flush),
            desc_killed: false,
            root_killed: false,
            done: false,
            status: None,
            last_sample: Instant::now(),
            #[cfg(target_os = "linux")]
            previous_subreaper,
        };
        let _ = previous_subreaper;
        group.sample();
        group
    }
    pub fn observed_exit(&mut self) -> bool {
        if self.status.is_some() {
            return true;
        }
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            if libc::waitid(
                libc::P_PID,
                self.pid as _,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            ) != 0
            {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::EINTR) {
                    self.warnings.add(format!("waitid: {e}"));
                }
                return false;
            }
            #[cfg(target_os = "linux")]
            let (pid, status) = (info.si_pid(), info.si_status());
            #[cfg(target_os = "macos")]
            let (pid, status) = (info.si_pid, info.si_status);
            if pid == self.pid {
                self.status = Some(if info.si_code == libc::CLD_EXITED {
                    (Some(status), None)
                } else {
                    (None, Some(status))
                });
                return true;
            }
        }
        false
    }
    pub fn sample(&mut self) {
        self.last_sample = Instant::now();
        let rows = match platform::snapshot() {
            Ok(r) => r,
            Err(e) => {
                self.warnings.add(format!("census: {e}"));
                return;
            }
        };
        if self.root.is_none() {
            self.root = rows.get(&self.pid).map(|r| r.id.clone());
        }
        let mut known: BTreeSet<Identity> = self.tracked.keys().cloned().collect();
        if let Some(root) = &self.root {
            known.insert(root.clone());
        }
        loop {
            let new: Vec<_> = rows
                .values()
                .filter_map(|r| {
                    if known.contains(&r.id) {
                        return None;
                    }
                    let p = rows.get(&r.ppid)?;
                    known.contains(&p.id).then(|| (r.clone(), p.id.clone()))
                })
                .collect();
            if new.is_empty() {
                break;
            }
            for (row, parent) in new {
                known.insert(row.id.clone());
                self.parents.insert(row.id.clone(), parent);
                self.tracked.insert(row.id.clone(), row);
            }
        }
        // A verified nested owner remains spared until this exact identity is
        // proven gone. Refresh the classification just before each signal.
        self.spared
            .retain(|id| rows.get(&id.pid).is_none_or(|r| r.id == *id && !r.zombie));
        for row in self.tracked.values() {
            if rows
                .get(&row.id.pid)
                .is_none_or(|r| r.id != row.id || r.zombie)
            {
                continue;
            }
            if self.spare_host
                && matches!(
                    row.command.as_str(),
                    "codex-code-mode-host" | "codex-code-mode"
                )
            {
                self.shared.insert(row.id.clone());
                self.spared.insert(row.id.clone());
            }
            if !self.spared.contains(&row.id)
                && row.pgid != self.pid
                && row.pgid != unsafe { libc::getpgrp() }
                && registered_owner(row, &self.attempt)
            {
                self.owners.insert(row.id.clone());
                self.spared.insert(row.id.clone());
            }
        }
        loop {
            let descendants: Vec<_> = self
                .parents
                .iter()
                .filter(|(id, p)| !self.spared.contains(*id) && self.spared.contains(*p))
                .map(|(id, _)| id.clone())
                .collect();
            if descendants.is_empty() {
                break;
            }
            self.spared.extend(descendants);
        }
        for row in rows.values() {
            if self.spared.contains(&row.id) && !row.zombie {
                self.spared_seen.insert(row.id.clone(), row.clone());
            }
        }
        for row in self.tracked.values_mut() {
            if let Some(new) = rows.get(&row.id.pid).filter(|r| r.id == row.id) {
                *row = new.clone();
            }
        }
    }
    fn live(&self) -> Vec<Row> {
        self.tracked
            .keys()
            .filter(|id| !self.spared.contains(*id))
            .filter_map(|id| platform::process(id.pid).filter(|r| r.id == *id && !r.zombie))
            .collect()
    }
    fn signal(&mut self, sig: i32, root: bool) {
        self.sample();
        let live = self.live();
        let shared = self.shared.iter().any(|id| self.spared.contains(id));
        let spared_groups: BTreeSet<_> = self
            .spared
            .iter()
            .filter_map(|id| {
                platform::process(id.pid)
                    .filter(|r| r.id == *id)
                    .map(|r| r.pgid)
            })
            .collect();
        let group = root && !shared && !spared_groups.contains(&self.pid);
        // We still own the unreaped leader, so this group ID cannot be reused.
        if group {
            unsafe {
                libc::kill(-self.pid, sig);
            }
        }
        for row in live {
            if sig == libc::SIGTERM && group && row.pgid == self.pid {
                continue;
            }
            if platform::process(row.id.pid).is_some_and(|r| r.id == row.id && !r.zombie) {
                unsafe {
                    libc::kill(row.id.pid, sig);
                }
            }
        }
        if root && (!group || sig == libc::SIGKILL) && !self.observed_exit() {
            unsafe {
                libc::kill(self.pid, sig);
            }
        }
    }
    /// Silence and I/O errors cannot construct a reason or call this method.
    pub fn stop(&mut self, _reason: StopReason) {
        if self.stop_at.is_some() || self.done {
            return;
        }
        self.sample();
        self.remember_reaped();
        self.stop_at = Some(Instant::now());
        self.signal(libc::SIGTERM, true);
    }
    fn remember_reaped(&mut self) {
        for row in self.live() {
            self.reaped.insert(row.id.clone(), row);
        }
    }
    pub fn tick(&mut self) {
        if self.done {
            return;
        }
        if self.last_sample.elapsed() >= Duration::from_millis(50) {
            self.sample();
        }
        let Some(start) = self.stop_at else {
            return;
        };
        let exited = self.observed_exit();
        if exited && self.live().is_empty() {
            self.finish();
            return;
        }
        self.remember_reaped();
        if !self.desc_killed && start.elapsed() >= self.child_grace {
            self.signal(libc::SIGKILL, false);
            self.desc_killed = true;
        }
        if !self.root_killed && start.elapsed() >= self.root_flush.max(self.child_grace) {
            self.signal(libc::SIGKILL, true);
            self.root_killed = true;
        }
        if self.root_killed && self.observed_exit() && self.live().is_empty() {
            self.finish();
        }
    }
    fn finish(&mut self) {
        // Cleanup is complete before Child::wait consumes/reaps the leader.
        if let Some(mut child) = self.child.take() {
            if let Err(e) = child.wait() {
                self.warnings.add(format!("wait: {e}"));
            }
        }
        #[cfg(target_os = "linux")]
        for id in self.tracked.keys().filter(|id| !self.spared.contains(*id)) {
            unsafe {
                libc::waitpid(id.pid, std::ptr::null_mut(), libc::WNOHANG);
            }
        }
        self.done = true;
    }
    pub fn reap_observer(child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
    }
    pub fn reaped(&self) -> Vec<String> {
        self.reaped
            .values()
            .filter(|r| !self.spared_seen.contains_key(&r.id))
            .map(item)
            .collect()
    }
    pub fn spared(&self) -> Vec<String> {
        self.spared_seen.values().map(item).collect()
    }
}
impl Drop for ProviderGroup {
    fn drop(&mut self) {
        if !self.done {
            self.stop(StopReason::SupervisorSignal);
            while !self.done {
                self.tick();
                unsafe {
                    libc::poll(std::ptr::null_mut(), 0, 10);
                }
            }
        }
        #[cfg(target_os = "linux")]
        unsafe {
            libc::prctl(
                libc::PR_SET_CHILD_SUBREAPER,
                self.previous_subreaper,
                0,
                0,
                0,
            );
        }
    }
}
