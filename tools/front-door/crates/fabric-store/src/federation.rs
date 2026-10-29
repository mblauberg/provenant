//! Durable peer mutation receipts and home-scoped run-event ingestion.
use crate::{events, *};
use provenant_core::{ActorId, Digest, HostId, OperationId, ProjectId};
use provenant_records::run::RunEvent;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

const MAX_PEER_PAYLOAD_BYTES: usize = MAX_RECORD_BYTES;
const MAX_RUN_EVENT_PAGE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerPhase {
    Prepared,
    Sent,
    Unknown,
    Committed,
}
impl PeerPhase {
    fn sql(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Sent => "sent",
            Self::Unknown => "unknown",
            Self::Committed => "committed",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "sent" => Ok(Self::Sent),
            "unknown" => Ok(Self::Unknown),
            "committed" => Ok(Self::Committed),
            _ => Err(Error::Invalid("stored peer phase")),
        }
    }
}

/// Frozen peer mutation data. `request` is never rewritten after preparation.
/// A committed `response` is the caller's own re-encoding of the owner's
/// decoded reply, never the bytes received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerOperation {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub project: ProjectId,
    pub actor: ActorId,
    pub id: OperationId,
    pub digest: Digest,
    pub phase: PeerPhase,
    pub request: Vec<u8>,
    pub response: Option<Vec<u8>>,
}

/// Only a sent request can become uncertain. An unknown request cannot be sent
/// again; reconciliation may only record the owner's committed receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerPhaseChange {
    Sent,
    Unknown,
    Committed { response: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RunEventDetail<'a> {
    host: HostId,
    event: &'a RunEvent,
}

fn checked_payload(bytes: &[u8], field: &'static str, allow_empty: bool) -> Result<()> {
    if (!allow_empty && bytes.is_empty()) || bytes.len() > MAX_PEER_PAYLOAD_BYTES {
        return Err(Error::Invalid(field));
    }
    Ok(())
}

fn load_peer(
    db: &rusqlite::Connection,
    project: ProjectId,
    actor: ActorId,
    id: &OperationId,
) -> Result<Option<PeerOperation>> {
    type PeerRow = (String, String, Vec<u8>, String, Option<Vec<u8>>);
    let row: Option<PeerRow> = db
        .query_row(
            "SELECT actor,digest,request,phase,response FROM peer_operations \
             WHERE project=?1 AND id=?2",
            params![project.to_string(), id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_actor, digest, request, phase, response)) = row else {
        return Ok(None);
    };
    let stored_actor =
        ActorId::parse(&stored_actor).map_err(|_| Error::Invalid("stored peer actor"))?;
    if stored_actor != actor {
        return Err(Error::Conflict);
    }
    let phase = PeerPhase::parse(&phase)?;
    checked_payload(&request, "stored peer request", false)?;
    if let Some(bytes) = &response {
        checked_payload(bytes, "stored peer response", true)?;
    }
    if (phase == PeerPhase::Committed) != response.is_some() {
        return Err(Error::Invalid("stored peer response phase"));
    }
    Ok(Some(PeerOperation {
        extra: Default::default(),
        project,
        actor,
        id: id.clone(),
        digest: Digest::parse(&digest).map_err(|_| Error::Invalid("stored peer digest"))?,
        phase,
        request,
        response,
    }))
}

impl Store {
    /// Freeze one outbound request before transport. Repeating the same key and
    /// bytes returns its current phase; it never resets `unknown` to `prepared`.
    pub fn prepare_peer(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        digest: Digest,
        request_bytes: &[u8],
        request: &Request<'_>,
    ) -> Result<PeerOperation> {
        checked_payload(request_bytes, "peer request", false)?;
        self.write(request, |tx| {
            if let Some(existing) = load_peer(tx, project, actor, id)? {
                if existing.digest != digest || existing.request.as_slice() != request_bytes {
                    return Err(Error::Conflict);
                }
                return Ok(existing);
            }
            tx.execute(
                "INSERT INTO peer_operations(project,id,actor,digest,phase,request,response) \
                 VALUES(?1,?2,?3,?4,'prepared',?5,NULL)",
                params![
                    project.to_string(),
                    id.as_str(),
                    actor.to_string(),
                    digest.to_string(),
                    request_bytes,
                ],
            )?;
            Ok(PeerOperation {
                extra: Default::default(),
                project,
                actor,
                id: id.clone(),
                digest,
                phase: PeerPhase::Prepared,
                request: request_bytes.to_vec(),
                response: None,
            })
        })
    }

