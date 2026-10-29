//! Advisory scopes only. Run's writer lock remains the authority to write.
use crate::{
    events::{append, event},
    *,
};
use provenant_core::{ClaimId, DurationMicros};
use provenant_records::RecordHeader;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkScope {
    Issue {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        key: String,
    },
    Paths {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        paths: Vec<PathBuf>,
    },
    Worktree {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        path: PathBuf,
    },
}
impl WorkScope {
    fn canonical(&self) -> Result<Self> {
        Ok(match self {
            Self::Issue { key, extra } => {
                text(key, "issue key")?;
                Self::Issue {
                    extra: extra.clone(),
                    key: key.clone(),
                }
            }
            Self::Paths { paths, extra } => {
                if paths.is_empty() || paths.len() > 100 {
                    return Err(Error::Invalid("claim paths"));
                }
                let mut paths = paths
                    .iter()
                    .map(|p| canonical_path(p))
                    .collect::<Result<Vec<_>>>()?;
                paths.sort();
                paths.dedup();
                Self::Paths {
                    extra: extra.clone(),
                    paths,
                }
            }
            Self::Worktree { path, extra } => Self::Worktree {
                extra: extra.clone(),
                path: canonical_path(path)?,
            },
        })
    }
    fn paths(&self) -> &[PathBuf] {
        match self {
            Self::Issue { .. } => &[],
            Self::Paths { paths, .. } => paths,
            Self::Worktree { path, .. } => std::slice::from_ref(path),
        }
    }
}
// Existing scopes only: resolving symlinks and /var aliases cannot produce two
// claims for the same filesystem object. Future paths can claim their parent.
fn canonical_path(path: &Path) -> Result<PathBuf> {
    let input = path.to_str().ok_or(Error::PathEncoding)?;
    if input.len() > 4096 {
        return Err(Error::Invalid("claim path length"));
    }
    if !path.is_absolute() {
        return Err(Error::Invalid("absolute claim path"));
    }
    let path = path.canonicalize().map_err(|source| Error::Io {
        path: path.into(),
        source,
    })?;
    let value = path.to_str().ok_or(Error::PathEncoding)?;
    if value.len() > 4096 {
        return Err(Error::Invalid("claim path length"));
    }
    Ok(path)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ClaimRequest {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub id: ClaimId,
    pub project: ProjectId,
    pub actor: ActorId,
    pub token: [u8; 16],
    pub lease: DurationMicros,
    pub scope: WorkScope,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFence {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub id: ClaimId,
    pub actor: ActorId,
    pub generation: u64,
    pub token: [u8; 16],
}
impl std::fmt::Debug for ClaimFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimFence")
            .field("id", &self.id)
            .field("actor", &self.actor)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkClaim {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub header: RecordHeader,
    pub project: ProjectId,
    pub id: ClaimId,
    pub scope: WorkScope,
    pub holder: Option<ActorId>,
    pub generation: u64,
    pub expires_at: Option<WallTime>,
    pub updated_at: WallTime,
}
pub(crate) fn lease_expiry(at: WallTime, lease: DurationMicros) -> Result<WallTime> {
    if !(1_000_000..=3_600_000_000).contains(&lease.as_micros()) {
        return Err(Error::Invalid("lease duration"));
    }
    Ok(at.checked_add(lease)?)
}
pub(crate) fn scope_key<T: Serialize>(kind: &str, value: &T) -> Result<String> {
    Ok(CanonicalV1::record_digest(kind, 1, value)?.to_string())
}
pub(crate) struct StoredClaim {
    pub id: ClaimId,
    pub generation: u64,
    pub holder: Option<ActorId>,
    pub token: Option<Vec<u8>>,
    pub expires: Option<WallTime>,
    pub bytes: Vec<u8>,
}
fn stored(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClaimRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}
type ClaimRow = (
    String,
    i64,
    Option<String>,
    Option<Vec<u8>>,
    Option<i64>,
    Vec<u8>,
);
fn checked(row: ClaimRow) -> Result<StoredClaim> {
    let (id, generation, holder, token, expires, bytes) = row;
    Ok(StoredClaim {
        id: ClaimId::parse(&id).map_err(|_| Error::Invalid("stored claim id"))?,
        generation: u64::try_from(generation).map_err(|_| Error::Invalid("stored generation"))?,
        holder: holder
            .map(|s| ActorId::parse(&s).map_err(|_| Error::Invalid("stored holder")))
            .transpose()?,
        token,
        expires: expires.map(WallTime::from_micros).transpose()?,
        bytes,
    })
}
pub(crate) fn by_scope(
    db: &rusqlite::Connection,
    project: ProjectId,
    kind: &str,
    key: &str,
) -> Result<Option<StoredClaim>> {
    db.query_row("SELECT n.id,n.generation,c.holder,c.token,c.expires,coalesce(substr(c.record,1,?4),x'') FROM claim_counters n LEFT JOIN claims c ON c.project=n.project AND c.id=n.id WHERE n.project=?1 AND n.kind=?2 AND n.scope_key=?3", params![project.to_string(), kind, key, MAX_RECORD_READ_BYTES], stored).optional()?.map(checked).transpose()
}
pub(crate) fn by_fence(
    db: &rusqlite::Connection,
    project: ProjectId,
    fence: &ClaimFence,
    kind: &str,
    live: Option<WallTime>,
) -> Result<StoredClaim> {
    let row = db.query_row("SELECT id,generation,holder,token,expires,substr(record,1,?4) FROM claims WHERE project=?1 AND id=?2 AND kind=?3", params![project.to_string(), fence.id.to_string(), kind, MAX_RECORD_READ_BYTES], stored).optional()?.ok_or(Error::StaleFence)?;
    let row = checked(row)?;
    if row.holder != Some(fence.actor)
        || row.generation != fence.generation
        || row.token.as_deref() != Some(fence.token.as_slice())
        || live.is_some_and(|at| row.expires.is_none_or(|t| t <= at))
    {
        return Err(Error::StaleFence);
    }
    Ok(row)
}
pub(crate) fn next_generation(old: Option<&StoredClaim>) -> Result<u64> {
    let next = old
        .map_or(0, |r| r.generation)
        .checked_add(1)
        .ok_or(Error::Invalid("claim generation"))?;
    i64::try_from(next).map_err(|_| Error::Invalid("claim generation"))?;
    Ok(next)
}
pub(crate) struct ClaimWrite<'a> {
    pub project: ProjectId,
    pub id: ClaimId,
    pub kind: &'a str,
    pub key: &'a str,
    pub generation: u64,
    pub holder: Option<ActorId>,
    pub token: Option<&'a [u8]>,
    pub expires: Option<WallTime>,
}
pub(crate) fn save_claim<T: Serialize>(
    tx: &rusqlite::Connection,
    row: ClaimWrite<'_>,
    value: &T,
) -> Result<()> {
    let generation =
        i64::try_from(row.generation).map_err(|_| Error::Invalid("claim generation"))?;
    let collision: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM claim_counters WHERE project=? AND id=? AND (kind<>? OR scope_key<>?))", params![row.project.to_string(),row.id.to_string(),row.kind,row.key], |r|r.get(0))?;
    if collision {
        return Err(Error::Conflict);
    }
    tx.execute("INSERT INTO claim_counters VALUES(?,?,?,?,?) ON CONFLICT(project,kind,scope_key) DO UPDATE SET generation=excluded.generation", params![row.project.to_string(),row.kind,row.key,row.id.to_string(),generation])?;
    tx.execute("INSERT INTO claims VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(project,id) DO UPDATE SET generation=excluded.generation,holder=excluded.holder,token=excluded.token,expires=excluded.expires,record=excluded.record", params![row.project.to_string(),row.id.to_string(),row.kind,row.key,generation,row.holder.map(|a|a.to_string()),row.token,row.expires.map(|t|t.as_micros()),encode(value)?])?;
    Ok(())
}
pub(crate) fn claim_event(
    tx: &rusqlite::Connection,
    project: ProjectId,
    fence: &ClaimFence,
    kind: EventKind,
    detail: &str,
    at: WallTime,
) -> Result<()> {
    append(
        tx,
        &event(
            project,
            Some(fence.actor),
            kind,
            Some(fence.id.to_string()),
            detail.into(),
            at,
        ),
        None,
    )?;
    Ok(())
}
// CROSS JOIN pins path lookup before the claim liveness check, even without
// database statistics; otherwise SQLite may scan every claim in the project.
// Binary ranges preserve component boundaries: /a/ <= descendants < /a0.
pub(crate) const DESCENDANT_OVERLAP: &str = "SELECT p.claim FROM claim_paths p CROSS JOIN claims c ON c.project=p.project AND c.id=p.claim WHERE p.project=?1 AND p.path>=?2 AND p.path<?3 AND c.holder IS NOT NULL AND c.expires>?4 LIMIT 1";
pub(crate) const ANCESTOR_OVERLAP: &str = "SELECT p.claim FROM claim_paths p CROSS JOIN claims c ON c.project=p.project AND c.id=p.claim WHERE p.project=?1 AND p.path=?2 AND c.holder IS NOT NULL AND c.expires>?3 LIMIT 1";
fn overlapping_path(
    db: &rusqlite::Connection,
    project: ProjectId,
    at: WallTime,
    path: &Path,
) -> Result<Option<ClaimId>> {
    let project = project.to_string();
    let value = path.to_str().ok_or(Error::PathEncoding)?;
    let (lower, upper) = if value == "/" {
        ("/".to_owned(), "0".to_owned())
    } else {
        (format!("{value}/"), format!("{value}0"))
    };
    let mut overlap: Option<String> = db
        .query_row(
            DESCENDANT_OVERLAP,
            params![project, lower, upper, at.as_micros()],
            |r| r.get(0),
        )
        .optional()?;
    let mut statement = db.prepare(ANCESTOR_OVERLAP)?;
    for ancestor in path.ancestors() {
        if overlap.is_some() {
            break;
        }
        overlap = statement
            .query_row(
                params![
                    project,
                    ancestor.to_str().ok_or(Error::PathEncoding)?,
                    at.as_micros()
                ],
                |r| r.get(0),
            )
            .optional()?;
    }
    overlap
        .map(|id| ClaimId::parse(&id).map_err(|_| Error::Invalid("stored claim id")))
        .transpose()
}
fn prune_released(tx: &rusqlite::Connection, project: ProjectId) -> Result<()> {
    let project = project.to_string();
    let mut statement = tx.prepare("SELECT c.id FROM claims c WHERE c.project=? AND c.holder IS NULL AND NOT EXISTS(SELECT 1 FROM operations o WHERE o.project=c.project AND o.claim=c.id) ORDER BY c.id LIMIT 100")?;
    let ids = statement
        .query_map([&project], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        tx.execute(
            "DELETE FROM claim_paths WHERE project=? AND claim=?",
            params![project, id],
        )?;
        tx.execute(
            "DELETE FROM claims WHERE project=? AND id=?",
            params![project, id],
        )?;
    }
    Ok(())
}
impl Store {
    pub fn acquire_claim(&self, claim: &ClaimRequest, request: &Request<'_>) -> Result<ClaimFence> {
        let scope = claim.scope.canonical()?;
        let key = scope_key("fabric-work-scope", &scope)?;
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let expires = lease_expiry(at, claim.lease)?;
            let previous = by_scope(tx, claim.project, "work", &key)?;
            if let Some(row) = &previous
                && row.holder.is_some()
                && row.expires.is_some_and(|t| t > at)
            {
                if row.holder == Some(claim.actor)
                    && row.token.as_deref() == Some(claim.token.as_slice())
                {
                    return Ok(ClaimFence {
                        extra: Default::default(),
                        id: row.id,
                        actor: claim.actor,
                        generation: row.generation,
                        token: claim.token,
                    });
                }
                return Err(Error::Overlap { claim: row.id });
            }
            for path in scope.paths() {
                if let Some(id) = overlapping_path(tx, claim.project, at, path)? {
                    return Err(Error::Overlap { claim: id });
                }
            }
            prune_released(tx, claim.project)?;
            let id = previous.as_ref().map_or(claim.id, |r| r.id);
            let generation = next_generation(previous.as_ref())?;
            let fence = ClaimFence {
                extra: Default::default(),
                id,
                actor: claim.actor,
                generation,
                token: claim.token,
            };
            let record = WorkClaim {
                extra: previous
                    .as_ref()
                    .filter(|row| !row.bytes.is_empty())
                    .map(|row| decode::<WorkClaim>(&row.bytes).map(|value| value.extra))
                    .transpose()?
                    .unwrap_or_default(),
                header: RecordHeader::V1,
                project: claim.project,
                id,
                scope: scope.clone(),
                holder: Some(claim.actor),
                generation,
                expires_at: Some(expires),
                updated_at: at,
            };
            save_claim(
                tx,
                ClaimWrite {
                    project: claim.project,
                    id,
                    kind: "work",
                    key: &key,
                    generation,
                    holder: Some(claim.actor),
                    token: Some(&claim.token),
                    expires: Some(expires),
                },
                &record,
            )?;
            for path in scope.paths() {
                tx.execute(
                    "INSERT INTO claim_paths VALUES(?,?,?) ON CONFLICT DO NOTHING",
                    params![
                        claim.project.to_string(),
                        id.to_string(),
                        path.to_str().ok_or(Error::PathEncoding)?
                    ],
                )?;
            }
            claim_event(
                tx,
                claim.project,
                &fence,
                EventKind::ClaimAcquired,
                "acquired",
                at,
            )?;
            Ok(fence)
        })
    }
    pub fn verify_claim(
        &self,
        project: ProjectId,
        fence: &ClaimFence,
        request: &Request<'_>,
    ) -> Result<WorkClaim> {
        request.retry(|| {
            let row = by_fence(
                &self.db,
                project,
                fence,
                "work",
                Some(request.clock.wall_now()?),
            )?;
            decode(&row.bytes)
        })
    }
    pub fn renew_claim(
        &self,
        project: ProjectId,
        fence: &ClaimFence,
        lease: DurationMicros,
        request: &Request<'_>,
    ) -> Result<()> {
        self.change_claim(project, fence, Some(lease), request)
    }
    pub fn release_claim(
        &self,
        project: ProjectId,
        fence: &ClaimFence,
        request: &Request<'_>,
    ) -> Result<()> {
        self.change_claim(project, fence, None, request)
    }
    fn change_claim(
        &self,
        project: ProjectId,
        fence: &ClaimFence,
        lease: Option<DurationMicros>,
        request: &Request<'_>,
    ) -> Result<()> {
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let row = by_fence(tx, project, fence, "work", lease.map(|_| at))?;
            let mut record: WorkClaim = decode(&row.bytes)?;
            record.expires_at = lease.map(|d| lease_expiry(at, d)).transpose()?;
            record.holder = lease.map(|_| fence.actor);
            record.updated_at = at;
            let key = scope_key("fabric-work-scope", &record.scope)?;
            save_claim(
                tx,
                ClaimWrite {
                    project,
                    id: fence.id,
                    kind: "work",
                    key: &key,
                    generation: fence.generation,
                    holder: record.holder,
                    token: lease.map(|_| fence.token.as_slice()),
                    expires: record.expires_at,
                },
                &record,
            )?;
            claim_event(
                tx,
                project,
                fence,
                if lease.is_some() {
                    EventKind::ClaimRenewed
                } else {
                    EventKind::ClaimReleased
                },
                if lease.is_some() {
                    "renewed"
                } else {
                    "released"
                },
                at,
            )
        })
    }
    pub fn claims(
        &self,
        project: ProjectId,
        after: Option<ClaimId>,
        limit: u64,
        request: &Request<'_>,
    ) -> Result<Page<WorkClaim>> {
        let limit = page(0, limit)?;
        request.retry(|| {
            let mut stmt=self.db.prepare("SELECT substr(record,1,?4) FROM claims WHERE project=?1 AND kind='work' AND (?2 IS NULL OR id>?2) ORDER BY id LIMIT ?3")?;
            let mut rows=stmt.query(params![project.to_string(),after.map(|id|id.to_string()),limit,MAX_RECORD_READ_BYTES])?;
            let mut items=Vec::new(); let mut total=0; let mut truncated=false;
            while let Some(row)=rows.next()? { let bytes:Vec<u8>=row.get(0)?; total+=bytes.len(); if total>MAX_PAGE_BYTES {truncated=true;break;} items.push(decode(&bytes)?); }
            Ok(Page { extra: Default::default(), items,truncated})
        })
    }
}
