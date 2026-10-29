//! Fenced landing records. The caller holds LandingGuard across Git (F7);
//! SQLite transactions never span that transport or lock acquisition.
use crate::{claims::*, *};
use provenant_core::{ClaimId, Digest, DurationMicros, GitOid, OperationId};
use provenant_os::fs::{LockGuard, LockMode};
use provenant_records::RecordHeader;
use std::ffi::OsStr;

/// Holds a safely acquired lock on a persistent private file. Store checks
/// the bound lock directory; no owner unlinks or replaces its lock files.
pub struct LandingGuard {
    _lock: LockGuard,
    project: ProjectId,
    reference: String,
    directory: (u64, u64),
}
impl LandingGuard {
    pub fn project(&self) -> ProjectId {
        self.project
    }
    pub fn reference(&self) -> &str {
        &self.reference
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LandingRequest {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub id: ClaimId,
    pub project: ProjectId,
    pub actor: ActorId,
    pub token: [u8; 16],
    pub reference: String,
    pub expected: GitOid,
    pub candidate: GitOid,
    pub lease: DurationMicros,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landing {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub header: RecordHeader,
    pub project: ProjectId,
    pub id: ClaimId,
    pub reference: String,
    pub expected: GitOid,
    pub candidate: GitOid,
    pub holder: Option<ActorId>,
    pub generation: u64,
    pub expires_at: Option<WallTime>,
    pub operation: Option<OperationId>,
    pub updated_at: WallTime,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushPhase {
    Prepared,
    Pushing,
    Unknown,
    Committed,
    Retryable,
    Conflict,
}
impl PushPhase {
    fn sql(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Pushing => "pushing",
            Self::Unknown => "unknown",
            Self::Committed => "committed",
            Self::Retryable => "retryable",
            Self::Conflict => "conflict",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PushAction {
    Begin,
    Uncertain,
    Confirmed,
    Reconcile { remote: Option<GitOid> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRequest {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub header: RecordHeader,
    pub project: ProjectId,
    pub claim: ClaimId,
    pub actor: ActorId,
    pub generation: u64,
    pub reference: String,
    pub expected: GitOid,
    pub candidate: GitOid,
    pub remote: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushOperation {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub header: RecordHeader,
    pub id: OperationId,
    pub digest: Digest,
    pub request: PushRequest,
    pub phase: PushPhase,
    pub observed_remote: Option<GitOid>,
    pub updated_at: WallTime,
}
// Landing only targets fully-qualified branch refs; no refspecs, revision
// expressions or lock-file names. F7 additionally verifies the repository.
fn reference(value: &str) -> Result<()> {
    if !value.starts_with("refs/heads/")
        || value.len() > 1024
        || !value.is_ascii()
        || value
            .bytes()
            .any(|b| b <= 32 || b == 127 || b"~^:?*[\\".contains(&b))
        || value.contains("..")
        || value.contains("@{")
        || value.ends_with('.')
        || value
            .split('/')
            .any(|s| s.is_empty() || s.starts_with('.') || s.ends_with(".lock"))
    {
        return Err(Error::Invalid("landing ref"));
    }
    Ok(())
}
fn load_operation(
    db: &rusqlite::Connection,
    project: ProjectId,
    id: &OperationId,
) -> Result<PushOperation> {
    let bytes: Vec<u8> = db
        .query_row(
            "SELECT substr(record,1,?3) FROM operations WHERE project=?1 AND id=?2",
            params![project.to_string(), id.as_str(), MAX_RECORD_READ_BYTES],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let operation: PushOperation = decode(&bytes)?;
    if operation.request.project != project || &operation.id != id {
        return Err(Error::Invalid("stored operation identity"));
    }
    Ok(operation)
}
fn save_operation(tx: &rusqlite::Connection, operation: &PushOperation) -> Result<()> {
    tx.execute("INSERT INTO operations VALUES(?,?,?,?,?,?) ON CONFLICT(project,id) DO UPDATE SET phase=excluded.phase,record=excluded.record",params![operation.request.project.to_string(),operation.id.as_str(),operation.request.claim.to_string(),operation.digest.to_string(),operation.phase.sql(),encode(operation)?])?;
    Ok(())
}
fn unresolved(
    db: &rusqlite::Connection,
    project: ProjectId,
    id: ClaimId,
) -> Result<Option<OperationId>> {
    let id:Option<String>=db.query_row("SELECT id FROM operations WHERE project=? AND claim=? AND phase IN ('pushing','unknown') LIMIT 1",params![project.to_string(),id.to_string()],|r|r.get(0)).optional()?;
    id.map(|s| OperationId::parse(&s).map_err(|_| Error::Invalid("stored operation id")))
        .transpose()
}
/// Digest's fixed ASCII hex is safe as a single filesystem component.
pub(crate) fn landing_lock_name(project: ProjectId, branch: &str) -> Result<String> {
    let key = scope_key("fabric-landing-lock", &(project, branch))?;
    Ok(format!(
        "{}.lock",
        key.strip_prefix("sha256:")
            .ok_or(Error::Invalid("lock digest"))?
    ))
}
impl Store {
    pub fn lock_landing(
        &self,
        project: ProjectId,
        branch: &str,
        request: &Request<'_>,
    ) -> Result<LandingGuard> {
        reference(branch)?;
        request.check()?;
        let name = landing_lock_name(project, branch)?;
        let locks = self
            .locks
            .as_ref()
            .ok_or(Error::Invalid("landing lock store"))?;
        let lock = locks.lock(
            OsStr::new(&name),
            LockMode::Exclusive,
            request.lock_budget(),
            request.cancel,
            request.clock,
        )?;
        let metadata = self
            .locks
            .as_ref()
            .ok_or(Error::Invalid("landing lock store"))?
            .metadata()?;
        Ok(LandingGuard {
            _lock: lock,
            project,
            reference: branch.into(),
            directory: (metadata.device, metadata.inode),
        })
    }
    fn check_guard(&self, guard: &LandingGuard) -> Result<()> {
        let metadata = self
            .locks
            .as_ref()
            .ok_or(Error::Invalid("landing lock store"))?
            .metadata()?;
        if guard.directory != (metadata.device, metadata.inode) {
            return Err(Error::Invalid("landing lock store"));
        }
        Ok(())
    }
    fn landing_fence(
        &self,
        tx: &rusqlite::Connection,
        guard: &LandingGuard,
        fence: &ClaimFence,
        live: Option<WallTime>,
    ) -> Result<Landing> {
        self.check_guard(guard)?;
        let row = by_fence(tx, guard.project, fence, "landing", live)?;
        let value: Landing = decode(&row.bytes)?;
        if value.reference != guard.reference
            || value.project != guard.project
            || value.id != fence.id
        {
            return Err(Error::StaleFence);
        }
        Ok(value)
    }
    fn save_landing(
        &self,
        tx: &rusqlite::Connection,
        value: &Landing,
        token: Option<&[u8]>,
    ) -> Result<()> {
        save_claim(
            tx,
            ClaimWrite {
                project: value.project,
                id: value.id,
                kind: "landing",
                key: &value.reference,
                generation: value.generation,
                holder: value.holder,
                token,
                expires: value.expires_at,
            },
            value,
        )
    }
    pub fn acquire_landing(
        &self,
        guard: &LandingGuard,
        claim: &LandingRequest,
        request: &Request<'_>,
    ) -> Result<ClaimFence> {
        self.check_guard(guard)?;
        if claim.project != guard.project || claim.reference != guard.reference {
            return Err(Error::Invalid("landing lock scope"));
        }
        if std::mem::discriminant(&claim.expected) != std::mem::discriminant(&claim.candidate)
            || claim.expected == claim.candidate
        {
            return Err(Error::Invalid("landing OIDs"));
        }
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let expires = lease_expiry(at, claim.lease)?;
            let previous = by_scope(tx, claim.project, "landing", &claim.reference)?;
            if let Some(row) = &previous {
                if row.holder == Some(claim.actor)
                    && row.token.as_deref() == Some(claim.token.as_slice())
                    && row.expires.is_some_and(|t| t > at)
                {
                    let value: Landing = decode(&row.bytes)?;
                    if value.expected != claim.expected || value.candidate != claim.candidate {
                        return Err(Error::Conflict);
                    }
                    return Ok(ClaimFence {
                        extra: Default::default(),
                        id: row.id,
                        actor: claim.actor,
                        generation: row.generation,
                        token: claim.token,
                    });
                }
                if let Some(operation) = unresolved(tx, claim.project, row.id)? {
                    return Err(Error::PushUnresolved { operation });
                }
                if row.holder.is_some() && row.expires.is_some_and(|t| t > at) {
                    return Err(Error::Conflict);
                }
            }
            let id = previous.as_ref().map_or(claim.id, |r| r.id);
            let generation = next_generation(previous.as_ref())?;
            let fence = ClaimFence {
                extra: Default::default(),
                id,
                actor: claim.actor,
                generation,
                token: claim.token,
            };
            let value = Landing {
                extra: previous
                    .as_ref()
                    .filter(|row| !row.bytes.is_empty())
                    .map(|row| decode::<Landing>(&row.bytes).map(|value| value.extra))
                    .transpose()?
                    .unwrap_or_default(),
                header: RecordHeader::V1,
                project: claim.project,
                id,
                reference: claim.reference.clone(),
                expected: claim.expected,
                candidate: claim.candidate,
                holder: Some(claim.actor),
                generation,
                expires_at: Some(expires),
                operation: None,
                updated_at: at,
            };
            self.save_landing(tx, &value, Some(&claim.token))?;
            claim_event(
                tx,
                claim.project,
                &fence,
                EventKind::LandingAcquired,
                "acquired",
                at,
            )?;
            Ok(fence)
        })
    }
    pub fn verify_landing(
        &self,
        guard: &LandingGuard,
        fence: &ClaimFence,
        request: &Request<'_>,
    ) -> Result<Landing> {
        self.landing_fence(&self.db, guard, fence, Some(request.clock.wall_now()?))
    }
    /// Observation only: callers can compare holder/generation/expiry for wait.
    pub fn landing(
        &self,
        project: ProjectId,
        branch: &str,
        request: &Request<'_>,
    ) -> Result<Option<Landing>> {
        request.check()?;
        reference(branch)?;
        let bytes: Option<Vec<u8>> = self.db.query_row(
            "SELECT substr(record,1,?3) FROM claims WHERE project=?1 AND kind='landing' AND scope_key=?2",
            params![project.to_string(), branch, MAX_RECORD_READ_BYTES], |row| row.get(0),
        ).optional()?;
        bytes.map(|bytes| decode(&bytes)).transpose()
    }
    pub fn renew_landing(
        &self,
        guard: &LandingGuard,
        fence: &ClaimFence,
        lease: DurationMicros,
        request: &Request<'_>,
    ) -> Result<()> {
        self.change_landing(guard, fence, Some(lease), request)
    }
    pub fn release_landing(
        &self,
        guard: &LandingGuard,
        fence: &ClaimFence,
        request: &Request<'_>,
    ) -> Result<()> {
        self.change_landing(guard, fence, None, request)
    }
    fn change_landing(
        &self,
        guard: &LandingGuard,
        fence: &ClaimFence,
        lease: Option<DurationMicros>,
        request: &Request<'_>,
    ) -> Result<()> {
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            // Only release may use an expired fence; unresolved pushes pin it.
            let mut value = self.landing_fence(tx, guard, fence, lease.map(|_| at))?;
            if lease.is_none()
                && let Some(operation) = unresolved(tx, guard.project, fence.id)?
            {
                return Err(Error::PushUnresolved { operation });
            }
            value.updated_at = at;
            value.expires_at = lease.map(|d| lease_expiry(at, d)).transpose()?;
            value.holder = lease.map(|_| fence.actor);
            self.save_landing(tx, &value, lease.map(|_| fence.token.as_slice()))?;
            claim_event(
                tx,
                guard.project,
                fence,
                if lease.is_some() {
                    EventKind::LandingRenewed
                } else {
                    EventKind::LandingReleased
                },
                if lease.is_some() {
                    "renewed"
                } else {
                    "released"
                },
                value.updated_at,
            )
        })
    }
    pub fn prepare_push(
        &self,
        guard: &LandingGuard,
        fence: &ClaimFence,
        id: &OperationId,
        remote: &str,
        request: &Request<'_>,
    ) -> Result<PushOperation> {
        if remote.is_empty()
            || remote.len() > 4096
            || remote.starts_with('-')
            || remote.bytes().any(|b| b.is_ascii_control())
            || remote.contains(['?', '#'])
            || remote.split_once("://").is_some_and(|(_, tail)| {
                tail.split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            })
        {
            return Err(Error::Invalid("push remote"));
        }
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let mut landing = self.landing_fence(tx, guard, fence, Some(at))?;
            if landing.operation.as_ref().is_some_and(|old| old != id) {
                return Err(Error::Conflict);
            }
            let frozen = PushRequest {
                extra: Default::default(),
                header: RecordHeader::V1,
                project: guard.project,
                claim: fence.id,
                actor: fence.actor,
                generation: fence.generation,
                reference: landing.reference.clone(),
                expected: landing.expected,
                candidate: landing.candidate,
                remote: remote.into(),
            };
            let digest = CanonicalV1::record_digest("fabric-push", 1, &frozen)?;
            match load_operation(tx, guard.project, id) {
                Ok(old) if old.digest == digest => return Ok(old),
                Ok(_) => return Err(Error::Conflict),
                Err(Error::NotFound) => {}
                Err(error) => return Err(error),
            }
            let operation = PushOperation {
                extra: Default::default(),
                header: RecordHeader::V1,
                id: id.clone(),
                digest,
                request: frozen,
                phase: PushPhase::Prepared,
                observed_remote: None,
                updated_at: at,
            };
            save_operation(tx, &operation)?;
            landing.operation = Some(id.clone());
            landing.updated_at = at;
            self.save_landing(tx, &landing, Some(&fence.token))?;
            claim_event(
                tx,
                guard.project,
                fence,
                EventKind::PushChanged,
                "prepared",
                at,
            )?;
            Ok(operation)
        })
    }
    pub fn push_operation(
        &self,
        project: ProjectId,
        id: &OperationId,
        request: &Request<'_>,
    ) -> Result<PushOperation> {
        request.check()?;
        load_operation(&self.db, project, id)
    }
    pub fn change_push(
        &self,
        guard: &LandingGuard,
        fence: &ClaimFence,
        id: &OperationId,
        action: PushAction,
        request: &Request<'_>,
    ) -> Result<PushOperation> {
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let landing = self.landing_fence(
                tx,
                guard,
                fence,
                if action == PushAction::Begin {
                    Some(at)
                } else {
                    None
                },
            )?;
            let mut operation = load_operation(tx, guard.project, id)?;
            if landing.operation.as_ref() != Some(id)
                || operation.request.claim != fence.id
                || operation.request.generation != fence.generation
                || operation.request.actor != fence.actor
            {
                return Err(Error::StaleFence);
            }
            let phase = match (operation.phase, action) {
                (PushPhase::Prepared | PushPhase::Retryable, PushAction::Begin) => {
                    PushPhase::Pushing
                }
                (PushPhase::Pushing, PushAction::Uncertain) => PushPhase::Unknown,
                (PushPhase::Pushing, PushAction::Confirmed) => PushPhase::Committed,
                (PushPhase::Pushing | PushPhase::Unknown, PushAction::Reconcile { remote }) => {
                    operation.observed_remote = remote;
                    match remote {
                        Some(oid) if oid == operation.request.candidate => PushPhase::Committed,
                        Some(oid) if oid == operation.request.expected => PushPhase::Retryable,
                        Some(_) => PushPhase::Conflict,
                        None => PushPhase::Unknown,
                    }
                }
                (from, _) => return Err(Error::PushTransition { from, action }),
            };
            if action == PushAction::Begin {
                operation.observed_remote = None;
            } else if action == PushAction::Confirmed {
                operation.observed_remote = Some(operation.request.candidate);
            }
            operation.phase = phase;
            operation.updated_at = at;
            save_operation(tx, &operation)?;
            claim_event(
                tx,
                guard.project,
                fence,
                EventKind::PushChanged,
                phase.sql(),
                at,
            )?;
            Ok(operation)
        })
    }
}
