//! Immutable host-owned chair associations, including explicitly unbound graphs.
use crate::*;
use provenant_core::{Digest, ProjectId};
use provenant_records::run::ChairBinding;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphChair {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub request_digest: Digest,
    pub chair: Option<ChairBinding>,
}

/// Only this handle can access authoritative chair records. Coordination
/// Store handles deliberately expose no association methods.
pub struct HostStore(Store);
impl HostStore {
    /// Read existing host associations without creating or migrating state.
    pub fn open_read_only(
        root: &provenant_core::StateRoot,
        deadline: std::time::Instant,
    ) -> Result<Option<Self>> {
        Store::open_read_only_host(root, deadline).map(|store| store.map(Self))
    }

    pub fn open(root: &provenant_core::StateRoot, request: &Request<'_>) -> Result<Self> {
        Store::open_host(root, request).map(Self)
    }

    pub fn graph_chair(
        &self,
        project: ProjectId,
        graph: &str,
        request: &Request<'_>,
    ) -> Result<Option<GraphChair>> {
        request.check()?;
        validate_graph_name(graph)?;
        let raw: Option<Vec<u8>> = self
            .0
            .db
            .query_row(
                "SELECT substr(record,1,?3) FROM graph_chairs WHERE project=?1 AND graph=?2",
                params![project.to_string(), graph, MAX_RECORD_READ_BYTES],
                |row| row.get(0),
            )
            .optional()?;
        raw.as_deref().map(decode).transpose()
    }

    /// Insert once; a replay must agree with both the request and binding.
    pub fn seal_graph_chair(
        &self,
        project: ProjectId,
        graph: &str,
        value: &GraphChair,
        request: &Request<'_>,
    ) -> Result<()> {
        validate_graph_name(graph)?;
        let bytes = encode(value)?;
        self.0.write(request, |tx| {
            let previous: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT substr(record,1,?3) FROM graph_chairs WHERE project=?1 AND graph=?2",
                    params![project.to_string(), graph, MAX_RECORD_READ_BYTES],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(previous) = previous {
                return if decode::<GraphChair>(&previous)? == *value {
                    Ok(())
                } else {
                    Err(Error::Conflict)
                };
            }
            tx.execute(
                "INSERT INTO graph_chairs(project,graph,record) VALUES(?1,?2,?3)",
                params![project.to_string(), graph, bytes],
            )?;
            Ok(())
        })
    }
}

fn validate_graph_name(graph: &str) -> Result<()> {
    if graph.is_empty()
        || graph.len() > 48
        || !graph.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err(Error::Invalid("graph name"));
    }
    Ok(())
}
