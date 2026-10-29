//! Reactions a settled attempt owes the chair that dispatched it. The row is
//! written in the transaction that commits the terminal record, with its
//! payload frozen there; a drain replays that payload at least once, and the
//! stores each reaction writes to make it exactly once. Rows exist only for
//! attempts that settle after the schema that introduced them.
use crate::{
    AttemptRecord, AttemptState, HostLaunchContext, Request, Result, RunError, RunStore,
    WriterDeliveryState, db::bytes, db::decode,
};
use provenant_core::{ActorId, AttemptId, ProjectId, RunId};
use provenant_records::run::ChairBinding;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// One drain step over a reaction: it records its progress on the reaction and
/// returns its first failure.
pub type Step<'a> =
    dyn FnMut(&mut Reaction) -> std::result::Result<(), Box<dyn std::error::Error>> + 'a;

/// A notice is an outcome for the chair to read, never an instruction.
pub const NOTICE_BYTES: usize = 300;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub schema_version: u64,
    pub attempt: AttemptId,
    pub run: RunId,
    pub project: ProjectId,
    /// Sender of the notice: the actor that committed the settlement.
    pub sender: ActorId,
    pub notice: String,
    pub writer: bool,
    pub finished_micros: i64,
    pub pr_ids: Vec<String>,
    pub issue_ids: Vec<String>,
    /// Trusted host launch inputs a graph advance needs; absent when the
    /// settlement committed without a live guardian.
    pub graph: Option<HostLaunchContext>,
    /// Set once by the first drain, which resolves the chair and its session
    /// and freezes both here.
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub chair: Option<ChairBinding>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub graph_done: bool,
    #[serde(default)]
    pub notice_done: bool,
    #[serde(default)]
    pub log_done: bool,
}

impl Reaction {
    pub fn pending(&self) -> bool {
        !(self.graph_done || self.graph.is_none()) || !self.notice_done || !self.log_done
    }
}

/// What the notice line is built from.
pub struct Line<'a> {
    pub attempt: &'a str,
    pub outcome: &'a str,
    pub partial: bool,
    pub model: Option<&'a str>,
    pub staged: Option<&'a str>,
    pub head: Option<&'a str>,
    pub pull_request: Option<&'a str>,
}

fn clip(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Each variable field has its own budget, so a long model ID cannot push the
/// recovery, head or pull request out. The fixed head of the line and every
/// budget together stay within `NOTICE_BYTES`.
pub fn notice_line(line: &Line<'_>) -> String {
    let mut out = format!(
        "settled {} {}",
        clip(line.attempt, 32),
        clip(line.outcome, 16)
    );
    if line.partial {
        out.push_str(" (partial)");
    }
    for (label, value, budget) in [
        ("model", line.model, 56),
        ("next", line.staged, 40),
        ("head", line.head, 12),
        ("pr", line.pull_request, 90),
    ] {
        if let Some(value) = value {
            out.push_str(&format!(" {label}={}", clip(value, budget)));
        }
    }
    debug_assert!(out.len() <= NOTICE_BYTES);
    out
}

fn number(text: &str) -> Option<String> {
    let digits = text.trim_start_matches('#');
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.to_owned())
}

impl RunStore {
    /// Freeze the reaction payload from the terminal record about to commit.
    pub(crate) fn freeze_reaction(
        &self,
        next: &AttemptRecord,
        writer: bool,
        staged: Option<&str>,
    ) -> Result<Reaction> {
        let AttemptState::Terminal {
            outcome,
            finished_at,
            ..
        } = &next.state
        else {
            return Err(RunError::Validation("terminal state missing"));
        };
        let pull_request = next
            .delivery
            .as_ref()
            .and_then(|receipt| match &receipt.state {
                WriterDeliveryState::Delivered { pull_request, .. } => pull_request.clone(),
                _ => None,
            });
        let head = next
            .delivery
            .as_ref()
            .and_then(|receipt| receipt.head)
            .or(next.head)
            .map(|oid| oid.to_string());
        let route = next.observed_route.as_ref().or(next.applied_route.as_ref());
        let outcome = serde_json::to_value(outcome).map_err(|_| RunError::Validation("outcome"))?;
        let attempt = next.attempt_id.to_string();
        let notice = notice_line(&Line {
            attempt: &attempt,
            outcome: outcome.as_str().unwrap_or("unknown"),
            partial: !next.partial_signals.is_empty(),
            model: route.and_then(|route| route.model.as_deref()),
            staged,
            head: head.as_deref().map(|head| clip(head, 12)),
            pull_request: pull_request.as_deref(),
        });
        Ok(Reaction {
            schema_version: 1,
            attempt: next.attempt_id,
            run: next.run_id,
            project: next.project_id,
            sender: self.context.actor_id,
            notice,
            writer,
            finished_micros: finished_at.as_micros(),
            pr_ids: pull_request
                .as_deref()
                .and_then(|url| url.rsplit('/').next())
                .and_then(number)
                .into_iter()
                .collect(),
            issue_ids: next
                .context
                .as_ref()
                .and_then(|context| context.work_item.as_deref())
                .and_then(number)
                .into_iter()
                .collect(),
            graph: self.reaction_launch.clone(),
            resolved: false,
            chair: None,
            session: None,
            graph_done: false,
            notice_done: false,
            log_done: false,
        })
    }

