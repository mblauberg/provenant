//! Durable host-owned pull request contribution links.
use crate::{
    Request, Result, RunError,
    db::{immediate, open_database},
};
use provenant_core::{AttemptId, GitOid, ProjectId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

const FILE: &str = "pr-contributions.sqlite3";
/// Allocation is represented in millionths for exact validation.
pub const WEIGHT_ONE: u32 = 1_000_000;
const MAX_LINKS: usize = 256;

/// Reviews only associate with a verified, registered linked worktree.
pub(crate) fn review_branch(
    host: &Path,
    project: ProjectId,
    cwd: &Path,
) -> Result<Option<provenant_confinement::repo::GitRefName>> {
    let writer = match crate::resolve_writer(host, cwd, cwd) {
        Ok(writer) => writer,
        // Ordinary read-only dispatches also run outside linked worktrees.
        Err(crate::RunError::Validation(_)) | Err(crate::RunError::Writer(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    match crate::prepare_writer(&writer, host, project) {
        Ok(writer) => Ok(Some(writer.branch)),
        Err(crate::RunError::WorktreeUnregistered(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

impl crate::RunStore {
    pub(crate) fn contribution_attempts(
        &self,
        run: provenant_core::RunId,
        call: &Request<'_>,
    ) -> Result<Vec<crate::AttemptRecord>> {
        call.retry(|| {
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
        })
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
        let branch: Option<(String, String)> =
            db.0.query_row(
                "SELECT branch,role FROM branch_attempts WHERE attempt=?1 AND project=?2",
                params![prior.to_string(), self.context.project_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let branch = replacement_branch
            .map(|branch| (branch.to_owned(), "repair".to_owned()))
            .or(branch);
        if let Some((branch, role)) = branch {
            for attempt in self.contribution_attempts(run, call)? {
                db.capture(
                    &attempt,
                    &branch,
                    if role == "review" { "review" } else { "repair" },
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
        let branch: Option<String> =
            db.0.query_row(
                "SELECT branch FROM branch_attempts WHERE attempt=?1 AND project=?2",
                params![parent.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(branch) = branch {
            let mut local = record.clone();
            // Lane-written route fields cannot establish host placement.
            local.resolved_route = None;
            db.capture(&local, &branch, "review", call)?;
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
}

/// Host callers supply facts from authoritative dispatch or workflow records.
pub struct Contributions(Connection);
impl Contributions {
    /// Read without creating or migrating a store, including committed WAL data.
    pub fn open_read_only(host: &Path, call: &Request<'_>) -> Result<Option<Self>> {
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
        crate::db::companions(&root, FILE, call)?;
        let db = provenant_sqlite::open_connection(&host.join(FILE), true)?;
        match provenant_sqlite::read_version(&db, 2) {
            Ok(2) => Ok(Some(Self(db))),
            Ok(_) => Err(RunError::Validation(
                "contribution store needs host migration",
            )),
            Err(provenant_sqlite::VersionError::Unsupported(version)) => {
                Err(RunError::Version(version))
            }
            Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
            Err(provenant_sqlite::VersionError::Sqlite(error)) => Err(error.into()),
        }
    }
    pub fn open(host: &Path, call: &Request<'_>) -> Result<Self> {
        let (_, db, _lock) = open_database(host, FILE, call)?;
        let version = call.retry(|| match provenant_sqlite::read_version(&db, 2) {
            Ok(v) => Ok(v),
            Err(provenant_sqlite::VersionError::Unsupported(v)) => Err(RunError::Version(v)),
            Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
            Err(provenant_sqlite::VersionError::Sqlite(e)) => Err(e.into()),
        })?;
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
        Ok(Self(db))
    }

    /// Snapshot a registered local worktree at dispatch, independent of outcome.
    pub(crate) fn capture(
        &self,
        record: &crate::AttemptRecord,
        branch: &str,
        role: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        immediate(&self.0, call, |tx| {
            tx.execute(
                "INSERT INTO branch_attempts(attempt,project,branch,role) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(attempt) DO NOTHING",
                params![
                    record.attempt_id.to_string(),
                    record.project_id.to_string(),
                    branch,
                    role
                ],
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
            Self::resolve(tx, record.project_id, branch)?;
            Ok(())
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
            tx.execute(
                "INSERT INTO branch_prs VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(project,branch,repository,number) DO UPDATE SET head=excluded.head",
                params![
                    project.to_string(),
                    branch,
                    pr.repository,
                    number,
                    head.to_string()
                ],
            )?;
            Self::resolve(tx, project, branch)
        })
    }

    fn resolve(tx: &Connection, project: ProjectId, branch: &str) -> Result<()> {
        let sealed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM branch_attempts a JOIN branch_prs p USING(project,branch)
             JOIN allocations s USING(attempt) LEFT JOIN links l
             ON l.attempt=a.attempt AND l.repository=p.repository AND l.number=p.number
             WHERE a.project=?1 AND a.branch=?2 AND l.attempt IS NULL)",
            params![project.to_string(), branch],
            |row| row.get(0),
        )?;
        if sealed {
            return Err(RunError::Validation("attempt allocation already sealed"));
        }
        tx.execute(
            "INSERT INTO links(attempt,repository,number)
             SELECT a.attempt,p.repository,p.number FROM branch_attempts a
             JOIN branch_prs p USING(project,branch) WHERE a.project=?1 AND a.branch=?2 ON CONFLICT DO NOTHING",
            params![project.to_string(), branch],
        )?;
        let excessive: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM links l JOIN branch_attempts a USING(attempt)
             WHERE a.project=?1 AND a.branch=?2 GROUP BY l.attempt HAVING count(*)>?3)",
            params![project.to_string(), branch, MAX_LINKS as i64],
            |row| row.get(0),
        )?;
        if excessive {
            return Err(RunError::Validation("too many PR links"));
        }
        Ok(())
    }

    /// Retain resolved placement; requested routes and missing facts stay unknown.
    pub(crate) fn route(&self, record: &crate::AttemptRecord, call: &Request<'_>) -> Result<()> {
        let Some(route) = record.resolved_route.as_ref() else {
            return Ok(());
        };
        immediate(&self.0, call, |tx| {
            tx.execute("UPDATE branch_attempts SET model=COALESCE(?2,model),account=COALESCE(?3,account) WHERE attempt=?1",
                params![record.attempt_id.to_string(), route.model, route.account_label])?;
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
            let sealed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM allocations WHERE attempt=?1)",
                [attempt.to_string()],
                |row| row.get(0),
            )?;
            if sealed {
                return Err(RunError::Validation("attempt allocation already sealed"));
            }
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM links WHERE attempt=?1",
                [attempt.to_string()],
                |row| row.get(0),
            )?;
            if count >= MAX_LINKS as i64 {
                return Err(RunError::Validation("too many PR links"));
            }
            tx.execute(
                "INSERT INTO links(attempt, repository, number) VALUES (?1,?2,?3)",
                params![attempt.to_string(), pr.repository, number],
            )?;
            Ok(())
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

    /// Read linked attempts once, preserving unknown weights.
    pub fn for_pr(&self, pr: &PullRequest, call: &Request<'_>) -> Result<Vec<Contribution>> {
        pr.validate()?;
        let number =
            i64::try_from(pr.number).map_err(|_| RunError::Validation("PR number too large"))?;
        call.retry(|| {
            let mut query = self.0.prepare(
                "SELECT l.attempt,l.weight,a.branch,p.head,a.role,a.model,a.account
                FROM links l LEFT JOIN branch_attempts a USING(attempt)
                LEFT JOIN branch_prs p ON p.project=a.project AND p.branch=a.branch
                    AND p.repository=l.repository AND p.number=l.number
                WHERE l.repository=?1 AND l.number=?2 ORDER BY l.attempt LIMIT 10001",
            )?;
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
                });
            }
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context};
    use provenant_core::CancelToken;
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
            crate::AttemptState::Starting,
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
        assert!(matches!(
            db.bind(context.project_id, "feat", &other, head, &call),
            Err(RunError::Validation("attempt allocation already sealed"))
        ));
        assert!(db.for_pr(&other, &call)?.is_empty());
        db.bind(context.project_id, "feat", &pr, head, &call)?;
        let rows = Contributions::open_read_only(context.state_root.as_path(), &call)?
            .ok_or("missing store")?
            .for_pr(&pr, &call)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_deref(), Some("fixture/model"));
        assert_eq!(rows[0].account.as_deref(), Some("work"));
        assert_eq!(rows[0].head, Some(head));
        assert_eq!(rows[0].weight_millionths, Some(WEIGHT_ONE));
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
        assert_eq!(rows.len(), 2);
        let row = rows
            .iter()
            .find(|row| row.attempt == child.attempt_id)
            .ok_or("missing child")?;
        assert_eq!(row.role.as_deref(), Some("review"));
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
            "DROP TABLE branch_attempts; DROP TABLE branch_prs; PRAGMA user_version=1;",
        )?;
        drop(db);
        let db = Contributions::open(context.state_root.as_path(), &call)?;
        let rows = db.for_pr(&pr, &call)?;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].role.is_none());
        assert!(rows[0].weight_millionths.is_none());
        assert!(rows[0].head.is_none());
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
        assert!(rows[1..].iter().all(|row| row.weight_millionths.is_none()));
        Ok(())
    }
}
