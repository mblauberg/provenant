//! Local board metadata read in one SQLite snapshot, without artifact I/O.
use crate::{db::decode, *};
use provenant_core::ProjectId;
use rusqlite::params;

#[derive(Debug, serde::Serialize)]
pub struct BoardLane {
    pub attempt: AttemptRecord,
    pub run: RunRecord,
}

#[derive(Debug)]
pub struct BoardPage {
    pub generation: String,
    pub high_water: u64,
    pub lanes: Vec<BoardLane>,
    pub next: Option<u64>,
}

impl RunStore {
    /// Cross-project host-store read. No child-store traversal or provider I/O.
    /// Each page observes current facts below a fixed attempt high-water mark.
    pub fn board_page(
        &self,
        project: Option<ProjectId>,
        all: bool,
        limit: u64,
        after: u64,
        through: Option<u64>,
        call: &Request<'_>,
    ) -> Result<BoardPage> {
        if !(1..=100).contains(&limit) {
            return Err(RunError::Validation("board page bounds"));
        }
        let after = crate::db::sql_int(after)?;
        call.retry(|| {
            let tx = self.db.unchecked_transaction()?;
            let project = project.map(|id| id.to_string());
            // All board-visible attempt/run writes append an event atomically.
            // Future run-only writes must do the same. GC retains the latest
            // event per attempt. Both MAX queries use primary-key end lookups.
            let (event, current): (i64, i64) = tx.query_row(
                "SELECT (SELECT COALESCE(MAX(sequence),0) FROM events), \
                        (SELECT COALESCE(MAX(sequence),0) FROM attempts)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let high_water = through
                .map(crate::db::sql_int)
                .transpose()?
                .unwrap_or(current);
            if high_water > current || after > high_water {
                return Err(RunError::Validation("board high-water mark"));
            }
            let generation = format!("events:{event}");
            let mut statement = tx.prepare(
                "SELECT a.sequence,a.id,a.project,a.run,a.task,a.record,r.record,\
                 a.number,a.revision,a.terminal FROM attempts a \
                 JOIN runs r ON r.id=a.run AND r.project=a.project \
                 WHERE (?1 IS NULL OR a.project=?1) AND (?2 OR a.terminal=0) \
                 AND (?3=0 OR a.sequence<?3) AND a.sequence<=?5 ORDER BY a.sequence DESC LIMIT ?4",
            )?;
            let mut rows =
                statement.query(params![project, all, after, (limit + 1) as i64, high_water])?;
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
                    || crate::db::sql_int(attempt.attempt_number)? != row.get::<_, i64>(7)?
                    || crate::db::sql_int(attempt.revision)? != row.get::<_, i64>(8)?
                    || matches!(attempt.state, AttemptState::Terminal { .. })
                        != row.get::<_, bool>(9)?
                    || run.run_id != attempt.run_id
                    || run.project_id != attempt.project_id
                    || run.operation_id != attempt.operation_id
                    || !run.task_ids.contains(&attempt.task_id)
                {
                    return Err(RunError::Validation("board index identity"));
                }
                crate::reader::validate_attempt(&attempt)?;
                last = u64::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| RunError::Validation("board sequence"))?;
                lanes.push(BoardLane { attempt, run });
            }
            Ok(BoardPage {
                generation,
                high_water: high_water as u64,
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
        let page = store.board_page(None, true, 100, 0, None, &call)?;
        assert_eq!(page.lanes.len(), 100);
        let next = page.next.ok_or("missing cursor")?;
        let last = store.board_page(None, true, 100, next, Some(page.high_water), &call)?;
        assert_eq!(last.lanes.len(), 1);
        assert!(last.next.is_none());
        assert_eq!(page.generation, last.generation);
        assert_eq!(page.lanes[0].attempt.title.as_deref(), Some("Task 101"));
        assert_eq!(page.lanes[0].attempt.usage, UsageRecord::default());
        assert_eq!(page.lanes[0].run.run_id, page.lanes[0].attempt.run_id);
        let reopened = RunStore::open_read_only(&context, &call)?.ok_or("missing store")?;
        assert_eq!(
            reopened
                .board_page(None, true, 100, 0, None, &call)?
                .generation,
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
        let updated = store.board_page(None, true, 100, 0, None, &call)?;
        assert_eq!(updated.lanes[0].attempt.usage, mixed.usage);
        assert_ne!(updated.generation, page.generation);
        let mut corrupt = mixed.clone();
        corrupt.usage.context_basis_points = Some(10_001);
        for (record, revision) in [(&corrupt, mixed.revision), (&mixed, mixed.revision + 1)] {
            store.db.execute(
                "UPDATE attempts SET record=?1,revision=?2 WHERE id=?3",
                params![
                    serde_json::to_vec(record)?,
                    revision as i64,
                    mixed.attempt_id.to_string()
                ],
            )?;
            assert!(matches!(
                store.board_page(None, true, 100, 0, None, &call),
                Err(RunError::Validation(_))
            ));
        }
        store.db.execute(
            "UPDATE attempts SET revision=?1 WHERE id=?2",
            params![mixed.revision as i64, mixed.attempt_id.to_string()],
        )?;
        store.withdraw(
            AttemptId::from_bytes([1; 16]),
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "test",
            call.clock.wall_now()?,
            &call,
        )?;
        assert!(matches!(
            store.board_page(None, true, 101, 0, None, &call),
            Err(RunError::Validation("board page bounds"))
        ));
        let history = store.record(AttemptId::from_bytes([1; 16]), &call)?;
        let mut history = history;
        history.attempt_id = AttemptId::from_bytes([0xee; 16]);
        history.trace.attempt = Some(history.attempt_id);
        history.prompt.path = crate::artifacts::artifact_path(history.attempt_id, "prompt.txt")?;
        if let Some(result) = &mut history.result {
            result.path = crate::artifacts::artifact_path(history.attempt_id, "result.md")?;
        }
        let event = RunEvent {
            extra: Default::default(),
            schema_version: 1,
            sequence: 1,
            project_id: history.project_id,
            run_id: history.run_id,
            attempt_id: history.attempt_id,
            revision: history.revision,
            change: history.state.clone(),
            trace: history.trace.clone(),
        };
        let old_id = history.attempt_id.to_string();
        let record = String::from_utf8(serde_json::to_vec(&history)?)?;
        let event_record = String::from_utf8(serde_json::to_vec(&event)?)?;
        store.write(&call, |tx| {
            tx.execute(
                "WITH RECURSIVE history(n) AS (VALUES(1000) UNION ALL SELECT n+1 FROM history WHERE n<100999) \
                 INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) \
                 SELECT printf('%032x',n),?1,?2,?3,n,?4,1, \
                 CAST(json_set(replace(?5,?6,printf('%032x',n)),'$.attempt_number',n) AS BLOB) FROM history",
                params![history.run_id.to_string(), history.project_id.to_string(), history.task_id.to_string(), history.revision as i64, record, old_id],
            )?;
            tx.execute(
                "WITH RECURSIVE history(n) AS (VALUES(1000) UNION ALL SELECT n+1 FROM history WHERE n<100999) \
                 INSERT INTO events(sequence,project,attempt,revision,record) \
                 SELECT n+(SELECT MAX(sequence) FROM events),?1,printf('%032x',n),?2, \
                 CAST(replace(?3,?4,printf('%032x',n)) AS BLOB) FROM history",
                params![history.project_id.to_string(), history.revision as i64, event_record, old_id],
            )?;
            Ok(())
        })?;
        for all in [false, true] {
            let page = reopened.board_page(None, all, 100, 0, None, &call)?;
            assert_eq!(page.lanes.len(), 100);
            assert_eq!(page.next.is_some(), all);
        }
        Ok(())
    }
}
