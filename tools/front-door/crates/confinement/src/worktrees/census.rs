//! Process census and worker observation.
use super::census_excusal::{
    ExcusalEvidence, PlacementEvidence, PolicyCwdObserver, UnreadableFailure,
    any_root_identity_unreadable, apply_override_floor, configured_entry_matches, excusal_applies,
    excuse_unreadable, listed_uid_excusal, resolve_entries_for_roots, unknown_worker_evidence,
    warn_unreadable_processes,
};
use super::*;

pub(super) const UNREADABLE_NO_CANDIDATE_PATH: &str =
    "unreadable process has no path under candidate roots";

/// One time budget (microseconds) shared by every advisory dweller census.
/// The same bound governs gc, hosts doctor and worktree doctor so a report
/// cannot silently change meaning between commands.
pub const HELD_CENSUS_BUDGET_MICROS: u64 = 2_000_000;

/// Printed by callers when a bounded dweller census did not finish. An empty
/// report must never read as "no dwellers" when the census was cut short.
pub const HELD_CENSUS_INCOMPLETE_WARNING: &str =
    "dweller census incomplete (budget/cancel); results may be missing";

/// One bounded dweller report. `incomplete` means the census was cut short by
/// its budget, cancellation or an observation gap, so rows may be missing.
#[derive(Clone, Debug, Default)]
pub struct HeldReport {
    pub processes: Vec<super::HeldProcess>,
    pub incomplete: bool,
}

/// Every process one census saw holding each root by cwd, executable image or
/// open descriptor. Diagnostic only: a cwd observation never grants signal
/// authority, so this is reported and never acted on.
pub type HeldProcesses = BTreeMap<PathBuf, Vec<ObservedProcess>>;

fn record_held(held: &mut HeldProcesses, root: &Path, process: &ObservedProcess) {
    let list = held.entry(root.to_path_buf()).or_default();
    if !list
        .iter()
        .any(|seen| seen.pid == process.pid && seen.kind == process.kind && seen.cwd == process.cwd)
    {
        list.push(process.clone());
    }
}

/// Whether a process the listing could not name may be named in a held
/// report. This applies the same caller-session and verified-ancestor
/// exclusion as the named paths: an ancestor is excluded by pid even when the
/// census could not describe it, and the caller's session is excluded when a
/// direct observation can name the process.
fn held_reportable(
    pid: i64,
    excluded: &BTreeSet<provenant_supervisor::census::ProcessIdentity>,
    current: &ProcessFact,
    observer: &dyn ProcessObserver,
) -> bool {
    if excluded.iter().any(|identity| identity.pid == pid) {
        return false;
    }
    match observer.observe(pid) {
        ProcessObservation::Present(fact) => {
            fact.identity.session_id != current.identity.session_id
        }
        _ => true,
    }
}

fn finish_held(mut held: HeldProcesses) -> HeldProcesses {
    for processes in held.values_mut() {
        processes.sort_by_key(|process| (process.pid, process.kind as u8));
    }
    held
}

/// When set by the gc CLI's `--full`, each PID is listed in addition to the
/// aggregate warning. Other callers keep the compact single-line warning.
pub(super) static UNREADABLE_DETAIL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// gc passes its global `--full` flag here so the census can keep per-PID EPERM
/// detail behind that flag instead of printing one line per process.
pub fn set_unreadable_census_detail(full: bool) {
    UNREADABLE_DETAIL.store(full, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn mark_unreadable_process_warning(boot: &str, pid: i64) -> bool {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<BTreeSet<(String, i64)>>> =
        std::sync::OnceLock::new();
    let warned = WARNED.get_or_init(|| std::sync::Mutex::new(BTreeSet::new()));
    let Ok(mut warned) = warned.lock() else {
        return false;
    };
    warned.insert((boot.to_owned(), pid))
}

/// Only a process that could own a candidate checkout can hold it. Credentials
/// read from the kernel decide the cohort: a process whose real, effective and
/// saved uids all differ from the current user and every root owner cannot hold
/// a same-user checkout. Missing credentials keep the warning, because an
/// unreadable process may still be the current user's. Foreign EPERM processes
/// (system daemons and other users on macOS included) say nothing.
pub(super) fn unreadable_cohort_pids(
    pids: &BTreeSet<i64>,
    cwd_observer: &dyn CwdObserver,
    roots: &[PathBuf],
) -> BTreeSet<i64> {
    let mut owners = vec![provenant_os::fs::effective_uid()];
    owners.extend(roots.iter().filter_map(|root| root_owner(root)));
    pids.iter()
        .copied()
        .filter(|pid| {
            cwd_observer
                .process_uids(*pid)
                .is_none_or(|uids| owners.iter().any(|owner| !uids.all_differ_from(*owner)))
        })
        .collect()
}

/// A single acquisition supplies both the decision and its refusal evidence.
#[derive(Clone, Default)]
pub struct WorkerEvidence {
    pub states: BTreeMap<PathBuf, Occupancy>,
    pub holders: BTreeMap<PathBuf, ObservedProcess>,
    /// Every process the same census saw inside each root. Diagnostic only.
    pub held: HeldProcesses,
    /// True when acquisition or coverage could have missed a dweller.
    pub held_incomplete: bool,
    pub details: BTreeMap<PathBuf, String>,
    pub taints: BTreeMap<PathBuf, TaintCause>,
}

fn attach_worker_evidence(
    states: &mut BTreeMap<PathBuf, Occupancy>,
    holders: &BTreeMap<PathBuf, ObservedProcess>,
    details: &BTreeMap<PathBuf, String>,
) {
    for (root, state) in states {
        match state {
            Occupancy::Unknown {
                scope,
                evidence,
                source,
            } => {
                *scope = root.clone();
                *source = OccupancyUnknownSource::Census;
                if let Some(detail) = details.get(root) {
                    *evidence = detail.clone();
                }
            }
            Occupancy::Observed { process } => {
                if let Some(holder) = holders.get(root) {
                    *process = Some(holder.clone());
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CensusEpochPolicy {
    pub entries: Vec<CensusIgnoreEntry>,
    pub exclude_ancestors: bool,
}

/// A scoped root's own identity. An absent root is decidable: nothing is held
/// at a path that does not exist, and a process inside a deleted tree is placed
/// against it lexically. `None` from [`root_identity`] means the root cannot be
/// read, including a dangling symlink, and keeps placement undecidable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootIdentity {
    Directory(u64, u64, u32),
    Absent,
}

fn root_identity(root: &Path) -> Option<RootIdentity> {
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(RootIdentity::Absent);
        }
        Err(_) => return None,
        Ok(_) => {}
    }
    let metadata = std::fs::metadata(root).ok()?;
    metadata
        .is_dir()
        .then(|| RootIdentity::Directory(metadata.dev(), metadata.ino(), metadata.uid()))
}

/// True when a scoped root does not exist, as opposed to being unreadable.
pub(super) fn root_is_absent(root: &Path) -> bool {
    root_identity(root) == Some(RootIdentity::Absent)
}

/// True when a scoped root's own identity cannot be read. Placement against it
/// is undecidable.
pub(super) fn root_is_undecidable(root: &Path) -> bool {
    root_identity(root).is_none()
}

/// Split an absolute path at its nearest existing directory: that directory's
/// `(dev, ino)` and the removed components below it. Identity, not spelling,
/// anchors the split, so a firmlink or symlink alias of the same directory
/// splits the same way. `None` for a relative path, a parent traversal, a
/// dangling symlink among the removed components or an unreadable ancestor.
pub(super) fn split_at_extant_directory(path: &Path) -> Option<((u64, u64), PathBuf)> {
    use std::path::Component;
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    let mut removed = Vec::new();
    for ancestor in path.ancestors() {
        match std::fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {
                let below = removed.iter().rev().collect::<PathBuf>();
                return Some(((metadata.dev(), metadata.ino()), below));
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(ancestor)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                removed.push(ancestor.file_name()?);
            }
            _ => return None,
        }
    }
    None
}

/// Place a path against a root that no longer exists. Nothing that still
/// exists can lie under a removed root, so a fully existing path is outside.
/// A path with removed components is inside exactly when it splits at the
/// root's nearest existing directory, by identity, and its removed components
/// start with the root's. `None` when the root exists or is unreadable, or
/// either path cannot be split.
pub(super) fn absent_root_contains(path: &Path, root: &Path) -> Option<bool> {
    if !root_is_absent(root) {
        return None;
    }
    let (root_directory, root_removed) = split_at_extant_directory(root)?;
    let (path_directory, path_removed) = split_at_extant_directory(path)?;
    // A removal between the two splits moves the anchor; undecidable, not outside.
    if split_at_extant_directory(root)? != (root_directory, root_removed.clone()) {
        return None;
    }
    if path_removed.as_os_str().is_empty() {
        return Some(false);
    }
    Some(path_directory == root_directory && path_removed.starts_with(&root_removed))
}

/// Rule out a removed root for a path that cannot be split because a
/// directory on it is unreadable to this user. A symlink-free path under the
/// removed root passes through every existing ancestor of that root, so when
/// the path's nearest readable directory is none of them, by identity, the
/// path cannot lie under it. Kernel-reported paths are symlink-free; a path
/// that shows a symlink below its nearest readable directory could still alias
/// the root and stays possibly inside, as does anything unresolved. The rule
/// assumes the root and the path are seen through one mount view.
fn absent_root_could_hold(path: &Path, root: &Path) -> bool {
    let directory = |candidate: &Path| {
        std::fs::metadata(candidate)
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .map(|metadata| (metadata.dev(), metadata.ino()))
    };
    let root_ancestors = root
        .ancestors()
        .skip(1)
        .filter_map(directory)
        .collect::<Vec<_>>();
    if root_ancestors.is_empty() {
        return true;
    }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => return true,
            Ok(metadata) if metadata.is_dir() => {
                return root_ancestors.contains(&(metadata.dev(), metadata.ino()));
            }
            _ => {}
        }
    }
    true
}

/// The owner of a scoped root. A removed root is owned as its nearest existing
/// directory is; an unreadable root has no provable owner.
pub(super) fn root_owner(root: &Path) -> Option<u32> {
    match root_identity(root)? {
        RootIdentity::Directory(_, _, uid) => Some(uid),
        RootIdentity::Absent => root
            .ancestors()
            .skip(1)
            .find_map(|ancestor| {
                std::fs::metadata(ancestor)
                    .ok()
                    .filter(|meta| meta.is_dir())
            })
            .map(|metadata| metadata.uid()),
    }
}

fn root_identities(roots: &[PathBuf]) -> Option<BTreeMap<PathBuf, RootIdentity>> {
    roots
        .iter()
        .map(|root| Some((root.clone(), root_identity(root)?)))
        .collect()
}

struct CensusEpoch {
    roots: BTreeMap<PathBuf, RootIdentity>,
    for_land: bool,
    policy: CensusEpochPolicy,
    observed_at: provenant_core::MonoTime,
    evidence: WorkerEvidence,
}

/// Command-local census. Changing the root set acquires a new complete result;
/// results are never carried across commands or explicit mutation fences.
pub struct CensusSession<'a> {
    inner: &'a dyn WorkerObserver,
    clock: &'a dyn Clock,
    epoch: std::sync::Mutex<Option<CensusEpoch>>,
}

