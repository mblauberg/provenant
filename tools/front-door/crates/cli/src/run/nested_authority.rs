use provenant_supervisor::census::{
    MAX_CENSUS_ENTRIES, ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver,
};

/// Capture the host guardian identity while the admission callback is running
/// inside that guardian. The lane cannot supply or replace this identity.
pub(super) fn current_guardian_identity() -> Option<ProcessIdentity> {
    let ProcessObservation::Present(fact) = provenant_supervisor::census::current_process() else {
        return None;
    };
    fact.validate().ok()?;
    Some(fact.identity)
}

/// Prove that a kernel supplied peer PID belongs to this guardian's process
/// tree. Unknown or changed identities are a refusal, never an absence proof.
pub(super) fn caller_is_guardian_descendant(
    peer_pid: Option<i64>,
    guardian: Option<&ProcessIdentity>,
) -> bool {
    caller_is_guardian_descendant_with(&SystemProcessObserver, peer_pid, guardian)
}

fn caller_is_guardian_descendant_with(
    observer: &dyn ProcessObserver,
    peer_pid: Option<i64>,
    guardian: Option<&ProcessIdentity>,
) -> bool {
    let (Some(peer_pid), Some(guardian)) = (peer_pid, guardian) else {
        return false;
    };
    let ProcessObservation::Present(guardian_now) = observer.observe(guardian.pid) else {
        return false;
    };
    if guardian_now.identity != *guardian {
        return false;
    }
    let ProcessObservation::Present(peer_now) = observer.observe(peer_pid) else {
        return false;
    };
    if peer_now.identity.pid != peer_pid
        || peer_now.identity == *guardian
        || peer_now.identity.boot != guardian.boot
    {
        return false;
    }
    // Follow the kernel-reported parent chain with exact live identities. A
    // host-wide census is not needed for this proof and can omit a process
    // that churned or was unreadable while the chain to this guardian stays
    // intact, which used to surface as `relay_peer_refused` on macOS. Every
    // accepted edge still carries the current identity and a strictly older
    // start stamp, so a replaced ancestor remains a refusal.
    let mut current = peer_now;
    for _ in 0..MAX_CENSUS_ENTRIES {
        if current.identity.pid == guardian.pid {
            return current.identity == *guardian;
        }
        let parent_pid = current.parent_pid;
        if parent_pid <= 0 {
            return false;
        }
        let ProcessObservation::Present(parent) = observer.observe(parent_pid) else {
            return false;
        };
        if parent.identity.pid != parent_pid
            || parent.identity.boot != guardian.boot
            || parent.identity.native_start >= current.identity.native_start
        {
            return false;
        }
        current = parent;
    }
    false
}

/// Protect the lane from a same-UID process that replaces the pathname socket:
/// the relay server must be an ancestor of this CLI process before any prompt
/// or dispatch proposal is sent.
pub(crate) fn server_is_current_ancestor(server_pid: i64) -> bool {
    let observer = SystemProcessObserver;
    let ProcessObservation::Present(server) = observer.observe(server_pid) else {
        return false;
    };
    caller_is_guardian_descendant(Some(i64::from(std::process::id())), Some(&server.identity))
}

