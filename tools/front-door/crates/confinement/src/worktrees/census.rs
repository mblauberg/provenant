//! Process census and worker observation.
use super::*;

/// A single acquisition supplies both the decision and its refusal evidence.
#[derive(Clone, Default)]
pub struct WorkerEvidence {
    pub states: BTreeMap<PathBuf, Occupancy>,
    pub holders: BTreeMap<PathBuf, ObservedProcess>,
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

type RootIdentity = (u64, u64, u32);

fn root_identity(root: &Path) -> Option<RootIdentity> {
    let metadata = std::fs::metadata(root).ok()?;
    metadata
        .is_dir()
        .then(|| (metadata.dev(), metadata.ino(), metadata.uid()))
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

/// Injectable native observation seam. Implementations return one conservative
/// state per canonical candidate root; they never grant signal authority.
pub trait WorkerObserver: Send + Sync {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy>;

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
            states,
            holders: BTreeMap::new(),
            details,
        }
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

    fn observe_evidence_with_policy(
        &self,
        roots: &[PathBuf],
        entries: &[CensusIgnoreEntry],
        for_land: bool,
    ) -> WorkerEvidence {
        let (states, details) = self.observe_roots_with_policy(roots, entries, for_land);
        WorkerEvidence {
            taints: default_taints(&states, &|_| None),
            states,
            holders: BTreeMap::new(),
            details,
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
            roots: roots.clone(),
            for_run,
        })
    }
}
impl WorkerObserver for CensusPolicyWorkers<'_> {
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
            entries: config.census_ignore_unreadable().ok()?,
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

pub(super) fn system_worker_roots_with_taints(
    roots: &[PathBuf],
    ancestors: bool,
) -> WorkerTaintObservations {
    system_worker_roots_with_policy_taints(roots, ancestors, &[])
}

pub(super) struct PolicyCwdObserver<'a>(pub(super) &'a [CensusIgnoreEntry]);
impl CwdObserver for PolicyCwdObserver<'_> {
    fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
        SystemCwdObserver.occupancy(pid)
    }
    fn occupancy_while(
        &self,
        pid: i64,
        keep_going: &dyn Fn() -> bool,
    ) -> provenant_os::pid::OccupancyObservation {
        SystemCwdObserver.occupancy_while(pid, keep_going)
    }
    fn executable(&self, pid: i64) -> Option<PathBuf> {
        SystemCwdObserver.executable(pid)
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
    fn process_details(&self, pid: i64) -> Option<String> {
        SystemCwdObserver.process_details(pid)
    }
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
            &PolicyCwdObserver(entries),
            i64::from(std::process::id()),
            "current process unavailable",
        );
        return WorkerEvidence {
            states: roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root.clone(),
                        Occupancy::census_unknown(root, detail.clone()),
                    )
                })
                .collect(),
            holders: BTreeMap::new(),
            taints: roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root,
                        TaintCause::Uncertainty(UncertaintyReason::CensusUnavailable),
                    )
                })
                .collect(),
            details: roots
                .iter()
                .cloned()
                .map(|root| (root, detail.clone()))
                .collect(),
        };
    };
    let (states, holders, details, taints) = observe_worker_holders_while(
        roots,
        &SystemProcessObserver,
        &PolicyCwdObserver(entries),
        &current,
        ancestors,
        &mut |_, _| {},
        keep_going,
    );
    WorkerEvidence {
        states,
        holders,
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
        &PolicyCwdObserver(entries),
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
) -> WorkerHolderAudit {
    observe_worker_holders_while(
        roots,
        observer,
        cwd_observer,
        current,
        ancestors,
        unplaceable,
        None,
    )
}

