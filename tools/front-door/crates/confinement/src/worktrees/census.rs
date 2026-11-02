//! Process census and worker observation.
use super::*;

/// Injectable native observation seam. Implementations return one conservative
/// state per canonical candidate root; they never grant signal authority.
pub trait WorkerObserver: Send + Sync {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState>;

    /// Landing requires positive observation of a free root. Ordinary removal
    /// still relies on writer custody when the host census is unobservable.
    fn requires_complete_census(&self) -> bool {
        false
    }

    /// Landing may remove its caller's checkout. Exclude only the caller and
    /// its verified ancestors through the session leader, never other workers.
    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
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

    /// Composition supplies host configuration. Injected observers retain
    /// their own refusal semantics; the native observer uses these entries.
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

/// Immutable host policy around either native observations or an injected seam.
pub struct CensusPolicyWorkers<'a> {
    inner: &'a dyn WorkerObserver,
    pub(super) entries: Vec<CensusIgnoreEntry>,
}
impl<'a> CensusPolicyWorkers<'a> {
    pub fn from_roots(
        inner: &'a dyn WorkerObserver,
        roots: &provenant_core::Roots,
    ) -> Result<Self, WorktreeError> {
        let config = provenant_config::read_knobs(roots)?;
        Ok(Self {
            inner,
            entries: config.census_ignore_unreadable()?,
        })
    }
}
impl WorkerObserver for CensusPolicyWorkers<'_> {
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, entries, for_land)
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
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        self.observe_roots_with_details(roots).0
    }
    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        self.observe_roots_for_land_with_details(roots).0
    }
    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, &self.entries, false)
    }
    fn observe_roots_for_land_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        self.inner
            .observe_roots_with_policy(roots, &self.entries, true)
    }
    fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
        self.inner
            .live_process_with_policy(root, &self.entries, false)
    }
    fn live_process_for_land(&self, root: &Path) -> Option<ObservedProcess> {
        self.inner
            .live_process_with_policy(root, &self.entries, true)
    }
    fn incomplete_census_detail(&self) -> String {
        self.inner.incomplete_census_detail()
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
    fn observe_roots_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerObservations {
        system_worker_roots_with_policy(roots, for_land, entries)
    }
    fn live_process_with_policy(
        &self,
        root: &Path,
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> Option<ObservedProcess> {
        system_live_process_with_policy(root, for_land, entries)
    }
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        system_worker_roots(roots, false).0
    }

    fn observe_roots_for_land(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
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
        &PolicyCwdObserver(entries),
        &current,
        ancestors,
    )
    .1
    .remove(root)
}

pub(super) fn system_worker_roots(roots: &[PathBuf], ancestors: bool) -> WorkerObservations {
    system_worker_roots_with_policy(roots, ancestors, &[])
}

pub(super) struct PolicyCwdObserver<'a>(pub(super) &'a [CensusIgnoreEntry]);
impl CwdObserver for PolicyCwdObserver<'_> {
    fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
        SystemCwdObserver.occupancy(pid)
    }
    fn sealed_system_image(&self, pid: i64) -> Option<provenant_os::pid::SealedSystemImage> {
        SystemCwdObserver.sealed_system_image(pid)
    }
    fn kernel_thread(&self, pid: i64) -> bool {
        SystemCwdObserver.kernel_thread(pid)
    }
    fn unreadable_policy_uids(
        &self,
        pid: i64,
        _: &[CensusIgnoreEntry],
    ) -> Result<
        provenant_os::pid::UnreadablePolicyExcusal,
        provenant_os::pid::UnreadablePolicyFailure,
    > {
        SystemCwdObserver.unreadable_policy_uids(pid, self.0)
    }
    fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
        SystemCwdObserver.cwd(pid)
    }
    fn ignored_unreadable_uids(&self, pid: i64, _: &[CensusIgnoreEntry]) -> Option<ProcessUids> {
        SystemCwdObserver.ignored_unreadable_uids(pid, self.0)
    }
    fn process_details(&self, pid: i64) -> Option<String> {
        SystemCwdObserver.process_details(pid)
    }
}