    /// Read one actor's durable peer receipt without changing its phase.
    pub fn lookup_peer(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        request: &Request<'_>,
    ) -> Result<Option<PeerOperation>> {
        request.retry(|| load_peer(&self.db, project, actor, id))
    }

    /// Return a sent request to Prepared after a known transient owner refusal.
    /// The frozen request and digest stay bound to the same operation ID.
    pub fn retry_peer(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        request: &Request<'_>,
    ) -> Result<()> {
        self.write(request, |tx| {
            let operation = load_peer(tx, project, actor, id)?.ok_or(Error::NotFound)?;
            if operation.phase != PeerPhase::Sent {
                return Err(Error::Conflict);
            }
            let updated = tx.execute(
                "UPDATE peer_operations SET phase='prepared',response=NULL \
                 WHERE project=?1 AND id=?2 AND actor=?3 AND phase='sent'",
                params![project.to_string(), id.as_str(), actor.to_string()],
            )?;
            if updated != 1 {
                return Err(Error::Conflict);
            }
            Ok(())
        })
    }

    /// Advance the journal after transport or reconciliation. `unknown` is
    /// terminal for sending; it can become committed only after owner proof.
    pub fn peer_phase(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &OperationId,
        change: PeerPhaseChange,
        request: &Request<'_>,
    ) -> Result<PeerOperation> {
        if let PeerPhaseChange::Committed { response } = &change {
            checked_payload(response, "peer response", true)?;
        }
        self.write(request, |tx| {
            let mut operation = load_peer(tx, project, actor, id)?.ok_or(Error::NotFound)?;
            let (next, response) = match (&operation.phase, &change) {
                (PeerPhase::Prepared, PeerPhaseChange::Sent) => (PeerPhase::Sent, None),
                (PeerPhase::Sent, PeerPhaseChange::Sent) => (PeerPhase::Sent, None),
                (PeerPhase::Sent, PeerPhaseChange::Unknown) => (PeerPhase::Unknown, None),
                (PeerPhase::Unknown, PeerPhaseChange::Unknown) => (PeerPhase::Unknown, None),
                (PeerPhase::Sent | PeerPhase::Unknown, PeerPhaseChange::Committed { response }) => {
                    (PeerPhase::Committed, Some(response.clone()))
                }
                (PeerPhase::Committed, PeerPhaseChange::Committed { response })
                    if operation.response.as_deref() == Some(response.as_slice()) =>
                {
                    (PeerPhase::Committed, operation.response.clone())
                }
                _ => return Err(Error::Conflict),
            };
            if operation.phase != next || operation.response != response {
                tx.execute(
                    "UPDATE peer_operations SET phase=?1,response=?2 \
                     WHERE project=?3 AND id=?4 AND actor=?5",
                    params![
                        next.sql(),
                        response,
                        project.to_string(),
                        id.as_str(),
                        actor.to_string(),
                    ],
                )?;
                operation.phase = next;
                operation.response = response;
            }
            Ok(operation)
        })
    }

