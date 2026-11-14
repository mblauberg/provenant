//! One durable advisory custody ledger, with atomic events and typed queries.
use crate::{
    events::{append, event},
    *,
};
use provenant_records::custody::*;

fn canonical(subject: &Subject) -> Result<Subject> {
    text(&subject.id, "custody subject")?;
    if subject.id.len() > 4096 || subject.id.chars().any(char::is_control) {
        return Err(Error::Invalid("custody subject"));
    }
    let mut subject = subject.clone();
    if subject.kind.is_path() {
        let path = std::path::Path::new(&subject.id);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(Error::Invalid("absolute custody path"));
        }
        // Resolve the nearest existing ancestor, so aliases stay stable even
        // before creation and after removal. No stored-shape fallback.
        let mut ancestor = path;
        let mut tail = Vec::new();
        let mut canonical = loop {
            match ancestor.canonicalize() {
                Ok(path) => break path,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    tail.push(
                        ancestor
                            .file_name()
                            .ok_or(Error::Invalid("custody path"))?
                            .to_owned(),
                    );
                    ancestor = ancestor.parent().ok_or(Error::Invalid("custody path"))?;
                }
                Err(source) => {
                    return Err(Error::Io {
                        path: ancestor.into(),
                        source,
                    });
                }
            }
        };
        for part in tail.iter().rev() {
            canonical.push(part);
        }
        subject.id = canonical.to_str().ok_or(Error::PathEncoding)?.to_owned();
        if subject.id.len() > 4096 {
            return Err(Error::Invalid("custody path"));
        }
    }
    Ok(subject)
}
fn chair_key(chair: &Chair) -> Result<String> {
    Ok(CanonicalV1::record_digest(
        "custody-chair",
        2,
        &(chair.host, chair.actor, &chair.session),
    )?
    .to_string())
}
fn holder_key(holder: &Holder) -> Result<String> {
    match holder {
        Holder::Chair { chair } => chair_key(chair),
        Holder::Attempt { attempt, .. } => Ok(format!("attempt:{attempt}")),
    }
}
fn read(
    db: &rusqlite::Connection,
    project: ProjectId,
    subject: &Subject,
) -> Result<Option<CustodyRecord>> {
    let raw: Option<Vec<u8>> = db
        .query_row(
            "SELECT substr(record,1,?4) FROM custody WHERE project=?1 AND kind=?2 AND subject=?3",
            params![
                project.to_string(),
                subject.kind.as_str(),
                subject.id,
                MAX_RECORD_READ_BYTES
            ],
            |row| row.get(0),
        )
        .optional()?;
    let record = raw.as_deref().map(decode::<CustodyRecord>).transpose()?;
    if record
        .as_ref()
        .is_some_and(|record| record.schema_version != 2)
    {
        return Err(Error::Invalid("custody schema version"));
    }
    Ok(record)
}
fn save(db: &rusqlite::Connection, record: &CustodyRecord) -> Result<()> {
    let bytes = encode(record)?;
    let chair = record.holder.chair().map(chair_key).transpose()?;
    db.execute("INSERT INTO custody(project,kind,subject,state,holder,chair,record) VALUES(?1,?2,?3,?4,?5,?6,?7)
        ON CONFLICT(project,kind,subject) DO UPDATE SET state=excluded.state,holder=excluded.holder,chair=excluded.chair,record=excluded.record",
        params![record.project.to_string(), record.subject.kind.as_str(), record.subject.id, record.state.kind().as_str(), holder_key(&record.holder)?, chair, bytes])?;
    Ok(())
}
fn changed(
    db: &rusqlite::Connection,
    change: &CustodyChange,
    actor: Option<ActorId>,
) -> Result<()> {
    save(db, &change.record)?;
    append(
        db,
        &event(
            change.record.project,
            actor,
            EventKind::CustodyChanged,
            Some(change.record.subject.to_string()),
            String::from_utf8(encode(&(
                &change.record.holder,
                &change.record.state,
                change.record.origin,
                &change.previous_holder,
            ))?)
            .map_err(|_| Error::Invalid("custody event"))?,
            change.record.last_activity,
        ),
        None,
    )?;
    Ok(())
}
impl HostStore {
    pub fn custody_item(
        &self,
        project: ProjectId,
        subject: &Subject,
        request: &Request<'_>,
    ) -> Result<Option<CustodyRecord>> {
        request.check()?;
        // Reads and lifecycle transitions must also work after a worktree was removed.
        let subject = canonical(subject)?;
        read(&self.0.db, project, &subject)
    }
    /// Acquire or adopt; another holder is evidence to report, never a lock.
    pub fn acquire_custody(
        &self,
        project: ProjectId,
        subject: &Subject,
        holder: &Holder,
        origin: Origin,
        request: &Request<'_>,
    ) -> Result<CustodyChange> {
        let subject = canonical(subject)?;
        self.0.write(request, |tx| {
            let previous = read(tx, project, &subject)?;
            let previous_holder = previous
                .as_ref()
                .filter(|old| old.state.kind() == StateKind::Active && old.holder != *holder)
                .map(|old| old.holder.clone());
            let record = CustodyRecord {
                schema_version: 2,
                subject: subject.clone(),
                holder: holder.clone(),
                project,
                origin,
                state: CustodyState::Active,
                last_activity: request.clock.wall_now()?,
                links: previous
                    .as_ref()
                    .map(|old| old.links.clone())
                    .unwrap_or_default(),
                provenance: previous.as_ref().and_then(|old| old.provenance),
                extra: previous.map(|old| old.extra).unwrap_or_default(),
            };
            let change = CustodyChange {
                record,
                previous_holder,
            };
            changed(tx, &change, holder.actor())?;
            Ok(change)
        })
    }
    pub fn acquire_custody_group(
        &self,
        project: ProjectId,
        subjects: &[Subject],
        holder: &Holder,
        origin: Origin,
        request: &Request<'_>,
    ) -> Result<Vec<CustodyChange>> {
        self.0.write(request, |_| {
            let changes = subjects
                .iter()
                .map(|subject| self.acquire_custody(project, subject, holder, origin, request))
                .collect::<Result<Vec<_>>>()?;
            self.link_custody(project, subjects, request)?;
            Ok(changes)
        })
    }
    pub fn link_custody(
        &self,
        project: ProjectId,
        subjects: &[Subject],
        request: &Request<'_>,
    ) -> Result<()> {
        let subjects = subjects.iter().map(canonical).collect::<Result<Vec<_>>>()?;
        self.0.write(request, |tx| {
            for subject in &subjects {
                let mut record = read(tx, project, subject)?.ok_or(Error::NotFound)?;
                for link in &subjects {
                    if link != subject && !record.links.contains(link) {
                        record.links.push(link.clone());
                    }
                }
                save(tx, &record)?;
            }
            Ok(())
        })
    }
    pub fn seal_custody_provenance(
        &self,
        project: ProjectId,
        subject: &Subject,
        digest: provenant_core::Digest,
        request: &Request<'_>,
    ) -> Result<()> {
        let subject = canonical(subject)?;
        self.0.write(request, |tx| {
            let mut record = read(tx, project, &subject)?.ok_or(Error::NotFound)?;
            if record.provenance.is_none() {
                record.provenance = Some(digest);
                save(tx, &record)?;
            }
            Ok(())
        })
    }
    pub fn change_custody_state(
        &self,
        project: ProjectId,
        subject: &Subject,
        state: CustodyState,
        actor: Option<ActorId>,
        request: &Request<'_>,
    ) -> Result<CustodyRecord> {
        let subject = canonical(subject)?;
        self.0.write(request, |tx| {
            let mut record = read(tx, project, &subject)?.ok_or(Error::NotFound)?;
            if record.state == state {
                return Ok(record);
            }
            record.state = state.clone();
            record.last_activity = request.clock.wall_now()?;
            changed(
                tx,
                &CustodyChange {
                    record: record.clone(),
                    previous_holder: None,
                },
                actor,
            )?;
            Ok(record)
        })
    }
    pub fn touch_custody(
        &self,
        project: ProjectId,
        subject: &Subject,
        actor: ActorId,
        request: &Request<'_>,
    ) -> Result<CustodyRecord> {
        let subject = canonical(subject)?;
        self.0.write(request, |tx| {
            let mut record = read(tx, project, &subject)?.ok_or(Error::NotFound)?;
            record.last_activity = request.clock.wall_now()?;
            changed(
                tx,
                &CustodyChange {
                    record: record.clone(),
                    previous_holder: None,
                },
                Some(actor),
            )?;
            Ok(record)
        })
    }
    /// Indexed pagination shared by retention, status and the session digest.
    pub fn custody(&self, query: &CustodyQuery, request: &Request<'_>) -> Result<CustodyPage> {
        request.check()?;
        let limit = i64::try_from(query.limit).map_err(|_| Error::Invalid("custody limit"))?;
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid("custody limit"));
        }
        let after = i64::try_from(query.after).map_err(|_| Error::Invalid("custody cursor"))?;
        let mut statement = self.0.db.prepare("SELECT seq,substr(record,1,?7) FROM custody WHERE seq>?1
            AND (?2 IS NULL OR project=?2) AND (?3 IS NULL OR state=?3) AND (?4 IS NULL OR holder=?4)
            AND (?5 IS NULL OR chair=?5) ORDER BY seq LIMIT ?6")?;
        let rows = statement.query_map(
            params![
                after,
                query.project.map(|p| p.to_string()),
                query.state.map(StateKind::as_str),
                query.holder.as_ref().map(holder_key).transpose()?,
                query.chair.as_ref().map(chair_key).transpose()?,
                limit + 1,
                MAX_RECORD_READ_BYTES
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let mut items = Vec::new();
        let mut cursor = query.after;
        let mut bytes = 0;
        let mut more = false;
        for row in rows {
            request.check()?;
            let (seq, raw) = row?;
            if items.len() as u64 == query.limit || bytes + raw.len() > MAX_PAGE_BYTES {
                more = true;
                break;
            }
            let record: CustodyRecord = decode(&raw)?;
            if record.schema_version != 2 {
                return Err(Error::Invalid("custody schema version"));
            }
            items.push(record);
            bytes += raw.len();
            cursor = u64::try_from(seq).map_err(|_| Error::Invalid("custody cursor"))?;
        }
        Ok(CustodyPage {
            items,
            next: more.then_some(cursor),
        })
    }
}