pub(super) fn system_worker_roots_with_policy(
    roots: &[PathBuf],
    ancestors: bool,
    entries: &[CensusIgnoreEntry],
) -> WorkerObservations {
    let ProcessObservation::Present(current) = current_process() else {
        return (
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
                .collect(),
            roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root,
                        process_detail(
                            &PolicyCwdObserver(entries),
                            i64::from(std::process::id()),
                            "current process unavailable",
                        ),
                    )
                })
                .collect(),
        );
    };
    observe_worker_roots(
        roots,
        &SystemProcessObserver,
        &PolicyCwdObserver(entries),
        &current,
        ancestors,
    )
}

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

/// The per-root state, and the first process seen inside each root.
pub(super) fn observe_worker_holders(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
) -> (
    BTreeMap<PathBuf, WorkerState>,
    BTreeMap<PathBuf, ObservedProcess>,
    BTreeMap<PathBuf, String>,
) {
    observe_worker_holders_with_audit(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        &mut |_, _| {},
    )
}

/// Audit the same decisions that produce custody, without a second census or
/// diagnostic re-query deciding why an image failed the policy.
pub(super) fn observe_worker_holders_with_audit(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
) -> (
    BTreeMap<PathBuf, WorkerState>,
    BTreeMap<PathBuf, ObservedProcess>,
    BTreeMap<PathBuf, String>,
) {
    let mut states: BTreeMap<_, _> = roots
        .iter()
        .cloned()
        .map(|root| (root, WorkerState::Free))
        .collect();
    let mut holders = BTreeMap::new();
    let mut details = BTreeMap::new();
    if roots.is_empty() {
        return (states, holders, details);
    }
    let census = match observer.census(65_536) {
        Ok(census) => census,
        Err(error) => {
            unplaceable(None, Some("complete process census unavailable"));
            let unknown = roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
                .collect();
            let detail = if matches!(
                error,
                provenant_supervisor::census::CensusError::ProcessList(_)
            ) {
                "process listing failed"
            } else {
                "process census unavailable"
            };
            return (
                unknown,
                holders,
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, detail.into()))
                    .collect(),
            );
        }
    };
    // Custody is decided per root. A gap that could hide a process anywhere
    // (an incomplete or truncated listing, or a process that could not be
    // described or placed) leaves every root that is not positively live
    // unknown. Current credentials and permissions cannot disprove a retained
    // cwd, including one acquired before a permission or credential change.
    for (&pid, &issue) in census.unknown_pids() {
        unplaceable(Some(pid), Some("stable process identity unavailable"));
        let detail = process_detail(cwd_observer, pid, &format!("{issue:?}"));
        record_unknown_all(&mut states, &mut details, &detail);
    }
    // Exclude the caller's verified ancestors. A walk that stops early leaves
    // the remaining ancestors in the census, where one inside a root still
    // makes it live, so an incomplete ancestry hides nothing.
    let mut excluded = BTreeSet::from([current.identity.clone()]);
    if ancestors {
        let mut child = current;
        while child.identity.pid != current.identity.session_id {
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
    // An incomplete census must not hide positively observed live workers.
    for fact in census.facts() {
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
        let cwd = observe_same_process(observer, cwd_observer, fact);
        let mut occupancy_gap = None;
        if !matches!(cwd, NativeCwd::Gone) {
            let policy_exception = matches!(&cwd, NativeCwd::PolicyExcluded(excusal)
                if excusal_applies(excusal, roots));
            let before = observer.observe(pid);
            let occupancy = cwd_observer.occupancy(pid);
            let after = observer.observe(pid);
            let stable = same_process(before) && same_process(after);
            if stable {
                for path in &occupancy.paths {
                    let Some(canonical) = canonical_cwd(path) else {
                        unplaceable(Some(pid), Some("occupancy path placement required"));
                        record_unknown_all(
                            &mut states,
                            &mut details,
                            &process_detail(cwd_observer, pid, "occupancy path unavailable"),
                        );
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
                        for (root, state) in &states {
                            if *state == WorkerState::Unknown {
                                details.entry(root.clone()).or_insert_with(|| {
                                    process_detail(
                                        cwd_observer,
                                        pid,
                                        "occupancy path or root identity unreadable",
                                    )
                                });
                            }
                        }
                    }
                    for root in placed {
                        holders
                            .entry(root.clone())
                            .or_insert_with(|| ObservedProcess {
                                pid,
                                command: fact.command.clone(),
                            });
                        details.insert(
                            root,
                            format!("process {pid}: executable or open file inside root"),
                        );
                    }
                }
            }
            if !stable {
                occupancy_gap = Some(provenant_os::pid::OccupancyGap::Churn);
            } else if !occupancy.complete && !policy_exception {
                occupancy_gap = Some(
                    occupancy
                        .gap
                        .unwrap_or(provenant_os::pid::OccupancyGap::Unobserved),
                );
            }
        }
        // Set by the cwd arms below: what the narrowing rules may rely on.
        let mut cwd_outside = false;
        let mut setuid_excused = false;
        match cwd {
            NativeCwd::Gone => {}
            NativeCwd::PolicyExcluded(excusal) => {
                // Accepted risk for sealed Apple code, applied per root owner.
                // All credentials must be system accounts and foreign to every
                // candidate root, including the real uid of a setuid tool.
                if !excusal_applies(&excusal, roots) {
                    unplaceable(
                        Some(pid),
                        Some(
                            "real/effective/saved system uids foreign to every root owner required",
                        ),
                    );
                    record_unknown_all(
                        &mut states,
                        &mut details,
                        &process_detail(cwd_observer, pid, "cwd unreadable"),
                    );
                }
            }
            NativeCwd::PolicyRejected(failure) => {
                // A setuid image on the sealed system volume has its cwd
                // refused by its own privilege transition (login, for one).
                // Its user-owned descendants are separate census entries.
                setuid_excused = cwd_observer
                    .sealed_system_image(pid)
                    .is_some_and(|image| image.setuid_transition);
                if !setuid_excused {
                    unplaceable(Some(pid), failure.builtin_condition);
                    let condition = failure.condition;
                    record_unknown_all(
                        &mut states,
                        &mut details,
                        &process_detail(
                            cwd_observer,
                            pid,
                            &if failure.configurable {
                                format!(
                                    "cwd unreadable; policy_condition={condition}; configure confinement.census_ignore_unreadable with this canonical path and team_id"
                                )
                            } else {
                                format!("cwd unreadable; policy_condition={condition}")
                            },
                        ),
                    );
                }
            }
            NativeCwd::Unknown(category) => {
                let condition = if category.starts_with("cwd ") {
                    "actual cwd EPERM required"
                } else {
                    "stable process identity unavailable"
                };
                unplaceable(Some(pid), Some(condition));
                record_unknown_all(
                    &mut states,
                    &mut details,
                    &process_detail(cwd_observer, pid, &category),
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
                            Some("actual cwd EPERM required (directory identity placement failed)"),
                        );
                    }
                    cwd_outside = placed.is_empty() && !placement_failed;
                    for (root, state) in &states {
                        if *state == WorkerState::Unknown {
                            details.entry(root.clone()).or_insert_with(|| {
                                process_detail(cwd_observer, pid, "cwd or root identity unreadable")
                            });
                        }
                    }
                    for root in placed {
                        let holder =
                            holders
                                .entry(root.clone())
                                .or_insert_with(|| ObservedProcess {
                                    pid,
                                    command: fact.command.clone(),
                                });
                        details.insert(root, format!("process {}: cwd inside root", holder.pid));
                    }
                } else {
                    unplaceable(
                        Some(pid),
                        Some("actual cwd EPERM required (readable cwd placement failed)"),
                    );
                    record_unknown_all(
                        &mut states,
                        &mut details,
                        &process_detail(cwd_observer, pid, "cwd unreadable"),
                    );
                }
            }
        }
        if let Some(gap) = occupancy_gap {
            // Only refused descriptor queries are excusable, and only for a
            // sealed system image that is known not to be in a candidate root:
            // by its known cwd, or because it is a setuid transition.
            let excused = gap == provenant_os::pid::OccupancyGap::DescriptorsDenied
                && (setuid_excused
                    || (cwd_outside && cwd_observer.sealed_system_image(pid).is_some()));
            if !excused {
                record_unknown_all(
                    &mut states,
                    &mut details,
                    &process_detail(
                        cwd_observer,
                        pid,
                        &format!("occupancy incomplete: {}", gap.describe()),
                    ),
                );
            }
        }
    }
    if !census.listing_is_complete() {
        unplaceable(None, Some("complete process listing required"));
        record_unknown_all(
            &mut states,
            &mut details,
            "process listing truncated or incomplete",
        );
    }
    (states, holders, details)
}

