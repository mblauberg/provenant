//! Bounded cross-project attempt summaries for local overview surfaces.
use crate::{AttemptRecord, AttemptState, Outcome, Request, Result, RunStore, db::decode};
use provenant_core::{AttemptId, ProjectId, RunId, TaskId};
use std::collections::BTreeSet;

/// A projection without prompts, results, or terminal reasons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewAttempt {
    pub project_id: ProjectId,
    pub run_id: RunId,
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub title: Option<String>,
    pub state: OverviewState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewState {
    Queued,
    Starting,
    Running,
    Settling,
    Terminal(Outcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOverview {
    pub attempts: Vec<OverviewAttempt>,
    pub bytes_read: usize,
    /// More attempts exist beyond the bounded scan; counts may be incomplete.
    pub truncated: bool,
}

impl RunStore {
    /// Read up to `limit` recent attempts across projects, newest first,
    /// keeping only the latest observed attempt for each task. Counts derived
    /// from these rows are partial when truncated.
    pub fn overview_attempts(
        &self,
        limit: usize,
        max_bytes: usize,
        call: &Request<'_>,
    ) -> Result<RunOverview> {
        let limit = limit.min(128);
        call.retry(|| {
            let mut statement = self.db.prepare(
                "WITH recent AS (SELECT sequence, length(record) AS bytes FROM attempts ORDER BY sequence DESC LIMIT ?1), \
                 sized AS (SELECT sequence, bytes, sum(bytes) OVER (ORDER BY sequence DESC) AS total FROM recent) \
                 SELECT a.project, a.run, a.task, a.id, s.bytes, s.total, \
                 CASE WHEN s.total <= ?2 AND s.bytes <= 1048576 THEN a.record END \
                 FROM sized s JOIN attempts a ON a.sequence=s.sequence ORDER BY s.sequence DESC",
            )?;
            let mut rows = statement.query(rusqlite::params![(limit + 1) as i64, max_bytes.min(1_048_576) as i64])?;
            let mut attempts = Vec::new();
            let mut seen = BTreeSet::new();
            let mut truncated = false;
            let mut examined = 0;
            let mut bytes_read = 0;
            while let Some(row) = rows.next()? {
                if examined == limit {
                    truncated = true;
                    break;
                }
                examined += 1;
                let bytes: i64 = row.get(4)?;
                let total: i64 = row.get(5)?;
                if total < 0 || bytes < 0 || total > max_bytes.min(1_048_576) as i64 || bytes > 1_048_576 {
                    truncated = true;
                    break;
                }
                bytes_read = total as usize;
                let project: String = row.get(0)?;
                let run: String = row.get(1)?;
                let task: String = row.get(2)?;
                let id: String = row.get(3)?;
                let record: AttemptRecord = decode(&row.get::<_, Vec<u8>>(6)?)?;
                if record.project_id.to_string() != project
                    || record.run_id.to_string() != run
                    || record.task_id.to_string() != task
                    || record.attempt_id.to_string() != id
                {
                    return Err(crate::RunError::Validation("attempt index identity"));
                }
                if !seen.insert((run, task)) {
                    continue;
                }
                let state = match record.state {
                    AttemptState::Queued => OverviewState::Queued,
                    AttemptState::Starting => OverviewState::Starting,
                    AttemptState::Running => OverviewState::Running,
                    AttemptState::Settling { .. } => OverviewState::Settling,
                    AttemptState::Terminal { outcome, .. } => OverviewState::Terminal(outcome),
                };
                attempts.push(OverviewAttempt {
                    project_id: record.project_id,
                    run_id: record.run_id,
                    task_id: record.task_id,
                    attempt_id: record.attempt_id,
                    title: record.title,
                    state,
                });
            }
            Ok(RunOverview {
                attempts,
                bytes_read,
                truncated,
            })
        })
    }
}
