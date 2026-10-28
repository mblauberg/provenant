//! Deadline-aware observations. These do not record offers or acknowledge mail.
use crate::*;
use provenant_records::inbox::*;
use std::time::Instant;

#[allow(clippy::disallowed_methods)]
pub(crate) fn before(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(Error::Busy)
    } else {
        Ok(())
    }
}

impl Store {
    /// Read at most limit+1 candidates. Oversized entries stay pending; this
    /// observation never increments attempts or claims provider consumption.
    pub fn read_human_answers(
        &self,
        target: &AnswerTarget,
        limit: u64,
        max_bytes: usize,
        deadline: Instant,
    ) -> Result<Page<AnswerOffer>> {
        self.read_candidates(target, limit, max_bytes, deadline, false)
    }

    pub fn read_queued_human_answers(
        &self,
        target: &AnswerTarget,
        deadline: Instant,
    ) -> Result<Page<AnswerOffer>> {
        self.read_candidates(
            target,
            MAX_LOCAL_OFFER_ITEMS,
            MAX_LOCAL_OFFER_BYTES,
            deadline,
            true,
        )
    }

    /// Prefer newly queued answers so retries cannot hide them. Once that page
    /// is empty, recover offers whose result may have missed the host's pipe.
    pub fn read_hook_human_answers(
        &self,
        target: &AnswerTarget,
        deadline: Instant,
    ) -> Result<Page<AnswerOffer>> {
        let queued = self.read_queued_human_answers(target, deadline)?;
        if !queued.items.is_empty() || queued.truncated {
            return Ok(queued);
        }
        self.read_human_answers(
            target,
            MAX_LOCAL_OFFER_ITEMS,
            MAX_LOCAL_OFFER_BYTES,
            deadline,
        )
    }

    fn read_candidates(
        &self,
        target: &AnswerTarget,
        limit: u64,
        max_bytes: usize,
        deadline: Instant,
        queued: bool,
    ) -> Result<Page<AnswerOffer>> {
        let rows_limit = page(0, limit)?;
        // Internal candidates may exceed a provider's output budget so one
        // heavily escaped answer cannot hide smaller deliverable answers.
        if !(64..=MAX_LOCAL_OFFER_BYTES).contains(&max_bytes) {
            return Err(Error::Invalid("offer byte limit"));
        }
        before(deadline)?;
        let (index, states) = if queued {
            ("human_queued_delivery", "='queued'")
        } else {
            ("human_local_delivery", "IN ('queued','offered')")
        };
        // An absent index is an error, never a fallback scan.
        let mut statement = self.db.prepare(&format!(
            "SELECT o.answer,substr(o.record,1,?5),
             (SELECT substr(a.record,1,?5) FROM human_answers a WHERE a.project=o.project AND a.id=o.answer)
             FROM human_answer_outbox o INDEXED BY {index}
             WHERE o.project=?4 AND json_extract(CAST(o.record AS TEXT),'$.target_host')=?1
             AND json_extract(CAST(o.record AS TEXT),'$.target_session')=?2
             AND json_extract(CAST(o.record AS TEXT),'$.target_actor')=?3
             AND json_extract(CAST(o.record AS TEXT),'$.state') {states} ORDER BY o.answer LIMIT ?6"))?;
        let mut rows = statement.query(params![
            target.host.to_string(),
            target.session,
            target.actor.to_string(),
            target.project.to_string(),
            MAX_RECORD_READ_BYTES,
            rows_limit + 1
        ])?;
        let mut batch = Page {
            items: Vec::new(),
            truncated: false,
        };
        let mut candidates = 0;
        while let Some(row) = rows.next()? {
            before(deadline)?;
            candidates += 1;
            if candidates > rows_limit {
                batch.truncated = true;
                break;
            }
            let key: String = row.get(0)?;
            let delivery = row
                .get::<_, Vec<u8>>(1)
                .map_err(Error::from)
                .and_then(|bytes| decode::<AnswerDelivery>(&bytes));
            let answer = row
                .get::<_, Vec<u8>>(2)
                .map_err(Error::from)
                .and_then(|bytes| decode::<HumanAnswer>(&bytes));
            let (Ok(delivery), Ok(answer)) = (delivery, answer) else {
                invalid_answer(&key);
                continue;
            };
            if delivery.schema_version != 1
                || answer.schema_version != 1
                || delivery.answer_id != answer.id
                || key != answer.id.to_string()
                || delivery.target_host != target.host
                || delivery.target_actor != target.actor
                || delivery.target_session != target.session
            {
                invalid_answer(&key);
                continue;
            }
            batch.items.push(AnswerOffer { answer, delivery });
            if encode(&batch)?.len() > max_bytes {
                batch.items.pop();
                batch.truncated = true;
            }
        }
        before(deadline)?;
        Ok(batch)
    }
}

pub(crate) fn invalid_answer(id: &str) {
    use std::io::Write;
    // IDs are opaque stored input; escape controls and bound the diagnostic.
    let id: String = id.chars().take(64).flat_map(char::escape_default).collect();
    let _ = writeln!(
        std::io::stderr().lock(),
        "answer delivery: skipping invalid answer {id}"
    );
}
