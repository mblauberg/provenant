//! Atomic replay for short coordination mutations, never process/network work.
use crate::*;
use provenant_core::OperationId;

impl Store {
    /// The closure may only call this store's coordination methods. It must
    /// perform no waits, external I/O or nested operation. Error/drop rolls
    /// back the effects and receipt together. Actor is part of the frozen
    /// request: another inbox cannot recover a claimant's secret proof.
    pub fn coordination_operation<T, E>(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        input: &impl Serialize,
        request: &Request<'_>,
        action: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        T: Serialize + DeserializeOwned,
        E: From<Error>,
    {
        let frozen = encode(input)?;
        if !self.db.is_autocommit() {
            return Err(Error::Invalid("nested operation").into());
        }
        let tx = request.retry(|| {
            Ok(rusqlite::Transaction::new_unchecked(
                &self.db,
                rusqlite::TransactionBehavior::Immediate,
            )?)
        })?;
        let prior: Option<(String, Vec<u8>, Vec<u8>)> = tx.query_row(
            "SELECT actor,request,result FROM coordination_operations WHERE project=?1 AND id=?2",
            params![project.to_string(), id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(Error::from)?;
        if let Some((owner, prior, result)) = prior {
            if owner != actor.to_string() || prior != frozen {
                return Err(Error::Invalid("operation ID reused with a different request").into());
            }
            return Ok(decode(&result)?);
        }
        let result = action()?;
        tx.execute(
            "INSERT INTO coordination_operations VALUES (?1,?2,?3,?4,?5)",
            params![
                project.to_string(),
                id.to_string(),
                actor.to_string(),
                frozen,
                encode(&result)?
            ],
        )
        .map_err(Error::from)?;
        tx.commit().map_err(Error::from)?;
        Ok(result)
    }
}
