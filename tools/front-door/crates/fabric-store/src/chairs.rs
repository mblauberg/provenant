//! Immutable host-owned chair associations, including explicitly unbound runs.
use crate::*;
use provenant_core::{Digest, RunId};
use provenant_records::run::ChairBinding;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunChair {
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

    pub fn run_chair(
        &self,
        project: ProjectId,
        run: RunId,
        request: &Request<'_>,
    ) -> Result<Option<RunChair>> {
        request.retry(|| {
            let raw: Option<Vec<u8>> = self
                .0
                .db
                .query_row(
                    "SELECT substr(record,1,?3) FROM run_chairs WHERE project=?1 AND run=?2",
                    params![project.to_string(), run.to_string(), MAX_RECORD_READ_BYTES],
                    |row| row.get(0),
                )
                .optional()?;
            raw.as_deref().map(decode).transpose()
        })
    }

    /// Insert once; a replay must agree with both the request and binding.
    pub fn seal_run_chair(
        &self,
        project: ProjectId,
        run: RunId,
        value: &RunChair,
        request: &Request<'_>,
    ) -> Result<()> {
        let bytes = encode(value)?;
        self.0.write(request, |tx| {
            let previous: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT substr(record,1,?3) FROM run_chairs WHERE project=?1 AND run=?2",
                    params![project.to_string(), run.to_string(), MAX_RECORD_READ_BYTES],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(previous) = previous {
                return if decode::<RunChair>(&previous)? == *value {
                    Ok(())
                } else {
                    Err(Error::Conflict)
                };
            }
            tx.execute(
                "INSERT INTO run_chairs(project,run,record) VALUES(?1,?2,?3)",
                params![project.to_string(), run.to_string(), bytes],
            )?;
            Ok(())
        })
    }
}