fn observe_worker_holders_while(
    roots: &[PathBuf],
    observer: &dyn ProcessObserver,
    cwd_observer: &dyn CwdObserver,
    current: &ProcessFact,
    ancestors: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
    keep_going: Option<&dyn Fn() -> bool>,
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
    // Custody is decided per root. A gap that could hide a process anywhere
    // (an incomplete or truncated listing, or a process that could not be
    // described or placed) leaves every root that is not positively live
    // unknown. Current credentials and permissions cannot disprove a retained
    // cwd, including one acquired before a permission or credential change.
    for (&pid, &issue) in census.unknown_pids() {
        if !within_budget() {
            return (states, holders, details, taints);
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
    // Exclude the caller's verified ancestors. A walk that stops early leaves
    // the remaining ancestors in the census, where one inside a root still
    // makes it live, so an incomplete ancestry hides nothing.
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
    let mut excused_gap_detail = None;
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
        let mut cwd = observe_same_process(observer, cwd_observer, fact);
        let mut occupancy_gap = None;
        if !matches!(cwd, NativeCwd::Gone) {
            let policy_exception = matches!(&cwd, NativeCwd::PolicyExcluded(excusal)
                if excusal_applies(excusal, roots));
            let before = observer.observe(pid);
            let occupancy = keep_going.map_or_else(
                || cwd_observer.occupancy(pid),
                |check| cwd_observer.occupancy_while(pid, check),
            );
            if !within_budget() {
                return (states, holders, details, taints);
            }
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
                        for (root, state) in &states {
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
                    record_unknown_all_with_taint(
                        &mut states,
                        &mut details,
                        &mut taints,
                        &process_detail(cwd_observer, pid, "cwd unreadable"),
                        TaintCause::Uncertainty(UncertaintyReason::PolicyRejected),
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
                    record_unknown_all_with_taint(
                        &mut states,
                        &mut details,
                        &mut taints,
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
                        TaintCause::Uncertainty(UncertaintyReason::PolicyRejected),
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
                            Some("actual cwd EPERM required (directory identity placement failed)"),
                        );
                    }
                    cwd_outside = placed.is_empty() && !placement_failed;
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
                        taints.insert(root.clone(), TaintCause::LiveWriter(holder.clone()));
                        details.insert(root, format!("process {}: cwd inside root", holder.pid));
                    }
                } else {
                    unplaceable(
                        Some(pid),
                        Some("actual cwd EPERM required (readable cwd placement failed)"),
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
            use provenant_os::pid::OccupancyGap;
            let descriptor_gap = matches!(
                gap,
                OccupancyGap::DescriptorsDenied
                    | OccupancyGap::DescriptorsUnavailable
                    | OccupancyGap::Churn
                    | OccupancyGap::LimitExceeded
            );
            let excused = (cwd_outside && descriptor_gap)
                || (setuid_excused && gap == OccupancyGap::DescriptorsDenied);
            if excused {
                excused_gap_detail.get_or_insert_with(|| {
                    process_detail(
                        cwd_observer,
                        pid,
                        &format!("occupancy incomplete (excused): {}", gap.describe()),
                    )
                });
            }
            if !excused {
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
    if let Some(detail) = excused_gap_detail {
        for root in states.keys() {
            details
                .entry(root.clone())
                .or_insert_with(|| detail.clone());
        }
    }
    (states, holders, details, taints)
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

// Resolve as much of the path as possible before ruling out a root. An
// unresolved ancestor, relative path, or parent traversal cannot disprove it.
fn occupancy_path_could_be_under(path: &Path, root: &Path) -> bool {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return true;
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
    if !cwd.is_absolute() {
        unplaceable();
        for (root, state) in states.iter_mut() {
            if !state.is_observed() {
                *state = Occupancy::census_unknown(
                    root.clone(),
                    "process occupancy could not be placed",
                );
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
        let inside = ancestors.as_ref().and_then(|ids| {
            let metadata = std::fs::metadata(root).ok()?;
            metadata
                .is_dir()
                .then(|| ids.contains(&(metadata.dev(), metadata.ino())))
        });
        match inside {
            Some(true) => {
                *state = Occupancy::Observed { process: None };
                placed.push(root.clone());
            }
            Some(false) => {}
            None => {
                unplaceable();
                if occupancy_path_could_be_under(cwd, root) && !state.is_observed() {
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
