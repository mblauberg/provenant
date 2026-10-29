//! Host-only dispatch associations. Never open this index in a lane's child root.
use provenant_core::{Digest, OperationId, ProjectId, RunId, StateRoot};
use provenant_fabric_store::HostStore;
pub use provenant_fabric_store::{Error, Request, RunChair, ThreadWait};
use provenant_records::run::CanonicalRequest;

pub struct ChairIndex(HostStore);

/// Who proposes a new association, and the run's own operation, from which the
/// host re-derives a versioned run ID before trusting it.
#[derive(Clone, Copy)]
pub enum Admission<'a> {
    /// A lane-proposed child or a lane-local continuation.
    Lane(&'a OperationId),
    /// A continuation the host root prepared from its own sealed request. An
    /// unversioned one carries a run ID the host generated; a versioned one is
    /// still checked against derivation.
    HostRoot(&'a OperationId),
}
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
    /// The run ID must be the host derivation for `admission`'s operation.
    pub fn inherit(
        &self,
        project: ProjectId,
        parent: (RunId, Digest),
        run: RunId,
        admission: Admission<'_>,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        self.admit(project, run, admission, proposal, call)?;
        self.bind(project, parent, run, proposal, call)
    }

    /// Seed an association as if sealed before derived run IDs existed.
    #[cfg(any(test, feature = "retained-seed"))]
    pub fn inherit_retained(
        &self,
        project: ProjectId,
        parent: (RunId, Digest),
        run: RunId,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        self.bind(project, parent, run, proposal, call)
    }

    fn admit(
        &self,
        project: ProjectId,
        run: RunId,
        admission: Admission<'_>,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        use provenant_records::run::ChildIdError;
        let (operation, host_root) = match admission {
            Admission::Lane(operation) => (operation, false),
            Admission::HostRoot(operation) => (operation, true),
        };
        // A versioned request must carry exactly the host-derived run ID; an
        // unsupported version is never trusted.
        match proposal.child_run_id(project, operation) {
            Ok(Some(expected)) if expected == run => Ok(()),
            Ok(Some(_)) => Err(Error::Invalid(
                "child run ID does not match host derivation",
            )),
            Err(ChildIdError::UnsupportedVersion) => {
                Err(Error::Invalid("unsupported child attempt ID version"))
            }
            Err(ChildIdError::MissingParent) => {
                Err(Error::Invalid("derived child IDs require a parent attempt"))
            }
            Err(ChildIdError::Canonical(error)) => Err(error.into()),
            // The host root generated this unversioned continuation's ID itself.
            Ok(None) if host_root => Ok(()),
            // A lane's unversioned request may only replay a retained host seal.
            Ok(None) => match self.read(project, run, call)? {
                Some(value) if value.request_digest == proposal.digest()? => Ok(()),
                Some(_) => Err(Error::Conflict),
                None => Err(Error::Invalid(
                    "new chair associations require host-derived child run IDs",
                )),
            },
        }
    }

    fn bind(
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
            child_attempt_id_version: Some(1),
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
            session: None,
            host: HostId::from_bytes([2; 16]),
            actor: ActorId::from_bytes([3; 16]),
        });
        let op = |name: &str| OperationId::parse(name);
        let derived = |request: &CanonicalRequest,
                       operation: &OperationId|
         -> Result<RunId, Box<dyn std::error::Error>> {
            Ok(request
                .child_run_id(project, operation)?
                .ok_or("versioned request")?)
        };
        let (a, b, c, d, e, f) = (
            op("op-a")?,
            op("op-b")?,
            op("op-c")?,
            op("op-d")?,
            op("op-e")?,
            op("op-f")?,
        );
        let mut parent = proposal();
        parent.chair = chair.clone();
        let index = ChairIndex::open(&root, &call)?;
        index.submit(project, run(1), &parent, &call)?;
        index.submit(project, run(1), &parent, &call)?;
        let mut child = proposal();
        child.parent_attempt = Some(AttemptId::from_bytes([4; 16]));
        child.lane_depth = Some(2);
        let child_run = derived(&child, &a)?;
        index.inherit(
            project,
            (run(1), parent.digest()?),
            child_run,
            Admission::Lane(&a),
            &child,
            &call,
        )?;
        let mut grandchild = child.clone();
        grandchild.parent_attempt = Some(AttemptId::from_bytes([5; 16]));
        grandchild.lane_depth = Some(3);
        let grandchild_run = derived(&grandchild, &a)?;
        index.inherit(
            project,
            (child_run, child.digest()?),
            grandchild_run,
            Admission::Lane(&a),
            &grandchild,
            &call,
        )?;
        drop(index);
        let index = ChairIndex::open(&root, &call)?;
        for id in [run(1), child_run, grandchild_run] {
            assert_eq!(index.read(project, id, &call)?.unwrap().chair, chair);
        }

        // A versioned proposal binds only its host derivation: neither a
        // lane-chosen ID, another operation's ID nor an unsupported version.
        for (id, operation) in [(run(20), &a), (child_run, &b)] {
            assert!(matches!(
                index.inherit(
                    project,
                    (run(1), parent.digest()?),
                    id,
                    Admission::Lane(operation),
                    &child,
                    &call,
                ),
                Err(Error::Invalid(
                    "child run ID does not match host derivation"
                ))
            ));
            assert!(matches!(
                index.inherit(
                    project,
                    (run(1), parent.digest()?),
                    id,
                    Admission::HostRoot(operation),
                    &child,
                    &call,
                ),
                Err(Error::Invalid(_))
            ));
        }
        assert!(index.read(project, run(20), &call)?.is_none());
        let mut unsupported = child.clone();
        unsupported.child_attempt_id_version = Some(2);
        for admission in [Admission::Lane(&a), Admission::HostRoot(&a)] {
            assert!(matches!(
                index.inherit(
                    project,
                    (run(1), parent.digest()?),
                    run(21),
                    admission,
                    &unsupported,
                    &call,
                ),
                Err(Error::Invalid("unsupported child attempt ID version"))
            ));
        }
        assert!(index.read(project, run(21), &call)?.is_none());

        // Lane-writable proposals and even a fabricated child-root index cannot
        // transfer ownership or replace the host's sealed association.
        let mut forged = child.clone();
        forged.chair = Some(ChairBinding {
            extra: Default::default(),
            session: None,
            host: HostId::from_bytes([8; 16]),
            actor: ActorId::from_bytes([9; 16]),
        });
        let forged_run = derived(&forged, &a)?;
        assert!(matches!(
            index.inherit(
                project,
                (run(1), parent.digest()?),
                forged_run,
                Admission::Lane(&a),
                &forged,
                &call,
            ),
            Err(Error::Conflict)
        ));
        assert!(index.read(project, forged_run, &call)?.is_none());
        inputs.state_dir = Some(root.as_path().join("children/fabricated"));
        let lane_root = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
        ChairIndex::open(&lane_root, &call)?.submit(project, child_run, &forged, &call)?;
        assert_eq!(index.read(project, child_run, &call)?.unwrap().chair, chair);
        assert!(matches!(
            index.submit(project, run(1), &forged, &call),
            Err(Error::Conflict)
        ));
        let other_run = derived(&child, &b)?;
        assert!(matches!(
            index.inherit(
                project,
                (run(1), forged.digest()?),
                other_run,
                Admission::Lane(&b),
                &child,
                &call,
            ),
            Err(Error::Conflict)
        ));

        // Continuations and retries copy the original index binding, including
        // a child whose lane-local request never contained that binding.
        let mut continuation = child.clone();
        continuation.parent = Some(AttemptId::from_bytes([6; 16]));
        let continuation_run = derived(&continuation, &c)?;
        for _ in 0..2 {
            index.inherit(
                project,
                (child_run, child.digest()?),
                continuation_run,
                Admission::Lane(&c),
                &continuation,
                &call,
            )?;
        }
        assert_eq!(
            index.read(project, continuation_run, &call)?.unwrap().chair,
            chair
        );

        // A legacy/unindexed worker remains unbound, regardless of provider
        // session handles kept elsewhere in its run store.
        assert!(index.read(project, run(9), &call)?.is_none());
        let unbound_run = derived(&child, &d)?;
        index.inherit(
            project,
            (run(9), parent.digest()?),
            unbound_run,
            Admission::Lane(&d),
            &child,
            &call,
        )?;
        assert!(
            index
                .read(project, unbound_run, &call)?
                .unwrap()
                .chair
                .is_none()
        );

        // An unversioned lane proposal cannot bind a lane-chosen run ID, but may
        // replay a retained host seal. The host root's own continuation may.
        let mut legacy = child.clone();
        legacy.child_attempt_id_version = None;
        assert!(matches!(
            index.inherit(
                project,
                (run(1), parent.digest()?),
                run(11),
                Admission::Lane(&a),
                &legacy,
                &call,
            ),
            Err(Error::Invalid(_))
        ));
        assert!(index.read(project, run(11), &call)?.is_none());
        index.submit(project, run(12), &legacy, &call)?;
        index.inherit(
            project,
            (run(9), parent.digest()?),
            run(12),
            Admission::Lane(&a),
            &legacy,
            &call,
        )?;
        index.inherit(
            project,
            (run(1), parent.digest()?),
            run(15),
            Admission::HostRoot(&a),
            &legacy,
            &call,
        )?;
        assert_eq!(index.read(project, run(15), &call)?.unwrap().chair, chair);

        // Chair dimension: inherit while the parent has no row, then the parent
        // gains chair A. The identical re-inherit conflicts and X stays unbound.
        let late_run = derived(&child, &e)?;
        index.inherit(
            project,
            (run(13), child.digest()?),
            late_run,
            Admission::Lane(&e),
            &child,
            &call,
        )?;
        let mut owned = child.clone();
        owned.chair = chair.clone();
        index.submit(project, run(13), &owned, &call)?;
        assert!(matches!(
            index.inherit(
                project,
                (run(13), owned.digest()?),
                late_run,
                Admission::Lane(&e),
                &child,
                &call,
            ),
            Err(Error::Conflict)
        ));
        assert!(
            index
                .read(project, late_run, &call)?
                .unwrap()
                .chair
                .is_none()
        );
        let forged_unbound = derived(&forged, &f)?;
        assert!(matches!(
            index.inherit(
                project,
                (run(9), parent.digest()?),
                forged_unbound,
                Admission::Lane(&f),
                &forged,
                &call,
            ),
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
