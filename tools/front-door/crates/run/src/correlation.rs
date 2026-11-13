//! Host-owned membership facts, sealed on the host store's attempt rows.
//! Lane stores are never lookup authority.
use crate::{db::open_database, *};
use provenant_core::{AttemptId, GitOid, ProjectId};
use provenant_records::AbsolutePath;
use rusqlite::{OptionalExtension, params};
const ROW_LIMIT: usize = 64;
const MEMBER_BYTES: usize = 64 * 1024;
/// Repository is the verified canonical Git common directory. Head is the
/// host-observed launch HEAD. No selector accepts a nested store locator.
#[derive(Debug, Clone)]
pub struct WorkSelector {
    pub project: ProjectId,
    /// The graph the work belongs to; `node` and `attempt` narrow it.
    pub graph: String,
    pub node: Option<String>,
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
    call.check()?;
    let (_, db, initialisation) = open_database(host.as_path(), "run.sqlite3", call)?;
    provenant_sqlite::ensure_schema(&db, crate::db::SCHEMA)?;
    drop(initialisation);
    // Exact predicates let SQLite use the key index even when a selector is
    // absent or names unknown work.
    let node = if selection.node.is_some() {
        "node=?3"
    } else {
        "?3 IS NULL"
    };
    let attempt = if selection.attempt.is_some() {
        "id=?4"
    } else {
        "?4 IS NULL"
    };
    let mut query = db.prepare(&format!(
        "SELECT substr(repository,1,{read_bytes}),head FROM attempts
        WHERE graph=?2 AND project=?1 AND {node} AND {attempt} AND sealed=1 AND refused=0
        ORDER BY node,id LIMIT {row_limit}",
        read_bytes = MEMBER_BYTES + 1,
        row_limit = ROW_LIMIT + 1
    ))?;
    let mut rows = query.query(params![
        selection.project.to_string(),
        selection.graph,
        selection.node,
        selection.attempt.map(|id| id.to_string())
    ])?;
    let mut coverage = WorkCoverage::Unindexed;
    let qualified = selection.node.is_some() || selection.attempt.is_some();
    for n in 0..=ROW_LIMIT {
        call.check()?;
        let Some(row) = rows.next()? else {
            if !qualified && coverage != WorkCoverage::Unindexed {
                return Ok(coverage.max(WorkCoverage::Partial));
            }
            return Ok(coverage);
        };
        if n == ROW_LIMIT {
            return Ok(if coverage == WorkCoverage::Mismatch {
                coverage
            } else {
                WorkCoverage::Limit
            });
        }
        let repository: Option<String> = row.get(0)?;
        if repository
            .as_ref()
            .is_some_and(|raw| raw.len() > MEMBER_BYTES)
        {
            return Err(RunError::Validation("work membership size"));
        }
        let repository: Option<AbsolutePath> = repository
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|_| RunError::Validation("work membership repository"))?;
        let head = row
            .get::<_, Option<String>>(1)?
            .map(|raw| GitOid::parse(&raw))
            .transpose()
            .map_err(|_| RunError::Validation("work membership head"))?;
        if selection
            .repository
            .as_ref()
            .is_some_and(|repo| repository.as_ref().is_some_and(|v| v != repo))
            || selection
                .head
                .is_some_and(|wanted| head.is_some_and(|v| v != wanted))
        {
            coverage = WorkCoverage::Mismatch;
            continue;
        }
        if selection.repository.is_some() && repository.is_none() {
            coverage = coverage.max(WorkCoverage::RepositoryUnavailable);
        } else if selection.head.is_some() && head.is_none() {
            coverage = coverage.max(WorkCoverage::HeadUnavailable);
        } else {
            coverage = coverage.max(WorkCoverage::Verified);
        }
    }
    Ok(coverage)
}
impl RunStore {
    /// Seal launch membership facts on a queued attempt row. A replay with the
    /// same facts succeeds; different facts conflict.
    pub(crate) fn seal_membership(
        &self,
        attempt: AttemptId,
        repository: Option<&AbsolutePath>,
        head: Option<GitOid>,
        call: &Request<'_>,
    ) -> Result<()> {
        let repository = repository
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| RunError::Validation("work membership repository"))?;
        if repository
            .as_ref()
            .is_some_and(|raw| raw.len() > MEMBER_BYTES)
        {
            return Err(RunError::TooLarge {
                limit: MEMBER_BYTES as u64,
            });
        }
        let head = head.map(|head| head.to_string());
        self.write(call, |tx| {
            let (sealed, terminal, stored_repository, stored_head): (bool, bool, Option<String>, Option<String>) = tx
                .query_row(
                    "SELECT sealed,terminal,repository,head FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                    params![attempt.to_string(), self.context.project_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .ok_or(RunError::NotFound)?;
            if sealed {
                return if stored_repository == repository && stored_head == head {
                    Ok(())
                } else {
                    Err(RunError::Conflict)
                };
            }
            if terminal {
                return Err(RunError::Conflict);
            }
            tx.execute(
                "UPDATE attempts SET sealed=1,repository=?1,head=?2 WHERE id=?3 AND sealed=0 AND terminal=0",
                params![repository, head, attempt.to_string()],
            )?;
            Ok(())
        })
    }
    pub(crate) fn seal_work(
        &self,
        record: &AttemptRecord,
        plan: &PreparedLaunch,
        call: &Request<'_>,
    ) -> Result<()> {
        if self.context.state_root.as_path() != self.writer_registry() {
            return Ok(());
        }
        self.seal_membership(
            record.attempt_id,
            plan.repository.as_ref(),
            plan.observed_head,
            call,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::CancelToken;
    use std::path::Path;
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
        let store = RunStore::open(&context, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let repository = AbsolutePath::new(Path::new("/repo/.git"))?;
        let head = GitOid::parse(&"a".repeat(40))?;
        let key = store.record(input.attempt_id, &call)?.key;
        let mut selection = WorkSelector {
            project: context.project_id,
            graph: key.graph().to_owned(),
            node: Some(key.node().to_owned()),
            attempt: Some(input.attempt_id),
            repository: Some(repository.clone()),
            head: Some(head),
        };
        let host = &context.state_root;
        assert_eq!(
            validate_work(host, &selection, &call)?,
            WorkCoverage::Unindexed
        );
        let mut run_only = selection.clone();
        run_only.node = None;
        run_only.attempt = None;
        assert_eq!(
            validate_work(host, &run_only, &call)?,
            WorkCoverage::Unindexed
        );
        let id = input.attempt_id;
        store.seal_membership(id, Some(&repository), Some(head), &call)?;
        store.seal_membership(id, Some(&repository), Some(head), &call)?;
        assert!(matches!(
            store.seal_membership(id, None, Some(head), &call),
            Err(RunError::Conflict)
        ));
        assert_eq!(
            validate_work(host, &selection, &call)?,
            WorkCoverage::Verified
        );
        assert_eq!(
            validate_work(host, &run_only, &call)?,
            WorkCoverage::Partial
        );
        selection.head = Some(GitOid::parse(&"b".repeat(40))?);
        assert_eq!(
            validate_work(host, &selection, &call)?,
            WorkCoverage::Mismatch
        );
        selection.head = Some(head);
        selection.repository = Some(AbsolutePath::new(Path::new("/other/.git"))?);
        assert_eq!(
            validate_work(host, &selection, &call)?,
            WorkCoverage::Mismatch
        );
        selection.repository = Some(repository);
        for field in 0..4 {
            let mut foreign = selection.clone();
            match field {
                0 => foreign.project = ProjectId::from_bytes([99; 16]),
                1 => foreign.graph = "other-graph".into(),
                2 => foreign.node = Some("other-node".into()),
                _ => foreign.attempt = Some(AttemptId::from_bytes([99; 16])),
            }
            assert_eq!(
                validate_work(host, &foreign, &call)?,
                WorkCoverage::Unindexed
            );
        }
        cancel.cancel();
        assert!(matches!(
            validate_work(host, &selection, &call),
            Err(RunError::Cancelled)
        ));
        Ok(())
    }

    #[test]
    fn request_correlation_missing_facts_and_bounds() -> TestResult {
        let (_temp, context, clock, cancel) = fixture()?;
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let id = input.attempt_id;
        store.seal_membership(id, None, None, &call)?;
        let key = store.record(id, &call)?.key;
        let mut selection = WorkSelector {
            project: context.project_id,
            graph: key.graph().to_owned(),
            node: Some(key.node().to_owned()),
            attempt: None,
            repository: Some(AbsolutePath::new(Path::new("/repo/.git"))?),
            head: None,
        };
        let host = &context.state_root;
        assert_eq!(
            validate_work(host, &selection, &call)?,
            WorkCoverage::RepositoryUnavailable
        );
        selection.repository = None;
        selection.head = Some(GitOid::parse(&"a".repeat(40))?);
        assert_eq!(
            validate_work(host, &selection, &call)?,
            WorkCoverage::HeadUnavailable
        );
        let long = AbsolutePath::new(Path::new(&format!("/{}", "x".repeat(66000))))?;
        assert!(matches!(
            store.seal_membership(id, Some(&long), None, &call),
            Err(RunError::TooLarge { limit: 65536 })
        ));
        Ok(())
    }

    #[test]
    fn request_correlation_host_resumes_seal_through_start() -> TestResult {
        let (_temp, context, clock, cancel) = fixture()?;
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let original = input();
        store.create(&original, &call)?;
        let mut prior = original.attempt_id;
        store.start(
            original.attempt_id,
            &crate::dispatch_tests::Ready,
            &|_, _| Err(RunError::Validation("fixture host handoff")),
            &call,
        )?;
        for (fresh, n) in [(false, 20_u8), (true, 30)] {
            let next = Resume {
                extend: Vec::new(),
                prior,
                expected_generation: 0,
                operation_id: provenant_core::OperationId::parse(&format!("resume-{n}"))?,
                attempt_id: AttemptId::from_bytes([n + 1; 16]),
                prompt: "continue".into(),
                created_at: original.created_at,
            };
            let run = if fresh {
                store.resume_fresh(&next, "fixture fresh", &call)?
            } else {
                store.resume(&next, &call)?
            };
            let key = &run.key;
            let selector = WorkSelector {
                project: context.project_id,
                graph: key.graph().to_owned(),
                node: Some(key.node().to_owned()),
                attempt: Some(next.attempt_id),
                repository: None,
                head: None,
            };
            assert_eq!(
                validate_work(&context.state_root, &selector, &call)?,
                WorkCoverage::Unindexed
            );
            let handed = std::cell::Cell::new(false);
            store.start(
                run.attempt_id,
                &crate::dispatch_tests::Ready,
                &|record, _| {
                    assert_eq!(record.attempt_id, next.attempt_id);
                    handed.set(true);
                    Err(RunError::Validation("fixture host handoff"))
                },
                &call,
            )?;
            assert!(handed.get());
            assert_eq!(
                validate_work(&context.state_root, &selector, &call)?,
                WorkCoverage::Verified
            );
            prior = next.attempt_id;
        }
        Ok(())
    }
}