impl<'a> CensusSession<'a> {
    pub fn new(inner: &'a dyn WorkerObserver, clock: &'a dyn Clock) -> Self {
        Self {
            inner,
            clock,
            epoch: std::sync::Mutex::new(None),
        }
    }

    fn evidence(&self, roots: &[PathBuf], for_land: bool) -> WorkerEvidence {
        let mut epoch = self
            .epoch
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let requested = root_identities(roots);
        let policy = self.inner.epoch_policy();
        if let (Some(current), Some(requested), Some(policy)) = (&*epoch, &requested, &policy) {
            let existing: Vec<_> = current.roots.keys().cloned().collect();
            if current.for_land == for_land
                && &current.policy == policy
                && root_identities(&existing).as_ref() == Some(&current.roots)
                && requested
                    .iter()
                    .all(|(path, id)| current.roots.get(path) == Some(id))
            {
                let mut result = current.evidence.clone();
                result.states.retain(|root, _| requested.contains_key(root));
                result
                    .holders
                    .retain(|root, _| requested.contains_key(root));
                result
                    .details
                    .retain(|root, _| requested.contains_key(root));
                result.taints.retain(|root, _| requested.contains_key(root));
                return result;
            }
        }
        let before: BTreeMap<_, _> = roots
            .iter()
            .map(|root| (root.clone(), root_identity(root)))
            .collect();
        let mut evidence = match &policy {
            Some(policy) => self.inner.observe_captured_policy(roots, for_land, policy),
            None => self.inner.observe_evidence(roots, for_land),
        };
        for root in roots {
            evidence.states.entry(root.clone()).or_insert_with(|| {
                Occupancy::census_unknown(
                    root.clone(),
                    "process census did not return the requested root",
                )
            });
        }
        for root in roots {
            match evidence.states.get(root) {
                Some(Occupancy::Observed { .. }) => {
                    evidence.taints.entry(root.clone()).or_insert_with(|| {
                        evidence
                            .holders
                            .get(root)
                            .cloned()
                            .map(TaintCause::LiveWriter)
                            .unwrap_or(TaintCause::UnattributedLiveWriter)
                    });
                }
                Some(Occupancy::Unknown { .. }) => {
                    evidence
                        .taints
                        .entry(root.clone())
                        .or_insert(TaintCause::Uncertainty(
                            UncertaintyReason::CensusUnavailable,
                        ));
                }
                _ => {
                    evidence.taints.remove(root);
                }
            }
        }
        let stable = requested.filter(|before| root_identities(roots).as_ref() == Some(before));
        let stable_policy =
            policy.filter(|before| self.inner.epoch_policy().as_ref() == Some(before));
        if stable.is_none() {
            for root in roots {
                let identity = before.get(root).copied().flatten();
                if identity.is_some() && identity == root_identity(root) {
                    continue;
                }
                if !evidence
                    .states
                    .get(root)
                    .is_some_and(Occupancy::is_observed)
                {
                    evidence.states.insert(
                        root.clone(),
                        Occupancy::census_unknown(
                            root.clone(),
                            "census observation could not be verified",
                        ),
                    );
                    evidence.taints.insert(
                        root.clone(),
                        TaintCause::Uncertainty(UncertaintyReason::RootUnreadable),
                    );
                    evidence
                        .details
                        .entry(root.clone())
                        .or_insert_with(|| "root identity unavailable or changed".into());
                }
            }
        }
        if stable_policy.is_none() {
            for root in roots {
                if !evidence
                    .states
                    .get(root)
                    .is_some_and(Occupancy::is_observed)
                {
                    evidence.states.insert(
                        root.clone(),
                        Occupancy::census_unknown(
                            root.clone(),
                            "census observation could not be verified",
                        ),
                    );
                    evidence.taints.insert(
                        root.clone(),
                        TaintCause::Uncertainty(UncertaintyReason::PolicyRejected),
                    );
                    evidence
                        .details
                        .entry(root.clone())
                        .or_insert_with(|| "census policy unavailable or changed".into());
                }
            }
        }
        attach_worker_evidence(&mut evidence.states, &evidence.holders, &evidence.details);
        *epoch = stable
            .zip(stable_policy)
            .map(|(roots, policy)| CensusEpoch {
                roots,
                for_land,
                policy,
                observed_at: self.clock.mono_now(),
                evidence: evidence.clone(),
            });
        evidence
    }

    pub fn invalidate(&self) {
        *self
            .epoch
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }

    pub fn observed_at(&self) -> Option<provenant_core::MonoTime> {
        self.epoch
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .map(|epoch| epoch.observed_at)
    }
}

impl WorkerObserver for CensusSession<'_> {
    fn reobserve_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.invalidate();
        let before = root_identities(roots)?;
        let policy = self.inner.epoch_policy()?;
        let (mut states, details) = self
            .inner
            .reobserve_roots_with_policy_while(roots, &policy, keep_going)?;
        if root_identities(roots).as_ref() != Some(&before)
            || self.inner.epoch_policy().as_ref() != Some(&policy)
        {
            for root in roots {
                if !states.get(root).is_some_and(Occupancy::is_observed) {
                    states.insert(
                        root.clone(),
                        Occupancy::census_unknown(
                            root.clone(),
                            "root identity or census policy changed during census",
                        ),
                    );
                }
            }
        }
        keep_going().then_some((states, details))
    }
    fn observe_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.invalidate();
        let before = root_identities(roots);
        let policy = self.inner.epoch_policy()?;
        let (mut states, details) = self
            .inner
            .observe_roots_with_policy_while(roots, &policy, keep_going)?;
        if before.is_none()
            || root_identities(roots) != before
            || self.inner.epoch_policy().as_ref() != Some(&policy)
        {
            for root in roots {
                if !states.get(root).is_some_and(Occupancy::is_observed) {
                    states.insert(
                        root.clone(),
                        Occupancy::census_unknown(
                            root.clone(),
                            "root identity or census policy changed during census",
                        ),
                    );
                }
            }
        }
        keep_going().then_some((states, details))
    }

    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        self.evidence(roots, false).states
    }
    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        self.evidence(roots, true).states
    }
    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        let e = self.evidence(roots, false);
        (e.states, e.details)
    }
    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        let e = self.evidence(roots, true);
        (e.states, e.details)
    }
    fn observe_evidence(&self, roots: &[PathBuf], for_land: bool) -> WorkerEvidence {
        self.evidence(roots, for_land)
    }
    fn observe_evidence_while(
        &self,
        roots: &[PathBuf],
        for_land: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerEvidence> {
        let policy = self.inner.epoch_policy()?;
        self.inner.observe_evidence_with_policy_while(
            roots,
            &policy.entries,
            for_land || policy.exclude_ancestors,
            keep_going,
        )
    }
    fn observe_roots_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        let e = self.evidence(roots, false);
        (e.states, e.taints)
    }
    fn observe_roots_for_land_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        let e = self.evidence(roots, true);
        (e.states, e.taints)
    }
    fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
        self.evidence(&[root.to_path_buf()], false)
            .holders
            .remove(root)
    }
    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        self.evidence(&[root.to_path_buf()], true)
            .holders
            .remove(root)
    }
    fn requires_complete_census(&self) -> bool {
        self.inner.requires_complete_census()
    }
    fn incomplete_census_detail(&self) -> String {
        self.inner.incomplete_census_detail()
    }
    fn invalidate(&self) {
        CensusSession::invalidate(self)
    }
}