    pub(crate) fn insert_reaction(tx: &rusqlite::Connection, reaction: &Reaction) -> Result<()> {
        tx.execute(
            "INSERT OR IGNORE INTO reactions(attempt,run,project,settled_at,pending,payload) VALUES (?1,?2,?3,?4,1,?5)",
            params![
                reaction.attempt.to_string(),
                reaction.run.to_string(),
                reaction.project.to_string(),
                reaction.finished_micros.div_euclid(1_000_000),
                bytes(reaction)?
            ],
        )?;
        Ok(())
    }

    /// Attempts of a run that still owe reactions.
    pub fn pending_reactions(&self, run: RunId, call: &Request<'_>) -> Result<Vec<AttemptId>> {
        let ids: Vec<String> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT attempt FROM reactions WHERE run=?1 AND project=?2 AND pending=1 ORDER BY rowid",
            )?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        ids.into_iter()
            .map(|id| AttemptId::parse(&id).map_err(|_| RunError::Validation("reaction attempt")))
            .collect()
    }

    pub fn reaction(&self, attempt: AttemptId, call: &Request<'_>) -> Result<Option<Reaction>> {
        let raw: Option<Vec<u8>> = call.retry(|| {
            Ok(self
                .db
                .query_row(
                    "SELECT payload FROM reactions WHERE attempt=?1 AND project=?2",
                    params![attempt.to_string(), self.context.project_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?)
        })?;
        raw.map(|raw| decode::<Reaction>(&raw)).transpose()
    }

    /// Replace a reaction's progress. Identity and the frozen payload must not
    /// change; the row stays pending until every reaction has finished.
    pub fn save_reaction(&self, reaction: &Reaction, call: &Request<'_>) -> Result<()> {
        let current = self
            .reaction(reaction.attempt, call)?
            .ok_or(RunError::NotFound)?;
        if current.notice != reaction.notice
            || current.run != reaction.run
            || (current.resolved && current.chair != reaction.chair)
            || (current.resolved && current.session != reaction.session)
        {
            return Err(RunError::Conflict);
        }
        self.write(call, |tx| {
            tx.execute(
                "UPDATE reactions SET payload=?1, pending=?2 WHERE attempt=?3 AND project=?4",
                params![
                    bytes(reaction)?,
                    i64::from(reaction.pending()),
                    reaction.attempt.to_string(),
                    self.context.project_id.to_string()
                ],
            )?;
            Ok(())
        })
    }

    /// Run `step` on every attempt of `run` that owes reactions, under the
    /// run's reaction lock so concurrent drains do not repeat each other (a
    /// busy lock means another drain covers the run). `step` records progress
    /// on the reaction it is given and returns its first failure; the progress
    /// is saved either way, a failed reaction stays pending for the next drain,
    /// and the others still run. The first failure is returned.
    pub fn drain_reactions(
        &self,
        run: RunId,
        call: &Request<'_>,
        step: &mut Step<'_>,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        if self.pending_reactions(run, call)?.is_empty() {
            return Ok(());
        }
        let Some(_lock) = self.reaction_lock(run)? else {
            return Ok(());
        };
        let mut first = None;
        // Re-read under the lock: a drain that just finished leaves nothing.
        for attempt in self.pending_reactions(run, call)? {
            let Some(mut reaction) = self.reaction(attempt, call)? else {
                continue;
            };
            if let Err(error) = step(&mut reaction) {
                first.get_or_insert(error);
            }
            if let Err(error) = self.save_reaction(&reaction, call) {
                first.get_or_insert(error.into());
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Record a reaction a settlement owes, outside a terminal commit.
    pub fn owe_reaction(&self, reaction: &Reaction, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| Self::insert_reaction(tx, reaction))
    }

    /// Exclude other drains of this run. `None` when another holds it.
    pub fn reaction_lock(&self, run: RunId) -> Result<Option<provenant_os::fs::LockGuard>> {
        use provenant_os::fs::{FsError, LockMode};
        let directory = self.run_dir(run, false)?;
        match directory.try_lock(std::ffi::OsStr::new("reactions.lock"), LockMode::Exclusive) {
            Ok(lock) => Ok(Some(lock)),
            Err(FsError::LockTimeout { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Drop finished rows older than `before` (unix seconds).
    pub(crate) fn prune_reactions(&self, before: i64, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            tx.execute(
                "DELETE FROM reactions WHERE pending=0 AND settled_at<?1",
                [before],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line<'a>(model: &'a str) -> Line<'a> {
        Line {
            attempt: "0123456789abcdef0123456789abcdef",
            outcome: "quota_limited",
            partial: true,
            model: Some(model),
            staged: Some("policy_fallback_with_a_long_name_that_overruns_it"),
            head: Some("0123456789abcdef"),
            pull_request: Some("https://github.com/example/example/pull/2125"),
        }
    }

    #[test]
    fn required_fields_survive_a_long_model_id() {
        let model = "m".repeat(512);
        let text = notice_line(&line(&model));
        assert!(text.len() <= NOTICE_BYTES, "{}", text.len());
        for needle in [
            "(partial)",
            " next=policy_fallback",
            " head=0123456789ab",
            " pr=https://",
        ] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        let model = "\u{e9}".repeat(300);
        let text = notice_line(&line(&model));
        assert!(text.len() <= NOTICE_BYTES);
    }
}
