//! Bounded live-history walk. This is not a complete host census.
#[cfg(target_os = "macos")]
use crate::census::ProcessObserver;
#[cfg(any(target_os = "macos", test))]
use crate::census::{Census, CensusError, ProcessIdentity};
#[cfg(any(target_os = "macos", test))]
use crate::census::{ProcessFact, ProcessObservation};
#[cfg(any(target_os = "macos", test))]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(any(target_os = "macos", test))]
use std::io;

#[cfg(any(target_os = "macos", test))]
trait Source {
    fn observe(&self, pid: i64) -> ProcessObservation;
    fn children(&self, pid: i64) -> Result<Vec<i64>, CensusError>;
    fn group(&self, pid: i64) -> Result<Vec<i64>, CensusError>;
}

#[cfg(any(target_os = "macos", test))]
fn unavailable() -> CensusError {
    CensusError::ProcessList(io::Error::other("scoped process observation unavailable"))
}

#[cfg(target_os = "macos")]
struct Native<'a>(&'a dyn ProcessObserver);
#[cfg(target_os = "macos")]
impl Source for Native<'_> {
    fn observe(&self, pid: i64) -> ProcessObservation {
        self.0.observe(pid)
    }
    fn children(&self, pid: i64) -> Result<Vec<i64>, CensusError> {
        let pid = u32::try_from(pid).map_err(|_| unavailable())?;
        super::process_tree::children(pid)
            .map(|pids| pids.into_iter().map(i64::from).collect())
            .map_err(|_| unavailable())
    }
    fn group(&self, pid: i64) -> Result<Vec<i64>, CensusError> {
        let pid = u32::try_from(pid).map_err(|_| unavailable())?;
        super::process_tree::group(pid)
            .map(|pids| pids.into_iter().map(i64::from).collect())
            .map_err(|_| unavailable())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn observe(
    observer: &dyn ProcessObserver,
    root: &ProcessIdentity,
    known: &[ProcessIdentity],
    bound: usize,
) -> Result<Option<Census>, CensusError> {
    if observer.boot_identity()? != root.boot {
        return Err(CensusError::InvalidRecord);
    }
    walk(&Native(observer), root, known, bound).map(Some)
}

/// Recheck identity, links and listings; churn, permission errors and overflow
/// invalidate the walk and cause its caller to take a fresh full census.
#[cfg(any(target_os = "macos", test))]
fn walk(
    source: &impl Source,
    root: &ProcessIdentity,
    known: &[ProcessIdentity],
    bound: usize,
) -> Result<Census, CensusError> {
    if bound == 0 || bound > crate::census::MAX_CENSUS_ENTRIES {
        return Err(CensusError::InvalidBound);
    }
    root.validate()?;
    let group: BTreeSet<_> = source.group(root.pid)?.into_iter().collect();
    if !group.contains(&root.pid) {
        return Err(unavailable());
    }
    let mut expected: BTreeMap<i64, ProcessIdentity> =
        known.iter().cloned().map(|id| (id.pid, id)).collect();
    expected.insert(root.pid, root.clone());
    let mut pending: BTreeSet<i64> = group.clone();
    pending.extend(expected.keys().copied());
    let mut facts: BTreeMap<i64, ProcessFact> = BTreeMap::new();
    let mut absent = BTreeSet::new();
    let mut lists = BTreeMap::new();
    while let Some(pid) = pending.pop_first() {
        if facts.contains_key(&pid) || absent.contains(&pid) {
            continue;
        }
        if facts.len().saturating_add(absent.len()) >= bound {
            return Err(CensusError::InvalidBound);
        }
        let fact = match source.observe(pid) {
            ProcessObservation::Present(fact)
                if fact.identity.pid == pid
                    && fact.identity.boot == root.boot
                    && fact.validate().is_ok() =>
            {
                fact
            }
            ProcessObservation::Absent if expected.contains_key(&pid) && !group.contains(&pid) => {
                absent.insert(pid);
                continue;
            }
            _ => return Err(unavailable()),
        };
        if expected.get(&pid).is_some_and(|id| *id != fact.identity) && !group.contains(&pid) {
            // Reused known PID cannot supply ancestry from its former owner.
            continue;
        }
        if pid == root.pid && fact.identity != *root {
            return Err(unavailable());
        }
        if group.contains(&pid) && fact.identity.group_id != root.pid {
            return Err(unavailable());
        }
        let children: BTreeSet<_> = source.children(pid)?.into_iter().collect();
        if children.len() > bound {
            return Err(CensusError::InvalidBound);
        }
        pending.extend(children.iter().copied());
        if pending
            .len()
            .saturating_add(facts.len())
            .saturating_add(absent.len())
            > bound
        {
            return Err(CensusError::InvalidBound);
        }
        lists.insert(pid, children);
        facts.insert(pid, fact);
    }
    if facts
        .get(&root.pid)
        .is_none_or(|fact| fact.identity != *root)
    {
        return Err(unavailable());
    }
    // Include parent chains without walking their children. They let the
    // ancestry policy prove that a nested owner is on a different branch;
    // omitted ancestors must not be mistaken for a negative ancestry result.
    let mut ancestors: BTreeSet<_> = facts
        .values()
        .map(|fact| fact.parent_pid)
        .filter(|pid| *pid > 0)
        .collect();
    while let Some(pid) = ancestors.pop_first() {
        if facts.contains_key(&pid) || absent.contains(&pid) {
            continue;
        }
        if facts.len().saturating_add(absent.len()) >= bound {
            return Err(CensusError::InvalidBound);
        }
        match source.observe(pid) {
            ProcessObservation::Present(fact)
                if fact.identity.pid == pid
                    && fact.identity.boot == root.boot
                    && fact.validate().is_ok() =>
            {
                if fact.parent_pid > 0 {
                    ancestors.insert(fact.parent_pid);
                }
                facts.insert(pid, fact);
            }
            ProcessObservation::Absent => {
                absent.insert(pid);
            }
            _ => return Err(unavailable()),
        }
    }
    for (pid, fact) in &facts {
        let now = match source.observe(*pid) {
            ProcessObservation::Present(fact) => fact,
            _ => return Err(unavailable()),
        };
        if now.identity != fact.identity
            || now.parent_pid != fact.parent_pid
            || now.identity.group_id != fact.identity.group_id
            || now.identity.session_id != fact.identity.session_id
        {
            return Err(unavailable());
        }
        if let Some(before) = lists.get(pid) {
            let children: BTreeSet<_> = source.children(*pid)?.into_iter().collect();
            if before != &children {
                return Err(unavailable());
            }
            for child in children {
                let Some(row) = facts.get(&child) else {
                    return Err(unavailable());
                };
                if row.parent_pid != *pid {
                    return Err(unavailable());
                }
            }
        }
    }
    if source.group(root.pid)?.into_iter().collect::<BTreeSet<_>>() != group {
        return Err(unavailable());
    }
    Census::from_observations(facts.into_values(), [], absent, false, false, bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::census::Ancestry;
    use std::cell::Cell;

    struct Fake {
        facts: BTreeMap<i64, ProcessFact>,
        members: Vec<i64>,
        fail: bool,
        groups: Cell<usize>,
        churn: bool,
    }
    impl Source for Fake {
        fn observe(&self, pid: i64) -> ProcessObservation {
            self.facts
                .get(&pid)
                .cloned()
                .map_or(ProcessObservation::Absent, ProcessObservation::Present)
        }
        fn children(&self, pid: i64) -> Result<Vec<i64>, CensusError> {
            if self.fail {
                return Err(unavailable());
            }
            Ok(self
                .facts
                .values()
                .filter(|fact| fact.parent_pid == pid)
                .map(|fact| fact.identity.pid)
                .collect())
        }
        fn group(&self, _: i64) -> Result<Vec<i64>, CensusError> {
            let calls = self.groups.get();
            self.groups.set(calls + 1);
            if self.churn && calls != 0 {
                return Err(unavailable());
            }
            Ok(self.members.clone())
        }
    }
    fn fact(pid: i64, parent: i64, group: i64, session: i64) -> ProcessFact {
        ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: group,
                session_id: session,
            },
            parent_pid: parent,
            zombie: false,
            cpu_micros: None,
            command: None,
        }
    }
    #[test]
    fn live_census_follows_known_escaped_descendants_without_claiming_host_completeness() {
        let root = fact(10, 1, 10, 1);
        let parent = fact(20, 10, 10, 1);
        let child = fact(30, 20, 10, 1);
        let first = Fake {
            facts: [root.clone(), parent.clone(), child.clone()]
                .into_iter()
                .map(|f| (f.identity.pid, f))
                .collect(),
            members: vec![10, 20, 30],
            fail: false,
            groups: Cell::new(0),
            churn: false,
        };
        let before = walk(&first, &root.identity, &[], 10).unwrap();
        assert_eq!(
            before.ancestry(&child.identity, &root.identity),
            Ancestry::Yes
        );
        assert!(!before.is_complete());
        let escaped = fact(30, 1, 30, 30);
        let grandchild = fact(40, 30, 30, 30);
        let second = Fake {
            facts: [root.clone(), escaped.clone(), grandchild.clone()]
                .into_iter()
                .map(|f| (f.identity.pid, f))
                .collect(),
            members: vec![10],
            fail: false,
            groups: Cell::new(0),
            churn: false,
        };
        let after = walk(
            &second,
            &root.identity,
            &[parent.identity, child.identity],
            10,
        )
        .unwrap();
        assert_eq!(
            after.ancestry(&grandchild.identity, &escaped.identity),
            Ancestry::Yes
        );
        assert!(matches!(
            after.process(20),
            crate::census::ProcessLookup::Absent
        ));
    }
    #[test]
    fn live_census_rejects_churn_unavailable_and_overflow() {
        let root = fact(10, 1, 10, 1);
        for (fail, churn, bound) in [(true, false, 10), (false, true, 10), (false, false, 1)] {
            let child = fact(20, 10, 10, 1);
            let source = Fake {
                facts: [root.clone(), child]
                    .into_iter()
                    .map(|f| (f.identity.pid, f))
                    .collect(),
                members: vec![10, 20],
                fail,
                groups: Cell::new(0),
                churn,
            };
            assert!(walk(&source, &root.identity, &[], bound).is_err());
        }
    }

    #[test]
    fn live_census_keeps_parent_chain_for_separate_nested_owners() {
        let init = fact(1, 0, 1, 1);
        let root = fact(10, 1, 10, 1);
        let build = fact(20, 10, 10, 1);
        let nested = fact(30, 10, 10, 1);
        let source = Fake {
            facts: [init, root.clone(), build.clone(), nested.clone()]
                .into_iter()
                .map(|fact| (fact.identity.pid, fact))
                .collect(),
            members: vec![10, 20, 30],
            fail: false,
            groups: Cell::new(0),
            churn: false,
        };
        let snapshot = walk(&source, &root.identity, &[], 10).unwrap();
        assert_eq!(
            snapshot.ancestry(&build.identity, &nested.identity),
            Ancestry::No
        );
    }

    #[test]
    fn live_census_does_not_describe_unrelated_host_processes() {
        use std::cell::RefCell;
        struct Counted<'a> {
            source: &'a Fake,
            observed: RefCell<BTreeSet<i64>>,
        }
        impl Source for Counted<'_> {
            fn observe(&self, pid: i64) -> ProcessObservation {
                self.observed.borrow_mut().insert(pid);
                self.source.observe(pid)
            }
            fn children(&self, pid: i64) -> Result<Vec<i64>, CensusError> {
                self.source.children(pid)
            }
            fn group(&self, pid: i64) -> Result<Vec<i64>, CensusError> {
                self.source.group(pid)
            }
        }
        let root = fact(10, 1, 10, 1);
        let child = fact(20, 10, 10, 1);
        let mut facts: BTreeMap<_, _> = [root.clone(), child]
            .into_iter()
            .map(|f| (f.identity.pid, f))
            .collect();
        facts.extend((100..1100).map(|pid| (pid, fact(pid, 0, pid, pid))));
        let source = Fake {
            facts,
            members: vec![10, 20],
            fail: false,
            groups: Cell::new(0),
            churn: false,
        };
        let counted = Counted {
            source: &source,
            observed: RefCell::new(BTreeSet::new()),
        };
        walk(&counted, &root.identity, &[], 20).unwrap();
        assert!(counted.observed.borrow().contains(&10));
        assert!(counted.observed.borrow().iter().all(|pid| *pid < 100));
    }

    #[test]
    fn live_census_does_not_walk_a_reused_known_pid() {
        let root = fact(10, 1, 10, 1);
        let old = fact(30, 10, 30, 30);
        let mut replacement = fact(30, 1, 30, 30);
        replacement.identity.native_start += 100;
        let unrelated_child = fact(40, 30, 30, 30);
        let source = Fake {
            facts: [root.clone(), replacement, unrelated_child]
                .into_iter()
                .map(|fact| (fact.identity.pid, fact))
                .collect(),
            members: vec![10],
            fail: false,
            groups: Cell::new(0),
            churn: false,
        };
        let snapshot = walk(&source, &root.identity, &[old.identity], 10).unwrap();
        assert!(matches!(
            snapshot.process(40),
            crate::census::ProcessLookup::Unknown
        ));
    }
}