/// Census one writer checkout with its detail. A tree the census does not
/// return is Unknown, never free. A removed checkout is censused like any other
/// root: it is free only when no process path lies under it.
pub fn observe_writer_tree(
    workers: &dyn WorkerObserver,
    path: &Path,
) -> (Occupancy, Option<String>) {
    let path = path.to_path_buf();
    let (states, details) = workers.observe_roots_with_details(std::slice::from_ref(&path));
    let state = states.get(&path).cloned().unwrap_or_else(|| {
        Occupancy::census_unknown(
            path.clone(),
            "worker census did not return the requested tree",
        )
    });
    (state, details.get(&path).cloned())
}

/// Injectable native observation seam. Implementations return one conservative
/// state per canonical candidate root; they never grant signal authority.
pub trait WorkerObserver: Send + Sync {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy>;

    /// Retry evidence must account for occupancy gaps even if first admission excuses them.
    fn reobserve_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.observe_roots_while(roots, keep_going)
    }

    fn reobserve_roots_with_policy_while(
        &self,
        roots: &[PathBuf],
        policy: &CensusEpochPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.observe_roots_with_policy_while(roots, policy, keep_going)
    }

    /// Fresh retry evidence, discarded if the caller's deadline expires.
    /// Native observers also poll the bound between process reads.
    fn observe_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        if !keep_going() {
            return None;
        }
        let evidence = self.observe_roots_with_details(roots);
        keep_going().then_some(evidence)
    }

    fn observe_roots_with_policy_while(
        &self,
        roots: &[PathBuf],
        policy: &CensusEpochPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        if !keep_going() {
            return None;
        }
        let evidence =
            self.observe_roots_with_policy(roots, &policy.entries, policy.exclude_ancestors);
        keep_going().then_some(evidence)
    }

    /// Effective policy defining the current epoch; `None` prevents reuse.
    fn epoch_policy(&self) -> Option<CensusEpochPolicy> {
        Some(CensusEpochPolicy::default())
    }

    fn observe_evidence(&self, roots: &[PathBuf], for_land: bool) -> WorkerEvidence {
        let (mut states, details) = if for_land {
            self.observe_roots_for_land_with_details(roots)
        } else {
            self.observe_roots_with_details(roots)
        };
        attach_worker_evidence(&mut states, &BTreeMap::new(), &details);
        WorkerEvidence {
            taints: default_taints(&states, &|_| None),
            held_incomplete: states
                .values()
                .any(|state| matches!(state, Occupancy::Unknown { .. })),
            states,
            holders: BTreeMap::new(),
            held: BTreeMap::new(),
            details,
        }
    }

    /// One bounded acquisition carrying both custody and every process seen
    /// inside each root. Native observers do a single census; injected seams
    /// may derive the named holder from one observation.
    fn observe_evidence_while(
        &self,
        roots: &[PathBuf],
        for_land: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerEvidence> {
        if !keep_going() {
            return None;
        }
        let (mut states, details) = if for_land {
            self.observe_roots_for_land_with_details(roots)
        } else {
            self.observe_roots_with_details(roots)
        };
        let mut held = BTreeMap::new();
        for root in states.keys() {
            if !keep_going() {
                return None;
            }
            if let Some(process) = self.live_process(root) {
                record_held(&mut held, root, &process);
            }
        }
        attach_worker_evidence(&mut states, &BTreeMap::new(), &details);
        let evidence = WorkerEvidence {
            taints: default_taints(&states, &|_| None),
            held_incomplete: states
                .values()
                .any(|state| matches!(state, Occupancy::Unknown { .. })),
            states,
            holders: BTreeMap::new(),
            held: finish_held(held),
            details,
        };
        keep_going().then_some(evidence)
    }

    /// Bounded acquisition under the caller's host policy. The default ignores
    /// host configuration, as injected observers own their refusal semantics.
    fn observe_evidence_with_policy_while(
        &self,
        roots: &[PathBuf],
        _entries: &[CensusIgnoreEntry],
        for_land: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerEvidence> {
        self.observe_evidence_while(roots, for_land, keep_going)
    }

    /// Acquire with the same policy used to identify the session epoch.
    fn observe_captured_policy(
        &self,
        roots: &[PathBuf],
        for_land: bool,
        _policy: &CensusEpochPolicy,
    ) -> WorkerEvidence {
        self.observe_evidence(roots, for_land)
    }

    /// Discard command-local evidence before a custody or mutation boundary.
    fn invalidate(&self) {}

    /// Landing requires positive observation of a free root. Ordinary removal
    /// still relies on writer custody when the host census is unobservable.
    fn requires_complete_census(&self) -> bool {
        false
    }

    /// Landing may remove its caller's checkout. Exclude only the caller and
    /// its verified ancestors through the session leader, never other workers.
    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        self.observe_roots(roots)
    }

    /// One process observed inside `root`, for a refusal that names it. The
    /// answer never decides custody; an observer without one returns `None`.
    fn live_process(&self, _root: &Path) -> Option<ObservedProcess> {
        None
    }

    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        self.live_process(root)
    }

    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        (self.observe_roots(roots), BTreeMap::new())
    }

    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        (self.observe_roots_for_land(roots), BTreeMap::new())
    }

    fn observe_roots_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        let (states, _) = self.observe_roots_with_details(roots);
        let taints = default_taints(&states, &|root| self.live_process(root));
        (states, taints)
    }

    fn observe_roots_for_land_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        let (states, _) = self.observe_roots_for_land_with_details(roots);
        let taints = default_taints(&states, &|root| self.live_process_for_land(root));
        (states, taints)
    }

    /// Composition supplies host configuration. Injected observers retain
    /// their own refusal semantics; the native observer uses these entries.
    fn observe_evidence_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerEvidence {
        let (states, details) = self.observe_roots_with_policy(roots, entries, for_land);
        WorkerEvidence {
            taints: default_taints(&states, &|_| None),
            held_incomplete: states
                .values()
                .any(|state| matches!(state, Occupancy::Unknown { .. })),
            states,
            holders: BTreeMap::new(),
            held: BTreeMap::new(),
            details,
        }
    }

    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        _entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        if for_land {
            self.observe_roots_for_land_with_details(roots)
        } else {
            self.observe_roots_with_details(roots)
        }
    }

    fn observe_roots_with_policy_taints(
        &self,
        roots: &[PathBuf],
        _entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerTaintObservations {
        if for_land {
            self.observe_roots_for_land_with_taints(roots)
        } else {
            self.observe_roots_with_taints(roots)
        }
    }

    fn live_process_with_policy(
        &self,
        root: &Path,
        _entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> Option<ObservedProcess> {
        if for_land {
            self.live_process_for_land(root)
        } else {
            self.live_process(root)
        }
    }

    /// A diagnostic for an incomplete census. It never affects custody; the
    /// default is used by observers that cannot identify the observation.
    fn incomplete_census_detail(&self) -> String {
        "process census unavailable".into()
    }
}

fn default_taints(
    states: &BTreeMap<PathBuf, Occupancy>,
    live: &dyn Fn(&Path) -> Option<ObservedProcess>,
) -> BTreeMap<PathBuf, TaintCause> {
    states
        .iter()
        .filter_map(|(root, state)| {
            let taint = match state {
                Occupancy::Available => return None,
                Occupancy::Held => TaintCause::LockContention,
                Occupancy::Observed { process } => {
                    process.clone().or_else(|| live(root)).map_or_else(
                        || TaintCause::UnattributedLiveWriter,
                        TaintCause::LiveWriter,
                    )
                }
                Occupancy::Unknown { .. } => {
                    TaintCause::Uncertainty(UncertaintyReason::CensusUnavailable)
                }
            };
            Some((root.clone(), taint))
        })
        .collect()
}

/// Immutable host policy around either native observations or an injected seam.
pub struct CensusPolicyWorkers<'a> {
    inner: &'a dyn WorkerObserver,
    pub(super) entries: Vec<CensusIgnoreEntry>,
    pub(super) exclude_ancestors: bool,
    pub(super) root_floor: Option<provenant_core::WallTime>,
    roots: provenant_core::Roots,
    for_run: bool,
}
impl<'a> CensusPolicyWorkers<'a> {
    pub fn from_roots(
        inner: &'a dyn WorkerObserver,
        roots: &provenant_core::Roots,
    ) -> Result<Self, WorktreeError> {
        Self::configured(inner, roots, false)
    }

    /// Apply the host ancestor-exclusion setting only for run admission.
    pub fn for_run(
        inner: &'a dyn WorkerObserver,
        roots: &provenant_core::Roots,
    ) -> Result<Self, WorktreeError> {
        Self::configured(inner, roots, true)
    }