    /// The cursor is scoped by the local project and remote host. The caller
    /// must verify the peer's ProjectKey/query scope before ingesting records;
    /// each RunEvent retains its host-local ProjectId in event detail.
    pub fn run_event_cursor(
        &self,
        project: ProjectId,
        host: HostId,
        request: &Request<'_>,
    ) -> Result<u64> {
        request.retry(|| {
            let sequence: Option<i64> = self
                .db
                .query_row(
                    "SELECT sequence FROM run_event_cursors WHERE project=?1 AND host=?2",
                    params![project.to_string(), host.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            u64::try_from(sequence.unwrap_or(0)).map_err(|_| Error::Invalid("stored run cursor"))
        })
    }

    /// Atomically add host-qualified run events to the project event stream and
    /// advance that host's source cursor. Remote ProjectIds are intentionally
    /// preserved in detail and are not compared with the local project ID.
    pub fn ingest_run_events(
        &self,
        project: ProjectId,
        host: HostId,
        incoming: &[RunEvent],
        request: &Request<'_>,
    ) -> Result<u64> {
        if incoming.len() > MAX_RUN_EVENT_PAGE {
            return Err(Error::Invalid("run event page size"));
        }
        let mut previous = 0_u64;
        for event in incoming {
            if event.schema_version != 1 || event.sequence == 0 || event.sequence <= previous {
                return Err(Error::Invalid("run event order or version"));
            }
            i64::try_from(event.sequence).map_err(|_| Error::Invalid("run event sequence"))?;
            previous = event.sequence;
        }
        self.write(request, |tx| {
            let current: i64 = tx
                .query_row(
                    "SELECT sequence FROM run_event_cursors WHERE project=?1 AND host=?2",
                    params![project.to_string(), host.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            let mut latest = current;
            for event in incoming {
                let detail = RunEventDetail { host, event };
                let canonical = CanonicalV1::bytes(&detail)?;
                let detail = String::from_utf8(canonical.as_bytes().to_vec())
                    .map_err(|_| Error::Invalid("run event detail"))?;
                text(&detail, "run event detail")?;
                let entity = event.attempt_id.to_string();
                let sequence = i64::try_from(event.sequence)
                    .map_err(|_| Error::Invalid("run event sequence"))?;
                let dedup = format!("run:{}:{sequence}", host);
                let previous_record: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT substr(record,1,?3) FROM events WHERE project=?1 AND dedup=?2",
                        params![project.to_string(), dedup, MAX_RECORD_READ_BYTES],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(bytes) = previous_record {
                    let previous: EventRecord = decode(&bytes)?;
                    if previous.project != project
                        || previous.actor.is_some()
                        || previous.kind != EventKind::RunChanged
                        || previous.entity.as_deref() != Some(entity.as_str())
                        || previous.detail != detail
                    {
                        return Err(Error::Conflict);
                    }
                } else {
                    let record = events::event(
                        project,
                        None,
                        EventKind::RunChanged,
                        Some(entity),
                        detail,
                        request.clock.wall_now()?,
                    );
                    events::append(tx, &record, Some(&dedup))?;
                }
                latest = latest.max(sequence);
            }
            if latest > current {
                tx.execute(
                    "INSERT INTO run_event_cursors(project,host,sequence) \
                     VALUES(?1,?2,?3) ON CONFLICT(project,host) \
                     DO UPDATE SET sequence=max(sequence,excluded.sequence)",
                    params![project.to_string(), host.to_string(), latest],
                )?;
            }
            u64::try_from(latest).map_err(|_| Error::Invalid("stored run cursor"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{
        CancelToken, Clock, MonoTime, PathResolver, RootInputs, StateRoot, TestClock, WaitBudget,
        WallTime,
    };
    use provenant_records::run::AttemptState;
    use std::path::{Path, PathBuf};

    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            if path.exists() {
                path.canonicalize()
            } else {
                let parent = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
                let name = path.file_name().ok_or(std::io::ErrorKind::InvalidInput)?;
                Ok(self.resolve_existing_ancestor(parent)?.join(name))
            }
        }
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        root: StateRoot,
        clock: TestClock,
        cancel: CancelToken,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = crate::test_support::tempdir().expect("temp directory");
            let base = temp.path().canonicalize().expect("canonical temp root");
            let mut roots = RootInputs::new(base.join("home"), base.join("checkout"));
            roots.state_dir = Some(base.join("state"));
            Self {
                _temp: temp,
                root: roots.resolve(&Resolver).expect("test roots").state,
                clock: TestClock::new(
                    WallTime::from_micros(2_000_000_000_000_000).expect("wall time"),
                    MonoTime::from_micros(0),
                ),
                cancel: CancelToken::new(),
            }
        }
        fn request(&self) -> Request<'_> {
            Request {
                clock: &self.clock,
                budget: WaitBudget::cli(self.clock.mono_now(), 5).expect("budget"),
                cancel: &self.cancel,
                backoff: &ThreadWait,
            }
        }
        fn open(&self) -> Store {
            Store::open(&self.root, &self.request()).expect("store")
        }
    }
    fn local_project() -> ProjectId {
        ProjectId::from_bytes([1; 16])
    }
    fn run_event(sequence: u64, revision: u64) -> RunEvent {
        RunEvent {
            extra: Default::default(),
            schema_version: 1,
            sequence,
            project_id: ProjectId::from_bytes([9; 16]),
            run_id: provenant_core::RunId::from_bytes([3; 16]),
            attempt_id: provenant_core::AttemptId::from_bytes([4; 16]),
            revision,
            change: AttemptState::Running {
                extra: Default::default(),
            },
            trace: Default::default(),
        }
    }

    #[test]
    fn peer_operation_replay_preserves_unknown_and_refuses_resend() {
        let fixture = Fixture::new();
        let store = fixture.open();
        let request = fixture.request();
        let actor = ActorId::from_bytes([2; 16]);
        let host_project = local_project();
        let id = OperationId::parse("peer-1").expect("operation ID");
        let digest = Digest::from_bytes([7; 32]);
        let body = br#"{"operation":"submit"}"#;
        assert_eq!(
            store
                .prepare_peer(host_project, actor, &id, digest, body, &request)
                .expect("prepare")
                .phase,
            PeerPhase::Prepared
        );
        store
            .peer_phase(host_project, actor, &id, PeerPhaseChange::Sent, &request)
            .expect("mark sent");
        let unknown = store
            .peer_phase(host_project, actor, &id, PeerPhaseChange::Unknown, &request)
            .expect("mark unknown");
        assert_eq!(unknown.phase, PeerPhase::Unknown);
        assert_eq!(
            store
                .prepare_peer(host_project, actor, &id, digest, body, &request)
                .expect("replay prepare")
                .phase,
            PeerPhase::Unknown
        );
        assert!(matches!(
            store.peer_phase(host_project, actor, &id, PeerPhaseChange::Sent, &request),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            store.prepare_peer(
                host_project,
                actor,
                &id,
                Digest::from_bytes([8; 32]),
                body,
                &request,
            ),
            Err(Error::Conflict)
        ));
        assert_eq!(
            store
                .lookup_peer(host_project, actor, &id, &request)
                .expect("lookup")
                .expect("operation")
                .phase,
            PeerPhase::Unknown
        );
    }

    #[test]
    fn run_event_rows_and_host_cursor_rollback_together_then_deduplicate() {
        let fixture = Fixture::new();
        let store = fixture.open();
        let request = fixture.request();
        let project = local_project();
        let host = HostId::from_bytes([5; 16]);
        let event = run_event(1, 1);
        store
            .db
            .execute_batch(
                r#"CREATE TRIGGER reject_run_cursor BEFORE INSERT ON run_event_cursors
                   BEGIN SELECT RAISE(ABORT,'cursor refused'); END;"#,
            )
            .expect("create trigger");
        assert!(
            store
                .ingest_run_events(project, host, std::slice::from_ref(&event), &request)
                .is_err()
        );
        assert_eq!(
            store
                .run_event_cursor(project, host, &request)
                .expect("cursor"),
            0
        );
        assert!(
            store
                .events(project, 0, 100, &request)
                .expect("events")
                .items
                .is_empty()
        );
        store
            .db
            .execute_batch("DROP TRIGGER reject_run_cursor")
            .expect("drop trigger");

        assert_eq!(
            store
                .ingest_run_events(project, host, std::slice::from_ref(&event), &request)
                .expect("ingest"),
            1
        );
        assert_eq!(
            store
                .ingest_run_events(project, host, std::slice::from_ref(&event), &request)
                .expect("idempotent ingest"),
            1
        );
        let page = store.events(project, 0, 100, &request).expect("events");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].record.kind, EventKind::RunChanged);
        assert!(page.items[0].record.detail.contains(&host.to_string()));
        assert!(
            page.items[0]
                .record
                .detail
                .contains(&event.project_id.to_string())
        );

        let changed = run_event(1, 2);
        assert!(matches!(
            store.ingest_run_events(project, host, &[changed], &request),
            Err(Error::Conflict)
        ));
        assert_eq!(
            store
                .run_event_cursor(project, host, &request)
                .expect("cursor"),
            1
        );
        assert_eq!(
            store
                .events(project, 0, 100, &request)
                .expect("events")
                .items
                .len(),
            1
        );
    }
}
