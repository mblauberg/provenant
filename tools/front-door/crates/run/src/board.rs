//! Local board metadata read in one SQLite snapshot, without artifact I/O.
use crate::*;
use provenant_core::ProjectId;
use rusqlite::params;

/// Upper bound for one run-board page.
pub const MAX_BOARD_PAGE_SIZE: u64 = 100;

#[derive(Debug, serde::Serialize)]
pub struct BoardLane {
    #[serde(serialize_with = "provenant_records::run::serialise_public_attempt")]
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
        if !(1..=MAX_BOARD_PAGE_SIZE).contains(&limit) {
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

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    struct Fixture {
        _temp: tempfile::TempDir,
        context: ExecutionContext,
        clock: provenant_core::TestClock,
        cancel: CancelToken,
        store: RunStore,
    }

    impl Fixture {
        fn call(&self) -> Request<'_> {
            call(&self.clock, &self.cancel)
        }

        fn add(&self, number: u8) -> TestResult<AttemptId> {
            let mut run = input();
            run.attempt_id = AttemptId::from_bytes([number; 16]);
            run.operation_id = provenant_core::OperationId::parse(&format!("board-{number}"))?;
            crate::tests::rekey(&mut run);
            run.request.task.title = Some(format!("Task {number}"));
            self.store.create(&run, &self.call())?;
            Ok(run.attempt_id)
        }
    }

    fn fixture() -> TestResult<Fixture> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let store = RunStore::open(&context, &call(&clock, &cancel))?;
        Ok(Fixture {
            _temp: temp,
            context,
            clock,
            cancel,
            store,
        })
    }

    #[test]
    fn board_pages_cover_the_high_water_snapshot() -> TestResult {
        let fixture = fixture()?;
        let expected: std::collections::HashSet<_> = (1u8..=5)
            .map(|number| fixture.add(number))
            .collect::<TestResult<_>>()?;
        let call = fixture.call();
        let mut current = fixture.store.board_page(None, true, 2, 0, None, &call)?;
        let generation = current.generation.clone();
        let high_water = current.high_water;
        let mut seen = std::collections::HashSet::new();
        let mut previous = None;
        loop {
            for lane in &current.lanes {
                assert!(seen.insert(lane.attempt.attempt_id));
                assert_eq!(
                    lane.operation.operation_id.as_str(),
                    format!("board-{}", lane.attempt.attempt_id.as_bytes()[0])
                );
            }
            assert_eq!(current.generation, generation);
            let Some(next) = current.next else { break };
            assert!(next > 0 && previous.is_none_or(|prior| next < prior));
            previous = Some(next);
            current = fixture
                .store
                .board_page(None, true, 2, next, Some(high_water), &call)?;
        }
        assert_eq!(seen, expected);
        let later = fixture.add(6)?;
        assert!(!expected.contains(&later));
        let snapshot = fixture.store.board_page(
            None,
            true,
            MAX_BOARD_PAGE_SIZE,
            0,
            Some(high_water),
            &call,
        )?;
        assert_eq!(
            snapshot
                .lanes
                .iter()
                .map(|lane| lane.attempt.attempt_id)
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        Ok(())
    }

    #[test]
    fn board_metadata_tracks_attempt_updates_in_read_only_views() -> TestResult {
        let fixture = fixture()?;
        let id = fixture.add(5)?;
        let call = fixture.call();
        let before = fixture
            .store
            .board_page(None, true, MAX_BOARD_PAGE_SIZE, 0, None, &call)?;
        let reopened = RunStore::open_read_only(&fixture.context, &call)?.ok_or("missing store")?;
        assert_eq!(
            reopened
                .board_page(None, true, MAX_BOARD_PAGE_SIZE, 0, None, &call)?
                .generation,
            before.generation
        );
        let old = fixture.store.record(id, &call)?;
        let mut updated = old.clone();
        updated.usage.uncached_input = Some(0);
        updated.usage.reasoning = Some(7);
        updated.revision += 1;
        fixture.store.write(&call, |tx| {
            crate::admission::update_attempt(tx, &old, &updated)
        })?;
        let after = fixture
            .store
            .board_page(None, true, MAX_BOARD_PAGE_SIZE, 0, None, &call)?;
        let row = after
            .lanes
            .iter()
            .find(|lane| lane.attempt.attempt_id == id)
            .ok_or("updated attempt missing")?;
        assert_eq!(row.attempt.usage, updated.usage);
        assert_ne!(after.generation, before.generation);
        Ok(())
    }

    #[test]
    fn board_rejects_invalid_records_and_revisions() -> TestResult {
        let fixture = fixture()?;
        let id = fixture.add(5)?;
        let call = fixture.call();
        let record = fixture.store.record(id, &call)?;
        let mut corrupt = record.clone();
        corrupt.usage.context_basis_points = Some(10_001);
        fixture.store.db.execute(
            "UPDATE attempts SET record=?1,revision=?2 WHERE id=?3",
            params![
                serde_json::to_vec(&corrupt)?,
                record.revision as i64,
                id.to_string()
            ],
        )?;
        assert!(matches!(
            fixture
                .store
                .board_page(None, true, MAX_BOARD_PAGE_SIZE, 0, None, &call),
            Err(RunError::Validation(_))
        ));
        fixture.store.db.execute(
            "UPDATE attempts SET record=?1,revision=?2 WHERE id=?3",
            params![
                serde_json::to_vec(&record)?,
                (record.revision + 1) as i64,
                id.to_string()
            ],
        )?;
        assert!(matches!(
            fixture
                .store
                .board_page(None, true, MAX_BOARD_PAGE_SIZE, 0, None, &call),
            Err(RunError::Validation(_))
        ));
        Ok(())
    }

    #[test]
    fn board_page_limit_is_declared_and_terminal_filter_is_applied() -> TestResult {
        let fixture = fixture()?;
        let id = fixture.add(1)?;
        let call = fixture.call();
        for limit in [0, MAX_BOARD_PAGE_SIZE + 1] {
            assert!(matches!(
                fixture.store.board_page(None, true, limit, 0, None, &call),
                Err(RunError::Validation(_))
            ));
        }
        for limit in [1, MAX_BOARD_PAGE_SIZE] {
            fixture
                .store
                .board_page(None, true, limit, 0, None, &call)?;
        }
        let row = fixture.store.record(id, &call)?;
        fixture.store.withdraw(
            id,
            row.state,
            Outcome::Cancelled,
            "test",
            call.clock.wall_now()?,
            &call,
        )?;
        let active = fixture
            .store
            .board_page(None, false, MAX_BOARD_PAGE_SIZE, 0, None, &call)?;
        assert!(
            !active
                .lanes
                .iter()
                .any(|lane| lane.attempt.attempt_id == id)
        );
        let all = fixture
            .store
            .board_page(None, true, MAX_BOARD_PAGE_SIZE, 0, None, &call)?;
        assert!(all.lanes.iter().any(|lane| lane.attempt.attempt_id == id));
        Ok(())
    }
}
