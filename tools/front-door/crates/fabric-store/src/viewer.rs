//! Viewer-owned presentation state, separate from request resolution.
use crate::*;
use provenant_core::HumanRequestId;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RequestView {
    pub seen_revision: Option<u64>,
    pub snoozed_until: Option<WallTime>,
    pub dismissed: bool,
}

fn viewer_id(viewer: &str) -> Result<()> {
    if viewer.is_empty() || viewer.len() > 128 || viewer.chars().any(char::is_control) {
        return Err(Error::Invalid("viewer"));
    }
    Ok(())
}

fn number(value: u64) -> Result<i64> {
    i64::try_from(value)
        .ok()
        .filter(|n| *n > 0)
        .ok_or(Error::Invalid("revision"))
}

fn capacity(
    tx: &rusqlite::Connection,
    table: &str,
    project: ProjectId,
    exists: bool,
) -> Result<()> {
    if !exists {
        let sql = match table {
            "request" => "SELECT count(*) FROM viewer_requests WHERE project=?1",
            _ => "SELECT count(*) FROM viewer_summaries WHERE project=?1",
        };
        let count: i64 = tx.query_row(sql, [project.to_string()], |r| r.get(0))?;
        if count >= 10_000 {
            return Err(Error::Invalid("viewer state retention limit"));
        }
    }
    Ok(())
}

impl Store {
    pub fn request_view(
        &self,
        project: ProjectId,
        viewer: &str,
        id: HumanRequestId,
        request: &Request<'_>,
    ) -> Result<RequestView> {
        viewer_id(viewer)?;
        request.retry(|| {
            let row: Option<(Option<i64>, Option<String>, bool)> = self.db.query_row(
                "SELECT seen_revision,snoozed_until,dismissed FROM viewer_requests WHERE project=?1 AND viewer=?2 AND request=?3",
                params![project.to_string(), viewer, id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).optional()?;
            match row {
                None => Ok(RequestView::default()),
                Some((seen, until, dismissed)) => Ok(RequestView {
                    seen_revision: seen.map(|n| u64::try_from(n).map_err(|_| Error::Invalid("seen revision"))).transpose()?,
                    snoozed_until: until.map(|s| WallTime::parse(&s)).transpose()?,
                    dismissed,
                }),
            }
        })
    }

    /// Store presentation state only after verifying the referenced request exists.
    pub fn set_request_view(
        &self,
        project: ProjectId,
        viewer: &str,
        id: HumanRequestId,
        value: &RequestView,
        request: &Request<'_>,
    ) -> Result<()> {
        viewer_id(viewer)?;
        let seen = value.seen_revision.map(number).transpose()?;
        self.write(request, |tx| {
            let current: Option<i64> = tx.query_row("SELECT revision FROM human_requests WHERE project=?1 AND id=?2", params![project.to_string(), id.to_string()], |r| r.get(0)).optional()?;
            let current = current.ok_or(Error::NotFound)?;
            if seen.is_some_and(|revision| revision > current) { return Err(Error::Invalid("seen revision")); }
            let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM viewer_requests WHERE project=?1 AND viewer=?2 AND request=?3)", params![project.to_string(), viewer, id.to_string()], |r| r.get(0))?;
            capacity(tx, "request", project, existing)?;
            tx.execute("INSERT INTO viewer_requests(project,viewer,request,seen_revision,snoozed_until,dismissed) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(project,viewer,request) DO UPDATE SET seen_revision=excluded.seen_revision,snoozed_until=excluded.snoozed_until,dismissed=excluded.dismissed",
                params![project.to_string(), viewer, id.to_string(), seen, value.snoozed_until.map(|t| t.to_string()), value.dismissed])?;
            Ok(())
        })
    }

    pub fn summary_baseline(
        &self,
        project: ProjectId,
        viewer: &str,
        session: &str,
        request: &Request<'_>,
    ) -> Result<Option<u64>> {
        viewer_id(viewer)?;
        text(session, "session")?;
        if session.len() > 128 {
            return Err(Error::Invalid("session"));
        }
        request.retry(|| {
            let value: Option<i64> = self.db.query_row("SELECT revision FROM viewer_summaries WHERE project=?1 AND viewer=?2 AND session=?3", params![project.to_string(), viewer, session], |r| r.get(0)).optional()?;
            value.map(|n| u64::try_from(n).map_err(|_| Error::Invalid("summary revision"))).transpose()
        })
    }

    /// Record the revision actually rendered, not a subsequent current revision.
    pub fn set_summary_baseline(
        &self,
        project: ProjectId,
        viewer: &str,
        session: &str,
        revision: u64,
        request: &Request<'_>,
    ) -> Result<()> {
        viewer_id(viewer)?;
        text(session, "session")?;
        if session.len() > 128 {
            return Err(Error::Invalid("session"));
        }
        let revision = number(revision)?;
        self.write(request, |tx| {
            let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM viewer_summaries WHERE project=?1 AND viewer=?2 AND session=?3)", params![project.to_string(), viewer, session], |r| r.get(0))?;
            capacity(tx, "summary", project, existing)?;
            tx.execute("INSERT INTO viewer_summaries(project,viewer,session,revision) VALUES(?1,?2,?3,?4) ON CONFLICT(project,viewer,session) DO UPDATE SET revision=excluded.revision", params![project.to_string(), viewer, session, revision])?;
            Ok(())
        })
    }
}