    fn configured(
        inner: &'a dyn WorkerObserver,
        roots: &provenant_core::Roots,
        for_run: bool,
    ) -> Result<Self, WorktreeError> {
        let config = provenant_config::read_knobs(roots)?;
        Ok(Self {
            inner,
            entries: config.census_ignore_unreadable()?,
            exclude_ancestors: for_run && config.census_exclude_ancestors()?,
            root_floor: None,
            roots: roots.clone(),
            for_run,
        })
    }
}
impl WorkerObserver for CensusPolicyWorkers<'_> {
    fn reobserve_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.inner
            .reobserve_roots_with_policy_while(roots, &self.epoch_policy()?, keep_going)
    }
    fn reobserve_roots_with_policy_while(
        &self,
        roots: &[PathBuf],
        policy: &CensusEpochPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.inner
            .reobserve_roots_with_policy_while(roots, policy, keep_going)
    }
    fn observe_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        if !keep_going() {
            return None;
        }
        let policy = self.epoch_policy()?;
        self.inner
            .observe_roots_with_policy_while(roots, &policy, keep_going)
    }
    fn observe_roots_with_policy_while(
        &self,
        roots: &[PathBuf],
        policy: &CensusEpochPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.inner
            .observe_roots_with_policy_while(roots, policy, keep_going)
    }

    fn epoch_policy(&self) -> Option<CensusEpochPolicy> {
        let config = provenant_config::read_knobs(&self.roots).ok()?;
        Some(CensusEpochPolicy {
            entries: apply_override_floor(config.census_ignore_unreadable().ok()?, self.root_floor),
            exclude_ancestors: self.for_run && config.census_exclude_ancestors().ok()?,
        })
    }
    fn observe_evidence(&self, roots: &[PathBuf], for_land: bool) -> WorkerEvidence {
        let Some(policy) = self.epoch_policy() else {
            return WorkerEvidence {
                states: roots
                    .iter()
                    .cloned()
                    .map(|root| {
                        (
                            root.clone(),
                            Occupancy::census_unknown(root, "process census unavailable"),
                        )
                    })
                    .collect(),
                held_incomplete: true,
                ..WorkerEvidence::default()
            };
        };
        self.inner.observe_evidence_with_policy(
            roots,
            &policy.entries,
            for_land || policy.exclude_ancestors,
        )
    }
    fn observe_captured_policy(
        &self,
        roots: &[PathBuf],
        for_land: bool,
        policy: &CensusEpochPolicy,
    ) -> WorkerEvidence {
        self.inner.observe_evidence_with_policy(
            roots,
            &policy.entries,
            for_land || policy.exclude_ancestors,
        )
    }
    fn observe_evidence_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerEvidence {
        self.inner
            .observe_evidence_with_policy(roots, entries, for_land)
    }
    fn observe_evidence_while(
        &self,
        roots: &[PathBuf],
        for_land: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerEvidence> {
        let policy = self.epoch_policy()?;
        self.inner.observe_evidence_with_policy_while(
            roots,
            &policy.entries,
            for_land || policy.exclude_ancestors,
            keep_going,
        )
    }
    fn observe_evidence_with_policy_while(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerEvidence> {
        self.inner
            .observe_evidence_with_policy_while(roots, entries, for_land, keep_going)
    }
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, entries, for_land)
    }
    fn observe_roots_with_policy_taints(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerTaintObservations {
        self.inner
            .observe_roots_with_policy_taints(roots, entries, for_land)
    }
    fn live_process_with_policy(
        &self,
        root: &Path,
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> Option<ObservedProcess> {
        self.inner.live_process_with_policy(root, entries, for_land)
    }
    fn requires_complete_census(&self) -> bool {
        self.inner.requires_complete_census()
    }
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        self.observe_roots_with_details(roots).0
    }
    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        self.observe_roots_for_land_with_details(roots).0
    }
    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, &self.entries, self.exclude_ancestors)
    }
    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, &self.entries, true)
    }
    fn observe_roots_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        self.inner
            .observe_roots_with_policy_taints(roots, &self.entries, false)
    }
    fn observe_roots_for_land_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        self.inner
            .observe_roots_with_policy_taints(roots, &self.entries, true)
    }
    fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
        self.inner
            .live_process_with_policy(root, &self.entries, self.exclude_ancestors)
    }
    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        self.inner
            .live_process_with_policy(root, &self.entries, true)
    }
    fn incomplete_census_detail(&self) -> String {
        self.inner.incomplete_census_detail()
    }
}

/// Report every process one bounded census saw inside any of `roots`. The
/// report is advisory: no caller of this function may signal a named PID from
/// a cwd, executable or descriptor observation. A census cut short by its
/// budget, cancellation or incomplete observation is reported rather than as no rows.
pub fn held_report(
    workers: &dyn WorkerObserver,
    roots: &[PathBuf],
    keep_going: &dyn Fn() -> bool,
) -> HeldReport {
    // `for_land` carries the ancestor-exclusion policy into the census: the
    // caller's own session and verified ancestors are never stray dwellers.
    let Some(evidence) = workers.observe_evidence_while(roots, true, keep_going) else {
        return HeldReport {
            incomplete: true,
            ..HeldReport::default()
        };
    };
    HeldReport {
        processes: evidence
            .held
            .iter()
            .flat_map(|(root, processes)| {
                processes
                    .iter()
                    .filter_map(|process| super::HeldProcess::new(root, process))
            })
            .collect(),
        incomplete: evidence.held_incomplete,
    }
}

/// The refusal for a root observed live: it names the process when the
/// observer can, and is the plain live-writer refusal otherwise.
pub fn live_refusal(workers: &dyn WorkerObserver, root: &Path) -> WorktreeError {
    workers
        .live_process(root)
        .map_or(WorktreeError::LiveWriter, WorktreeError::LiveProcess)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWorkerObserver;

impl WorkerObserver for SystemWorkerObserver {
    fn reobserve_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.reobserve_roots_with_policy_while(roots, &CensusEpochPolicy::default(), keep_going)
    }
    fn reobserve_roots_with_policy_while(
        &self,
        roots: &[PathBuf],
        policy: &CensusEpochPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        if !keep_going() {
            return None;
        }
        let evidence = system_worker_evidence_while(
            roots,
            policy.exclude_ancestors,
            &policy.entries,
            Some(keep_going),
        );
        keep_going().then_some((evidence.states, evidence.details))
    }
    fn observe_roots_while(
        &self,
        roots: &[PathBuf],
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        self.observe_roots_with_policy_while(roots, &CensusEpochPolicy::default(), keep_going)
    }
    fn observe_roots_with_policy_while(
        &self,
        roots: &[PathBuf],
        policy: &CensusEpochPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerObservations> {
        if !keep_going() {
            return None;
        }
        let evidence = system_worker_evidence_while(
            roots,
            policy.exclude_ancestors,
            &policy.entries,
            Some(keep_going),
        );
        keep_going().then_some((evidence.states, evidence.details))
    }

    fn observe_evidence_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerEvidence {
        system_worker_evidence(roots, for_land, entries)
    }
    fn observe_evidence(&self, roots: &[PathBuf], for_land: bool) -> WorkerEvidence {
        system_worker_evidence(roots, for_land, &[])
    }
    fn observe_evidence_while(
        &self,
        roots: &[PathBuf],
        for_land: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerEvidence> {
        self.observe_evidence_with_policy_while(roots, &[], for_land, keep_going)
    }
    fn observe_evidence_with_policy_while(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<WorkerEvidence> {
        if !keep_going() {
            return None;
        }
        let evidence = system_worker_evidence_while(roots, for_land, entries, Some(keep_going));
        keep_going().then_some(evidence)
    }
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        system_worker_roots_with_policy(roots, for_land, entries)
    }
    fn observe_roots_with_policy_taints(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerTaintObservations {
        system_worker_roots_with_policy_taints(roots, for_land, entries)
    }
    fn live_process_with_policy(
        &self,
        root: &Path,
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> Option<ObservedProcess> {
        system_live_process_with_policy(root, for_land, entries)
    }
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        system_worker_roots(roots, false).0
    }

    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        system_worker_roots(roots, true).0
    }

    fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
        system_live_process(root, false)
    }

    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        system_live_process(root, true)
    }

    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        system_worker_roots(roots, false)
    }

    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        system_worker_roots(roots, true)
    }

    fn observe_roots_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        system_worker_roots_with_taints(roots, false)
    }

    fn observe_roots_for_land_with_taints(&self, roots: &[PathBuf]) -> WorkerTaintObservations {
        system_worker_roots_with_taints(roots, true)
    }
}

pub(super) fn system_live_process(root: &Path, ancestors: bool) -> Option<ObservedProcess> {
    system_live_process_with_policy(root, ancestors, &[])
}

pub(super) fn system_live_process_with_policy(
    root: &Path,
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
) -> Option<ObservedProcess> {
    let ProcessObservation::Present(current) = current_process() else {
        return None;
    };
    let roots = [root.to_path_buf()];
    observe_worker_holders(
        &roots,
        &SystemProcessObserver,
        &PolicyCwdObserver::for_roots(&roots, entries),
        &current,
        ancestors,
    )
    .1
    .remove(root)
}

pub(super) fn system_worker_roots(roots: &[PathBuf], ancestors: bool) -> WorkerObservations {
    system_worker_roots_with_policy(roots, ancestors, &[])
}

pub(super) fn system_worker_roots_with_taints(
    roots: &[PathBuf],
    ancestors: bool,
) -> WorkerTaintObservations {
    system_worker_roots_with_policy_taints(roots, ancestors, &[])
}