/// Canonicalise a process's working directory. A directory that has since been
/// deleted, such as a removed plugin cache or a pruned worktree, cannot be
/// resolved, but its place is still known: resolve the nearest existing
/// ancestor and re-append the rest. A process inside a deleted subdirectory of
/// a root therefore still counts as inside that root. Any other failure is
/// unobservable.
/// An excusal holds when every credential is a system account foreign to each
/// candidate root's owner.
fn excusal_applies(
    excusal: &provenant_os::pid::UnreadablePolicyExcusal,
    roots: &[PathBuf],
) -> bool {
    excusal.uids.all_system()
        && roots.iter().all(|root| {
            std::fs::metadata(root)
                .is_ok_and(|metadata| excusal.uids.all_differ_from(metadata.uid()))
        })
}

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

pub(super) enum NativeCwd {
    Path(PathBuf),
    Gone,
    PolicyExcluded(provenant_os::pid::UnreadablePolicyExcusal),
    PolicyRejected(provenant_os::pid::UnreadablePolicyFailure),
    Unknown(String),
}

pub(super) fn process_detail(observer: &dyn CwdObserver, pid: i64, category: &str) -> String {
    let mut detail = format!("process {pid}: {category}");
    if let Some(fields) = observer.process_details(pid) {
        detail.push_str("; ");
        detail.push_str(&fields);
    }
    detail
}

