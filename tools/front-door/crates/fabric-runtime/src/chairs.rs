//! Host-only dispatch associations. Never open this index in a lane's child root.
use provenant_core::{Digest, ProjectId, StateRoot};
use provenant_fabric_store::HostStore;
pub use provenant_fabric_store::{Error, GraphChair, Request};
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
        graph: &str,
        call: &Request<'_>,
    ) -> Result<Option<GraphChair>, Error> {
        self.0.graph_chair(project, graph, call)
    }

    /// Seal an association at root submission before any provider can launch.
    pub fn submit(
        &self,
        project: ProjectId,
        graph: &str,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        self.0.seal_graph_chair(
            project,
            graph,
            &GraphChair {
                extra: Default::default(),
                request_digest: proposal.digest()?,
                chair: proposal.chair.clone(),
            },
            call,
        )
    }

    /// Carry a sealed association to a named child graph. An explicit
    /// disagreement refuses the launch; an absent proposal inherits the seal.
    pub fn inherit(
        &self,
        project: ProjectId,
        parent: (&str, Digest),
        graph: &str,
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
        self.0.seal_graph_chair(
            project,
            graph,
            &GraphChair {
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
    use provenant_core::{ActorId, CancelToken, HostId, MonoTime, RootInputs, TestClock, WallTime};
    use provenant_records::run::ChairBinding;

    fn proposal() -> CanonicalRequest {
        CanonicalRequest {
            extra: Default::default(),
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            task: serde_json::from_value(serde_json::json!({
                "prompt_digest": provenant_core::Digest::from_bytes([4; 32]),
                "route": {"kind":"model","provider":"fixture","model":"fixture/model"},
                "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
                "provider_timeout_seconds": 30,
                "delivery_id": null
            }))
            .unwrap(),
            parent: None,
            lane_depth: None,
            parent_attempt: None,
        }
    }

    fn binding() -> ChairBinding {
        ChairBinding {
            extra: Default::default(),
            session: None,
            host: HostId::from_bytes([2; 16]),
            actor: ActorId::from_bytes([3; 16]),
        }
    }

    #[test]
    fn associations_are_keyed_by_project_and_graph_and_inherit_by_name()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = crate::test_support::tempdir()?;
        let base = temp.path().canonicalize()?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let root = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
        let clock = TestClock::new(WallTime::from_micros(1)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let project = ProjectId::from_bytes([1; 16]);
        let other_project = ProjectId::from_bytes([4; 16]);
        let chair = binding();
        let mut parent = proposal();
        parent.chair = Some(chair.clone());
        let index = ChairIndex::open(&root, &call)?;
        index.submit(project, "graph", &parent, &call)?;
        index.submit(project, "graph", &parent, &call)?;

        let child = proposal();
        index.inherit(
            project,
            ("graph", parent.digest()?),
            "graph-child",
            &child,
            &call,
        )?;
        assert_eq!(
            index.read(project, "graph", &call)?.unwrap().chair,
            Some(chair.clone())
        );
        assert_eq!(
            index.read(project, "graph-child", &call)?.unwrap().chair,
            Some(chair)
        );
        assert!(index.read(project, "missing", &call)?.is_none());
        assert!(index.read(other_project, "graph", &call)?.is_none());

        let mut changed = parent.clone();
        changed.chair = None;
        assert!(matches!(
            index.submit(project, "graph", &changed, &call),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            index.inherit(
                project,
                ("graph", Digest::from_bytes([8; 32])),
                "other-child",
                &child,
                &call
            ),
            Err(Error::Conflict)
        ));
        let mut conflicting_child = child.clone();
        conflicting_child.chair = Some(ChairBinding {
            extra: Default::default(),
            session: None,
            host: HostId::from_bytes([9; 16]),
            actor: ActorId::from_bytes([8; 16]),
        });
        assert!(matches!(
            index.inherit(
                project,
                ("graph", parent.digest()?),
                "other-child",
                &conflicting_child,
                &call
            ),
            Err(Error::Conflict)
        ));
        index.inherit(
            project,
            ("unknown-parent", Digest::from_bytes([7; 32])),
            "unbound-child",
            &child,
            &call,
        )?;
        assert!(
            index
                .read(project, "unbound-child", &call)?
                .unwrap()
                .chair
                .is_none()
        );
        assert!(matches!(
            index.submit(project, "Not a graph", &child, &call),
            Err(Error::Invalid("graph name"))
        ));
        Ok(())
    }
}
