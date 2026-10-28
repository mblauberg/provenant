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
    /// More attempts exist beyond the bounded scan; counts may be incomplete.
    pub truncated: bool,
}

impl RunStore {
    /// Read up to `limit` recent attempts across projects, newest first,
    /// keeping only the latest observed attempt for each task. Counts derived
    /// from these rows are partial when truncated.
    pub fn overview_attempts(&self, limit: usize, call: &Request<'_>) -> Result<RunOverview> {
        let limit = limit.min(128);
        call.retry(|| {
            let mut statement = self.db.prepare(
                "SELECT project, run, task, id, substr(record, 1, 1048577) \
                 FROM attempts ORDER BY sequence DESC LIMIT ?1",
            )?;
            let mut rows = statement.query([(limit + 1) as i64])?;
            let mut attempts = Vec::new();
            let mut seen = BTreeSet::new();
            let mut truncated = false;
            let mut examined = 0;
            while let Some(row) = rows.next()? {
                if examined == limit {
                    truncated = true;
                    break;
                }
                examined += 1;
                let project: String = row.get(0)?;
                let run: String = row.get(1)?;
                let task: String = row.get(2)?;
                let id: String = row.get(3)?;
                let record: AttemptRecord = decode(&row.get::<_, Vec<u8>>(4)?)?;
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
                truncated,
            })
        })
    }
}
