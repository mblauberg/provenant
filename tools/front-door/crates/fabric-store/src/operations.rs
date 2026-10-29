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
        request.check()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(Error::from)?;
        if let Some(result) = replay(&tx, project, actor, id, &frozen)? {
            return Ok(result);
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
        self.signal_changes();
        Ok(result)
    }
}

impl Store {
    /// Validate replay before external preparation. The write path rechecks this
    /// exact actor/input under its transaction, including concurrent receipts.
    pub fn coordination_replay<T: DeserializeOwned>(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        input: &impl Serialize,
        request: &Request<'_>,
    ) -> Result<Option<T>> {
        request.check()?;
        let frozen = encode(input)?;
        replay(&self.db, project, actor, id, &frozen)
    }

    /// Read the home owner's atomic receipt without repeating a mutation.
    pub fn coordination_receipt<T: DeserializeOwned>(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        request: &Request<'_>,
    ) -> Result<Option<T>> {
        self.coordination_receipt_with_input::<serde::de::IgnoredAny, T>(
            project, actor, id, request,
        )
        .map(|value| value.map(|(_, result)| result))
    }

    /// Recover the frozen input alongside its result, so overlapping result
    /// shapes can be decoded according to the operation's recorded domain.
    pub fn coordination_receipt_with_input<I: DeserializeOwned, T: DeserializeOwned>(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        request: &Request<'_>,
    ) -> Result<Option<(I, T)>> {
        request.check()?;
        let row: Option<(String, Vec<u8>, Vec<u8>)> = self
            .db
            .query_row(
                "SELECT actor,request,result FROM coordination_operations WHERE project=?1 AND id=?2",
                params![project.to_string(), id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match row {
            Some((owner, input, result)) if owner == actor.to_string() => {
                Ok(Some((decode(&input)?, decode(&result)?)))
            }
            Some(_) => Err(Error::Conflict),
            None => Ok(None),
        }
    }
}

fn replay<T: DeserializeOwned>(
    db: &rusqlite::Connection,
    project: ProjectId,
    actor: ActorId,
    id: &OperationId,
    frozen: &[u8],
) -> Result<Option<T>> {
    let prior: Option<(String, Vec<u8>, Vec<u8>)> = db
        .query_row(
            "SELECT actor,request,result FROM coordination_operations WHERE project=?1 AND id=?2",
            params![project.to_string(), id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    match prior {
        Some((owner, input, result)) if owner == actor.to_string() && input == frozen => {
            Ok(Some(decode(&result)?))
        }
        Some(_) => Err(Error::Invalid(
            "operation ID reused with a different request",
        )),
        None => Ok(None),
    }
}
