//! Host-only dispatch associations. Never open this index in a lane's child root.
use provenant_core::{Digest, ProjectId, RunId, StateRoot};
use provenant_fabric_store::HostStore;
pub use provenant_fabric_store::{Error, Request, RunChair, ThreadWait};
use provenant_records::run::CanonicalRequest;

pub struct ChairIndex(HostStore);
impl ChairIndex {
    /// Read existing host associations without creating or migrating state.
    pub fn open_read_only(
        root: &StateRoot,
        deadline: std::time::Instant,
    ) -> Result<Option<Self>, Error> {
        HostStore::open_read_only(root, deadline).map(|store| store.map(Self))
    }

    pub fn open(root: &StateRoot, call: &Request<'_>) -> Result<Self, Error> {
        HostStore::open(root, call).map(Self)
    }

    /// Missing records mean unbound. No provider/session or run-store fallback.
    pub fn read(
        &self,
        project: ProjectId,
        run: RunId,
        call: &Request<'_>,
    ) -> Result<Option<RunChair>, Error> {
        self.0.run_chair(project, run, call)
    }

    /// Called only at explicit root submission, before any provider can launch.
    pub fn submit(
        &self,
        project: ProjectId,
        run: RunId,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        let value = RunChair {
            extra: Default::default(),
            request_digest: proposal.digest()?,
            chair: proposal.chair.clone(),
        };
        self.0.seal_run_chair(project, run, &value, call)
    }

    /// Parent digest comes from the host-sealed holder or original host request.
    /// Absent proposals inherit; an explicit disagreement refuses the launch.
    pub fn inherit(
        &self,
        project: ProjectId,
        parent: (RunId, Digest),
        run: RunId,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        let original = self.read(project, parent.0, call)?;
        if original
            .as_ref()
            .is_some_and(|value| value.request_digest != parent.1)
        {
            return Err(Error::Conflict);
        }
        let chair = original.and_then(|value| value.chair);
        if proposal.chair.is_some() && proposal.chair != chair {
            return Err(Error::Conflict);
        }
        self.0.seal_run_chair(
            project,
            run,
            &RunChair {
                extra: Default::default(),
                request_digest: proposal.digest()?,
                chair,
            },
            call,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, HostId, MonoTime, RootInputs, TestClock,
        WaitBudget, WallTime,
    };
    use provenant_records::run::ChairBinding;

    fn proposal() -> CanonicalRequest {
        CanonicalRequest {
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![],
            concurrency: 1,
            parent: None,
            lane_depth: Some(1),
            parent_attempt: None,
        }
    }

    #[test]
    fn nested_chair_binding() -> Result<(), Box<dyn std::error::Error>> {
        let temp = crate::test_support::tempdir()?;
        let base = temp.path().canonicalize()?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let root = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
        let clock = TestClock::new(WallTime::from_micros(1)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
            cancel: &cancel,
            backoff: &ThreadWait,
        };
        let project = ProjectId::from_bytes([1; 16]);
        let run = |n| RunId::from_bytes([n; 16]);
        let chair = Some(ChairBinding {
            extra: Default::default(),
            host: HostId::from_bytes([2; 16]),
            actor: ActorId::from_bytes([3; 16]),
        });
        let mut parent = proposal();
        parent.chair = chair.clone();
        let index = ChairIndex::open(&root, &call)?;
        index.submit(project, run(1), &parent, &call)?;
        index.submit(project, run(1), &parent, &call)?;
        let mut child = proposal();
        child.parent_attempt = Some(AttemptId::from_bytes([4; 16]));
        child.lane_depth = Some(2);
        index.inherit(project, (run(1), parent.digest()?), run(2), &child, &call)?;
        let mut grandchild = child.clone();
        grandchild.parent_attempt = Some(AttemptId::from_bytes([5; 16]));
        grandchild.lane_depth = Some(3);
        index.inherit(
            project,
            (run(2), child.digest()?),
            run(3),
            &grandchild,
            &call,
        )?;
        drop(index);
        let index = ChairIndex::open(&root, &call)?;
        for id in [1, 2, 3] {
            assert_eq!(index.read(project, run(id), &call)?.unwrap().chair, chair);
        }

        // Lane-writable proposals and even a fabricated child-root index cannot
        // transfer ownership or replace the host's sealed association.
        let mut forged = child.clone();
        forged.chair = Some(ChairBinding {
            extra: Default::default(),
            host: HostId::from_bytes([8; 16]),
            actor: ActorId::from_bytes([9; 16]),
        });
        assert!(matches!(
            index.inherit(project, (run(1), parent.digest()?), run(4), &forged, &call),
            Err(Error::Conflict)
        ));
        assert!(index.read(project, run(4), &call)?.is_none());
        inputs.state_dir = Some(root.as_path().join("children/fabricated"));
        let lane_root = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
        ChairIndex::open(&lane_root, &call)?.submit(project, run(2), &forged, &call)?;
        assert_eq!(index.read(project, run(2), &call)?.unwrap().chair, chair);
        assert!(matches!(
            index.submit(project, run(1), &forged, &call),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            index.inherit(project, (run(1), forged.digest()?), run(4), &child, &call),
            Err(Error::Conflict)
        ));

        // Continuations and retries copy the original index binding, including
        // a child whose lane-local request never contained that binding.
        let mut continuation = child.clone();
        continuation.parent = Some(AttemptId::from_bytes([6; 16]));
        index.inherit(
            project,
            (run(2), child.digest()?),
            run(5),
            &continuation,
            &call,
        )?;
        index.inherit(
            project,
            (run(2), child.digest()?),
            run(5),
            &continuation,
            &call,
        )?;
        assert_eq!(index.read(project, run(5), &call)?.unwrap().chair, chair);

        // A legacy/unindexed worker remains unbound, regardless of provider
        // session handles kept elsewhere in its run store.
        assert!(index.read(project, run(9), &call)?.is_none());
        index.inherit(project, (run(9), parent.digest()?), run(6), &child, &call)?;
        assert!(index.read(project, run(6), &call)?.unwrap().chair.is_none());
        assert!(matches!(
            index.inherit(project, (run(9), parent.digest()?), run(7), &forged, &call),
            Err(Error::Conflict)
        ));
        assert!(
            index
                .read(ProjectId::from_bytes([10; 16]), run(1), &call)?
                .is_none()
        );
        Ok(())
    }
}
