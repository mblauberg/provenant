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
    if let Some(record) = &record
        && let CustodyState::Retired { retirement_id } = &record.state
    {
        let retirement = read_retirement(db, project, retirement_id)?
            .ok_or(Error::Invalid("orphan retirement state"))?;
        if !retirement.owns_subject(&record.subject) {
            return Err(Error::Invalid("retirement subject mismatch"));
        }
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
fn read_retirement(
    db: &rusqlite::Connection,
    project: ProjectId,
    id: &str,
) -> Result<Option<RetirementRecord>> {
    let raw: Option<Vec<u8>> = db
        .query_row(
            "SELECT substr(record,1,?3) FROM retirements WHERE project=?1 AND id=?2",
            params![project.to_string(), id, MAX_RECORD_READ_BYTES],
            |row| row.get(0),
        )
        .optional()?;
    let record = raw.as_deref().map(decode::<RetirementRecord>).transpose()?;
    if let Some(record) = &record {
        if record.project != project || record.id != id {
            return Err(Error::Invalid("retirement record identity"));
        }
        validate_retirement_identity(record)?;
    }
    Ok(record)
}
fn validate_retirement_identity(record: &RetirementRecord) -> Result<()> {
    text(&record.id, "retirement id")?;
    let identity = RetirementRecord::identity(
        record.project,
        &record.related_subjects,
        &record.reference,
        record.expected_tip,
        &record.decision,
    )
    .map_err(Error::Encode)?;
    let expected_subject = record.reference.custody_subject().map_err(Error::Encode)?;
    let expected_steps = RetirementStep::for_subjects(&record.related_subjects);
    if record.id != identity
        || record.schema_version != 2
        || record.revision == 0
        || record.related_subjects.windows(2).any(|subjects| {
            (subjects[0].kind.as_str(), &subjects[0].id)
                >= (subjects[1].kind.as_str(), &subjects[1].id)
        })
        || record.reference.identity.repository != record.project
        || record.reference.generation == 0
        || record.subject != expected_subject
        || record.related_subjects.iter().any(|subject| {
            let path = std::path::Path::new(&subject.id);
            subject.kind != SubjectKind::Worktree
                || subject.id.is_empty()
                || subject.id.len() > 4096
                || subject.id.chars().any(char::is_control)
                || !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
                || subject == &record.subject
        })
        || record.outcomes.len() != expected_steps.len()
        || record
            .outcomes
            .iter()
            .zip(&expected_steps)
            .any(|(outcome, expected)| &outcome.step != expected)
        || record.outcomes.iter().any(|outcome| {
            !matches!(&outcome.state, RetirementStepState::Pending)
                && !outcome.step.prerequisites_complete(&record.outcomes)
        })
        || record.outcomes.iter().any(|outcome| {
            matches!(
                &outcome.state,
                RetirementStepState::Completed { evidence }
                    if !outcome
                        .step
                        .accepts_completion_evidence(record.decision.evidence(), evidence)
            )
        })
    {
        return Err(Error::Invalid("retirement record"));
    }
    Ok(())
}
fn validate_retirement(record: &RetirementRecord) -> Result<()> {
    validate_retirement_identity(record)?;
    if record.revision != 1
        || record
            .outcomes
            .iter()
            .any(|outcome| !matches!(&outcome.state, RetirementStepState::Pending))
    {
        return Err(Error::Invalid("retirement record"));
    }
    Ok(())
}
fn retirement_authority(decision: &RetirementDecision) -> ActorId {
    match decision {
        RetirementDecision::Discard { authority, .. } => *authority,
    }
}
fn same_retirement_intent(existing: &RetirementRecord, requested: &RetirementRecord) -> bool {
    existing.schema_version == requested.schema_version
        && existing.id == requested.id
        && existing.project == requested.project
        && existing.subject == requested.subject
        && existing.related_subjects == requested.related_subjects
        && existing.reference == requested.reference
        && existing.expected_tip == requested.expected_tip
        && existing.decision == requested.decision
}
fn save_retirement(db: &rusqlite::Connection, record: &RetirementRecord) -> Result<()> {
    validate_retirement(record)?;
    db.execute(
        "INSERT INTO retirements(project,id,revision,kind,subject,record) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            record.project.to_string(),
            record.id,
            i64::try_from(record.revision).map_err(|_| Error::Invalid("retirement revision"))?,
            record.subject.kind.as_str(),
            record.subject.id,
            encode(record)?,
        ],
    )?;
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
    /// Read the single custody-owned retirement authority record for recovery.
    pub fn retirement(
        &self,
        project: ProjectId,
        id: &str,
        request: &Request<'_>,
    ) -> Result<Option<RetirementRecord>> {
        request.check()?;
        read_retirement(&self.0.db, project, id)
    }

    /// Start a durable, resumable retirement and point custody at its owner in
    /// the same transaction. The host adapter resolves authority from trusted
    /// host identity before calling; a repeated identical request is idempotent.
    pub fn start_retirement(
        &self,
        record: &RetirementRecord,
        request: &Request<'_>,
    ) -> Result<RetirementRecord> {
        validate_retirement(record)?;
        let mut subjects = Vec::with_capacity(record.related_subjects.len() + 1);
        subjects.push(canonical(&record.subject)?);
        subjects.extend(
            record
                .related_subjects
                .iter()
                .map(canonical)
                .collect::<Result<Vec<_>>>()?,
        );
        if subjects[0] != record.subject || subjects[1..] != record.related_subjects {
            return Err(Error::Invalid("canonical retirement subjects"));
        }
        if subjects
            .iter()
            .enumerate()
            .any(|(index, subject)| subjects[..index].contains(subject))
        {
            return Err(Error::Invalid("duplicate retirement subject"));
        }
        if let Some(existing) = self.retirement(record.project, &record.id, request)? {
            return if same_retirement_intent(&existing, record) {
                Ok(existing)
            } else {
                Err(Error::Conflict)
            };
        }
        self.0.write(request, |tx| {
            if let Some(existing) = read_retirement(tx, record.project, &record.id)? {
                return if same_retirement_intent(&existing, record) {
                    Ok(existing)
                } else {
                    Err(Error::Conflict)
                };
            }
            let mut custody = Vec::with_capacity(subjects.len());
            for subject in &subjects {
                let item = read(tx, record.project, subject)?.ok_or(Error::NotFound)?;
                match &item.state {
                    CustodyState::Active | CustodyState::Landed { .. } => {}
                    _ => return Err(Error::Conflict),
                }
                custody.push(item);
            }
            save_retirement(tx, record)?;
            for mut item in custody {
                item.state = CustodyState::Retired {
                    retirement_id: record.id.clone(),
                };
                item.last_activity = request.clock.wall_now()?;
                changed(
                    tx,
                    &CustodyChange {
                        record: item,
                        previous_holder: None,
                    },
                    Some(retirement_authority(&record.decision)),
                )?;
            }
            Ok(record.clone())
        })
    }

    /// Advance or reopen one step with an optimistic revision check. Completed
    /// steps are immutable; refused and uncertain work can explicitly resume.
    pub fn set_retirement_step(
        &self,
        project: ProjectId,
        id: &str,
        expected_revision: u64,
        next: RetirementStepOutcome,
        request: &Request<'_>,
    ) -> Result<RetirementRecord> {
        request.check()?;
        self.0.write(request, |tx| {
            let mut record = read_retirement(tx, project, id)?.ok_or(Error::NotFound)?;
            if record.revision != expected_revision {
                return Err(Error::StaleFence);
            }
            let current_index = record
                .outcomes
                .iter()
                .position(|outcome| outcome.step == next.step)
                .ok_or(Error::Invalid("retirement step"))?;
            if matches!(
                &next.state,
                RetirementStepState::Completed { evidence }
                    if !next
                        .step
                        .accepts_completion_evidence(record.decision.evidence(), evidence)
            ) {
                return Err(Error::Invalid("retirement completion evidence"));
            }
            if !matches!(&next.state, RetirementStepState::Pending)
                && !next.step.prerequisites_complete(&record.outcomes)
            {
                return Err(Error::Conflict);
            }
            let current = &mut record.outcomes[current_index];
            if current == &next {
                return Ok(record);
            }
            let can_resume = matches!(
                (&current.state, &next.state),
                (RetirementStepState::Refused { .. } | RetirementStepState::Unknown { .. }, RetirementStepState::Pending)
            );
            let can_finish = matches!(
                (&current.state, &next.state),
                (RetirementStepState::Pending | RetirementStepState::Unknown { .. }, RetirementStepState::Completed { .. } | RetirementStepState::Refused { .. } | RetirementStepState::Unknown { .. })
            );
            if !can_resume && !can_finish {
                return Err(Error::Conflict);
            }
            *current = next;
            record.revision = record
                .revision
                .checked_add(1)
                .ok_or(Error::Invalid("retirement revision"))?;
            validate_retirement_identity(&record)?;
            let changed = tx.execute(
                "UPDATE retirements SET revision=?3,record=?4 WHERE project=?1 AND id=?2 AND revision=?5",
                params![
                    project.to_string(),
                    id,
                    i64::try_from(record.revision).map_err(|_| Error::Invalid("retirement revision"))?,
                    encode(&record)?,
                    i64::try_from(expected_revision).map_err(|_| Error::Invalid("retirement revision"))?,
                ],
            )?;
            if changed != 1 {
                return Err(Error::StaleFence);
            }
            Ok(record)
        })
    }

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

    /// Ensure a subject has a custody row, inheriting advisory ownership and
    /// provenance from an observed related subject only when the target is
    /// absent. Existing target state is returned unchanged.
    pub fn ensure_custody_from_existing(
        &self,
        project: ProjectId,
        subject: &Subject,
        source_subject: &Subject,
        request: &Request<'_>,
    ) -> Result<CustodyRecord> {
        let subject = canonical(subject)?;
        self.0.write(request, |tx| {
            if let Some(existing) = read(tx, project, &subject)? {
                return Ok(existing);
            }
            let source_subject = canonical(source_subject)?;
            if subject == source_subject {
                return Err(Error::Invalid("custody source subject"));
            }
            let source = read(tx, project, &source_subject)?.ok_or(Error::NotFound)?;
            let record = CustodyRecord {
                schema_version: 2,
                subject: subject.clone(),
                holder: source.holder,
                project,
                origin: source.origin,
                state: CustodyState::Active,
                last_activity: request.clock.wall_now()?,
                links: Vec::new(),
                provenance: source.provenance,
                extra: Default::default(),
            };
            changed(
                tx,
                &CustodyChange {
                    record: record.clone(),
                    previous_holder: None,
                },
                record.holder.actor(),
            )?;
            Ok(record)
        })
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
            if previous
                .as_ref()
                .is_some_and(|old| matches!(&old.state, CustodyState::Retired { .. }))
            {
                return Err(Error::Conflict);
            }
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
            if let CustodyState::Retired { retirement_id } = &state {
                let retirement =
                    read_retirement(tx, project, retirement_id)?.ok_or(Error::Conflict)?;
                let can_link = matches!(
                    &record.state,
                    CustodyState::Active | CustodyState::Landed { .. }
                ) || matches!(
                    &record.state,
                    CustodyState::Retired { retirement_id: current } if current == retirement_id
                );
                if !retirement.owns_subject(&subject) || !can_link {
                    return Err(Error::Conflict);
                }
            } else if matches!(&record.state, CustodyState::Retired { .. }) {
                return Err(Error::Conflict);
            }
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
