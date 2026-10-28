//! Host-owned membership facts. Lane stores are never lookup authority.
use crate::{
    db::{decode, immediate, open_database},
    *,
};
use provenant_core::{AttemptId, GitOid, ProjectId, RunId, TaskId};
use provenant_records::AbsolutePath;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
pub const WORK_INDEX_FILE: &str = "work-index.sqlite3";
const ROW_LIMIT: usize = 64;
const MEMBER_BYTES: usize = 64 * 1024;
/// Repository is the verified canonical Git common directory. Head is the
/// host-observed launch HEAD. No selector accepts a nested store locator.
#[derive(Debug, Clone)]
pub struct WorkSelector {
    pub project: ProjectId,
    pub run: RunId,
    pub task: Option<TaskId>,
    pub attempt: Option<AttemptId>,
    pub repository: Option<AbsolutePath>,
    pub head: Option<GitOid>,
}
pub use provenant_records::inbox::WorkCoverage;

/// Validate only against the configured host state root. No caller store locator.
pub fn validate_work(
    host: &provenant_core::StateRoot,
    selection: &WorkSelector,
    call: &Request<'_>,
) -> Result<WorkCoverage> {
    WorkIndex::open(host.as_path(), call)?.validate(selection, call)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Membership {
    schema_version: u64,
    project: ProjectId,
    run: RunId,
    task: TaskId,
    attempt: AttemptId,
    repository: Option<AbsolutePath>,
    head: Option<GitOid>,
}
/// Open only at the trusted host root. Consumers cannot insert membership.
/// Reads perform no nested traversal, Git probes or provider I/O.
/// ```compile_fail
/// use provenant_run::{WorkIndex, Request};
/// fn untrusted(path: &std::path::Path, call: &Request<'_>) {
///     let _ = WorkIndex::open(path, call);
/// }
/// ```
pub struct WorkIndex(Connection);
impl WorkIndex {
    pub(crate) fn open(host: &Path, call: &Request<'_>) -> Result<Self> {
        let (_, db, _lock) = open_database(host, WORK_INDEX_FILE, call)?;
        let version = call.retry(|| match provenant_sqlite::read_version(&db, 1) {
            Ok(version) => Ok(version),
            Err(provenant_sqlite::VersionError::Unsupported(version)) => {
                Err(RunError::Version(version))
            }
            Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
            Err(provenant_sqlite::VersionError::Sqlite(error)) => Err(error.into()),
        })?;
        match version {
            0 => immediate(&db, call, |tx| {
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS membership (
                    project TEXT NOT NULL, run TEXT NOT NULL, task TEXT NOT NULL,
                    attempt TEXT PRIMARY KEY, record BLOB NOT NULL
                ) STRICT;
                CREATE INDEX IF NOT EXISTS work_runs ON membership(project,run,task,attempt);
                PRAGMA user_version=1;",
                )?;
                Ok(())
            })?,
            1 => {}
            other => return Err(RunError::Version(other as i64)),
        }
        Ok(Self(db))
    }
    pub fn validate(&self, selection: &WorkSelector, call: &Request<'_>) -> Result<WorkCoverage> {
        call.retry(|| {
            // Exact predicates let SQLite use the attempt or project/run/task
            // index even when a selector is absent or names unknown work.
            let task = if selection.task.is_some() {
                "task=?3"
            } else {
                "?3 IS NULL"
            };
            let attempt = if selection.attempt.is_some() {
                "attempt=?4"
            } else {
                "?4 IS NULL"
            };
            let mut query = self.0.prepare(&format!(
                "SELECT substr(record,1,{read_bytes}) FROM membership
                WHERE project=?1 AND run=?2 AND {task} AND {attempt}
                ORDER BY task,attempt LIMIT {row_limit}",
                read_bytes = MEMBER_BYTES + 1,
                row_limit = ROW_LIMIT + 1
            ))?;
            let mut rows = query.query(params![
                selection.project.to_string(),
                selection.run.to_string(),
                selection.task.map(|id| id.to_string()),
                selection.attempt.map(|id| id.to_string())
            ])?;
            let mut coverage = WorkCoverage::Unindexed;
            let qualified = selection.task.is_some() || selection.attempt.is_some();
            for n in 0..=ROW_LIMIT {
                let Some(row) = rows.next()? else {
                    if !qualified {
                        return Ok(WorkCoverage::Partial);
                    }
                    return Ok(coverage);
                };
                if n == ROW_LIMIT {
                    return Ok(WorkCoverage::Limit);
                }
                let raw: Vec<u8> = row.get(0)?;
                if raw.len() > MEMBER_BYTES {
                    return Err(RunError::Validation("work membership size"));
                }
                let value: Membership = decode(&raw)?;
                if value.schema_version != 1
                    || value.project != selection.project
                    || value.run != selection.run
                    || selection.task.is_some_and(|task| task != value.task)
                    || selection
                        .attempt
                        .is_some_and(|attempt| attempt != value.attempt)
                {
                    return Err(RunError::Validation("work membership identity"));
                }
                if selection
                    .repository
                    .as_ref()
                    .is_some_and(|repo| value.repository.as_ref().is_some_and(|v| v != repo))
                    || selection
                        .head
                        .is_some_and(|head| value.head.is_some_and(|v| v != head))
                {
                    coverage = WorkCoverage::Mismatch;
                    continue;
                }
                if selection.repository.is_some() && value.repository.is_none() {
                    coverage = coverage.max(WorkCoverage::RepositoryUnavailable);
                } else if selection.head.is_some() && value.head.is_none() {
                    coverage = coverage.max(WorkCoverage::HeadUnavailable);
                } else {
                    coverage = coverage.max(WorkCoverage::Verified);
                }
            }
            Ok(coverage)
        })
    }
    fn seal(&self, value: &Membership, call: &Request<'_>) -> Result<()> {
        let canonical = provenant_records::CanonicalV1::bytes(value)?;
        let raw = canonical.as_bytes();
        if raw.len() > MEMBER_BYTES {
            return Err(RunError::TooLarge {
                limit: MEMBER_BYTES as u64,
            });
        }
        immediate(&self.0, call, |tx| {
            let previous: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT record FROM membership WHERE attempt=?1",
                    [value.attempt.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(raw) = previous {
                return if decode::<Membership>(&raw)? == *value {
                    Ok(())
                } else {
                    Err(RunError::Conflict)
                };
            }
            tx.execute(
                "INSERT INTO membership(project,run,task,attempt,record) VALUES(?1,?2,?3,?4,?5)",
                params![
                    value.project.to_string(),
                    value.run.to_string(),
                    value.task.to_string(),
                    value.attempt.to_string(),
                    raw
                ],
            )?;
            Ok(())
        })
    }
}
impl RunStore {
    pub(crate) fn seal_work(
        &self,
        record: &AttemptRecord,
        plan: &PreparedLaunch,
        call: &Request<'_>,
    ) -> Result<()> {
        if self.sealed_requester.is_some()
            || self.context.state_root.as_path() != self.writer_registry()
        {
            return Ok(());
        }
        WorkIndex::open(self.writer_registry(), call)?.seal(
            &Membership {
                schema_version: 1,
                project: self.context.project_id,
                run: record.run_id,
                task: record.task_id,
                attempt: record.attempt_id,
                repository: plan.repository.clone(),
                head: plan.observed_head,
            },
            call,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::CancelToken;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
    type Fixture = (
        tempfile::TempDir,
        ExecutionContext,
        provenant_core::TestClock,
        CancelToken,
    );
    fn fixture() -> std::result::Result<Fixture, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        Ok((temp, context, clock(), CancelToken::new()))
    }

    #[test]
    fn request_correlation_lookup() -> TestResult {
        let (_temp, context, clock, cancel) = fixture()?;
        let call = call(&clock, &cancel);
        let index = WorkIndex::open(context.state_root.as_path(), &call)?;
        let input = input();
        let value = Membership {
            schema_version: 1,
            project: context.project_id,
            run: input.run_id,
            task: input.attempts[0].task_id,
            attempt: input.attempts[0].attempt_id,
            repository: Some(AbsolutePath::new(Path::new("/repo/.git"))?),
            head: Some(GitOid::parse(&"a".repeat(40))?),
        };
        let mut selection = WorkSelector {
            project: value.project,
            run: value.run,
            task: Some(value.task),
            attempt: Some(value.attempt),
            repository: value.repository.clone(),
            head: value.head,
        };
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Unindexed);
        let mut run_only = selection.clone();
        run_only.task = None;
        run_only.attempt = None;
        assert_eq!(index.validate(&run_only, &call)?, WorkCoverage::Partial);
        index.seal(&value, &call)?;
        index.seal(&value, &call)?;
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Verified);
        selection.head = Some(GitOid::parse(&"b".repeat(40))?);
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Mismatch);
        selection.head = value.head;
        selection.repository = Some(AbsolutePath::new(Path::new("/other/.git"))?);
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Mismatch);
        selection.repository = value.repository.clone();
        for field in 0..4 {
            let mut foreign = selection.clone();
            match field {
                0 => foreign.project = ProjectId::from_bytes([99; 16]),
                1 => foreign.run = RunId::from_bytes([99; 16]),
                2 => foreign.task = Some(TaskId::from_bytes([99; 16])),
                _ => foreign.attempt = Some(AttemptId::from_bytes([99; 16])),
            }
            assert_eq!(index.validate(&foreign, &call)?, WorkCoverage::Unindexed);
        }
        let mut forged = value.clone();
        forged.task = TaskId::from_bytes([99; 16]);
        assert!(matches!(
            index.seal(&forged, &call),
            Err(RunError::Conflict)
        ));
        drop(index);
        let index = WorkIndex::open(context.state_root.as_path(), &call)?;
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Verified);
        assert_eq!(index.validate(&run_only, &call)?, WorkCoverage::Partial);
        Ok(())
    }

    #[test]
    fn request_correlation_missing_facts_and_bounds() -> TestResult {
        let (_temp, context, clock, cancel) = fixture()?;
        let call = call(&clock, &cancel);
        let index = WorkIndex::open(context.state_root.as_path(), &call)?;
        let input = input();
        let mut value = Membership {
            schema_version: 1,
            project: context.project_id,
            run: input.run_id,
            task: input.attempts[0].task_id,
            attempt: input.attempts[0].attempt_id,
            repository: None,
            head: None,
        };
        index.seal(&value, &call)?;
        let mut selection = WorkSelector {
            project: value.project,
            run: value.run,
            task: Some(value.task),
            attempt: None,
            repository: Some(AbsolutePath::new(Path::new("/repo/.git"))?),
            head: None,
        };
        assert_eq!(
            index.validate(&selection, &call)?,
            WorkCoverage::RepositoryUnavailable
        );
        let contradictory = Membership {
            attempt: AttemptId::from_bytes([99; 16]),
            repository: Some(AbsolutePath::new(Path::new("/other/.git"))?),
            ..value.clone()
        };
        index.seal(&contradictory, &call)?;
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Mismatch);
        index.0.execute(
            "DELETE FROM membership WHERE attempt=?1",
            [contradictory.attempt.to_string()],
        )?;
        selection.repository = None;
        selection.head = Some(GitOid::parse(&"a".repeat(40))?);
        assert_eq!(
            index.validate(&selection, &call)?,
            WorkCoverage::HeadUnavailable
        );
        for n in 1..=64_u8 {
            value.attempt = AttemptId::from_bytes([n; 16]);
            if value.attempt == input.attempts[0].attempt_id {
                continue;
            }
            index.seal(&value, &call)?;
        }
        value.attempt = AttemptId::from_bytes([65; 16]);
        index.seal(&value, &call)?;
        selection.task = None;
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Limit);
        selection.attempt = Some(value.attempt);
        selection.head = None;
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Verified);
        value.repository = Some(AbsolutePath::new(Path::new(&format!(
            "/{}",
            "x".repeat(66000)
        )))?);
        assert!(matches!(
            index.seal(&value, &call),
            Err(RunError::TooLarge { limit: 65536 })
        ));
        cancel.cancel();
        assert!(matches!(
            index.validate(&selection, &call),
            Err(RunError::Cancelled)
        ));
        Ok(())
    }

    #[test]
    fn request_correlation_rewritten_child_cannot_poison_sibling() -> TestResult {
        let temp = tempfile::tempdir()?;
        let host = context(temp.path())?;
        let lane_dir = temp.path().join("lane");
        std::fs::create_dir(&lane_dir)?;
        let lane = context(&lane_dir)?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let sibling = input();
        let root = RunStore::open(&host, &call)?;
        root.create(&sibling, &call)?;
        let store = RunStore::open(&lane, &call)?
            .with_writer_registry(host.state_root.as_path())
            .with_sealed_requester(host.actor_id);
        let mut nested = input();
        nested.run_id = RunId::from_bytes([8; 16]);
        nested.attempts[0].attempt_id = AttemptId::from_bytes([7; 16]);
        nested.request.lane_depth = Some(2);
        let parent = AttemptId::from_bytes([9; 16]);
        nested.request.parent_attempt = Some(parent);
        store.create(&nested, &call)?;
        let old = nested.attempts[0].attempt_id.to_string();
        let stolen = sibling.attempts[0].attempt_id.to_string();
        store.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;
            UPDATE attempts SET id='{stolen}',record=CAST(replace(CAST(record AS TEXT),'{old}','{stolen}') AS BLOB);
            UPDATE admission SET attempt='{stolen}',record=CAST(replace(CAST(record AS TEXT),'{old}','{stolen}') AS BLOB);
            UPDATE events SET attempt='{stolen}',record=CAST(replace(CAST(record AS TEXT),'{old}','{stolen}') AS BLOB);"))?;
        let attempts = lane
            .state_root
            .as_path()
            .join("runs")
            .join(nested.run_id.to_string())
            .join("attempts");
        std::fs::rename(attempts.join(&old), attempts.join(&stolen))?;
        let validated = store.validate_child(
            nested.run_id,
            parent,
            &input().request.tasks[0],
            1,
            2,
            &call,
        )?;
        store.start_child(
            nested.run_id,
            &validated,
            &crate::dispatch_tests::Ready,
            &|_, _| Err(RunError::Validation("fixture child handoff")),
            &call,
        )?;
        let outcome = root.start(
            sibling.run_id,
            &crate::dispatch_tests::Ready,
            &|_, _| Err(RunError::Validation("fixture sibling handoff")),
            &call,
        )?;
        let index = WorkIndex::open(host.state_root.as_path(), &call)?;
        let forged = WorkSelector {
            project: host.project_id,
            run: nested.run_id,
            task: Some(nested.attempts[0].task_id),
            attempt: Some(sibling.attempts[0].attempt_id),
            repository: None,
            head: None,
        };
        let coverage = index.validate(&forged, &call)?;
        assert!(
            matches!(outcome.as_slice(), [Started::Refused {reason, ..}]
            if reason.contains("fixture sibling handoff")),
            "forged lookup={coverage:?}, sibling={outcome:?}"
        );
        assert_eq!(coverage, WorkCoverage::Unindexed);
        Ok(())
    }
    #[test]
    fn request_correlation_capture_failures_leave_work_queued() -> TestResult {
        use provenant_os::fs::{Directory, LockMode};
        let (_temp, context, clock, cancel) = fixture()?;
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let prepared = input();
        store.create(&prepared, &call)?;
        let index = WorkIndex::open(store.writer_registry(), &call)?;
        let directory = Directory::open_anchor(store.writer_registry())?;
        let lock = directory.try_lock_existing(
            std::ffi::OsStr::new("work-index.sqlite3.lock"),
            LockMode::Exclusive,
        )?;
        let nonblocking = Request {
            budget: provenant_core::WaitBudget::nonblocking(provenant_core::Clock::mono_now(
                &clock,
            )),
            ..call
        };
        assert!(matches!(
            store.start(
                prepared.run_id,
                &crate::dispatch_tests::Ready,
                &|_, _| Err(RunError::Validation("must not reach handoff")),
                &nonblocking
            ),
            Err(RunError::Contention)
        ));
        assert_eq!(
            store.record(prepared.attempts[0].attempt_id, &call)?.state,
            AttemptState::Queued
        );
        drop(lock);
        cancel.cancel();
        let result = store.start(
            prepared.run_id,
            &crate::dispatch_tests::Ready,
            &|_, _| unreachable!("cancelled call reached handoff"),
            &call,
        );
        let fresh_cancel = CancelToken::new();
        let call = Request {
            cancel: &fresh_cancel,
            ..call
        };
        assert!(matches!(result, Err(RunError::Cancelled)));
        assert_eq!(
            store.record(prepared.attempts[0].attempt_id, &call)?.state,
            AttemptState::Queued
        );
        index.0.execute_batch("CREATE TRIGGER reject_membership BEFORE INSERT ON membership BEGIN SELECT RAISE(ABORT,'fixture seal failure'); END;")?;
        let result = store.start(
            prepared.run_id,
            &crate::dispatch_tests::Ready,
            &|_, _| unreachable!("seal failure reached handoff"),
            &call,
        );
        assert!(matches!(result, Err(RunError::Sqlite(_))));
        assert_eq!(
            store.record(prepared.attempts[0].attempt_id, &call)?.state,
            AttemptState::Queued
        );
        index.0.execute_batch("DROP TRIGGER reject_membership;")?;
        let before = store.record(prepared.attempts[0].attempt_id, &call)?;
        let head = Some(GitOid::parse(&"a".repeat(40))?);
        // Launcher A has captured its observed HEAD but has not claimed Starting.
        index.seal(
            &Membership {
                schema_version: 1,
                project: context.project_id,
                run: prepared.run_id,
                task: before.task_id,
                attempt: before.attempt_id,
                repository: None,
                head,
            },
            &call,
        )?;
        // Launcher B prepared different facts (HEAD unavailable). It must not
        // withdraw the queued attempt before A can claim it.
        let second = store.start(
            prepared.run_id,
            &crate::dispatch_tests::Ready,
            &|_, _| unreachable!("conflicting capture reached handoff"),
            &call,
        );
        assert!(matches!(second, Err(RunError::Conflict)));
        assert_eq!(
            store.record(before.attempt_id, &call)?.state,
            AttemptState::Queued
        );
        let claimed = store.mark_starting(
            &before,
            &crate::tests::route(),
            &[],
            &Default::default(),
            head,
            &call,
        )?;
        assert_eq!(claimed.state, AttemptState::Starting);
        Ok(())
    }
    #[test]
    fn request_correlation_host_resumes_seal_through_start() -> TestResult {
        let (_temp, context, clock, cancel) = fixture()?;
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let original = input();
        store.create(&original, &call)?;
        let index = WorkIndex::open(store.writer_registry(), &call)?;
        let mut prior = original.attempts[0].attempt_id;
        store.start(
            original.run_id,
            &crate::dispatch_tests::Ready,
            &|_, _| Err(RunError::Validation("fixture host handoff")),
            &call,
        )?;
        for (fresh, n) in [(false, 20_u8), (true, 30)] {
            let next = Resume {
                prior,
                expected_generation: 0,
                operation_id: provenant_core::OperationId::parse(&format!("resume-{n}"))?,
                run_id: RunId::from_bytes([n; 16]),
                attempt_id: AttemptId::from_bytes([n + 1; 16]),
                prompt: "continue".into(),
                created_at: original.created_at,
            };
            let run = if fresh {
                store.resume_fresh(&next, "fixture fresh", &call)?
            } else {
                store.resume(&next, &call)?
            };
            let selector = WorkSelector {
                project: context.project_id,
                run: run.run_id,
                task: Some(original.attempts[0].task_id),
                attempt: Some(next.attempt_id),
                repository: None,
                head: None,
            };
            assert_eq!(index.validate(&selector, &call)?, WorkCoverage::Unindexed);
            let handed = std::cell::Cell::new(false);
            store.start(
                run.run_id,
                &crate::dispatch_tests::Ready,
                &|record, _| {
                    assert_eq!(record.attempt_id, next.attempt_id);
                    handed.set(true);
                    Err(RunError::Validation("fixture host handoff"))
                },
                &call,
            )?;
            assert!(handed.get());
            assert_eq!(index.validate(&selector, &call)?, WorkCoverage::Verified);
            prior = next.attempt_id;
        }
        Ok(())
    }
}
