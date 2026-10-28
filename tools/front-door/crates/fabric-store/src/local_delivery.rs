//! Deadline-aware observations. These do not record offers or acknowledge mail.
use crate::*;
use provenant_records::inbox::*;
use std::time::Instant;

// Keep this value in sync with the partial index predicates in migration 012.
pub(crate) const HOOK_ENVELOPE_TOO_LARGE: &str = "hook_envelope_too_large";

#[derive(Clone, Copy)]
enum Candidates {
    Pending,
    Queued,
    Offered,
}

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
        self.read_candidates(
            target,
            limit,
            max_bytes,
            deadline,
            Candidates::Pending,
            None,
            false,
        )
        .map(|(page, _, _)| page)
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
            Candidates::Queued,
            None,
            false,
        )
        .map(|(page, _, _)| page)
    }

    /// Read only committed offers for recovery after final protocol fitting.
    pub fn read_offered_human_answers(
        &self,
        target: &AnswerTarget,
        deadline: Instant,
    ) -> Result<Page<AnswerOffer>> {
        self.read_candidates(
            target,
            MAX_LOCAL_OFFER_ITEMS,
            MAX_LOCAL_OFFER_BYTES,
            deadline,
            Candidates::Offered,
            None,
            false,
        )
        .map(|(page, _, _)| page)
    }

    /// A hook can advance past an undeliverable page without changing delivery
    /// state. The cursor tracks scanned keys, including invalid or oversized
    /// rows, and must be reused only with the same target and state.
    pub fn read_hook_answer_page(
        &self,
        target: &AnswerTarget,
        state: AnswerDeliveryState,
        after: Option<&str>,
        deadline: Instant,
    ) -> Result<(Page<AnswerOffer>, Option<String>, Vec<String>)> {
        let candidates = match state {
            AnswerDeliveryState::Queued => Candidates::Queued,
            AnswerDeliveryState::Offered => Candidates::Offered,
            AnswerDeliveryState::Acknowledged => {
                return Err(Error::Invalid("hook candidate state"));
            }
        };
        self.read_candidates(
            target,
            MAX_LOCAL_OFFER_ITEMS,
            MAX_LOCAL_OFFER_BYTES,
            deadline,
            candidates,
            after,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)] // Shared bounded reader for the public and hook APIs.
    fn read_candidates(
        &self,
        target: &AnswerTarget,
        limit: u64,
        max_bytes: usize,
        deadline: Instant,
        candidates: Candidates,
        after: Option<&str>,
        tolerate_invalid: bool,
    ) -> Result<(Page<AnswerOffer>, Option<String>, Vec<String>)> {
        let rows_limit = page(0, limit)?;
        // Internal candidates may exceed a provider's output budget so one
        // heavily escaped answer cannot hide smaller deliverable answers.
        if !(64..=MAX_LOCAL_OFFER_BYTES).contains(&max_bytes) {
            return Err(Error::Invalid("offer byte limit"));
        }
        before(deadline)?;
        let (index, states) = match candidates {
            Candidates::Queued => ("human_queued_delivery", "='queued'"),
            Candidates::Pending => ("human_local_delivery", "IN ('queued','offered')"),
            Candidates::Offered => (
                "human_local_delivery",
                "IN ('queued','offered') AND json_extract(CAST(o.record AS TEXT),'$.state')='offered'",
            ),
        };
        let (index, hook_filter) = if tolerate_invalid {
            (
                match candidates {
                    Candidates::Queued => "human_hook_queued",
                    Candidates::Offered => "human_hook_pending",
                    Candidates::Pending => unreachable!(),
                },
                format!(
                    "AND coalesce(json_extract(CAST(o.record AS TEXT),'$.error'),'') != '{HOOK_ENVELOPE_TOO_LARGE}'"
                ),
            )
        } else {
            (index, String::new())
        };
        // An absent index is an error, never a fallback scan.
        let mut statement = self.db.prepare(&format!(
            "SELECT o.answer,substr(o.record,1,?5),
             (SELECT substr(a.record,1,?5) FROM human_answers a WHERE a.project=o.project AND a.id=o.answer)
             FROM human_answer_outbox o INDEXED BY {index}
             WHERE o.project=?4 AND json_extract(CAST(o.record AS TEXT),'$.target_host')=?1
             AND json_extract(CAST(o.record AS TEXT),'$.target_session')=?2
             AND json_extract(CAST(o.record AS TEXT),'$.target_actor')=?3
             AND json_extract(CAST(o.record AS TEXT),'$.state') {states}
             {hook_filter} AND (?7 IS NULL OR o.answer>?7) ORDER BY o.answer LIMIT ?6"))?;
        let mut rows = statement.query(params![
            target.host.to_string(),
            target.session,
            target.actor.to_string(),
            target.project.to_string(),
            MAX_RECORD_READ_BYTES,
            rows_limit + 1,
            after,
        ])?;
        let mut batch = Page {
            items: Vec::new(),
            truncated: false,
        };
        let mut candidates = 0;
        let mut last_scanned = None;
        let mut oversized = Vec::new();
        while let Some(row) = rows.next()? {
            before(deadline)?;
            candidates += 1;
            if candidates > rows_limit {
                batch.truncated = true;
                break;
            }
            let key: String = row.get(0)?;
            let previous_scanned = last_scanned.replace(key.clone());
            let delivery = row
                .get::<_, Vec<u8>>(1)
                .map_err(Error::from)
                .and_then(|bytes| decode::<AnswerDelivery>(&bytes));
            let answer = row
                .get::<_, Vec<u8>>(2)
                .map_err(Error::from)
                .and_then(|bytes| decode::<HumanAnswer>(&bytes));
            let (delivery, answer) = match (delivery, answer) {
                (Ok(delivery), Ok(answer)) => (delivery, answer),
                (Err(error), _) | (_, Err(error)) if !tolerate_invalid => return Err(error),
                _ => {
                    invalid_answer(&key);
                    continue;
                }
            };
            if delivery.schema_version != 1
                || answer.schema_version != 1
                || delivery.answer_id != answer.id
                || key != answer.id.to_string()
                || delivery.target_host != target.host
                || delivery.target_actor != target.actor
                || delivery.target_session != target.session
            {
                if !tolerate_invalid {
                    return Err(Error::Invalid("answer identity or schema version"));
                }
                invalid_answer(&key);
                continue;
            }
            batch.items.push(AnswerOffer { answer, delivery });
            // A page can temporarily exceed the record-size limit even though
            // each stored answer is valid. Measure it without that limit.
            if CanonicalV1::bytes(&batch)?.as_bytes().len() > max_bytes {
                let candidate = batch
                    .items
                    .pop()
                    .ok_or(Error::Invalid("empty answer page"))?;
                batch.truncated = true;
                if tolerate_invalid {
                    let single = Page {
                        items: vec![candidate],
                        truncated: false,
                    };
                    if CanonicalV1::bytes(&single)?.as_bytes().len() > max_bytes {
                        oversized_answer(&key);
                        oversized.push(key);
                    } else {
                        // Revisit a candidate that fits alone on the next page.
                        last_scanned = previous_scanned;
                        break;
                    }
                }
            }
        }
        before(deadline)?;
        let next = batch.truncated.then_some(last_scanned).flatten();
        Ok((batch, next, oversized))
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

fn oversized_answer(id: &str) {
    use std::io::Write;
    let id: String = id.chars().take(64).flat_map(char::escape_default).collect();
    let _ = writeln!(
        std::io::stderr().lock(),
        "answer delivery: answer {id} exceeds internal page limit; retained for Fabric mail"
    );
}