pub(super) fn system_worker_roots_with_policy(
    roots: &[PathBuf],
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
) -> WorkerObservations {
    let evidence = system_worker_evidence(roots, ancestors, entries);
    (evidence.states, evidence.details)
}

fn system_worker_evidence(
    roots: &[PathBuf],
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
) -> WorkerEvidence {
    system_worker_evidence_while(roots, ancestors, entries, None)
}

fn system_worker_evidence_while(
    roots: &[PathBuf],
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
    keep_going: Option<&dyn Fn() -> bool>,
) -> WorkerEvidence {
    let ProcessObservation::Present(current) = current_process() else {
        let detail = process_detail(
            &PolicyCwdObserver::for_roots(roots, entries),
            i64::from(std::process::id()),
            "current process unavailable",
        );
        return unknown_worker_evidence(roots, &detail);
    };
    worker_evidence_from_census(
        roots,
        ancestors,
        &SystemProcessObserver,
        &PolicyCwdObserver::for_roots(roots, entries),
        &current,
        keep_going,
        entries,
    )
}

pub(super) fn worker_evidence_from_census(
    roots: &[PathBuf],
    ancestors: bool,
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    keep_going: Option<&dyn Fn() -> bool>,
    entries: &[CensusIgnoreEntry],
) -> WorkerEvidence {
    let mut incomplete = false;
    let mut unreadable_warnings = BTreeSet::new();
    let mut held = BTreeMap::new();
    let (states, holders, details, taints) = observe_worker_holders_while(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        &mut |pid, condition| {
            incomplete = true;
            if condition == Some(UNREADABLE_NO_CANDIDATE_PATH)
                && let Some(pid) = pid
            {
                unreadable_warnings.insert(pid);
            }
        },
        &mut held,
        keep_going,
        entries,
    );
    warn_unreadable_processes(
        &current.identity.boot,
        &unreadable_cohort_pids(&unreadable_warnings, cwd_observer, roots),
    );
    WorkerEvidence {
        held_incomplete: incomplete
            || states
                .values()
                .any(|state| matches!(state, Occupancy::Unknown { .. })),
        states,
        holders,
        held: finish_held(held),
        details,
        taints,
    }
}

pub(super) fn system_worker_roots_with_policy_taints(
    roots: &[PathBuf],
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
) -> WorkerTaintObservations {
    let ProcessObservation::Present(current) = current_process() else {
        return (
            roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root.clone(),
                        Occupancy::census_unknown(root, "process census unavailable"),
                    )
                })
                .collect(),
            roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root,
                        TaintCause::Uncertainty(UncertaintyReason::CensusUnavailable),
                    )
                })
                .collect(),
        );
    };
    observe_worker_root_taints(
        roots,
        &SystemProcessObserver,
        &PolicyCwdObserver::for_roots(roots, entries),
        &current,
        ancestors,
    )
}

#[cfg(test)]
pub(super) fn observe_worker_roots(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
) -> WorkerObservations {
    let (states, _, details) =
        observe_worker_holders(roots, observer, cwd_observer, current, ancestors);
    (states, details)
}

pub(super) fn observe_worker_root_taints(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
) -> WorkerTaintObservations {
    let (states, _, _, taints) = observe_worker_holders_with_taints_and_audit(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        &mut |_, _| {},
        &[],
    );
    (states, taints)
}

/// The per-root state, and the first process seen inside each root.
pub(super) fn observe_worker_holders(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
) -> (
    BTreeMap<PathBuf, Occupancy>,
    BTreeMap<PathBuf, ObservedProcess>,
    BTreeMap<PathBuf, String>,
) {
    let (states, holders, details, _) = observe_worker_holders_with_taints_and_audit(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        &mut |_, _| {},
        &[],
    );
    (states, holders, details)
}

/// Audit the same decisions that produce custody, without a second census or
/// diagnostic re-query deciding why an image failed the policy.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn observe_worker_holders_with_audit(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
) -> (
    BTreeMap<PathBuf, Occupancy>,
    BTreeMap<PathBuf, ObservedProcess>,
    BTreeMap<PathBuf, String>,
) {
    let (states, holders, details, _) = observe_worker_holders_with_taints_and_audit(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        unplaceable,
        &[],
    );
    (states, holders, details)
}

type WorkerHolderAudit = (
    BTreeMap<PathBuf, Occupancy>,
    BTreeMap<PathBuf, ObservedProcess>,
    BTreeMap<PathBuf, String>,
    BTreeMap<PathBuf, TaintCause>,
);

pub(super) fn observe_worker_holders_with_taints_and_audit(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
    entries: &[CensusIgnoreEntry],
) -> WorkerHolderAudit {
    let mut held = BTreeMap::new();
    observe_worker_holders_while(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        unplaceable,
        &mut held,
        None,
        entries,
    )
}