pub(super) fn record_unknown_all(
    states: &mut BTreeMap<PathBuf, WorkerState>,
    details: &mut BTreeMap<PathBuf, String>,
    detail: &str,
) {
    for (root, state) in states {
        if *state != WorkerState::Live {
            *state = WorkerState::Unknown;
            details
                .entry(root.clone())
                .or_insert_with(|| detail.to_owned());
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
    states: &mut BTreeMap<PathBuf, WorkerState>,
    cwd: &Path,
) -> Vec<PathBuf> {
    record_live_path_with_audit(states, cwd, &mut || {})
}

pub(super) fn record_live_path_with_audit(
    states: &mut BTreeMap<PathBuf, WorkerState>,
    cwd: &Path,
    unplaceable: &mut dyn FnMut(),
) -> Vec<PathBuf> {
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
        let inside = ancestors.as_ref().and_then(|ids| {
            let metadata = std::fs::metadata(root).ok()?;
            metadata
                .is_dir()
                .then(|| ids.contains(&(metadata.dev(), metadata.ino())))
        });
        match inside {
            Some(true) => {
                *state = WorkerState::Live;
                placed.push(root.clone());
            }
            Some(false) => {}
            None => {
                unplaceable();
                if *state != WorkerState::Live {
                    *state = WorkerState::Unknown;
                }
            }
        }
    }
    placed
}
