//! Local board metadata read in one SQLite snapshot, without artifact I/O.
use crate::*;
use provenant_core::ProjectId;
use rusqlite::params;

#[derive(Debug, serde::Serialize)]
pub struct BoardLane {
    pub attempt: AttemptRecord,
    /// The operation that admitted the attempt.
    pub operation: OperationRecord,
}

#[derive(Debug)]
pub struct BoardPage {
    pub generation: String,
    pub high_water: u64,
    pub lanes: Vec<BoardLane>,
    pub next: Option<u64>,
}

impl RunStore {
    /// Monotonic event generation for every board-visible run mutation.
    /// Read directly from the owner store so transitions are observable even
    /// when a best-effort change-file hint was skipped.
    pub fn board_generation(&self, call: &Request<'_>) -> Result<u64> {
        call.check()?;
        let event: i64 =
            self.db
                .query_row("SELECT COALESCE(MAX(sequence),0) FROM events", [], |row| {
                    row.get(0)
                })?;
        u64::try_from(event).map_err(|_| RunError::Validation("board generation"))
    }

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
        call.check()?;
        if !(1..=100).contains(&limit) {
            return Err(RunError::Validation("board page bounds"));
        }
        let after = crate::db::sql_int(after)?;
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
            "SELECT a.sequence,a.id,a.project,a.operation_record,a.record,a.operation,\
             a.number,a.revision,a.terminal FROM attempts a \
             WHERE a.refused=0 AND (?1 IS NULL OR a.project=?1) AND (?2 OR a.terminal=0) \
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
            let attempt: AttemptRecord = crate::db::decode_attempt(&row.get::<_, Vec<u8>>(4)?)?;
            let operation: OperationRecord = crate::db::decode(&row.get::<_, Vec<u8>>(3)?)?;
            if attempt.attempt_id.to_string() != row.get::<_, String>(1)?
                || attempt.project_id.to_string() != row.get::<_, String>(2)?
                || operation.project_id != attempt.project_id
                || operation.operation_id.as_str() != row.get::<_, String>(5)?
                || crate::db::sql_int(attempt.attempt_number)? != row.get::<_, i64>(6)?
                || crate::db::sql_int(attempt.revision)? != row.get::<_, i64>(7)?
                || matches!(attempt.state, AttemptState::Terminal { .. })
                    != row.get::<_, bool>(8)?
            {
                return Err(RunError::Validation("board index identity"));
            }
            crate::reader::validate_attempt(&attempt)?;
            last = u64::try_from(row.get::<_, i64>(0)?)
                .map_err(|_| RunError::Validation("board sequence"))?;
            lanes.push(BoardLane { attempt, operation });
        }
        Ok(BoardPage {
            generation,
            high_water: high_water as u64,
            lanes,
            next: more.then_some(last),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::{AttemptId, CancelToken};

    #[test]
    fn console_changes_after_creation_transition_and_terminal_commit()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let state = context.state_root.as_path();
        assert!(provenant_os::changes::read(state)?.board.is_none());
        let input = input();
        store.create(&input, &call)?;
        let created = provenant_os::changes::read(state)?;
        assert!(created.board.is_some());
        let generation = store.board_generation(&call)?;
        let queued = store.record(input.attempt_id, &call)?;
        let attempt = store.mark_starting(
            &queued,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        assert!(matches!(attempt.state, AttemptState::Starting { .. }));
        let starting = store.board_generation(&call)?;
        assert!(starting > generation);
        store.withdraw(
            attempt.attempt_id,
            attempt.state,
            Outcome::Cancelled,
            "test",
            input.created_at,
            &call,
        )?;
        let terminal = provenant_os::changes::read(state)?;
        assert_eq!(created.epoch, terminal.epoch);
        assert!(terminal.board > created.board);
        assert!(store.board_generation(&call)? > starting);
        assert!(
            store
                .withdraw(
                    input.attempt_id,
                    AttemptState::Queued {
                        extra: Default::default()
                    },
                    Outcome::Cancelled,
                    "test",
                    input.created_at,
                    &call
                )?
                .is_none()
        );
        assert_eq!(provenant_os::changes::read(state)?.board, terminal.board);
        Ok(())
    }

    #[test]
    fn board_paging_snapshot_and_metadata() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        for n in 1u8..=5 {
            let mut input = input();
            input.attempt_id = AttemptId::from_bytes([n; 16]);
            input.operation_id = provenant_core::OperationId::parse(&format!("board-{n}"))?;
            input.attempt_id = AttemptId::from_bytes([n; 16]);
            crate::tests::rekey(&mut input);
            input.request.task.title = Some(format!("Task {n}"));
            store.create(&input, &call)?;
        }
        let page = store.board_page(None, true, 2, 0, None, &call)?;
        let expected: std::collections::HashSet<_> =
            (1u8..=5).map(|n| AttemptId::from_bytes([n; 16])).collect();
        let mut seen = std::collections::HashSet::new();
        let mut current = page;
        let generation = current.generation.clone();
        let high_water = current.high_water;
        let mut previous = None;
        loop {
            for lane in &current.lanes {
                assert!(seen.insert(lane.attempt.attempt_id), "duplicate attempt");
                assert_eq!(
                    lane.operation.operation_id.as_str(),
                    format!("board-{}", lane.attempt.attempt_id.as_bytes()[0])
                );
                assert_eq!(lane.attempt.usage, UsageRecord::default());
                assert_eq!(
                    lane.attempt.title,
                    store.record(lane.attempt.attempt_id, &call)?.title
                );
            }
            assert_eq!(current.generation, generation);
            let Some(next) = current.next else { break };
            assert!(next > 0 && previous.is_none_or(|prior| next < prior));
            previous = Some(next);
            current = store.board_page(None, true, 2, next, Some(high_water), &call)?;
        }
        assert_eq!(seen, expected);
        let mut later = input();
        later.attempt_id = AttemptId::from_bytes([6; 16]);
        later.operation_id = provenant_core::OperationId::parse("board-6")?;
        later.attempt_id = AttemptId::from_bytes([6; 16]);
        crate::tests::rekey(&mut later);
        store.create(&later, &call)?;
        let snapshot = store.board_page(None, true, 100, 0, Some(high_water), &call)?;
        assert_eq!(
            snapshot
                .lanes
                .iter()
                .map(|lane| lane.attempt.attempt_id)
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        let reopened = RunStore::open_read_only(&context, &call)?.ok_or("missing store")?;
        assert_eq!(
            reopened
                .board_page(None, true, 100, 0, None, &call)?
                .generation,
            snapshot.generation
        );
        let old = store.record(AttemptId::from_bytes([5; 16]), &call)?;
        let mut mixed = old.clone();
        mixed.usage.uncached_input = Some(0);
        mixed.usage.reasoning = Some(7);
        mixed.revision += 1;
        store.write(&call, |tx| {
            crate::admission::update_attempt(tx, &old, &mixed)
        })?;
        let updated = store.board_page(None, true, 100, 0, None, &call)?;
        assert_eq!(
            updated
                .lanes
                .iter()
                .find(|lane| lane.attempt.attempt_id == mixed.attempt_id)
                .ok_or("missing updated attempt")?
                .attempt
                .usage,
            mixed.usage
        );
        assert_ne!(updated.generation, snapshot.generation);
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
        let active = store.board_page(None, false, 100, 0, None, &call)?;
        assert!(
            !active
                .lanes
                .iter()
                .any(|lane| lane.attempt.attempt_id == AttemptId::from_bytes([1; 16]))
        );
        let all = store.board_page(None, true, 100, 0, None, &call)?;
        assert!(
            all.lanes
                .iter()
                .any(|lane| lane.attempt.attempt_id == AttemptId::from_bytes([1; 16]))
        );
        Ok(())
    }
}
