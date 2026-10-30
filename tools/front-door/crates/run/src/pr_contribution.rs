//! Durable host-owned pull request contribution links.
use crate::{
    Request, Result, RunError,
    db::{immediate, open_database},
};
use provenant_core::{AttemptId, GitOid, ProjectId, RunId, WallTime};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

const FILE: &str = "pr-contributions.sqlite3";
/// Allocation is represented in millionths for exact validation.
pub const WEIGHT_ONE: u32 = 1_000_000;
const MAX_LINKS: usize = 256;

/// Read-only attempts only associate with a verified, registered linked worktree.
pub(crate) fn read_only_branch(
    host: &Path,
    project: ProjectId,
    cwd: &Path,
) -> Result<Option<provenant_confinement::repo::GitRefName>> {
    let writer = match crate::resolve_writer(host, cwd, cwd) {
        Ok(writer) => writer,
        // Ordinary read-only dispatches also run outside linked worktrees.
        Err(crate::RunError::Validation(_))
        | Err(crate::RunError::Writer(_))
        | Err(crate::RunError::WriterProjectMismatch { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    match crate::prepare_writer(&writer, host, project) {
        Ok(writer) => Ok(Some(writer.branch)),
        Err(crate::RunError::WorktreeUnregistered(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

impl crate::RunStore {
    /// Attribution must never change the result of an already accepted run.
    pub(crate) fn contribution_diagnostic(&self, run: RunId, result: Result<()>) {
        if let Err(error) = result {
            let message = format!("PR contribution capture failed: {error}\n");
            if let Ok(dir) = self.run_dir(run, false) {
                let _ = crate::artifacts::seal(
                    &dir,
                    "pr-contribution-diagnostic.txt",
                    message.as_bytes(),
                    8192,
                );
            }
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "warning: {}", message.trim());
        }
    }
    pub(crate) fn contribution_attempts(
        &self,
        run: provenant_core::RunId,
        call: &Request<'_>,
    ) -> Result<Vec<crate::AttemptRecord>> {
        let mut query = self.db.prepare(
            "SELECT id FROM attempts WHERE run=?1 AND project=?2 ORDER BY sequence LIMIT 10001",
        )?;
        let ids = query
            .query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if ids.len() > 10000 {
            return Err(RunError::Validation("too many contribution attempts"));
        }
        ids.into_iter()
            .map(|id| {
                self.record(
                    id.parse()
                        .map_err(|_| RunError::Validation("invalid contribution attempt"))?,
                    call,
                )
            })
            .collect()
    }
    pub(crate) fn inherit_contribution(
        &self,
        prior: AttemptId,
        run: provenant_core::RunId,
        replacement_branch: Option<&str>,
        call: &Request<'_>,
    ) -> Result<()> {
        if !self.writer_registry().join(FILE).try_exists()? {
            return Ok(());
        }
        let db = Contributions::open(self.writer_registry(), call)?;
        let branch: Option<(String, String, i64)> =
            db.0.query_row(
                "SELECT branch,role,generation FROM branch_attempts WHERE attempt=?1 AND project=?2",
                params![prior.to_string(), self.context.project_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((branch, role, generation)) = branch {
            let replacement = replacement_branch.filter(|replacement| *replacement != branch);
            for attempt in self.contribution_attempts(run, call)? {
                db.capture_generation(
                    &attempt,
                    replacement.unwrap_or(&branch),
                    if role == "read_only" {
                        "read_only"
                    } else {
                        "repair"
                    },
                    replacement.is_none().then_some(generation),
                    call,
                )?;
            }
        }
        Ok(())
    }
    /// Called only after host validation of the child's derived identity.
    pub(crate) fn capture_child_contribution(
        &self,
        parent: AttemptId,
        record: &crate::AttemptRecord,
        call: &Request<'_>,
    ) -> Result<()> {
        if !self.writer_registry().join(FILE).try_exists()? {
            return Ok(());
        }
        let db = Contributions::open(self.writer_registry(), call)?;
        let branch: Option<(String, i64)> =
            db.0.query_row(
                "SELECT branch,generation FROM branch_attempts WHERE attempt=?1 AND project=?2",
                params![parent.to_string(), self.context.project_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((branch, generation)) = branch {
            let mut local = record.clone();
            // Lane-written route fields cannot establish host placement.
            local.resolved_route = None;
            db.capture_generation(&local, &branch, "read_only", Some(generation), call)?;
        }
        Ok(())
    }
    pub(crate) fn retain_contribution_route(
        &self,
        record: &crate::AttemptRecord,
        call: &Request<'_>,
    ) -> Result<()> {
        match std::fs::symlink_metadata(self.writer_registry().join(FILE)) {
            Ok(_) => Contributions::open(self.writer_registry(), call)?.route(record, call),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Canonical lower-case GitHub repository and PR number.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PullRequest {
    pub repository: String,
    pub number: u64,
}
impl PullRequest {
    fn validate(&self) -> Result<()> {
        let parts: Vec<_> = self.repository.split('/').collect();
        if parts.len() != 2
            || self.repository.len() > 200
            || self.number == 0
            || parts.iter().any(|part| {
                part.is_empty()
                    || !part.bytes().all(|b| {
                        b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || matches!(b, b'-' | b'_' | b'.')
                    })
            })
        {
            return Err(RunError::Validation(
                "invalid canonical PR repository or number",
            ));
        }
        Ok(())
    }
}

/// Unknown allocation is represented by None, never zero.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Contribution {
    pub attempt: AttemptId,
    pub pull_request: PullRequest,
    pub weight_millionths: Option<u32>,
    pub branch: Option<String>,
    pub head: Option<GitOid>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub run: Option<RunId>,
    pub outcome: Option<crate::Outcome>,
    pub queued_at: Option<WallTime>,
    pub finished_at: Option<WallTime>,
    pub pr_count: u64,
    pub ambiguous: bool,
    pub truncated: bool,
}

/// Host callers supply facts from authoritative dispatch or workflow records.
pub struct Contributions(Connection, usize);
impl Contributions {
    /// Read without creating or migrating a store, including committed WAL data.
    pub fn open_read_only(host: &Path, call: &Request<'_>) -> Result<Option<Self>> {
        call.check()?;
        let root = match provenant_os::fs::Directory::open_anchor(host) {
            Ok(root) => root,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        crate::db::private(&root)?;
        match root.metadata_at(std::ffi::OsStr::new(FILE), false) {
            Ok(_) => {}
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        crate::db::companions(&root, FILE)?;
        let db = provenant_sqlite::open_connection(&host.join(FILE), true)?;
        match provenant_sqlite::read_version(&db, 4) {
            Ok(version @ 1..=4) => Ok(Some(Self(db, version))),
            Ok(_) => Err(RunError::Version(0)),
            Err(provenant_sqlite::VersionError::Unsupported(version)) => {
                Err(RunError::Version(version))
            }
            Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
            Err(provenant_sqlite::VersionError::Sqlite(error)) => Err(error.into()),
        }
    }
    pub fn open(host: &Path, call: &Request<'_>) -> Result<Self> {
        let (_, db, _lock) = open_database(host, FILE, call)?;
        let version = match provenant_sqlite::read_version(&db, 4) {
            Ok(v) => Ok(v),
            Err(provenant_sqlite::VersionError::Unsupported(v)) => Err(RunError::Version(v)),
            Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
            Err(provenant_sqlite::VersionError::Sqlite(e)) => Err(e.into()),
        }?;
        if version == 0 {
            immediate(&db, call, |tx| {
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS links (
                    attempt TEXT NOT NULL, repository TEXT NOT NULL, number INTEGER NOT NULL,
                    weight INTEGER, PRIMARY KEY(attempt, repository, number)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS links_pr ON links(repository, number, attempt);
                CREATE TABLE IF NOT EXISTS allocations (attempt TEXT PRIMARY KEY) STRICT;
                PRAGMA user_version=1;",
                )?;
                Ok(())
            })?;
        }
        if version < 2 {
            immediate(&db, call, |tx| {
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS branch_attempts (
                        attempt TEXT PRIMARY KEY, project TEXT NOT NULL,
                        branch TEXT NOT NULL, role TEXT NOT NULL, model TEXT, account TEXT
                    ) STRICT;
                    CREATE INDEX IF NOT EXISTS branch_attempts_branch ON branch_attempts(project, branch);
                    CREATE TABLE IF NOT EXISTS branch_prs (
                        project TEXT NOT NULL, branch TEXT NOT NULL,
                        repository TEXT NOT NULL, number INTEGER NOT NULL, head TEXT NOT NULL,
                        PRIMARY KEY(project, branch, repository, number)
                    ) STRICT;
                    PRAGMA user_version=2;",
                )?;
                Ok(())
            })?;
        }
        if version < 3 {
            immediate(&db, call, |tx| {
                tx.execute_batch(
                    "ALTER TABLE branch_attempts ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
                    ALTER TABLE branch_attempts ADD COLUMN run TEXT;
                    ALTER TABLE branch_attempts ADD COLUMN queued_at INTEGER;
                    ALTER TABLE branch_attempts ADD COLUMN outcome TEXT;
                    ALTER TABLE branch_attempts ADD COLUMN finished_at INTEGER;
                    ALTER TABLE branch_prs RENAME TO branch_prs_v2;
                    CREATE TABLE branch_prs (
                        project TEXT NOT NULL, branch TEXT NOT NULL, repository TEXT NOT NULL,
                        number INTEGER NOT NULL, head TEXT NOT NULL,
                        generation INTEGER NOT NULL, closed INTEGER NOT NULL DEFAULT 0,
                        PRIMARY KEY(project,branch,repository,number,generation)
                    ) STRICT;
                    INSERT INTO branch_prs(project,branch,repository,number,head,generation)
                        SELECT project,branch,repository,number,head,0 FROM branch_prs_v2;
                    DROP TABLE branch_prs_v2;
                    CREATE TABLE branch_generations (
                        project TEXT NOT NULL, branch TEXT NOT NULL, generation INTEGER NOT NULL,
                        PRIMARY KEY(project,branch)
                    ) STRICT;
                    CREATE TABLE truncations (attempt TEXT PRIMARY KEY) STRICT;
                    UPDATE branch_attempts SET role='read_only' WHERE role='review';
                    PRAGMA user_version=3;",
                )?;
                Ok(())
            })?;
        }
        if version < 4 {
            immediate(&db, call, |tx| {
                tx.execute_batch(
                    "CREATE TABLE reassignments (
                        attempt TEXT NOT NULL, repository TEXT NOT NULL, number INTEGER NOT NULL,
                        PRIMARY KEY(attempt, repository, number)
                    ) STRICT;
                    PRAGMA user_version=4;",
                )?;
                Ok(())
            })?;
        }
        Ok(Self(db, 4))
    }

    /// Snapshot a registered local worktree at dispatch, independent of outcome.
    pub(crate) fn capture(
        &self,
        record: &crate::AttemptRecord,
        branch: &str,
        role: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        self.capture_generation(record, branch, role, None, call)
    }

    fn generation(tx: &Connection, project: ProjectId, branch: &str) -> Result<i64> {
        Ok(tx
            .query_row(
                "SELECT generation FROM branch_generations WHERE project=?1 AND branch=?2",
                params![project.to_string(), branch],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    fn capture_generation(
        &self,
        record: &crate::AttemptRecord,
        branch: &str,
        role: &str,
        generation: Option<i64>,
        call: &Request<'_>,
    ) -> Result<()> {
        immediate(&self.0, call, |tx| {
            let generation = generation.unwrap_or(Self::generation(tx, record.project_id, branch)?);
            tx.execute(
                "INSERT INTO branch_attempts(attempt,project,branch,role,generation) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(attempt) DO NOTHING",
                params![record.attempt_id.to_string(), record.project_id.to_string(), branch, role, generation],
            )?;
            let matches: bool = tx.query_row(
                "SELECT project=?2 AND branch=?3 AND role=?4 FROM branch_attempts WHERE attempt=?1",
                params![
                    record.attempt_id.to_string(),
                    record.project_id.to_string(),
                    branch,
                    role
                ],
                |row| row.get(0),
            )?;
            if !matches {
                return Err(RunError::Conflict);
            }
            Self::resolve(tx, record.project_id, branch)
        })?;
        self.route(record, call)
    }

    /// Bind an observed PR head before workflow cleanup can remove the branch.
    pub fn bind(
        &self,
        project: ProjectId,
        branch: &str,
        pr: &PullRequest,
        head: GitOid,
        call: &Request<'_>,
    ) -> Result<()> {
        pr.validate()?;
        let number =
            i64::try_from(pr.number).map_err(|_| RunError::Validation("PR number too large"))?;
        immediate(&self.0, call, |tx| {
            let generation = Self::generation(tx, project, branch)?;
            tx.execute(
                "INSERT INTO branch_prs(project,branch,repository,number,head,generation)
                 SELECT ?1,?2,?3,?4,?5,?6 WHERE NOT EXISTS(
                    SELECT 1 FROM branch_prs WHERE project=?1 AND branch=?2 AND repository=?3 AND number=?4 AND closed=1)
                 ON CONFLICT(project,branch,repository,number,generation) DO UPDATE SET head=excluded.head",
                params![project.to_string(), branch, pr.repository, number, head.to_string(), generation],
            )?;
            // A still-open PR can span generations; refresh each open binding.
            tx.execute("UPDATE branch_prs SET head=?5 WHERE project=?1 AND branch=?2 AND repository=?3 AND number=?4 AND closed=0", params![project.to_string(), branch, pr.repository, number, head.to_string()])?;
            Self::resolve(tx, project, branch)
        })
    }

    /// Each observed closure ends a generation. Still-open PRs span the boundary.
    /// Replays never reopen the PR or advance the generation a second time.
    /// Returns false when no open binding was available to close.
    pub fn close(&self, project: ProjectId, pr: &PullRequest, call: &Request<'_>) -> Result<bool> {
        self.close_at(project, pr, None, call)
    }

    /// Close with the PR's observed end time. Unsealed attempts queued after it
    /// belong to the next generation, so they leave the closed PR's set.
    /// The move is recorded in `reassignments` and cannot repeat.
    pub fn close_at(
        &self,
        project: ProjectId,
        pr: &PullRequest,
        ended_at: Option<WallTime>,
        call: &Request<'_>,
    ) -> Result<bool> {
        pr.validate()?;
        immediate(&self.0, call, |tx| {
            let bindings = {
                let mut query = tx.prepare("SELECT DISTINCT branch FROM branch_prs WHERE project=?1 AND repository=?2 AND number=?3 AND closed=0")?;
                query
                    .query_map(
                        params![
                            project.to_string(),
                            pr.repository,
                            i64::try_from(pr.number)
                                .map_err(|_| RunError::Validation("PR number too large"))?
                        ],
                        |row| row.get::<_, String>(0),
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            tx.execute(
                "UPDATE branch_prs SET closed=1 WHERE project=?1 AND repository=?2 AND number=?3",
                params![
                    project.to_string(),
                    pr.repository,
                    i64::try_from(pr.number)
                        .map_err(|_| RunError::Validation("PR number too large"))?
                ],
            )?;
            let closed = !bindings.is_empty();
            for branch in bindings {
                let generation = Self::generation(tx, project, &branch)?;
                if let Some(ended_at) = ended_at {
                    let number = i64::try_from(pr.number)
                        .map_err(|_| RunError::Validation("PR number too large"))?;
                    let moved = {
                        let mut query = tx.prepare(
                            "SELECT attempt FROM branch_attempts
                             WHERE project=?1 AND branch=?2 AND generation=?3 AND queued_at>?4
                             AND NOT EXISTS(SELECT 1 FROM allocations s WHERE s.attempt=branch_attempts.attempt)")?;
                        query
                            .query_map(
                                params![
                                    project.to_string(),
                                    branch,
                                    generation,
                                    ended_at.as_micros()
                                ],
                                |row| row.get::<_, String>(0),
                            )?
                            .collect::<std::result::Result<Vec<_>, _>>()?
                    };
                    for attempt in moved {
                        let unlinked = tx.execute(
                            "DELETE FROM links WHERE attempt=?1 AND repository=?2 AND number=?3",
                            params![attempt, pr.repository, number],
                        )?;
                        if unlinked > 0 {
                            tx.execute(
                                "INSERT INTO reassignments VALUES (?1,?2,?3) ON CONFLICT DO NOTHING",
                                params![attempt, pr.repository, number],
                            )?;
                        }
                        tx.execute(
                            "UPDATE branch_attempts SET generation=?2 WHERE attempt=?1",
                            params![attempt, generation + 1],
                        )?;
                    }
                }
                // Retain old generations for inherited repairs and children.
                // Only PRs already open at the boundary span into the new one.
                tx.execute(
                    "INSERT INTO branch_prs(project,branch,repository,number,head,generation)
                     SELECT project,branch,repository,number,head,?3+1 FROM branch_prs
                     WHERE project=?1 AND branch=?2 AND generation=?3 AND closed=0
                     ON CONFLICT DO NOTHING",
                    params![project.to_string(), branch, generation],
                )?;
                tx.execute("INSERT INTO branch_generations VALUES (?1,?2,?3) ON CONFLICT(project,branch) DO UPDATE SET generation=excluded.generation", params![project.to_string(), branch, generation+1])?;
            }
            Ok(closed)
        })
    }

    /// Offline candidates for the command layer's GitHub state sweep.
    pub fn open_bindings(
        &self,
        project: ProjectId,
        branch: &str,
        call: &Request<'_>,
    ) -> Result<Vec<PullRequest>> {
        call.check()?;
        if self.1 < 3 {
            return Ok(Vec::new());
        }
        {
            let mut query = self.0.prepare(
                "SELECT DISTINCT repository,number FROM branch_prs
                 WHERE project=?1 AND branch=?2 AND closed=0 ORDER BY repository,number",
            )?;
            let rows = query.query_map(params![project.to_string(), branch], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            rows.map(|row| {
                let (repository, number) = row?;
                let number = u64::try_from(number)
                    .map_err(|_| RunError::Validation("invalid contribution PR number"))?;
                Ok(PullRequest { repository, number })
            })
            .collect()
        }
    }

    fn resolve(tx: &Connection, project: ProjectId, branch: &str) -> Result<()> {
        // Each insertion sees the previous one, so no attempt can cross the cap.
        let candidates = {
            let mut query = tx.prepare(
                "SELECT a.attempt,p.repository,p.number FROM branch_attempts a
                 JOIN branch_prs p USING(project,branch,generation)
                 LEFT JOIN links l ON l.attempt=a.attempt AND l.repository=p.repository AND l.number=p.number
                 WHERE a.project=?1 AND a.branch=?2 AND p.closed=0 AND l.attempt IS NULL
                 AND NOT EXISTS(SELECT 1 FROM allocations s WHERE s.attempt=a.attempt)
                 ORDER BY a.attempt,p.repository,p.number")?;
            query
                .query_map(params![project.to_string(), branch], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (attempt, repository, number) in candidates {
            Self::link_on(tx, &attempt, &repository, number)?;
        }
        Ok(())
    }

    fn link_on(tx: &Connection, attempt: &str, repository: &str, number: i64) -> Result<()> {
        let sealed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM allocations WHERE attempt=?1)",
            [attempt],
            |row| row.get(0),
        )?;
        if sealed {
            return Err(RunError::Validation("attempt allocation already sealed"));
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM links WHERE attempt=?1",
            [attempt],
            |row| row.get(0),
        )?;
        if count >= MAX_LINKS as i64 {
            tx.execute(
                "INSERT INTO truncations VALUES (?1) ON CONFLICT DO NOTHING",
                [attempt],
            )?;
        } else {
            tx.execute("INSERT INTO links(attempt,repository,number) VALUES (?1,?2,?3) ON CONFLICT DO NOTHING", params![attempt, repository, number])?;
        }
        Ok(())
    }

    /// Retain resolved placement; requested routes and missing facts stay unknown.
    pub(crate) fn route(&self, record: &crate::AttemptRecord, call: &Request<'_>) -> Result<()> {
        let (outcome, finished_at) = match record.state {
            crate::AttemptState::Terminal {
                outcome,
                finished_at,
                ..
            } => (
                Some(
                    serde_json::to_string(&outcome)
                        .map_err(|_| RunError::Validation("invalid contribution outcome"))?,
                ),
                Some(finished_at.as_micros()),
            ),
            _ => (None, None),
        };
        immediate(&self.0, call, |tx| {
            tx.execute("UPDATE branch_attempts SET run=?2,queued_at=?3,outcome=COALESCE(?4,outcome),finished_at=COALESCE(?5,finished_at),model=COALESCE(?6,model),account=COALESCE(?7,account) WHERE attempt=?1",
                params![record.attempt_id.to_string(), record.run_id.to_string(), record.queued_at.as_micros(), outcome, finished_at,
                    record.resolved_route.as_ref().and_then(|r| r.model.as_deref()), record.resolved_route.as_ref().and_then(|r| r.account_label.as_deref())])?;
            Ok(())
        })
    }

    /// Add an idempotent link. No new links can be added after sealing.
    pub fn link(&self, attempt: AttemptId, pr: &PullRequest, call: &Request<'_>) -> Result<()> {
        pr.validate()?;
        let number =
            i64::try_from(pr.number).map_err(|_| RunError::Validation("PR number too large"))?;
        immediate(&self.0, call, |tx| {
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM links WHERE attempt=?1 AND repository=?2 AND number=?3)", params![attempt.to_string(), pr.repository, number], |row| row.get(0))?;
            if exists {
                return Ok(());
            }
            Self::link_on(tx, &attempt.to_string(), &pr.repository, number)
        })
    }

    /// Seal a complete allocation. Identical replays are harmless.
    pub fn allocate(
        &self,
        attempt: AttemptId,
        weights: &[(PullRequest, u32)],
        call: &Request<'_>,
    ) -> Result<()> {
        if weights.is_empty() || weights.len() > MAX_LINKS {
            return Err(RunError::Validation("invalid allocation size"));
        }
        let mut unique = BTreeSet::new();
        let mut sum = 0_u64;
        for (pr, weight) in weights {
            pr.validate()?;
            if !unique.insert(pr.clone()) {
                return Err(RunError::Validation("duplicate PR allocation"));
            }
            sum += u64::from(*weight);
        }
        if sum != u64::from(WEIGHT_ONE) {
            return Err(RunError::Validation("PR weights must sum to one"));
        }
        immediate(&self.0, call, |tx| {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM links WHERE attempt=?1",
                [attempt.to_string()],
                |row| row.get(0),
            )?;
            if count != weights.len() as i64 {
                return Err(RunError::Validation("allocation must cover all PR links"));
            }
            let sealed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM allocations WHERE attempt=?1)",
                [attempt.to_string()],
                |row| row.get(0),
            )?;
            for (pr, weight) in weights {
                let number = i64::try_from(pr.number)
                    .map_err(|_| RunError::Validation("PR number too large"))?;
                let current: Option<Option<i64>> = tx
                    .query_row(
                        "SELECT weight FROM links WHERE attempt=?1 AND repository=?2 AND number=?3",
                        params![attempt.to_string(), pr.repository, number],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(current) = current else {
                    return Err(RunError::Validation("allocation names an unlinked PR"));
                };
                if sealed {
                    if current != Some(i64::from(*weight)) {
                        return Err(RunError::Validation("conflicting allocation replay"));
                    }
                } else {
                    tx.execute("UPDATE links SET weight=?4 WHERE attempt=?1 AND repository=?2 AND number=?3", params![attempt.to_string(), pr.repository, number, weight])?;
                }
            }
            if !sealed {
                tx.execute(
                    "INSERT INTO allocations(attempt) VALUES (?1)",
                    [attempt.to_string()],
                )?;
            }
            Ok(())
        })
    }

    /// A binding can have no attempts; retained legacy links also establish coverage.
    pub fn is_bound(&self, pr: &PullRequest, call: &Request<'_>) -> Result<bool> {
        call.check()?;
        pr.validate()?;
        let bindings = if self.1 == 1 {
            ""
        } else {
            " OR EXISTS(SELECT 1 FROM branch_prs WHERE repository=?1 AND number=?2)"
        };
        Ok(self.0.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM links WHERE repository=?1 AND number=?2){bindings}"
            ),
            params![
                pr.repository,
                i64::try_from(pr.number)
                    .map_err(|_| RunError::Validation("PR number too large"))?
            ],
            |row| row.get(0),
        )?)
    }

    /// Truncation includes attempts whose link to this PR could not be added.
    pub fn truncated_for_pr(&self, pr: &PullRequest, call: &Request<'_>) -> Result<bool> {
        call.check()?;
        pr.validate()?;
        if self.1 < 3 {
            return Ok(false);
        }
        {
            Ok(self.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM truncations t WHERE
                    EXISTS(SELECT 1 FROM links l WHERE l.attempt=t.attempt AND l.repository=?1 AND l.number=?2)
                    OR EXISTS(SELECT 1 FROM branch_attempts a JOIN branch_prs p USING(project,branch,generation)
                        WHERE a.attempt=t.attempt AND p.repository=?1 AND p.number=?2))",
                params![
                    pr.repository,
                    i64::try_from(pr.number)
                        .map_err(|_| RunError::Validation("PR number too large"))?
                ],
                |row| row.get(0),
            )?)
        }
    }

    /// Attempts moved off this PR to the next generation after a late close.
    pub fn reassigned_from_pr(&self, pr: &PullRequest, call: &Request<'_>) -> Result<u64> {
        call.check()?;
        pr.validate()?;
        if self.1 < 4 {
            return Ok(0);
        }
        {
            let count: i64 = self.0.query_row(
                "SELECT count(*) FROM reassignments WHERE repository=?1 AND number=?2",
                params![
                    pr.repository,
                    i64::try_from(pr.number)
                        .map_err(|_| RunError::Validation("PR number too large"))?
                ],
                |row| row.get(0),
            )?;
            u64::try_from(count).map_err(|_| RunError::Validation("invalid contribution count"))
        }
    }

    /// Linked attempts with no queue time, which a late close cannot place.
    pub fn unplaced_for_pr(&self, pr: &PullRequest, call: &Request<'_>) -> Result<u64> {
        call.check()?;
        pr.validate()?;
        if self.1 < 3 {
            return Ok(0);
        }
        {
            let count: i64 = self.0.query_row(
                "SELECT count(*) FROM links l JOIN branch_attempts a USING(attempt)
                 WHERE l.repository=?1 AND l.number=?2 AND a.queued_at IS NULL",
                params![
                    pr.repository,
                    i64::try_from(pr.number)
                        .map_err(|_| RunError::Validation("PR number too large"))?
                ],
                |row| row.get(0),
            )?;
            u64::try_from(count).map_err(|_| RunError::Validation("invalid contribution count"))
        }
    }

    /// Sealed allocations omitted by automatic resolution, including absent rows.
    pub fn skipped_sealed_for_pr(&self, pr: &PullRequest, call: &Request<'_>) -> Result<u64> {
        call.check()?;
        pr.validate()?;
        if self.1 < 3 {
            return Ok(0);
        }
        {
            let count: i64 = self.0.query_row(
                "SELECT count(DISTINCT a.attempt) FROM branch_attempts a
                 JOIN branch_prs p USING(project,branch,generation)
                 JOIN allocations s ON s.attempt=a.attempt
                 WHERE p.repository=?1 AND p.number=?2 AND NOT EXISTS(
                     SELECT 1 FROM links l WHERE l.attempt=a.attempt
                     AND l.repository=p.repository AND l.number=p.number)",
                params![
                    pr.repository,
                    i64::try_from(pr.number)
                        .map_err(|_| RunError::Validation("PR number too large"))?
                ],
                |row| row.get(0),
            )?;
            u64::try_from(count).map_err(|_| RunError::Validation("invalid contribution count"))
        }
    }

    /// Read linked attempts once, preserving unknown weights.
    pub fn for_pr(&self, pr: &PullRequest, call: &Request<'_>) -> Result<Vec<Contribution>> {
        call.check()?;
        pr.validate()?;
        let number =
            i64::try_from(pr.number).map_err(|_| RunError::Validation("PR number too large"))?;
        let metadata = if self.1 == 1 {
            "NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL"
        } else if self.1 == 2 {
            "a.branch,p.head,CASE a.role WHEN 'review' THEN 'read_only' ELSE a.role END,a.model,a.account,NULL,NULL,NULL,NULL"
        } else {
            "a.branch,p.head,a.role,a.model,a.account,a.run,a.outcome,a.queued_at,a.finished_at"
        };
        let joins = if self.1 == 1 {
            ""
        } else {
            "LEFT JOIN branch_attempts a USING(attempt) LEFT JOIN branch_prs p ON p.project=a.project AND p.branch=a.branch AND p.repository=l.repository AND p.number=l.number"
        };
        let generation_join = if self.1 < 3 {
            ""
        } else {
            " AND p.generation=a.generation"
        };
        let truncated = if self.1 < 3 {
            "0"
        } else {
            "EXISTS(SELECT 1 FROM truncations t WHERE t.attempt=l.attempt)"
        };
        let sql = format!("SELECT l.attempt,l.weight,{metadata},
            (SELECT count(*) FROM links c WHERE c.attempt=l.attempt),
            NOT EXISTS(SELECT 1 FROM allocations s WHERE s.attempt=l.attempt),{truncated}
            FROM links l {joins}{generation_join} WHERE l.repository=?1 AND l.number=?2 ORDER BY 10,l.attempt LIMIT 10001");
        let mut query = self.0.prepare(&sql)?;
        let mut rows = query.query(params![pr.repository, number])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            if result.len() == 10000 {
                return Err(RunError::Validation("too many PR contributions"));
            }
            let raw: String = row.get(0)?;
            let attempt = raw
                .parse()
                .map_err(|_| RunError::Validation("invalid contribution attempt"))?;
            let weight: Option<i64> = row.get(1)?;
            let weight_millionths = weight
                .map(|w| {
                    u32::try_from(w)
                        .map_err(|_| RunError::Validation("invalid contribution weight"))
                })
                .transpose()?;
            let head: Option<String> = row.get(3)?;
            let head = head
                .map(|head| {
                    GitOid::parse(&head)
                        .map_err(|_| RunError::Validation("invalid contribution head"))
                })
                .transpose()?;
            result.push(Contribution {
                attempt,
                pull_request: pr.clone(),
                weight_millionths,
                branch: row.get(2)?,
                head,
                role: row.get(4)?,
                model: row.get(5)?,
                account: row.get(6)?,
                run: row
                    .get::<_, Option<String>>(7)?
                    .map(|id| {
                        id.parse()
                            .map_err(|_| RunError::Validation("invalid contribution run"))
                    })
                    .transpose()?,
                outcome: row
                    .get::<_, Option<String>>(8)?
                    .map(|raw| {
                        serde_json::from_str(&raw)
                            .map_err(|_| RunError::Validation("invalid contribution outcome"))
                    })
                    .transpose()?,
                queued_at: row
                    .get::<_, Option<i64>>(9)?
                    .map(|time| {
                        WallTime::from_micros(time)
                            .map_err(|_| RunError::Validation("invalid contribution time"))
                    })
                    .transpose()?,
                finished_at: row
                    .get::<_, Option<i64>>(10)?
                    .map(|time| {
                        WallTime::from_micros(time)
                            .map_err(|_| RunError::Validation("invalid contribution time"))
                    })
                    .transpose()?,
                pr_count: u64::try_from(row.get::<_, i64>(11)?)
                    .map_err(|_| RunError::Validation("invalid contribution count"))?,
                ambiguous: row.get::<_, i64>(11)? > 1 && row.get::<_, bool>(12)?,
                truncated: row.get(13)?,
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context};
    use provenant_core::CancelToken;
    #[test]
    fn branch_generations_keep_reused_names_disjoint()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = crate::RunStore::open(&context, &call)?;
        let input = crate::tests::input();
        store.create(&input, &call)?;
        let mut attempt = store.record(input.attempts[0].attempt_id, &call)?;
        let old = attempt.clone();
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let first = PullRequest {
            repository: "owner/repo".into(),
            number: 10,
        };
        let second = PullRequest {
            number: 11,
            ..first.clone()
        };
        let third = PullRequest {
            number: 12,
            ..first.clone()
        };
        let head = GitOid::parse(&"a".repeat(40))?;
        db.capture(&attempt, "feat/x", "worker", &call)?;
        db.bind(context.project_id, "feat/x", &first, head, &call)?;
        assert!(db.close(context.project_id, &first, &call)?);
        assert!(!db.close(context.project_id, &first, &call)?);
        attempt.attempt_id = AttemptId::from_bytes([20; 16]);
        db.capture(&attempt, "feat/x", "worker", &call)?;
        assert_eq!(db.for_pr(&first, &call)?.len(), 1);
        db.bind(context.project_id, "feat/x", &second, head, &call)?;
        // Late capture/bind replays cannot move an old attempt or reopen a PR.
        db.capture(&old, "feat/x", "worker", &call)?;
        db.bind(
            context.project_id,
            "feat/x",
            &first,
            GitOid::parse(&"b".repeat(40))?,
            &call,
        )?;
        assert_eq!(db.for_pr(&first, &call)?[0].attempt, old.attempt_id);
        assert_eq!(db.for_pr(&first, &call)?[0].head, Some(head));
        assert_eq!(db.for_pr(&second, &call)?[0].attempt, attempt.attempt_id);
        // Simultaneously open PRs retain links, with explicit ambiguity.
        db.bind(context.project_id, "feat/x", &third, head, &call)?;
        for pr in [&second, &third] {
            let rows = db.for_pr(pr, &call)?;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].attempt, attempt.attempt_id);
            assert_eq!(rows[0].pr_count, 2);
            assert!(rows[0].ambiguous);
            assert!(rows[0].weight_millionths.is_none());
        }
        db.close(context.project_id, &second, &call)?;
        attempt.attempt_id = AttemptId::from_bytes([21; 16]);
        db.capture(&attempt, "feat/x", "worker", &call)?;
        assert_eq!(db.for_pr(&second, &call)?.len(), 1);
        assert_eq!(db.for_pr(&third, &call)?.len(), 2);
        let overlapping = PullRequest {
            number: 14,
            ..first.clone()
        };
        db.bind(context.project_id, "feat/x", &overlapping, head, &call)?;
        let rows = db.for_pr(&overlapping, &call)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].attempt, attempt.attempt_id);
        // PR 12 spans the boundary, but PR 14 cannot absorb the earlier
        // generation's attempt even while PR 12 is still open.
        assert_eq!(db.for_pr(&third, &call)?.len(), 2);
        db.close(context.project_id, &overlapping, &call)?;
        db.close(context.project_id, &third, &call)?;
        attempt.attempt_id = AttemptId::from_bytes([22; 16]);
        db.capture(&attempt, "feat/x", "worker", &call)?;
        let fourth = PullRequest {
            number: 13,
            ..first.clone()
        };
        db.bind(context.project_id, "feat/x", &fourth, head, &call)?;
        assert_eq!(db.for_pr(&fourth, &call)?.len(), 1);
        assert_eq!(db.for_pr(&fourth, &call)?[0].attempt, attempt.attempt_id);
        Ok(())
    }

    #[test]
    fn link_cap_is_soft_and_reports_even_an_unlinked_pr()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = crate::RunStore::open(&context, &call)?;
        let input = crate::tests::input();
        store.create(&input, &call)?;
        let attempt = store.record(input.attempts[0].attempt_id, &call)?;
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let mut pr = PullRequest {
            repository: "owner/repo".into(),
            number: 1,
        };
        let head = GitOid::parse(&"a".repeat(40))?;
        db.capture(&attempt, "feat", "worker", &call)?;
        for number in 1..=MAX_LINKS as u64 + 1 {
            pr.number = number;
            db.bind(context.project_id, "feat", &pr, head, &call)?;
        }
        assert!(db.for_pr(&pr, &call)?.is_empty());
        assert!(db.is_bound(&pr, &call)?);
        assert!(db.truncated_for_pr(&pr, &call)?);
        db.link(attempt.attempt_id, &pr, &call)?;
        db.capture(&attempt, "feat", "worker", &call)?;
        db.close(context.project_id, &pr, &call)?;
        pr.number = 1;
        let rows = db.for_pr(&pr, &call)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pr_count, MAX_LINKS as u64);
        assert!(rows[0].truncated);
        assert!(rows[0].ambiguous);
        assert!(db.truncated_for_pr(&pr, &call)?);
        Ok(())
    }

    #[test]
    fn late_close_moves_later_attempts_to_the_next_generation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = crate::RunStore::open(&context, &call)?;
        let input = crate::tests::input();
        store.create(&input, &call)?;
        let mut attempt = store.record(input.attempts[0].attempt_id, &call)?;
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let old = PullRequest {
            repository: "owner/repo".into(),
            number: 10,
        };
        let new = PullRequest {
            number: 11,
            ..old.clone()
        };
        let head = GitOid::parse(&"a".repeat(40))?;
        db.bind(context.project_id, "feat/x", &old, head, &call)?;
        // Ids 1 is before the merge; 2 and 3 are dispatched after the UI merge.
        for (id, time) in [(1, 100), (2, 300), (3, 400)] {
            attempt.attempt_id = AttemptId::from_bytes([id; 16]);
            attempt.queued_at = WallTime::from_micros(time)?;
            db.capture(&attempt, "feat/x", "worker", &call)?;
        }
        assert_eq!(db.for_pr(&old, &call)?.len(), 3);
        let merged = Some(WallTime::from_micros(200)?);
        assert!(db.close_at(context.project_id, &old, merged, &call)?);
        assert!(!db.close_at(context.project_id, &old, merged, &call)?);
        db.bind(context.project_id, "feat/x", &new, head, &call)?;
        let ids = |pr: &PullRequest| -> std::result::Result<Vec<AttemptId>, RunError> {
            Ok(db
                .for_pr(pr, &call)?
                .iter()
                .map(|row| row.attempt)
                .collect())
        };
        assert_eq!(ids(&old)?, [AttemptId::from_bytes([1; 16])]);
        assert_eq!(ids(&new)?, [2, 3].map(|id| AttemptId::from_bytes([id; 16])));
        assert_eq!(db.reassigned_from_pr(&old, &call)?, 2);
        Ok(())
    }

    fn fixture(
        dir: &Path,
    ) -> std::result::Result<
        (
            crate::ExecutionContext,
            crate::AttemptRecord,
            PullRequest,
            PullRequest,
        ),
        Box<dyn std::error::Error>,
    > {
        let context = context(dir)?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = crate::RunStore::open(&context, &call)?;
        let input = crate::tests::input();
        store.create(&input, &call)?;
        let attempt = store.record(input.attempts[0].attempt_id, &call)?;
        let old = PullRequest {
            repository: "owner/repo".into(),
            number: 10,
        };
        let new = PullRequest {
            number: 11,
            ..old.clone()
        };
        Ok((context, attempt, old, new))
    }
    #[test]
    fn late_close_boundary_null_time_and_sealed_attempts_stay_put()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (context, mut attempt, old, new) = fixture(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let head = GitOid::parse(&"a".repeat(40))?;
        db.bind(context.project_id, "feat/x", &old, head, &call)?;
        let name = |id: u8| AttemptId::from_bytes([id; 16]);
        for (id, time) in [(1, 100), (2, 200), (3, 300), (4, 300), (5, 300)] {
            attempt.attempt_id = name(id);
            attempt.queued_at = WallTime::from_micros(time)?;
            db.capture(&attempt, "feat/x", "worker", &call)?;
        }
        // 3 is sealed; 4 has no queue time; 2 sits exactly on the boundary.
        db.allocate(name(3), &[(old.clone(), WEIGHT_ONE)], &call)?;
        db.0.execute(
            "UPDATE branch_attempts SET queued_at=NULL WHERE attempt=?1",
            [name(4).to_string()],
        )?;
        let before = db.for_pr(&old, &call)?.len();
        assert_eq!(before, 5);
        db.close_at(
            context.project_id,
            &old,
            Some(WallTime::from_micros(200)?),
            &call,
        )?;
        db.bind(context.project_id, "feat/x", &new, head, &call)?;
        let left: Vec<_> = db.for_pr(&old, &call)?.iter().map(|r| r.attempt).collect();
        assert_eq!(left.len(), 4);
        assert!(!left.contains(&name(5)));
        let moved: Vec<_> = db.for_pr(&new, &call)?.iter().map(|r| r.attempt).collect();
        assert_eq!(moved, [name(5)]);
        assert_eq!(db.unplaced_for_pr(&old, &call)?, 1);
        assert_eq!(db.reassigned_from_pr(&old, &call)?, 1);
        Ok(())
    }

    #[test]
    fn close_without_a_time_moves_nothing() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (context, mut attempt, old, new) = fixture(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let head = GitOid::parse(&"a".repeat(40))?;
        db.bind(context.project_id, "feat/x", &old, head, &call)?;
        attempt.attempt_id = AttemptId::from_bytes([1; 16]);
        attempt.queued_at = WallTime::from_micros(900)?;
        db.capture(&attempt, "feat/x", "worker", &call)?;
        db.close_at(context.project_id, &old, None, &call)?;
        db.bind(context.project_id, "feat/x", &new, head, &call)?;
        assert_eq!(db.for_pr(&old, &call)?.len(), 1);
        assert!(db.for_pr(&new, &call)?.is_empty());
        assert_eq!(db.reassigned_from_pr(&old, &call)?, 0);
        Ok(())
    }

    #[test]
    fn v3_store_migrates_to_v4_keeping_its_links()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (context, mut attempt, old, _) = fixture(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let head = GitOid::parse(&"a".repeat(40))?;
        db.bind(context.project_id, "feat/x", &old, head, &call)?;
        attempt.attempt_id = AttemptId::from_bytes([1; 16]);
        db.capture(&attempt, "feat/x", "worker", &call)?;
        db.0.execute_batch("DROP TABLE reassignments; PRAGMA user_version=3;")?;
        drop(db);
        let v3 = Contributions::open_read_only(context.state_root.as_path(), &call)?
            .ok_or("v3 store")?;
        assert_eq!(v3.for_pr(&old, &call)?.len(), 1);
        assert_eq!(v3.reassigned_from_pr(&old, &call)?, 0);
        drop(v3);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        assert_eq!(db.for_pr(&old, &call)?.len(), 1);
        let tables: i64 = db.0.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='reassignments'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tables, 1);
        assert_eq!(
            db.0.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            4
        );
        Ok(())
    }

    #[test]
    fn reader_orders_by_queue_time_then_attempt()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = crate::RunStore::open(&context, &call)?;
        let input = crate::tests::input();
        store.create(&input, &call)?;
        let mut attempt = store.record(input.attempts[0].attempt_id, &call)?;
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let pr = PullRequest {
            repository: "owner/repo".into(),
            number: 10,
        };
        for (id, time) in [(3, 1), (1, 2), (2, 1)] {
            attempt.attempt_id = AttemptId::from_bytes([id; 16]);
            attempt.queued_at = WallTime::from_micros(time)?;
            db.capture(&attempt, "feat", "read_only", &call)?;
        }
        db.bind(
            context.project_id,
            "feat",
            &pr,
            GitOid::parse(&"a".repeat(40))?,
            &call,
        )?;
        let rows = db.for_pr(&pr, &call)?;
        assert_eq!(
            rows.iter().map(|row| row.attempt).collect::<Vec<_>>(),
            [2, 3, 1].map(|id| AttemptId::from_bytes([id; 16]))
        );
        Ok(())
    }
    #[test]
    fn bindings_capture_late_attempts_and_retain_resolved_placement()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        assert!(Contributions::open_read_only(context.state_root.as_path(), &call)?.is_none());
        let store = crate::RunStore::open(&context, &call)?;
        let input = crate::tests::input();
        store.create(&input, &call)?;
        let id = input.attempts[0].attempt_id;
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let pr = PullRequest {
            repository: "owner/repo".into(),
            number: 42,
        };
        let head = GitOid::parse(&"a".repeat(40))?;
        db.bind(context.project_id, "feat", &pr, head, &call)?;
        let queued = store.record(id, &call)?;
        db.capture(&queued, "feat", "worker", &call)?;
        db.capture(&queued, "feat", "worker", &call)?;
        assert!(db.for_pr(&pr, &call)?[0].model.is_none());
        let mut route = crate::tests::route();
        route.account_label = Some("work".into());
        store.mark_starting(&queued, &route, &[], &Default::default(), None, &call)?;
        store.withdraw(
            id,
            crate::AttemptState::Starting {
                extra: Default::default(),
            },
            crate::Outcome::Cancelled,
            "cancelled",
            queued.queued_at,
            &call,
        )?;
        // Replaying dispatch after cancellation cannot erase known placement.
        db.capture(&queued, "feat", "worker", &call)?;
        db.allocate(id, &[(pr.clone(), WEIGHT_ONE)], &call)?;
        let other = PullRequest {
            number: 43,
            ..pr.clone()
        };
        let mut unsealed = queued.clone();
        unsealed.attempt_id = AttemptId::from_bytes([11; 16]);
        db.capture(&unsealed, "feat", "worker", &call)?;
        db.bind(context.project_id, "feat", &other, head, &call)?;
        db.bind(context.project_id, "feat", &other, head, &call)?;
        let other_rows = db.for_pr(&other, &call)?;
        assert_eq!(other_rows.len(), 1);
        assert_eq!(other_rows[0].attempt, unsealed.attempt_id);
        assert_eq!(db.skipped_sealed_for_pr(&other, &call)?, 1);
        assert_eq!(db.skipped_sealed_for_pr(&pr, &call)?, 0);
        db.bind(context.project_id, "feat", &pr, head, &call)?;
        let rows = Contributions::open_read_only(context.state_root.as_path(), &call)?
            .ok_or("missing store")?
            .for_pr(&pr, &call)?;
        let rows: Vec<_> = rows.into_iter().filter(|row| row.attempt == id).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_deref(), Some("fixture/model"));
        assert_eq!(rows[0].account.as_deref(), Some("work"));
        assert_eq!(rows[0].head, Some(head));
        assert_eq!(rows[0].weight_millionths, Some(WEIGHT_ONE));
        assert_eq!(rows[0].run, Some(queued.run_id));
        assert_eq!(rows[0].queued_at, Some(queued.queued_at));
        assert_eq!(rows[0].outcome, Some(crate::Outcome::Cancelled));
        assert_eq!(rows[0].finished_at, Some(queued.queued_at));
        assert_eq!(rows[0].pr_count, 1);
        assert!(!rows[0].ambiguous);
        assert!(matches!(
            db.capture(&queued, "other", "worker", &call),
            Err(RunError::Conflict)
        ));
        let mut child = queued.clone();
        child.attempt_id = AttemptId::from_bytes([10; 16]);
        child.resolved_route = Some(crate::RouteRecord {
            model: Some("forged/model".into()),
            ..Default::default()
        });
        store.capture_child_contribution(id, &child, &call)?;
        store.capture_child_contribution(id, &child, &call)?;
        let rows = db.for_pr(&pr, &call)?;
        assert_eq!(rows.len(), 3);
        let row = rows
            .iter()
            .find(|row| row.attempt == child.attempt_id)
            .ok_or("missing child")?;
        assert_eq!(row.role.as_deref(), Some("read_only"));
        assert!(row.model.is_none());
        assert!(row.account.is_none());
        Ok(())
    }
    #[test]
    fn migrates_legacy_links_without_inventing_metadata()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let pr = PullRequest {
            repository: "owner/repo".into(),
            number: 42,
        };
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        db.link(AttemptId::from_bytes([1; 16]), &pr, &call)?;
        db.0.execute_batch(
            "DROP TABLE branch_attempts; DROP TABLE branch_prs; DROP TABLE branch_generations; DROP TABLE truncations; DROP TABLE reassignments; PRAGMA user_version=1;",
        )?;
        drop(db);
        let legacy = Contributions::open_read_only(context.state_root.as_path(), &call)?
            .ok_or("legacy store")?;
        let rows = legacy.for_pr(&pr, &call)?;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].role.is_none());
        assert!(rows[0].run.is_none());
        assert!(rows[0].queued_at.is_none());
        assert_eq!(
            legacy
                .0
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            1
        );
        drop(legacy);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let rows = db.for_pr(&pr, &call)?;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].role.is_none());
        assert!(rows[0].weight_millionths.is_none());
        assert!(rows[0].head.is_none());
        Ok(())
    }
    #[test]
    fn v2_metadata_is_readable_and_migrates_without_losing_links()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let pr = PullRequest {
            repository: "owner/repo".into(),
            number: 42,
        };
        let id = AttemptId::from_bytes([1; 16]);
        let head = "a".repeat(40);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        db.link(id, &pr, &call)?;
        db.0.execute_batch(
            "DROP TABLE branch_attempts; DROP TABLE branch_prs;
             DROP TABLE branch_generations; DROP TABLE truncations; DROP TABLE reassignments;
             CREATE TABLE branch_attempts (
                 attempt TEXT PRIMARY KEY, project TEXT NOT NULL, branch TEXT NOT NULL,
                 role TEXT NOT NULL, model TEXT, account TEXT) STRICT;
             CREATE TABLE branch_prs (
                 project TEXT NOT NULL, branch TEXT NOT NULL, repository TEXT NOT NULL,
                 number INTEGER NOT NULL, head TEXT NOT NULL,
                 PRIMARY KEY(project,branch,repository,number)) STRICT;
             PRAGMA user_version=2;",
        )?;
        db.0.execute(
            "INSERT INTO branch_attempts VALUES (?1,?2,'feat','review','fixture/model',NULL)",
            params![id.to_string(), context.project_id.to_string()],
        )?;
        db.0.execute(
            "INSERT INTO branch_prs VALUES (?1,'feat','owner/repo',42,?2)",
            params![context.project_id.to_string(), head],
        )?;
        drop(db);
        let legacy =
            Contributions::open_read_only(context.state_root.as_path(), &call)?.ok_or("store")?;
        let before = legacy.for_pr(&pr, &call)?;
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].role.as_deref(), Some("read_only"));
        assert_eq!(before[0].model.as_deref(), Some("fixture/model"));
        assert!(before[0].run.is_none());
        assert_eq!(
            legacy
                .0
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            2
        );
        drop(legacy);
        let migrated = Contributions::open(context.state_root.as_path(), &call)?;
        assert_eq!(migrated.for_pr(&pr, &call)?, before);
        migrated.close(context.project_id, &pr, &call)?;
        assert_eq!(
            Contributions::generation(&migrated.0, context.project_id, "feat")?,
            1
        );
        Ok(())
    }

    #[test]
    fn allocation_roundtrip() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let first = PullRequest {
            repository: "owner/repo".into(),
            number: 1,
        };
        let second = PullRequest {
            repository: "owner/repo".into(),
            number: 2,
        };
        let attempts = [
            AttemptId::from_bytes([1; 16]),
            AttemptId::from_bytes([2; 16]),
            AttemptId::from_bytes([3; 16]),
            AttemptId::from_bytes([4; 16]),
        ];
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        for id in attempts {
            db.link(id, &first, &call)?;
            db.link(id, &first, &call)?;
        }
        db.link(attempts[0], &second, &call)?;
        assert_eq!(db.for_pr(&first, &call)?.len(), 4);
        assert!(
            db.for_pr(&first, &call)?
                .iter()
                .all(|row| row.weight_millionths.is_none())
        );
        assert!(
            db.allocate(
                attempts[0],
                &[(first.clone(), 500_000), (second.clone(), 499_999)],
                &call
            )
            .is_err()
        );
        assert!(
            db.allocate(attempts[0], &[(first.clone(), 1_000_000)], &call)
                .is_err()
        );
        let unallocated = db.for_pr(&first, &call)?;
        assert_eq!(unallocated[0].pr_count, 2);
        assert!(unallocated[0].ambiguous);
        assert!(unallocated[1..].iter().all(|row| !row.ambiguous));
        let weights = [(first.clone(), 250_000), (second.clone(), 750_000)];
        db.allocate(attempts[0], &weights, &call)?;
        db.allocate(attempts[0], &weights, &call)?;
        assert!(
            db.allocate(
                attempts[0],
                &[(first.clone(), 500_000), (second.clone(), 500_000)],
                &call
            )
            .is_err()
        );
        drop(db);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let rows = db.for_pr(&first, &call)?;
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].weight_millionths, Some(250_000));
        assert_eq!(rows[0].pr_count, 2);
        assert!(!rows[0].ambiguous);
        assert!(rows[1..].iter().all(|row| row.weight_millionths.is_none()));
        Ok(())
    }
}