pub(crate) fn same_worktree_incarnation(
    parent: Option<&provenant_records::custody::Subject>,
    child: Option<&provenant_records::custody::Subject>,
) -> bool {
    let (Some(parent), Some(child)) = (parent, child) else {
        return false;
    };
    parent.worktree_identity().is_some() && parent == child
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_supervisor::census::{Census, CensusError};

    fn identity(pid: i64, start: u64) -> ProcessIdentity {
        ProcessIdentity {
            boot: "fixture-boot".into(),
            pid,
            native_start: start,
            group_id: pid,
            session_id: pid,
        }
    }

    fn fact(
        identity: ProcessIdentity,
        parent_pid: i64,
    ) -> provenant_supervisor::census::ProcessFact {
        provenant_supervisor::census::ProcessFact {
            identity,
            parent_pid,
            zombie: false,
            cpu_micros: None,
            command: None,
        }
    }

    /// Live observations only. Every census call fails, so a proof that still
    /// succeeds cannot be leaning on a host-wide scan or a scoped snapshot.
    struct ChainObserver {
        facts: Vec<provenant_supervisor::census::ProcessFact>,
    }

    impl ProcessObserver for ChainObserver {
        fn boot_identity(&self) -> std::result::Result<String, CensusError> {
            Ok("fixture-boot".into())
        }

        fn observe(&self, pid: i64) -> ProcessObservation {
            self.facts
                .iter()
                .find(|current| current.identity.pid == pid)
                .cloned()
                .map_or(ProcessObservation::Absent, ProcessObservation::Present)
        }

        fn census(&self, _max_entries: usize) -> std::result::Result<Census, CensusError> {
            Err(CensusError::ProcessList(std::io::Error::other(
                "host census unavailable",
            )))
        }
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn authenticated_child_must_be_a_live_descendant_of_the_host_guardian() {
        let system = identity(1, 1);
        let guardian = identity(10, 10);
        let child = identity(11, 11);
        let observer = ChainObserver {
            facts: vec![
                fact(system, 0),
                fact(guardian.clone(), 1),
                fact(child.clone(), 10),
            ],
        };
        assert!(caller_is_guardian_descendant_with(
            &observer,
            Some(child.pid),
            Some(&guardian),
        ));
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn unsandboxed_sibling_process_is_not_a_descendant_of_the_other_guardian() {
        let system = identity(1, 1);
        let guardian = identity(10, 10);
        let sibling_guardian = identity(20, 20);
        let sibling_caller = identity(21, 21);
        let observer = ChainObserver {
            facts: vec![
                fact(system, 0),
                fact(guardian.clone(), 1),
                fact(sibling_guardian, 1),
                fact(sibling_caller.clone(), 20),
            ],
        };
        assert!(!caller_is_guardian_descendant_with(
            &observer,
            Some(sibling_caller.pid),
            Some(&guardian),
        ));
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn only_the_exact_live_worktree_incarnation_is_shared() {
        let parent = provenant_records::custody::Subject::worktree("/repo/wt", "generation-1");
        let same = provenant_records::custody::Subject::worktree("/repo/wt", "generation-1");
        let recreated = provenant_records::custody::Subject::worktree("/repo/wt", "generation-2");
        let unrelated =
            provenant_records::custody::Subject::worktree("/repo/other", "generation-1");
        assert!(same_worktree_incarnation(Some(&parent), Some(&same)));
        assert!(!same_worktree_incarnation(Some(&parent), Some(&recreated)));
        assert!(!same_worktree_incarnation(Some(&parent), Some(&unrelated)));
        assert!(!same_worktree_incarnation(None, Some(&same)));
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn recycled_guardian_identity_cannot_authorise_a_forged_parent() {
        let system = identity(1, 1);
        let actual_guardian = identity(10, 10);
        let stale_guardian = identity(10, 9);
        let child = identity(11, 11);
        let observer = ChainObserver {
            facts: vec![
                fact(system, 0),
                fact(actual_guardian, 1),
                fact(child.clone(), 10),
            ],
        };
        assert!(!caller_is_guardian_descendant_with(
            &observer,
            Some(child.pid),
            Some(&stale_guardian),
        ));
    }

    /// A failed or inconclusive census is not an absence proof. The invariant
    /// is the live parent chain to the exact guardian, so a healthy relay that
    /// a census cannot see must still be accepted, and an outsider refused.
    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn live_parent_chain_decides_ancestry_without_a_census() {
        let guardian = identity(10, 10);
        let child = identity(11, 11);
        let observer = ChainObserver {
            facts: vec![
                fact(identity(1, 1), 0),
                fact(guardian.clone(), 1),
                fact(child.clone(), 10),
            ],
        };
        assert!(caller_is_guardian_descendant_with(
            &observer,
            Some(child.pid),
            Some(&guardian),
        ));
        assert!(observer.census(MAX_CENSUS_ENTRIES).is_err());

        let sibling = identity(21, 21);
        let outsider = ChainObserver {
            facts: vec![
                fact(identity(1, 1), 0),
                fact(guardian.clone(), 1),
                fact(identity(20, 20), 1),
                fact(sibling.clone(), 20),
            ],
        };
        assert!(!caller_is_guardian_descendant_with(
            &outsider,
            Some(sibling.pid),
            Some(&guardian),
        ));
    }
}
