//! Graph ownership is a view of the host custody ledger.
use provenant_core::{ProjectId, StateRoot};
use provenant_fabric_store::HostStore;
pub use provenant_fabric_store::{Error, Request};
use provenant_records::{custody::*, run::CanonicalRequest};

pub struct ChairIndex(HostStore);
impl ChairIndex {
    pub fn open_read_only(
        root: &StateRoot,
        deadline: std::time::Instant,
    ) -> Result<Option<Self>, Error> {
        HostStore::open_read_only(root, deadline).map(|store| store.map(Self))
    }
    pub fn open(root: &StateRoot, call: &Request<'_>) -> Result<Self, Error> {
        HostStore::open(root, call).map(Self)
    }
    pub fn read(
        &self,
        project: ProjectId,
        graph: &str,
        call: &Request<'_>,
    ) -> Result<Option<CustodyRecord>, Error> {
        self.0
            .custody_item(project, &Subject::new(SubjectKind::Graph, graph), call)
    }
    pub fn submit(
        &self,
        project: ProjectId,
        graph: &str,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        self.seal(project, graph, proposal.chair.clone(), call)?;
        if self.read(project, graph, call)?.is_some() {
            self.0.seal_custody_provenance(
                project,
                &Subject::new(SubjectKind::Graph, graph),
                proposal.digest()?,
                call,
            )?;
        }
        Ok(())
    }
    pub fn inherit(
        &self,
        project: ProjectId,
        parent: &str,
        graph: &str,
        proposal: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        let chair = self
            .read(project, parent, call)?
            .and_then(|record| record.holder.chair().cloned());
        if proposal.chair.is_some() && proposal.chair != chair {
            return Err(Error::Conflict);
        }
        self.seal(project, graph, chair, call)?;
        if self.read(project, graph, call)?.is_some() {
            self.0.seal_custody_provenance(
                project,
                &Subject::new(SubjectKind::Graph, graph),
                proposal.digest()?,
                call,
            )?;
        }
        Ok(())
    }
    fn seal(
        &self,
        project: ProjectId,
        graph: &str,
        chair: Option<Chair>,
        call: &Request<'_>,
    ) -> Result<(), Error> {
        if graph.is_empty()
            || graph.len() > 48
            || !graph.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
            })
        {
            return Err(Error::Invalid("graph name"));
        }
        // An implicit child inherits; explicit advisory custody may change
        // holders without becoming a second launch lock.
        if let Some(chair) = chair {
            let change = self.0.acquire_custody(
                project,
                &Subject::new(SubjectKind::Graph, graph),
                &Holder::Chair { chair },
                Origin::Provenant,
                call,
            )?;
            crate::custody::warn(std::slice::from_ref(&change));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, CancelToken, HostId, MonoTime, RootInputs, TestClock, WallTime};
    use provenant_records::custody::Chair;

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

    fn binding() -> Chair {
        Chair {
            process: None,
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
        // A second node of the same graph naming no chair resolves the seal.
        let mut sibling = proposal();
        sibling.lane_depth = Some(1);
        index.submit(project, "graph", &sibling, &call)?;
        index.inherit(project, "graph", "graph-child", &child, &call)?;
        assert_eq!(
            index
                .read(project, "graph", &call)?
                .unwrap()
                .holder
                .chair()
                .cloned(),
            Some(chair.clone())
        );
        assert_eq!(
            index
                .read(project, "graph-child", &call)?
                .unwrap()
                .holder
                .chair()
                .cloned(),
            Some(chair)
        );
        assert!(index.read(project, "missing", &call)?.is_none());
        assert!(index.read(other_project, "graph", &call)?.is_none());

        let mut conflicting_child = child.clone();
        conflicting_child.chair = Some(Chair {
            process: None,
            extra: Default::default(),
            session: None,
            host: HostId::from_bytes([9; 16]),
            actor: ActorId::from_bytes([8; 16]),
        });
        index.submit(project, "graph", &conflicting_child, &call)?;
        assert_eq!(
            index.read(project, "graph", &call)?.unwrap().holder.chair(),
            conflicting_child.chair.as_ref()
        );
        assert!(matches!(
            index.inherit(
                project,
                "graph-child",
                "other-child",
                &conflicting_child,
                &call
            ),
            Err(Error::Conflict)
        ));
        index.inherit(project, "unknown-parent", "unbound-child", &child, &call)?;
        assert!(index.read(project, "unbound-child", &call)?.is_none());
        assert!(matches!(
            index.submit(project, "Not a graph", &child, &call),
            Err(Error::Invalid("graph name"))
        ));
        Ok(())
    }
}
