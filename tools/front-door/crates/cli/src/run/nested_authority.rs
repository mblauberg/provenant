use provenant_supervisor::census::{
    Ancestry, Census, MAX_CENSUS_ENTRIES, ProcessIdentity, ProcessLookup, ProcessObservation,
    ProcessObserver, SystemProcessObserver,
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
    let (Some(peer_pid), Some(guardian)) = (peer_pid, guardian) else {
        return false;
    };
    let observer = SystemProcessObserver;
    let ProcessObservation::Present(guardian_now) = observer.observe(guardian.pid) else {
        return false;
    };
    if guardian_now.identity != *guardian {
        return false;
    }
    let ProcessObservation::Present(peer_now) = observer.observe(peer_pid) else {
        return false;
    };
    let census = match observer.live_census(guardian, &[], MAX_CENSUS_ENTRIES) {
        Ok(Some(census)) => census,
        Ok(None) => match observer.census(MAX_CENSUS_ENTRIES) {
            Ok(census) => census,
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    descendant_in_census(peer_pid, &peer_now.identity, guardian, &census)
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

fn descendant_in_census(
    peer_pid: i64,
    peer: &ProcessIdentity,
    guardian: &ProcessIdentity,
    census: &Census,
) -> bool {
    if peer.pid != peer_pid || peer == guardian {
        return false;
    }
    matches!(census.process(guardian.pid), ProcessLookup::Present(fact) if fact.identity == *guardian)
        && matches!(census.process(peer_pid), ProcessLookup::Present(fact) if fact.identity == *peer)
        && census.ancestry(peer, guardian) == Ancestry::Yes
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

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn authenticated_child_must_be_a_live_descendant_of_the_host_guardian() {
        let system = identity(1, 1);
        let guardian = identity(10, 10);
        let child = identity(11, 11);
        let census = Census::from_observations(
            [
                fact(system, 0),
                fact(guardian.clone(), 1),
                fact(child.clone(), 10),
            ],
            [],
            [],
            true,
            false,
            16,
        )
        .unwrap();
        assert!(descendant_in_census(child.pid, &child, &guardian, &census,));
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn unsandboxed_sibling_process_is_not_a_descendant_of_the_other_guardian() {
        let system = identity(1, 1);
        let guardian = identity(10, 10);
        let sibling_guardian = identity(20, 20);
        let sibling_caller = identity(21, 21);
        let census = Census::from_observations(
            [
                fact(system, 0),
                fact(guardian.clone(), 1),
                fact(sibling_guardian, 1),
                fact(sibling_caller.clone(), 20),
            ],
            [],
            [],
            true,
            false,
            16,
        )
        .unwrap();
        assert!(!descendant_in_census(
            sibling_caller.pid,
            &sibling_caller,
            &guardian,
            &census,
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
        let census = Census::from_observations(
            [
                fact(system, 0),
                fact(actual_guardian, 1),
                fact(child.clone(), 10),
            ],
            [],
            [],
            true,
            false,
            16,
        )
        .unwrap();
        assert!(!descendant_in_census(
            child.pid,
            &child,
            &stale_guardian,
            &census,
        ));
    }
}
