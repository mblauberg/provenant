//! Durable host-owned pull request contribution links.
use crate::{
    Request, Result, RunError,
    db::{immediate, open_database},
};
use provenant_core::AttemptId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

const FILE: &str = "pr-contributions.sqlite3";
/// Allocation is represented in millionths for exact validation.
pub const WEIGHT_ONE: u32 = 1_000_000;
const MAX_LINKS: usize = 256;

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
}

/// Host callers supply facts from authoritative dispatch or workflow records.
pub struct Contributions(Connection);
impl Contributions {
    pub fn open(host: &Path, call: &Request<'_>) -> Result<Self> {
        let (_, db, _lock) = open_database(host, FILE, call)?;
        let version = call.retry(|| match provenant_sqlite::read_version(&db, 1) {
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
        Ok(Self(db))
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
            let mut query = self.0.prepare("SELECT attempt, weight FROM links WHERE repository=?1 AND number=?2 ORDER BY attempt LIMIT 10001")?;
            let mut rows = query.query(params![pr.repository, number])?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                if result.len() == 10000 { return Err(RunError::Validation("too many PR contributions")); }
                let raw: String = row.get(0)?;
                let attempt = raw.parse().map_err(|_| RunError::Validation("invalid contribution attempt"))?;
                let weight: Option<i64> = row.get(1)?;
                let weight_millionths = weight.map(|w| u32::try_from(w).map_err(|_| RunError::Validation("invalid contribution weight"))).transpose()?;
                result.push(Contribution { attempt, pull_request: pr.clone(), weight_millionths });
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
    fn pr_contribution_roundtrip() -> std::result::Result<(), Box<dyn std::error::Error>> {
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
