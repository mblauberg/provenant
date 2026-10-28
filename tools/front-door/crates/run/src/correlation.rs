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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkCoverage {
    Verified,
    /// Includes unknown, historical and not yet prepared work.
    Unindexed,
    RepositoryUnavailable,
    HeadUnavailable,
    Mismatch,
    /// More candidates remain. Absence has not been established.
    Limit,
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
pub struct WorkIndex(Connection);
impl WorkIndex {
    pub fn open(host: &Path, call: &Request<'_>) -> Result<Self> {
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
                "SELECT substr(record,1,65537) FROM membership
                WHERE project=?1 AND run=?2 AND {task} AND {attempt}
                ORDER BY task,attempt LIMIT 65"
            ))?;
            let mut rows = query.query(params![
                selection.project.to_string(),
                selection.run.to_string(),
                selection.task.map(|id| id.to_string()),
                selection.attempt.map(|id| id.to_string())
            ])?;
            let mut coverage = WorkCoverage::Unindexed;
            for n in 0..=ROW_LIMIT {
                let Some(row) = rows.next()? else {
                    return Ok(coverage);
                };
                if n == ROW_LIMIT {
                    return Ok(WorkCoverage::Limit);
                }
                let raw: Vec<u8> = row.get(0)?;
                if raw.len() > 65536 {
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
                    if coverage == WorkCoverage::Unindexed {
                        coverage = WorkCoverage::Mismatch;
                    }
                    continue;
                }
                if selection.repository.is_some() && value.repository.is_none() {
                    coverage = WorkCoverage::RepositoryUnavailable;
                } else if selection.head.is_some() && value.head.is_none() {
                    coverage = WorkCoverage::HeadUnavailable;
                } else {
                    return Ok(WorkCoverage::Verified);
                }
            }
            Ok(coverage)
        })
    }
    // Only Run's trusted host launch path publishes immutable membership.
    fn seal(&self, value: &Membership, call: &Request<'_>) -> Result<()> {
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
                    crate::db::bytes(value)?
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

    #[test]
    fn request_correlation_lookup() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
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
        Ok(())
    }

    #[test]
    fn request_correlation_missing_facts_and_bounds() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
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
            task: None,
            attempt: None,
            repository: Some(AbsolutePath::new(Path::new("/repo/.git"))?),
            head: None,
        };
        assert_eq!(
            index.validate(&selection, &call)?,
            WorkCoverage::RepositoryUnavailable
        );
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
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Limit);
        selection.attempt = Some(value.attempt);
        selection.head = None;
        assert_eq!(index.validate(&selection, &call)?, WorkCoverage::Verified);
        cancel.cancel();
        assert!(matches!(
            index.validate(&selection, &call),
            Err(RunError::Cancelled)
        ));
        Ok(())
    }
    #[test]
    fn request_correlation_nested_host_authority() -> TestResult {
        let temp = tempfile::tempdir()?;
        let host = context(temp.path())?;
        let lane_dir = temp.path().join("lane");
        std::fs::create_dir(&lane_dir)?;
        let lane = context(&lane_dir)?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&lane, &call)?
            .with_writer_registry(host.state_root.as_path())
            .with_sealed_requester(host.actor_id);
        let mut nested = input();
        let parent = AttemptId::from_bytes([9; 16]);
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        store.create(&nested, &call)?;
        let validated = store.validate_child(
            nested.run_id,
            parent,
            &input().request.tasks[0],
            1,
            2,
            &call,
        )?;
        let selector = WorkSelector {
            project: host.project_id,
            run: nested.run_id,
            task: Some(nested.attempts[0].task_id),
            attempt: Some(nested.attempts[0].attempt_id),
            repository: None,
            head: None,
        };
        let index = WorkIndex::open(host.state_root.as_path(), &call)?;
        assert_eq!(index.validate(&selector, &call)?, WorkCoverage::Unindexed);
        // A child-root index cannot establish membership in the host index.
        let forged = Membership {
            schema_version: 1,
            project: selector.project,
            run: selector.run,
            task: selector.task.unwrap(),
            attempt: selector.attempt.unwrap(),
            repository: Some(AbsolutePath::new(Path::new("/forged/.git"))?),
            head: None,
        };
        WorkIndex::open(lane.state_root.as_path(), &call)?.seal(&forged, &call)?;
        assert_eq!(index.validate(&selector, &call)?, WorkCoverage::Unindexed);
        store.start_child(
            nested.run_id,
            &validated,
            &crate::dispatch_tests::Ready,
            &|_, _| Err(RunError::Validation("fixture stops before guardian")),
            &call,
        )?;
        assert_eq!(index.validate(&selector, &call)?, WorkCoverage::Verified);
        // The lane can replace its database, including its SQL guards.
        store
            .db
            .execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER immutable_terminal_delete; DELETE FROM attempts;")?;
        assert_eq!(index.validate(&selector, &call)?, WorkCoverage::Verified);
        let mut claimed_repository = selector;
        claimed_repository.repository = forged.repository;
        assert_eq!(
            index.validate(&claimed_repository, &call)?,
            WorkCoverage::RepositoryUnavailable
        );
        Ok(())
    }
}
