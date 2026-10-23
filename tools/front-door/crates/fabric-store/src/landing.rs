//! Fenced landing records. The caller holds LandingGuard across Git (F7);
//! SQLite transactions never span that transport or lock acquisition.
use crate::{claims::*, *};
use provenant_core::{ClaimId, Digest, DurationMicros, GitOid, OperationId};
use provenant_records::RecordHeader;
use provenant_supervisor::fs::{LockGuard, LockMode};
use std::ffi::OsStr;

/// Holds the lock returned by `lock_landing`. Store validation proves only
/// that the lock directory matches; it does not prove that a lock is held on
/// a particular file. F7 must re-verify the file binding before Git operations.
pub struct LandingGuard {
    _lock: LockGuard,
    project: ProjectId,
    reference: String,
    directory: (u64, u64),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandingRequest {
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
#[serde(deny_unknown_fields)]
pub struct Landing {
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
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PushAction {
    Begin,
    Uncertain,
    Confirmed,
    Reconcile { remote: Option<GitOid> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushRequest {
    pub header: RecordHeader,
    pub project: ProjectId,
    pub claim: ClaimId,
    pub actor: ActorId,
    pub generation: u64,
    pub reference: String,
    pub expected: GitOid,
    pub candidate: GitOid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushOperation {
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
            "SELECT substr(record,1,262145) FROM operations WHERE project=? AND id=?",
            params![project.to_string(), id.as_str()],
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
fn save_operation(tx: &Transaction<'_>, operation: &PushOperation) -> Result<()> {
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
impl Store {
    pub fn lock_landing(
        &self,
        project: ProjectId,
        branch: &str,
        request: &Request<'_>,
    ) -> Result<LandingGuard> {
        reference(branch)?;
        request.retry(|| Ok(()))?;
        let key = scope_key("fabric-landing-lock", &(project, branch))?;
        // Digest's fixed ASCII hex is safe as a single filesystem component.
        let name = format!(
            "{}.lock",
            key.strip_prefix("sha256:")
                .ok_or(Error::Invalid("lock digest"))?
        );
        let lock = self.locks.lock(
            OsStr::new(&name),
            LockMode::Exclusive,
            request.budget,
            request.cancel,
            request.clock,
        )?;
        let metadata = self.locks.metadata()?;
        Ok(LandingGuard {
            _lock: lock,
            project,
            reference: branch.into(),
            directory: (metadata.device, metadata.inode),
        })
    }
    fn check_guard(&self, guard: &LandingGuard) -> Result<()> {
        let metadata = self.locks.metadata()?;
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
        tx: &Transaction<'_>,
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
                id,
                actor: claim.actor,
                generation,
                token: claim.token,
            };
            let value = Landing {
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
        request
            .retry(|| self.landing_fence(&self.db, guard, fence, Some(request.clock.wall_now()?)))
    }
    /// Observation only: callers can compare holder/generation/expiry for wait.
    pub fn landing(
        &self,
        project: ProjectId,
        branch: &str,
        request: &Request<'_>,
    ) -> Result<Option<Landing>> {
        reference(branch)?;
        request.retry(|| {
            let bytes: Option<Vec<u8>> = self.db.query_row(
                "SELECT substr(record,1,262145) FROM claims WHERE project=? AND kind='landing' AND scope_key=?",
                params![project.to_string(), branch], |row| row.get(0),
            ).optional()?;
            bytes.map(|bytes| decode(&bytes)).transpose()
        })
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
        request: &Request<'_>,
    ) -> Result<PushOperation> {
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let mut landing = self.landing_fence(tx, guard, fence, Some(at))?;
            if landing.operation.as_ref().is_some_and(|old| old != id) {
                return Err(Error::Conflict);
            }
            let frozen = PushRequest {
                header: RecordHeader::V1,
                project: guard.project,
                claim: fence.id,
                actor: fence.actor,
                generation: fence.generation,
                reference: landing.reference.clone(),
                expected: landing.expected,
                candidate: landing.candidate,
            };
            let digest = CanonicalV1::record_digest("fabric-push", 1, &frozen)?;
            match load_operation(tx, guard.project, id) {
                Ok(old) if old.digest == digest => return Ok(old),
                Ok(_) => return Err(Error::Conflict),
                Err(Error::NotFound) => {}
                Err(error) => return Err(error),
            }
            let operation = PushOperation {
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
        request.retry(|| load_operation(&self.db, project, id))
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
