//! Immutable host-owned chair associations, including explicitly unbound runs.
use crate::*;
use provenant_core::{Digest, RunId};
use provenant_records::run::ChairBinding;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunChair {
    pub request_digest: Digest,
    pub chair: Option<ChairBinding>,
}

impl Store {
    pub fn run_chair(
        &self,
        project: ProjectId,
        run: RunId,
        request: &Request<'_>,
    ) -> Result<Option<RunChair>> {
        request.retry(|| {
            let raw: Option<Vec<u8>> = self
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
        self.write(request, |tx| {
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