/// Collect every process a single census places inside each root into `held`.
/// The caller's own identity and verified ancestors are already excluded by
/// the census; none of these observations grants signal authority.
#[allow(clippy::too_many_arguments)] // The holder audit and held report share one bounded census.
pub(super) fn observe_worker_holders_while(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
    held: &mut HeldProcesses,
    keep_going: Option<&dyn Fn() -> bool>,
    entries: &[CensusIgnoreEntry],
) -> WorkerHolderAudit {
    let within_budget = || keep_going.is_none_or(|check| check());
    let mut states: BTreeMap<_, _> = roots
        .iter()
        .cloned()
        .map(|root| (root, Occupancy::Available))
        .collect();
    let mut holders = BTreeMap::new();
    let mut details = BTreeMap::new();
    let mut taints = BTreeMap::new();
    if roots.is_empty() {
        return (states, holders, details, taints);
    }
    let resolved_entries = resolve_entries_for_roots(roots, entries);
    let entries = resolved_entries.as_ref();
    let census = match observer.census_while(65_536, &within_budget) {
        Ok(census) => census,
        Err(error) => {
            if !within_budget() {
                return (states, holders, details, taints);
            }
            unplaceable(None, Some("complete process census unavailable"));
            let detail = if matches!(
                error,
                provenant_supervisor::census::CensusError::ProcessList(_)
            ) {
                "process listing failed"
            } else {
                "process census unavailable"
            };
            let unknown = roots
                .iter()
                .cloned()
                .map(|root| (root.clone(), Occupancy::census_unknown(root, detail)))
                .collect();
            return (
                unknown,
                holders,
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, detail.into()))
                    .collect(),
                roots
                    .iter()
                    .cloned()
                    .map(|root| {
                        (
                            root,
                            TaintCause::Uncertainty(UncertaintyReason::CensusUnavailable),
                        )
                    })
                    .collect(),
            );
        }
    };
    // Exclude the caller and its verified ancestors before any held rows are
    // recorded, including EPERM processes that a direct census could not name.
    // A walk that stops early leaves the remaining ancestors in the census,
    // where one inside a root still makes it live, so an incomplete ancestry
    // hides nothing.
    let mut excluded = BTreeSet::from([current.identity.clone()]);
    if ancestors {
        let mut child = current;
        while child.identity.pid != current.identity.session_id {
            if !within_budget() {
                return (states, holders, details, taints);
            }
            let ProcessLookup::Present(parent) = census.process(child.parent_pid) else {
                break;
            };
            if parent.identity.boot != current.identity.boot
                || parent.identity.session_id != current.identity.session_id
                || census.ancestry(&child.identity, &parent.identity) != Ancestry::Yes
                || !excluded.insert(parent.identity.clone())
            {
                break;
            }
            child = parent;
        }
    }
    // Custody is decided per root. A gap that could hide a process anywhere
    // (an incomplete or truncated listing, or a process that could not be
    // described or placed) leaves every root that is not positively live
    // unknown. Foreign system credentials are an explicit host trust policy,
    // not proof against a privileged process retaining a user cwd.
    for (&pid, &issue) in census.unknown_pids() {
        if !within_budget() {
            return (states, holders, details, taints);
        }
        if issue == provenant_supervisor::census::ObservationIssue::PermissionDenied {
            let cwd = cwd_observer.cwd(pid);
            let occupancy = cwd_observer.occupancy_with_retries_while(pid, &within_budget);
            if !within_budget() {
                return (states, holders, details, taints);
            }
            // EPERM is a common limit of process listings. Place every
            // positive path first; only a complete descriptor observation can
            // prove that the other roots are clear.
            let descriptors_denied =
                occupancy.gap == Some(provenant_os::pid::OccupancyGap::DescriptorsDenied);
            let listed = listed_uid_excusal(cwd_observer, pid, roots, entries);
            let configured = configured_entry_matches(cwd_observer, pid, entries);
            let cwd_supported = matches!(
                &cwd,
                Ok(CwdObservation::Path(_)
                    | CwdObservation::Gone
                    | CwdObservation::PermissionDenied)
            );
            let mut under_candidate = false;
            let mut path_unreadable = false;
            // A root whose own identity cannot be read is undecidable for every
            // unreadable process, however its paths resolve: fail closed.
            let mut root_undecidable = any_root_identity_unreadable(roots);
            if let Ok(CwdObservation::Path(path)) = &cwd {
                let (inside, unreadable, undecidable) = record_unreadable_path(
                    &mut states,
                    &mut holders,
                    held,
                    &mut details,
                    &mut taints,
                    cwd_observer,
                    pid,
                    path.as_path(),
                    ProcessEvidenceKind::WorkingDirectory,
                    held_reportable(pid, &excluded, current, observer),
                    unplaceable,
                    listed || configured,
                );
                under_candidate |= inside;
                path_unreadable |= unreadable;
                root_undecidable |= undecidable;
            }
            for path in occupancy.paths {
                if !within_budget() {
                    return (states, holders, details, taints);
                }
                let (inside, unreadable, undecidable) = record_unreadable_path(
                    &mut states,
                    &mut holders,
                    held,
                    &mut details,
                    &mut taints,
                    cwd_observer,
                    pid,
                    &path,
                    ProcessEvidenceKind::OtherHeldPath,
                    held_reportable(pid, &excluded, current, observer),
                    unplaceable,
                    listed || configured,
                );
                under_candidate |= inside;
                path_unreadable |= unreadable;
                root_undecidable |= undecidable;
            }
            // A process path that cannot be placed is one excusable failure;
            // an undecidable root is a separate blocking signal, never a
            // failure that a listed uid or configured entry could trade away.
            let failure = path_unreadable
                .then_some(UnreadableFailure::ProcessPathUnplaceable)
                .or_else(|| descriptors_denied.then_some(UnreadableFailure::DescriptorsDenied))
                .or_else(|| {
                    matches!(&cwd, Ok(CwdObservation::PermissionDenied))
                        .then_some(UnreadableFailure::CwdDenied)
                });
            if let Some(failure) = failure
                && excuse_unreadable(
                    cwd_observer,
                    pid,
                    roots,
                    ExcusalEvidence {
                        failure,
                        listed,
                        placed_inside: under_candidate,
                        root_undecidable,
                        configured,
                    },
                    unplaceable,
                )
            {
                continue;
            }
            if cwd_supported && occupancy.complete {
                if !under_candidate {
                    unplaceable(Some(pid), Some(UNREADABLE_NO_CANDIDATE_PATH));
                }
                continue;
            }
        }
        unplaceable(Some(pid), Some("stable process identity unavailable"));
        let detail = process_detail(cwd_observer, pid, &format!("{issue:?}"));
        record_unknown_all_with_taint(
            &mut states,
            &mut details,
            &mut taints,
            &detail,
            TaintCause::Uncertainty(UncertaintyReason::UnstableProcessIdentity),
        );
    }
    // An incomplete census must not hide positively observed live workers.
    for fact in census.facts() {
        if !within_budget() {
            return (states, holders, details, taints);
        }
        // A zombie holds no cwd, executable or descriptor.
        if excluded.contains(&fact.identity) || fact.zombie {
            continue;
        }
        let pid = fact.identity.pid;
        let same_process = |process: ProcessObservation| matches!(process, ProcessObservation::Present(ref p) if p.identity == fact.identity);
        // A kernel thread holds no file in any worktree.
        if cwd_observer.kernel_thread(pid) && same_process(observer.observe(pid)) {
            continue;
        }
        let uids_before = cwd_observer.process_uids(pid);
        let mut cwd = observe_same_process(observer, cwd_observer, fact);
        let mut occupancy_gap = None;
        let mut occupancy_complete = false;
        let mut foreign_system = false;
        let mut descriptor_roots = BTreeSet::new();
        let mut descriptor_uncertain_roots = BTreeSet::new();
        if !matches!(cwd, NativeCwd::Gone) {
            let before = observer.observe(pid);
            let occupancy = cwd_observer.occupancy_with_retries_while(pid, &within_budget);
            occupancy_complete = occupancy.complete;
            if !within_budget() {
                return (states, holders, details, taints);
            }
            let uids_after = cwd_observer.process_uids(pid);
            let after = observer.observe(pid);
            // A process that exited during the occupancy query (absent, or a
            // zombie of the same identity) now holds no cwd or open file, even
            // if an earlier observation named a root.
            let exited = match &after {
                ProcessObservation::Absent => true,
                ProcessObservation::Present(process) => {
                    process.identity == fact.identity && process.zombie
                }
                ProcessObservation::Unknown(_) => false,
            };
            if exited {
                continue;
            }
            let pid_reused = matches!(
                &before,
                ProcessObservation::Present(process) if process.identity != fact.identity
            ) || matches!(
                &after,
                ProcessObservation::Present(process) if process.identity != fact.identity
            );
            let stable = same_process(before) && same_process(after);
            if uids_before != uids_after && !matches!(cwd, NativeCwd::Path(_)) {
                cwd = NativeCwd::Unknown("credentials changed during the census".into());
            }
            let foreign_uids = uids_before.filter(|uids| {
                stable
                    && Some(*uids) == uids_after
                    && excusal_applies(
                        &provenant_os::pid::UnreadablePolicyExcusal { uids: *uids },
                        roots,
                    )
                    && matches!(
                        cwd,
                        NativeCwd::Path(_)
                            | NativeCwd::PolicyExcluded(_)
                            | NativeCwd::PolicyRejected(_)
                    )
            });
            foreign_system = foreign_uids.is_some();
            // Still inspect and place every positive path. Only missing evidence
            // is scoped out; a root-owned process inside a worktree is live.
            if let Some(uids) = foreign_uids
                && matches!(cwd, NativeCwd::PolicyRejected(_))
            {
                cwd =
                    NativeCwd::PolicyExcluded(provenant_os::pid::UnreadablePolicyExcusal { uids });
            }
            if pid_reused {
                // Do not let observations from the listed process classify a
                // different process that reused its PID during the census.
                cwd = NativeCwd::Unknown("identity race".into());
            }
            if stable {
                for path in &occupancy.paths {
                    if !within_budget() {
                        return (states, holders, details, taints);
                    }
                    let Some(canonical) = canonical_cwd(path) else {
                        let detail = process_detail(
                            cwd_observer,
                            pid,
                            &format!("occupancy path unavailable: {}", path.display()),
                        );
                        for (root, state) in &mut states {
                            if occupancy_path_could_be_under(path, root) && !state.is_observed() {
                                descriptor_uncertain_roots.insert(root.clone());
                                *state = Occupancy::census_unknown(
                                    root.clone(),
                                    "process occupancy could not be placed",
                                );
                                details
                                    .entry(root.clone())
                                    .or_insert_with(|| detail.clone());
                                taints.entry(root.clone()).or_insert_with(|| {
                                    TaintCause::Uncertainty(UncertaintyReason::PathUnplaceable)
                                });
                            }
                        }
                        unplaceable(Some(pid), Some("occupancy path placement required"));
                        continue;
                    };
                    let location = match std::fs::metadata(&canonical) {
                        Ok(meta) if !meta.is_dir() => canonical.parent().unwrap_or(&canonical),
                        _ => &canonical,
                    };
                    let mut placement_failed = false;
                    let placed = record_live_path_with_audit(&mut states, location, &mut || {
                        placement_failed = true
                    });
                    if placement_failed {
                        unplaceable(
                            Some(pid),
                            Some(
                                "occupancy path placement required (directory identity placement failed)",
                            ),
                        );
                        for (root, state) in &mut states {
                            if occupancy_path_could_be_under(location, root) && !state.is_observed()
                            {
                                descriptor_uncertain_roots.insert(root.clone());
                            }
                            if state.is_unknown() {
                                details.entry(root.clone()).or_insert_with(|| {
                                    process_detail(
                                        cwd_observer,
                                        pid,
                                        "occupancy path or root identity unreadable",
                                    )
                                });
                                taints.entry(root.clone()).or_insert_with(|| {
                                    TaintCause::Uncertainty(UncertaintyReason::PathUnplaceable)
                                });
                            }
                        }
                    }
                    let is_exe = cwd_observer
                        .executable(pid)
                        .and_then(|path| canonical_cwd(&path))
                        .as_ref()
                        == Some(&canonical);
                    let kind = if is_exe {
                        ProcessEvidenceKind::ExecutableImage
                    } else {
                        ProcessEvidenceKind::OtherHeldPath
                    };
                    for root in placed {
                        descriptor_roots.insert(root.clone());
                        let holder = holders
                            .entry(root.clone())
                            .and_modify(|h| {
                                if h.pid == pid
                                    && h.kind == ProcessEvidenceKind::OtherHeldPath
                                    && kind == ProcessEvidenceKind::ExecutableImage
                                {
                                    h.kind = ProcessEvidenceKind::ExecutableImage;
                                    h.cwd = Some(location.to_path_buf());
                                }
                            })
                            .or_insert_with(|| ObservedProcess {
                                pid,
                                command: fact.command.clone(),
                                kind,
                                cwd: Some(location.to_path_buf()),
                            });
                        if fact.identity.session_id != current.identity.session_id {
                            // `holder` is the first process stored for this root,
                            // not necessarily this one. Report the current fact.
                            let mut observed =
                                ObservedProcess::new(pid, fact.command.clone(), kind);
                            observed.cwd = Some(location.to_path_buf());
                            record_held(held, &root, &observed);
                        }
                        taints.insert(root.clone(), TaintCause::LiveWriter(holder.clone()));
                        details.insert(
                            root,
                            format!("process {}: {} inside root", holder.pid, holder.kind),
                        );
                    }
                }
            }
            if !stable {
                occupancy_gap = Some(provenant_os::pid::OccupancyGap::Churn);
            } else if !occupancy.complete && !foreign_system {
                occupancy_gap = Some(
                    occupancy
                        .gap
                        .unwrap_or(provenant_os::pid::OccupancyGap::Unobserved),
                );
            }
        }
        // Placement evidence for every root, from the cwd as well as the
        // descriptors, is computed before any excusal. A cwd inside a root (or
        // a root whose identity cannot be decided) always wins, for every arm
        // including the configured one; it never reaches a `continue`.
        let cwd_placement = match &cwd {
            NativeCwd::Path(path) => PlacementEvidence::from_path(path, roots),
            _ => PlacementEvidence::default(),
        };
        let placed = cwd_placement.blocks_excusal()
            || !descriptor_roots.is_empty()
            || !descriptor_uncertain_roots.is_empty();
        // These processes were listed, so the listed-uid policy never applies;
        // `placed` already folds in undecidable roots, so no second blocking
        // signal is needed here.
        let descriptors_denied =
            occupancy_gap == Some(provenant_os::pid::OccupancyGap::DescriptorsDenied);
        let cwd_denied = matches!(
            &cwd,
            NativeCwd::PolicyExcluded(_) | NativeCwd::PolicyRejected(_)
        );
        // One configured-image decision serves both arms below.
        let configured = (descriptors_denied || cwd_denied)
            && configured_entry_matches(cwd_observer, pid, entries);
        if descriptors_denied
            && excuse_unreadable(
                cwd_observer,
                pid,
                roots,
                ExcusalEvidence {
                    failure: UnreadableFailure::DescriptorsDenied,
                    listed: false,
                    placed_inside: placed,
                    root_undecidable: any_root_identity_unreadable(roots),
                    configured,
                },
                unplaceable,
            )
        {
            continue;
        }
        if cwd_denied
            && excuse_unreadable(
                cwd_observer,
                pid,
                roots,
                ExcusalEvidence {
                    failure: UnreadableFailure::CwdDenied,
                    listed: false,
                    placed_inside: placed,
                    root_undecidable: any_root_identity_unreadable(roots),
                    configured,
                },
                unplaceable,
            )
        {
            continue;
        }
        match cwd {
            NativeCwd::Gone => {}
            NativeCwd::PolicyExcluded(_) | NativeCwd::PolicyRejected(_) => {
                if matches!(
                    &cwd,
                    NativeCwd::PolicyExcluded(excusal) if !excusal_applies(excusal, roots)
                ) {
                    unplaceable(
                        Some(pid),
                        Some(
                            "real/effective/saved system uids foreign to every root owner required",
                        ),
                    );
                }
                let path_evidence_complete = occupancy_complete || foreign_system;
                if occupancy_gap.is_none() && path_evidence_complete {
                    if descriptor_roots.is_empty() && descriptor_uncertain_roots.is_empty() {
                        unplaceable(Some(pid), Some(UNREADABLE_NO_CANDIDATE_PATH));
                    }
                } else {
                    let detail = match &cwd {
                        NativeCwd::PolicyRejected(failure) => {
                            unplaceable(Some(pid), failure.builtin_condition);
                            if failure.configurable {
                                format!(
                                    "cwd unreadable; policy_condition={}; configure confinement.census_ignore_unreadable with this canonical path and team_id",
                                    failure.condition
                                )
                            } else {
                                format!("cwd unreadable; policy_condition={}", failure.condition)
                            }
                        }
                        _ => "cwd unreadable".into(),
                    };
                    record_unknown_all_with_taint(
                        &mut states,
                        &mut details,
                        &mut taints,
                        &process_detail(cwd_observer, pid, &detail),
                        TaintCause::Uncertainty(UncertaintyReason::PolicyRejected),
                    );
                }
            }
            NativeCwd::Unknown(category) => {
                let condition = if category.starts_with("cwd ") {
                    "native cwd permission denial required"
                } else {
                    "stable process identity unavailable"
                };
                unplaceable(Some(pid), Some(condition));
                let reason = if category.starts_with("cwd ") {
                    UncertaintyReason::CwdUnreadable
                } else {
                    UncertaintyReason::UnstableProcessIdentity
                };
                record_unknown_all_with_taint(
                    &mut states,
                    &mut details,
                    &mut taints,
                    &process_detail(cwd_observer, pid, &category),
                    TaintCause::Uncertainty(reason),
                );
            }
            NativeCwd::Path(path) => {
                if let Some(canonical) = canonical_cwd(&path) {
                    let mut placement_failed = false;
                    let placed = record_live_path_with_audit(&mut states, &canonical, &mut || {
                        placement_failed = true
                    });
                    if placement_failed {
                        unplaceable(
                            Some(pid),
                            Some(
                                "native cwd permission denial required (directory identity placement failed)",
                            ),
                        );
                    }
                    for (root, state) in &states {
                        if state.is_unknown() {
                            details.entry(root.clone()).or_insert_with(|| {
                                process_detail(cwd_observer, pid, "cwd or root identity unreadable")
                            });
                            taints.entry(root.clone()).or_insert_with(|| {
                                TaintCause::Uncertainty(UncertaintyReason::PathUnplaceable)
                            });
                        }
                    }
                    for root in placed {
                        let holder = holders
                            .entry(root.clone())
                            .and_modify(|h| {
                                if h.pid == pid {
                                    h.kind = ProcessEvidenceKind::WorkingDirectory;
                                    h.cwd = Some(canonical.clone());
                                }
                            })
                            .or_insert_with(|| ObservedProcess {
                                pid,
                                command: fact.command.clone(),
                                kind: ProcessEvidenceKind::WorkingDirectory,
                                cwd: Some(canonical.clone()),
                            });
                        if fact.identity.session_id != current.identity.session_id {
                            // Keep the taint's first holder, but name this
                            // process in the held report.
                            let mut observed = ObservedProcess::cwd(pid, fact.command.clone());
                            observed.cwd = Some(canonical.clone());
                            record_held(held, &root, &observed);
                        }
                        taints.insert(root.clone(), TaintCause::LiveWriter(holder.clone()));
                        details.insert(root, format!("process {}: cwd inside root", holder.pid));
                    }
                } else {
                    unplaceable(
                        Some(pid),
                        Some(
                            "native cwd permission denial required (readable cwd placement failed)",
                        ),
                    );
                    let detail = process_detail(cwd_observer, pid, "cwd unreadable");
                    for (root, state) in &mut states {
                        if occupancy_path_could_be_under(&path, root) && !state.is_observed() {
                            *state = Occupancy::census_unknown(
                                root.clone(),
                                "process occupancy could not be placed",
                            );
                            details
                                .entry(root.clone())
                                .or_insert_with(|| detail.clone());
                            taints.entry(root.clone()).or_insert_with(|| {
                                TaintCause::Uncertainty(UncertaintyReason::CwdUnreadable)
                            });
                        }
                    }
                }
            }
        }
        if let Some(gap) = occupancy_gap {
            record_unknown_all_with_taint(
                &mut states,
                &mut details,
                &mut taints,
                &process_detail(
                    cwd_observer,
                    pid,
                    &format!("occupancy incomplete: {}", gap.describe()),
                ),
                TaintCause::Uncertainty(UncertaintyReason::OccupancyIncomplete),
            );
        }
    }
    if !census.listing_is_complete() {
        unplaceable(None, Some("complete process listing required"));
        record_unknown_all_with_taint(
            &mut states,
            &mut details,
            &mut taints,
            "process listing truncated or incomplete",
            TaintCause::Uncertainty(UncertaintyReason::ListingIncomplete),
        );
    }
    attach_worker_evidence(&mut states, &holders, &details);
    (states, holders, details, taints)
}

