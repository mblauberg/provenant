//! Native process facts and conservative ancestry/sparing policy.
//!
//! Observations never grant signalling authority. Process names are bounded
//! diagnostics only; native identity is the boot, PID, start stamp, group and
//! session tuple.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const MAX_CENSUS_ENTRIES: usize = 65_536;
const MAX_BOOT_ID_BYTES: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub boot: String,
    pub pid: i64,
    /// Linux clock ticks; Darwin start seconds and microseconds combined.
    pub native_start: u64,
    pub group_id: i64,
    pub session_id: i64,
}

// Group/session membership can change during the same process lifetime.
// Equality denotes the native process, never its mutable group membership.
impl PartialEq for ProcessIdentity {
    fn eq(&self, other: &Self) -> bool {
        (self.boot.as_str(), self.pid, self.native_start)
            == (other.boot.as_str(), other.pid, other.native_start)
    }
}
impl Eq for ProcessIdentity {}
impl PartialOrd for ProcessIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ProcessIdentity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.boot.as_str(), self.pid, self.native_start).cmp(&(
            other.boot.as_str(),
            other.pid,
            other.native_start,
        ))
    }
}
impl ProcessIdentity {
    pub fn validate(&self) -> Result<(), CensusError> {
        if self.boot.is_empty()
            || self.boot.len() > MAX_BOOT_ID_BYTES
            || self.pid <= 0
            || self.group_id <= 0
            || self.session_id <= 0
        {
            return Err(CensusError::InvalidRecord);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessFact {
    pub identity: ProcessIdentity,
    pub parent_pid: i64,
    pub zombie: bool,
    /// Unknown task accounting remains None, never zero CPU progress.
    pub cpu_micros: Option<u64>,
    /// A short native comm/name for diagnostics only.
    pub command: Option<String>,
}

impl ProcessFact {
    pub fn validate(&self) -> Result<(), CensusError> {
        self.identity.validate()?;
        if self.parent_pid < 0
            || self
                .command
                .as_ref()
                .is_some_and(|command| command.len() > 512)
        {
            return Err(CensusError::InvalidRecord);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ObservationIssue {
    PermissionDenied,
    Malformed,
    Unsupported,
    Io,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CensusCompleteness {
    Complete,
    Partial,
    Truncated,
    PartialAndTruncated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Census {
    processes: BTreeMap<i64, ProcessFact>,
    unknown_pids: BTreeMap<i64, ObservationIssue>,
    absent_pids: BTreeSet<i64>,
    process_list_complete: bool,
    truncated: bool,
    bound: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessLookup<'a> {
    Present(&'a ProcessFact),
    Absent,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessObservation {
    Present(ProcessFact),
    Absent,
    Unknown(ObservationIssue),
}

#[derive(Debug, Error)]
pub enum CensusError {
    #[error("census bound must be between 1 and {MAX_CENSUS_ENTRIES}")]
    InvalidBound,
    #[error("process census record is malformed or inconsistent")]
    InvalidRecord,
    #[error("could not read the host boot identity")]
    BootIdentity(#[source] std::io::Error),
    #[error("could not enumerate processes")]
    ProcessList(#[source] std::io::Error),
    #[error("native process census is unsupported on this platform")]
    Unsupported,
}

impl Census {
    /// Build a bounded observation from native facts or recorded fixtures.
    /// A partial listing never implies absence.
    pub fn from_observations(
        facts: impl IntoIterator<Item = ProcessFact>,
        unknown_pids: impl IntoIterator<Item = (i64, ObservationIssue)>,
        absent_pids: impl IntoIterator<Item = i64>,
        process_list_complete: bool,
        truncated: bool,
        bound: usize,
    ) -> Result<Self, CensusError> {
        if bound == 0 || bound > MAX_CENSUS_ENTRIES {
            return Err(CensusError::InvalidBound);
        }
        let mut processes = BTreeMap::new();
        let mut census_boot: Option<String> = None;
        for fact in facts {
            fact.validate()?;
            if census_boot
                .as_ref()
                .is_some_and(|boot| boot != &fact.identity.boot)
            {
                return Err(CensusError::InvalidRecord);
            }
            census_boot.get_or_insert_with(|| fact.identity.boot.clone());
            if processes.insert(fact.identity.pid, fact).is_some() {
                return Err(CensusError::InvalidRecord);
            }
        }
        let mut unknown = BTreeMap::new();
        for (pid, issue) in unknown_pids {
            if pid <= 0 || unknown.insert(pid, issue).is_some() {
                return Err(CensusError::InvalidRecord);
            }
        }
        let mut absent = BTreeSet::new();
        for pid in absent_pids {
            if pid <= 0 || !absent.insert(pid) {
                return Err(CensusError::InvalidRecord);
            }
        }
        let observed = processes
            .len()
            .saturating_add(unknown.len())
            .saturating_add(absent.len());
        if observed > bound
            || processes
                .keys()
                .any(|pid| unknown.contains_key(pid) || absent.contains(pid))
            || unknown.keys().any(|pid| absent.contains(pid))
            || (truncated && process_list_complete)
        {
            return Err(CensusError::InvalidRecord);
        }
        Ok(Self {
            processes,
            unknown_pids: unknown,
            absent_pids: absent,
            process_list_complete,
            truncated,
            bound,
        })
    }

    pub fn process(&self, pid: i64) -> ProcessLookup<'_> {
        if let Some(fact) = self.processes.get(&pid) {
            return ProcessLookup::Present(fact);
        }
        if self.unknown_pids.contains_key(&pid) {
            return ProcessLookup::Unknown;
        }
        if self.absent_pids.contains(&pid) || (self.process_list_complete && !self.truncated) {
            return ProcessLookup::Absent;
        }
        ProcessLookup::Unknown
    }

    pub fn processes(&self) -> &BTreeMap<i64, ProcessFact> {
        &self.processes
    }

    pub fn unknown_pids(&self) -> &BTreeMap<i64, ObservationIssue> {
        &self.unknown_pids
    }

    pub fn bound(&self) -> usize {
        self.bound
    }

    pub fn facts(&self) -> impl Iterator<Item = &ProcessFact> {
        self.processes.values()
    }

    pub fn is_complete(&self) -> bool {
        self.completeness() == CensusCompleteness::Complete
    }

    pub fn completeness(&self) -> CensusCompleteness {
        match (
            self.process_list_complete && self.unknown_pids.is_empty(),
            self.truncated,
        ) {
            (true, false) => CensusCompleteness::Complete,
            (false, false) => CensusCompleteness::Partial,
            (true, true) => CensusCompleteness::Truncated,
            (false, true) => CensusCompleteness::PartialAndTruncated,
        }
    }

    /// Yes requires every parent edge to have a current native identity and an
    /// older start stamp. Coarse stamps, cycles and incomplete edges are Unknown.
    pub fn ancestry(&self, child: &ProcessIdentity, ancestor: &ProcessIdentity) -> Ancestry {
        if child.boot != ancestor.boot || child == ancestor {
            return Ancestry::No;
        }
        match self.process(child.pid) {
            ProcessLookup::Present(fact) if fact.identity == *child => {}
            ProcessLookup::Present(_) | ProcessLookup::Absent => return Ancestry::No,
            ProcessLookup::Unknown => return Ancestry::Unknown,
        }
        match self.process(ancestor.pid) {
            ProcessLookup::Present(fact) if fact.identity == *ancestor => {}
            ProcessLookup::Present(_) | ProcessLookup::Absent => return Ancestry::No,
            ProcessLookup::Unknown => return Ancestry::Unknown,
        }
        match self.lineage(child, ancestor) {
            Some(lineage) if lineage.last() == Some(ancestor) => Ancestry::Yes,
            Some(_) => Ancestry::No,
            None => Ancestry::Unknown,
        }
    }

    fn lineage(
        &self,
        child: &ProcessIdentity,
        ancestor: &ProcessIdentity,
    ) -> Option<Vec<ProcessIdentity>> {
        let mut chain = vec![child.clone()];
        let mut current = child.clone();
        let mut seen = BTreeSet::from([child.pid]);
        loop {
            let fact = match self.process(current.pid) {
                ProcessLookup::Present(fact) if fact.identity == current => fact,
                ProcessLookup::Present(_) | ProcessLookup::Absent | ProcessLookup::Unknown => {
                    return None;
                }
            };
            if fact.parent_pid <= 0 {
                return Some(chain);
            }
            let parent = match self.process(fact.parent_pid) {
                ProcessLookup::Present(parent) => parent,
                ProcessLookup::Absent => return Some(chain),
                ProcessLookup::Unknown => return None,
            };
            if parent.identity.boot != current.boot
                || parent.identity.native_start >= current.native_start
                || !seen.insert(parent.identity.pid)
            {
                return None;
            }
            current = parent.identity.clone();
            chain.push(current.clone());
            if current == *ancestor {
                return Some(chain);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ancestry {
    Yes,
    No,
    Unknown,
}

/// A validated child registration from Run. Only crate code that has decoded
/// and checked a durable registration should create this value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerRegistration {
    identity: ProcessIdentity,
}

impl OwnerRegistration {
    pub(crate) fn from_validated_registration(identity: ProcessIdentity) -> Self {
        Self { identity }
    }

    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }
}

/// Previously proven identities stay spared while their census row is unknown.
#[derive(Clone, Debug, Default)]
pub struct SparingTracker {
    spared: BTreeSet<ProcessIdentity>,
    parents: BTreeMap<ProcessIdentity, ProcessIdentity>,
    roots: BTreeMap<ProcessIdentity, ProcessIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum SparingError {
    #[error("registration and held custody proof name different identities")]
    ProofMismatch,
    #[error("registered process identity is no longer observed")]
    IdentityMismatch,
    #[error("registered process identity cannot be observed")]
    IdentityUnknown,
    #[error("registered process is not a live descendant of the custody root")]
    NotDescendant,
}

impl SparingTracker {
    /// Requires registration, exact current native identity, proven ancestry and
    /// a sys-issued proof of the held custody description.
    pub(crate) fn spare_registered_owner(
        &mut self,
        census: &Census,
        custody_root: &ProcessIdentity,
        registration: &OwnerRegistration,
        proof: &crate::sys::census::HeldCustodyProof,
    ) -> Result<(), SparingError> {
        if proof.owner_identity() != registration.identity() {
            return Err(SparingError::ProofMismatch);
        }
        let fact = match census.process(registration.identity.pid) {
            ProcessLookup::Present(fact) if fact.identity == registration.identity => fact,
            ProcessLookup::Present(_) | ProcessLookup::Absent => {
                return Err(SparingError::IdentityMismatch);
            }
            ProcessLookup::Unknown => return Err(SparingError::IdentityUnknown),
        };
        if fact.zombie || census.ancestry(&registration.identity, custody_root) != Ancestry::Yes {
            return Err(SparingError::NotDescendant);
        }
        self.spared.insert(registration.identity.clone());
        self.roots
            .insert(registration.identity.clone(), custody_root.clone());
        self.record_lineage(census, &registration.identity, custody_root);
        self.refresh(census);
        Ok(())
    }

    fn record_lineage(
        &mut self,
        census: &Census,
        child: &ProcessIdentity,
        ancestor: &ProcessIdentity,
    ) {
        let mut current = child.clone();
        let mut seen = BTreeSet::from([current.pid]);
        while current != *ancestor {
            let Some(fact) = census.processes.get(&current.pid) else {
                break;
            };
            if fact.identity != current || fact.parent_pid <= 0 {
                break;
            }
            let ProcessLookup::Present(parent) = census.process(fact.parent_pid) else {
                break;
            };
            if parent.identity.boot != current.boot
                || parent.identity.native_start >= current.native_start
                || !seen.insert(parent.identity.pid)
            {
                break;
            }
            self.parents
                .insert(current.clone(), parent.identity.clone());
            current = parent.identity.clone();
        }
    }

    /// Refresh parent edges and known descendants. PID reuse removes old proof;
    /// an unknown census row preserves it.
    pub fn refresh(&mut self, census: &Census) {
        self.spared
            .retain(|identity| match census.process(identity.pid) {
                ProcessLookup::Present(fact) => fact.identity == *identity,
                ProcessLookup::Absent => false,
                ProcessLookup::Unknown => true,
            });
        self.roots
            .retain(|identity, _| self.spared.contains(identity));
        let no_longer_descendant: Vec<_> = self
            .spared
            .iter()
            .filter_map(|identity| {
                let root = self.roots.get(identity)?;
                let exact = match census.process(identity.pid) {
                    ProcessLookup::Present(fact) => fact.identity == *identity,
                    ProcessLookup::Absent => false,
                    ProcessLookup::Unknown => return None,
                };
                if !exact || census.ancestry(identity, root) == Ancestry::No {
                    Some(identity.clone())
                } else {
                    None
                }
            })
            .collect();
        for identity in no_longer_descendant {
            self.spared.remove(&identity);
            self.roots.remove(&identity);
        }
        self.parents.retain(|child, parent| {
            let child_live = match census.process(child.pid) {
                ProcessLookup::Present(fact) if fact.identity == *child => {
                    fact.parent_pid == parent.pid
                }
                ProcessLookup::Present(_) | ProcessLookup::Absent => false,
                ProcessLookup::Unknown => true,
            };
            let parent_live = match census.process(parent.pid) {
                ProcessLookup::Present(fact) => fact.identity == *parent,
                ProcessLookup::Absent => false,
                ProcessLookup::Unknown => true,
            };
            child_live && parent_live
        });
        for fact in census.processes.values() {
            if fact.parent_pid <= 0 {
                continue;
            }
            let ProcessLookup::Present(parent) = census.process(fact.parent_pid) else {
                continue;
            };
            if parent.identity.boot == fact.identity.boot
                && parent.identity.native_start < fact.identity.native_start
            {
                self.parents
                    .insert(fact.identity.clone(), parent.identity.clone());
            }
        }
        loop {
            let additions: Vec<_> = self
                .parents
                .iter()
                .filter(|(child, parent)| {
                    !self.spared.contains(*child) && self.spared.contains(*parent)
                })
                .map(|(child, parent)| (child.clone(), self.roots.get(parent).cloned()))
                .collect();
            if additions.is_empty() {
                break;
            }
            for (child, root) in additions {
                self.spared.insert(child.clone());
                if let Some(root) = root {
                    self.roots.insert(child, root);
                }
            }
        }
    }

    pub fn is_spared(&self, identity: &ProcessIdentity) -> bool {
        self.spared.contains(identity)
    }

    pub fn spared_identities(&self) -> impl Iterator<Item = &ProcessIdentity> {
        self.spared.iter()
    }
}

pub use crate::sys::census::SystemProcessObserver;

pub trait ProcessObserver {
    fn boot_identity(&self) -> Result<String, CensusError>;
    fn observe(&self, pid: i64) -> ProcessObservation;
    fn census(&self, max_entries: usize) -> Result<Census, CensusError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: i64, start: u64) -> ProcessIdentity {
        ProcessIdentity {
            boot: "test-boot".into(),
            pid,
            native_start: start,
            group_id: 1,
            session_id: 1,
        }
    }

    fn fact(pid: i64, start: u64, parent_pid: i64) -> ProcessFact {
        ProcessFact {
            identity: identity(pid, start),
            parent_pid,
            zombie: false,
            cpu_micros: Some(0),
            command: None,
        }
    }

    fn make_census(facts: Vec<ProcessFact>, unknown: Vec<i64>, complete: bool) -> Option<Census> {
        Census::from_observations(
            facts,
            unknown.into_iter().map(|pid| (pid, ObservationIssue::Io)),
            [],
            complete,
            false,
            16,
        )
        .ok()
    }

    #[test]
    fn ancestry_requires_exact_identities_and_strictly_older_parent_stamps() {
        let root = identity(1, 100);
        let owner = identity(2, 200);
        let snapshot_result = make_census(
            vec![fact(1, 100, 0), fact(2, 200, 1), fact(3, 300, 2)],
            vec![],
            true,
        );
        assert!(
            snapshot_result.is_some(),
            "valid census fixture was rejected"
        );
        let Some(snapshot) = snapshot_result else {
            return;
        };
        assert_eq!(snapshot.ancestry(&owner, &root), Ancestry::Yes);

        let reused = identity(2, 250);
        assert_eq!(snapshot.ancestry(&reused, &root), Ancestry::No);
        let equal_stamp = identity(2, 100);
        let equal_result = make_census(vec![fact(1, 100, 0), fact(2, 100, 1)], vec![], true);
        assert!(
            equal_result.is_some(),
            "valid equal-stamp fixture was rejected"
        );
        let Some(equal_snapshot) = equal_result else {
            return;
        };
        assert_eq!(
            equal_snapshot.ancestry(&equal_stamp, &root),
            Ancestry::Unknown
        );
    }

    #[test]
    fn partial_census_never_turns_a_missing_parent_into_absence() {
        let root = identity(1, 100);
        let owner = identity(2, 200);
        let snapshot_result = make_census(vec![fact(2, 200, 1)], vec![1], false);
        assert!(
            snapshot_result.is_some(),
            "valid partial census fixture was rejected"
        );
        let Some(snapshot) = snapshot_result else {
            return;
        };
        assert_eq!(snapshot.process(1), ProcessLookup::Unknown);
        assert_eq!(snapshot.ancestry(&owner, &root), Ancestry::Unknown);
        assert_eq!(snapshot.completeness(), CensusCompleteness::Partial);
    }

    #[test]
    fn prior_sparing_survives_unknown_and_clears_on_pid_reuse() {
        let spared = identity(2, 200);
        let mut tracker = SparingTracker::default();
        tracker.spared.insert(spared.clone());
        let partial_result = make_census(vec![], vec![2], false);
        assert!(
            partial_result.is_some(),
            "valid partial census fixture was rejected"
        );
        let Some(partial) = partial_result else {
            return;
        };
        tracker.refresh(&partial);
        assert!(tracker.is_spared(&spared));

        let reused_result = make_census(vec![fact(2, 201, 1)], vec![], false);
        assert!(
            reused_result.is_some(),
            "valid reuse census fixture was rejected"
        );
        let Some(reused) = reused_result else {
            return;
        };
        tracker.refresh(&reused);
        assert!(!tracker.is_spared(&spared));

        let root = identity(1, 100);
        let owner = identity(2, 200);
        let moved = make_census(
            vec![fact(1, 100, 0), fact(2, 200, 4), fact(4, 150, 9)],
            vec![],
            true,
        );
        assert!(moved.is_some(), "valid moved-owner fixture was rejected");
        let Some(moved) = moved else {
            return;
        };
        tracker.spared.insert(owner.clone());
        tracker.roots.insert(owner.clone(), root.clone());
        tracker.parents.insert(owner.clone(), root);
        tracker.refresh(&moved);
        assert!(!tracker.is_spared(&owner));
    }

    #[test]
    fn census_records_strictly_bound_and_validate_pid_keys() {
        assert!(matches!(
            Census::from_observations([], [], [], false, false, 0),
            Err(CensusError::InvalidBound)
        ));
        assert!(matches!(
            Census::from_observations([fact(1, 1, 0), fact(1, 2, 0)], [], [], true, false, 4,),
            Err(CensusError::InvalidRecord)
        ));
    }
}
