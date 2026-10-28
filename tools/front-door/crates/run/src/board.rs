//! Local board metadata read in one SQLite snapshot, without artifact I/O.
use crate::{db::decode, *};
use provenant_core::ProjectId;
use rusqlite::params;
use sha2::{Digest as _, Sha256};

#[derive(Debug, serde::Serialize)]
pub struct BoardLane {
    pub attempt: AttemptRecord,
    pub run: RunRecord,
}

#[derive(Debug)]
pub struct BoardPage {
    pub generation: String,
    pub lanes: Vec<BoardLane>,
    pub next: Option<u64>,
}

impl RunStore {
    /// Cross-project host-store read. No child-store traversal or provider I/O.
    /// Generation covers retained attempt revisions and run metadata in the
    /// project scope, including rows outside this page or state filter.
    pub fn board_page(
        &self,
        project: Option<ProjectId>,
        all: bool,
        limit: u64,
        after: u64,
        call: &Request<'_>,
    ) -> Result<BoardPage> {
        if !(1..=100).contains(&limit) {
            return Err(RunError::Validation("board page bounds"));
        }
        let after = crate::db::sql_int(after)?;
        call.retry(|| {
            let tx = self.db.unchecked_transaction()?;
            let project = project.map(|id| id.to_string());
            let mut hash = Sha256::new();
            let mut index = tx.prepare(
                "SELECT a.sequence,a.id,a.revision,r.record FROM attempts a \
                 JOIN runs r ON r.id=a.run AND r.project=a.project \
                 WHERE (?1 IS NULL OR a.project=?1) ORDER BY a.sequence",
            )?;
            let mut rows = index.query([&project])?;
            while let Some(row) = rows.next()? {
                if call.budget.deadline().is_expired(call.clock.mono_now()) {
                    return Err(RunError::Contention);
                }
                if call.cancel.is_cancelled() {
                    return Err(RunError::Cancelled);
                }
                hash.update(row.get::<_, i64>(0)?.to_le_bytes());
                hash.update(row.get::<_, i64>(2)?.to_le_bytes());
                for column in [1, 3] {
                    let bytes = row
                        .get_ref(column)?
                        .as_bytes()
                        .map_err(|_| RunError::Validation("board generation column"))?;
                    hash.update((bytes.len() as u64).to_le_bytes());
                    hash.update(bytes);
                }
            }
            let generation = provenant_core::hex(&hash.finalize());
            let mut statement = tx.prepare(
                "SELECT a.sequence,a.id,a.project,a.run,a.task,a.record,r.record FROM attempts a \
                 JOIN runs r ON r.id=a.run AND r.project=a.project \
                 WHERE (?1 IS NULL OR a.project=?1) AND (?2 OR a.terminal=0) \
                 AND (?3=0 OR a.sequence<?3) ORDER BY a.sequence DESC LIMIT ?4",
            )?;
            let mut rows = statement.query(params![project, all, after, (limit + 1) as i64])?;
            let mut lanes = Vec::new();
            let mut last = 0;
            let mut more = false;
            while let Some(row) = rows.next()? {
                if lanes.len() == limit as usize {
                    more = true;
                    break;
                }
                let attempt: AttemptRecord = decode(&row.get::<_, Vec<u8>>(5)?)?;
                let run: RunRecord = decode(&row.get::<_, Vec<u8>>(6)?)?;
                if attempt.attempt_id.to_string() != row.get::<_, String>(1)?
                    || attempt.project_id.to_string() != row.get::<_, String>(2)?
                    || attempt.run_id.to_string() != row.get::<_, String>(3)?
                    || attempt.task_id.to_string() != row.get::<_, String>(4)?
                    || run.run_id != attempt.run_id
                    || run.project_id != attempt.project_id
                {
                    return Err(RunError::Validation("board index identity"));
                }
                last = u64::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| RunError::Validation("board sequence"))?;
                lanes.push(BoardLane { attempt, run });
            }
            Ok(BoardPage {
                generation,
                lanes,
                next: more.then_some(last),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::{AttemptId, CancelToken, RunId};

    #[test]
    #[allow(clippy::disallowed_methods, clippy::print_stderr)] // Explicit benchmark boundary.
    fn board_paging_snapshot_and_metadata() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        for n in 1u8..=101 {
            let mut input = input();
            input.run_id = RunId::from_bytes([n; 16]);
            input.operation_id = provenant_core::OperationId::parse(&format!("board-{n}"))?;
            input.attempts[0].attempt_id = AttemptId::from_bytes([n; 16]);
            input.request.tasks[0].title = Some(format!("Task {n}"));
            store.create(&input, &call)?;
        }
        let page = store.board_page(None, true, 100, 0, &call)?;
        assert_eq!(page.lanes.len(), 100);
        let next = page.next.ok_or("missing cursor")?;
        let last = store.board_page(None, true, 100, next, &call)?;
        assert_eq!(last.lanes.len(), 1);
        assert!(last.next.is_none());
        assert_eq!(page.generation, last.generation);
        assert_eq!(page.lanes[0].attempt.title.as_deref(), Some("Task 101"));
        assert_eq!(page.lanes[0].attempt.usage, UsageRecord::default());
        assert_eq!(page.lanes[0].run.run_id, page.lanes[0].attempt.run_id);
        let reopened = RunStore::open_read_only(&context, &call)?.ok_or("missing store")?;
        assert_eq!(
            reopened.board_page(None, true, 100, 0, &call)?.generation,
            page.generation
        );
        let old = store.record(AttemptId::from_bytes([101; 16]), &call)?;
        let mut mixed = old.clone();
        mixed.usage.uncached_input = Some(0);
        mixed.usage.reasoning = Some(7);
        mixed.revision += 1;
        store.write(&call, |tx| {
            crate::admission::update_attempt(tx, &old, &mixed)
        })?;
        let updated = store.board_page(None, true, 100, 0, &call)?;
        assert_eq!(updated.lanes[0].attempt.usage, mixed.usage);
        assert_ne!(updated.generation, page.generation);
        store.withdraw(
            AttemptId::from_bytes([1; 16]),
            AttemptState::Queued,
            Outcome::Cancelled,
            "test",
            call.clock.wall_now()?,
            &call,
        )?;
        assert_ne!(
            store.board_page(None, true, 100, 0, &call)?.generation,
            page.generation
        );
        assert!(matches!(
            store.board_page(None, true, 101, 0, &call),
            Err(RunError::Validation("board page bounds"))
        ));
        let history = store.record(AttemptId::from_bytes([1; 16]), &call)?;
        store.write(&call, |tx| {
            for n in 1000..11_000u128 {
                let mut record = history.clone();
                record.attempt_id = AttemptId::from_bytes(n.to_le_bytes());
                record.attempt_number = n as u64;
                tx.execute("INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES(?1,?2,?3,?4,?5,?6,1,?7)",
                    params![record.attempt_id.to_string(), record.run_id.to_string(), record.project_id.to_string(), record.task_id.to_string(), n as i64, record.revision as i64, crate::db::bytes(&record)?])?;
            }
            Ok(())
        })?;
        let mut samples = Vec::new();
        for _ in 0..20 {
            let start = std::time::Instant::now();
            let page = reopened.board_page(None, true, 100, 0, &call)?;
            assert_eq!(page.lanes.len(), 100);
            let _ = serde_json::to_vec(&page.lanes)?;
            samples.push(start.elapsed());
        }
        samples.sort();
        eprintln!("board_paging 10k-history 100-row p95: {:?}", samples[18]);
        assert!(samples[18] <= std::time::Duration::from_secs(1));
        Ok(())
    }
}