/// Canonicalise a process's working directory. A directory that has since been
/// deleted, such as a removed plugin cache or a pruned worktree, cannot be
/// resolved, but its place is still known: resolve the nearest existing
/// ancestor and re-append the rest. A process inside a deleted subdirectory of
/// a root therefore still counts as inside that root. Any other failure is
/// unobservable.
pub(super) fn canonical_cwd(path: &Path) -> Option<PathBuf> {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        match canonicalize(existing) {
            Ok(resolved) => return Some(rest.iter().rev().fold(resolved, |p, c| p.join(c))),
            Err(FsError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        rest.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
}

// Resolve as much of the path as possible before ruling out a root. An
// unresolved ancestor, relative path, or parent traversal cannot disprove it.
pub(super) fn occupancy_path_could_be_under(path: &Path, root: &Path) -> bool {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return true;
    }
    if let Some(inside) = absent_root_contains(path, root) {
        return inside;
    }
    if root_is_absent(root) {
        return absent_root_could_hold(path, root);
    }
    if !std::fs::metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
        return true;
    }
    let Some(root) = canonical_cwd(root) else {
        return true;
    };
    for ancestor in path.ancestors() {
        if let Ok(resolved) = canonicalize(ancestor) {
            let suffix = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            let candidate = resolved.join(suffix);
            // If resolution stopped above a root, unresolved components may
            // include an alias into it.
            return candidate.starts_with(&root) || root.starts_with(&resolved);
        }
    }
    true
}

pub(super) enum NativeCwd {
    Path(PathBuf),
    Gone,
    PolicyExcluded(provenant_os::pid::UnreadablePolicyExcusal),
    PolicyRejected(provenant_os::pid::UnreadablePolicyFailure),
    Unknown(String),
}

/// Place positive cwd/descriptor evidence from an EPERM process. Unreadable
/// process metadata is scoped to roots that its known paths could occupy.
/// Returns `(inside, path_unplaceable, root_undecidable)`: only the middle signal
/// is excusable, while an undecidable root keeps its Unknown state and blocks
/// every arm.
#[allow(clippy::too_many_arguments)] // The census owns these five correlated evidence maps.
fn record_unreadable_path(
    states: &mut BTreeMap<PathBuf, Occupancy>,
    holders: &mut BTreeMap<PathBuf, ObservedProcess>,
    held: &mut HeldProcesses,
    details: &mut BTreeMap<PathBuf, String>,
    taints: &mut BTreeMap<PathBuf, TaintCause>,
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    path: &Path,
    kind: ProcessEvidenceKind,
    reportable: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
    scope_unreadable: bool,
) -> (bool, bool, bool) {
    let Some(canonical) = canonical_cwd(path) else {
        unplaceable(Some(pid), Some("occupancy path placement required"));
        if !scope_unreadable {
            let detail = process_detail(
                cwd_observer,
                pid,
                &format!("occupancy path unavailable: {}", path.display()),
            );
            for (root, state) in states {
                if occupancy_path_could_be_under(path, root) && !state.is_observed() {
                    *state = Occupancy::census_unknown(
                        root.clone(),
                        "process occupancy could not be placed",
                    );
                    details
                        .entry(root.clone())
                        .or_insert_with(|| detail.clone());
                    taints.entry(root.clone()).or_insert_with(|| {
                        TaintCause::Uncertainty(UncertaintyReason::PathUnplaceable)
                    });
                }
            }
        }
        return (false, true, false);
    };
    let location = match std::fs::metadata(&canonical) {
        Ok(metadata) if !metadata.is_dir() => canonical.parent().unwrap_or(&canonical),
        _ => &canonical,
    };
    let mut placement_failed = false;
    let mut undecidable_roots = Vec::new();
    let placed = record_live_path_with_audit_scoped(
        states,
        location,
        &mut || placement_failed = true,
        !scope_unreadable,
        &mut undecidable_roots,
    );
    let inside = !placed.is_empty();
    if placement_failed {
        unplaceable(Some(pid), Some("occupancy path placement required"));
        if !scope_unreadable {
            let detail = process_detail(
                cwd_observer,
                pid,
                "occupancy path or root identity unreadable",
            );
            for (root, state) in states {
                if occupancy_path_could_be_under(location, root) && !state.is_observed() {
                    *state = Occupancy::census_unknown(
                        root.clone(),
                        "process occupancy could not be placed",
                    );
                    details
                        .entry(root.clone())
                        .or_insert_with(|| detail.clone());
                    taints.entry(root.clone()).or_insert_with(|| {
                        TaintCause::Uncertainty(UncertaintyReason::PathUnplaceable)
                    });
                }
            }
        }
    }
    for root in placed {
        let mut holder = ObservedProcess::new(pid, cwd_observer.process_details(pid), kind);
        if kind == ProcessEvidenceKind::WorkingDirectory {
            holder.cwd = Some(location.to_path_buf());
        }
        details.insert(
            root.clone(),
            format!("process {pid}: {} inside root", holder.kind),
        );
        taints.insert(root.clone(), TaintCause::LiveWriter(holder.clone()));
        if reportable {
            record_held(held, &root, &holder);
        }
        holders.entry(root).or_insert(holder);
    }
    (inside, placement_failed, !undecidable_roots.is_empty())
}

pub(super) fn process_detail(observer: &dyn CwdObserver, pid: i64, category: &str) -> String {
    let mut detail = format!("process {pid}: {category}");
    if let Some(fields) = observer.process_details(pid) {
        detail.push_str("; ");
        detail.push_str(&fields);
    }
    detail
}

pub(super) fn record_unknown_all_with_taint(
    states: &mut BTreeMap<PathBuf, Occupancy>,
    details: &mut BTreeMap<PathBuf, String>,
    taints: &mut BTreeMap<PathBuf, TaintCause>,
    detail: &str,
    taint: TaintCause,
) {
    for (root, state) in states {
        if !state.is_observed() {
            *state =
                Occupancy::census_unknown(root.clone(), "process occupancy could not be placed");
            details
                .entry(root.clone())
                .or_insert_with(|| detail.to_owned());
            taints.entry(root.clone()).or_insert_with(|| taint.clone());
        }
    }
}

pub(super) fn observe_same_process(
    process_observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    expected: &ProcessFact,
) -> NativeCwd {
    match process_observer.observe(expected.identity.pid) {
        ProcessObservation::Present(before) if before.identity == expected.identity => {}
        ProcessObservation::Absent => return NativeCwd::Gone,
        ProcessObservation::Present(_) => return NativeCwd::Unknown("identity race".into()),
        ProcessObservation::Unknown(issue) => return NativeCwd::Unknown(format!("{issue:?}")),
    }
    let observed = match cwd_observer.cwd(expected.identity.pid) {
        Ok(CwdObservation::Path(path)) => NativeCwd::Path(path),
        // A missing cwd is not proof that the process vanished. The stable
        // identity recheck below distinguishes a real exit from a query gap.
        Ok(CwdObservation::Gone) => NativeCwd::Unknown("cwd unavailable".into()),
        Ok(CwdObservation::PermissionDenied) => cwd_observer
            .unreadable_policy_uids(expected.identity.pid, &[])
            .map(NativeCwd::PolicyExcluded)
            .unwrap_or_else(NativeCwd::PolicyRejected),
        Ok(CwdObservation::Unsupported) => NativeCwd::Unknown("cwd unsupported".into()),
        Err(_) => NativeCwd::Unknown("cwd query failed".into()),
    };
    match process_observer.observe(expected.identity.pid) {
        ProcessObservation::Present(after) if after.identity == expected.identity => observed,
        ProcessObservation::Absent => NativeCwd::Gone,
        ProcessObservation::Present(_) => NativeCwd::Unknown("identity race".into()),
        ProcessObservation::Unknown(issue) => NativeCwd::Unknown(format!("{issue:?}")),
    }
}

/// Place by directory identity, including the nearest extant ancestor of a
/// deleted cwd. Darwin's Data firmlink can give one directory two canonical
/// path spellings; textual prefix comparisons cannot establish occupancy.
#[cfg(test)]
pub(super) fn record_live_path(
    states: &mut BTreeMap<PathBuf, Occupancy>,
    cwd: &Path,
) -> Vec<PathBuf> {
    record_live_path_with_audit(states, cwd, &mut || {})
}

pub(super) fn record_live_path_with_audit(
    states: &mut BTreeMap<PathBuf, Occupancy>,
    cwd: &Path,
    unplaceable: &mut dyn FnMut(),
) -> Vec<PathBuf> {
    record_live_path_with_audit_scoped(states, cwd, unplaceable, true, &mut Vec::new())
}

/// A scoped unreadable process may have its missing *process path* evidence
/// discarded by host policy, but a positively placed path is always live
/// occupancy. A root whose own identity cannot be read is different: placement
/// for it is undecidable, so it always stays Unknown even when the process's
/// own path may be excused. `undecidable_roots` reports those roots to the
/// caller, which uses them to block every excusal arm.
fn record_live_path_with_audit_scoped(
    states: &mut BTreeMap<PathBuf, Occupancy>,
    cwd: &Path,
    unplaceable: &mut dyn FnMut(),
    mark_unplaceable: bool,
    undecidable_roots: &mut Vec<PathBuf>,
) -> Vec<PathBuf> {
    if !cwd.is_absolute() {
        unplaceable();
        if mark_unplaceable {
            for (root, state) in states.iter_mut() {
                if !state.is_observed() {
                    *state = Occupancy::census_unknown(
                        root.clone(),
                        "process occupancy could not be placed",
                    );
                }
            }
        }
        return Vec::new();
    }
    let ancestors = cwd.ancestors().try_fold(BTreeSet::new(), |mut ids, path| {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                ids.insert((metadata.dev(), metadata.ino()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return None,
        }
        Some(ids)
    });
    let mut placed = Vec::new();
    for (root, state) in states {
        let root_metadata = std::fs::metadata(root).ok().filter(|meta| meta.is_dir());
        let inside = match (&ancestors, &root_metadata) {
            (Some(ids), Some(metadata)) => Some(ids.contains(&(metadata.dev(), metadata.ino()))),
            (_, None) => absent_root_contains(cwd, root),
            (None, Some(_)) => None,
        };
        match inside {
            Some(true) => {
                *state = Occupancy::Observed { process: None };
                placed.push(root.clone());
            }
            Some(false) => {}
            None => {
                unplaceable();
                let undecidable = root_metadata.is_none();
                if undecidable {
                    undecidable_roots.push(root.clone());
                }
                if (mark_unplaceable || undecidable)
                    && occupancy_path_could_be_under(cwd, root)
                    && !state.is_observed()
                {
                    *state = Occupancy::census_unknown(
                        root.clone(),
                        "process occupancy could not be placed",
                    );
                }
            }
        }
    }
    placed
}
