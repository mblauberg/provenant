//! Explicit chair registration, published after checkpoint revision metadata.
use super::*;
use provenant_core::{ActorId, HostId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChairIdentity {
    pub host: HostId,
    pub actor: ActorId,
}

const DISCOVERY: &str = "discovery.json";
const DISCOVERY_LIMIT: u64 = 64 * 1024;
const REBIND_LIMIT: usize = 8;

/// One per-session index entry, stored in `discovery.json`. Unknown provider,
/// summary, lifecycle and lane facts are not inferred from a checkpoint save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDiscovery {
    pub schema_version: u64,
    pub host: HostId,
    pub project_id: ProjectId,
    pub host_session_id: HostSessionId,
    pub chair_actor: ActorId,
    pub created_at: UtcMicros,
    pub last_saved_at: UtcMicros,
    pub checkpoint: Revision,
    /// Summary parsed from the checkpoint identified by `checkpoint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "rebind_history"
    )]
    pub rebound_from: Vec<ChairRebind>,
}

// Accept the previous single-hop record and upgrade it on the next save.
fn rebind_history<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ChairRebind>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum History {
        Many(Vec<ChairRebind>),
        One(ChairRebind),
    }
    let mut history = match Option::<History>::deserialize(deserializer)? {
        Some(History::Many(history)) => history,
        Some(History::One(previous)) => vec![previous],
        None => Vec::new(),
    };
    let excess = history.len().saturating_sub(REBIND_LIMIT);
    history.drain(..excess);
    Ok(history)
}

/// The previous binding and when an operator explicitly replaced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChairRebind {
    pub host: HostId,
    pub actor: ActorId,
    pub changed_at: UtcMicros,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryConsistency {
    Current,
    CheckpointChanged,
    CheckpointMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredSession {
    pub record: SessionDiscovery,
    pub consistency: DiscoveryConsistency,
}

fn sessions_directory(
    root: &StateRoot,
    project: ProjectId,
) -> Result<Option<Directory>, SessionError> {
    let open = || -> Result<Directory, FsError> {
        let mut dir = Directory::open_anchor(root.as_path())?;
        for name in ["workflow", "projects", &project.to_string(), "sessions"] {
            dir = dir.open_verified_directory(OsStr::new(name))?;
        }
        Ok(dir)
    };
    match open() {
        Ok(dir) => Ok(Some(dir)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Result of a bounded session-directory scan.
pub struct SessionScan {
    pub sessions: Vec<DiscoveredSession>,
    pub examined: usize,
    pub truncated: bool,
}

/// Enumerate only explicitly registered chairs for an already bound project.
/// The bounded scan fails on unreadable entries rather than claiming coverage.
pub fn discover_sessions(
    root: &StateRoot,
    project: ProjectId,
) -> Result<Vec<DiscoveredSession>, SessionError> {
    let Some(dir) = sessions_directory(root, project)? else {
        return Ok(Vec::new());
    };
    let mut names = provenant_os::fs::entry_names(&dir, 16_384)?;
    names.sort();
    let mut found = Vec::new();
    for name in names {
        if let Some(session) = name
            .to_str()
            .and_then(|name| HostSessionId::parse(name).ok())
            && let Some(record) = SessionStore::new(root.clone(), project, session).discovery()?
        {
            found.push(record);
        }
    }
    Ok(found)
}

/// Scan at most `budget` directory entries, including invalid and unregistered
/// names. Exhausting the budget conservatively reports truncation without
/// examining another entry. Reads do not repair or create records.
pub fn discover_sessions_bounded(
    root: &StateRoot,
    project: ProjectId,
    budget: usize,
) -> Result<SessionScan, SessionError> {
    if budget == 0 {
        return Ok(SessionScan {
            sessions: Vec::new(),
            examined: 0,
            truncated: true,
        });
    }
    let Some(dir) = sessions_directory(root, project)? else {
        return Ok(SessionScan {
            sessions: Vec::new(),
            examined: 0,
            truncated: false,
        });
    };
    let mut names = dir.entries()?.take(budget).collect::<Result<Vec<_>, _>>()?;
    let examined = names.len();
    names.sort();
    let mut found = Vec::new();
    for name in names {
        if let Some(session) = name
            .to_str()
            .and_then(|name| HostSessionId::parse(name).ok())
            && let Some(record) = SessionStore::new(root.clone(), project, session).discovery()?
        {
            found.push(record);
        }
    }
    Ok(SessionScan {
        sessions: found,
        examined,
        truncated: examined == budget,
    })
}

impl SessionStore {
    /// Publish an advisory direct lookup for this explicitly saved workspace.
    /// Canonicalisation is filesystem-only; it never discovers a Git project.
    #[must_use]
    pub fn with_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Only an edge with explicit chair identity opts a session into discovery.
    /// A later save cannot silently transfer its host or actor ownership.
    #[must_use]
    pub fn with_chair(mut self, host: HostId, actor: ActorId) -> Self {
        self.chair = Some(ChairIdentity { host, actor });
        self
    }

    /// Explicitly authorise replacing the registered host/actor on this save.
    #[must_use]
    pub fn with_rebind(mut self, rebind: bool) -> Self {
        self.rebind = rebind;
        self
    }

    /// Preflight an existing chair binding without requiring readable history.
    pub fn validate_chair(&self) -> Result<(), SessionError> {
        match self.directory(false) {
            Ok(dir) => self.check_discovery_identity(self.load_discovery(&dir)?.as_ref()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn load_discovery(
        &self,
        dir: &Directory,
    ) -> Result<Option<SessionDiscovery>, SessionError> {
        match dir.read_bounded(OsStr::new(DISCOVERY), DISCOVERY_LIMIT) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| SessionError::History),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn publish_discovery(
        &self,
        dir: &Directory,
        record: &SessionDiscovery,
    ) -> Result<(), SessionError> {
        let bytes = serde_json::to_vec(record).map_err(|_| SessionError::History)?;
        publish(dir, DISCOVERY, &bytes)?;
        if let Some(workspace) = &self.workspace {
            let workspace = provenant_os::fs::canonicalize(workspace)?;
            let index = location_directory(&self.root, true)?;
            publish(&index, &location_key(&workspace, &self.session), &bytes)?;
        }
        Ok(())
    }

    pub(super) fn check_discovery_identity(
        &self,
        existing: Option<&SessionDiscovery>,
    ) -> Result<(), SessionError> {
        if let Some(record) = existing
            && (record.schema_version != 1
                || record.project_id != self.project
                || record.host_session_id != self.session
                || !self.rebind
                    && self.chair.as_ref().is_some_and(|chair| {
                        chair.host != record.host || chair.actor != record.chair_actor
                    }))
        {
            return Err(SessionError::DiscoveryIdentity);
        }
        Ok(())
    }

    pub(super) fn saved_discovery(
        &self,
        existing: Option<SessionDiscovery>,
        checkpoint: &Revision,
        summary: Option<Summary>,
        now: UtcMicros,
    ) -> Option<SessionDiscovery> {
        let mut record = existing.or_else(|| {
            self.chair.as_ref().map(|chair| SessionDiscovery {
                schema_version: 1,
                host: chair.host,
                project_id: self.project,
                host_session_id: self.session.clone(),
                chair_actor: chair.actor,
                created_at: now,
                last_saved_at: now,
                checkpoint: checkpoint.clone(),
                summary: None,
                rebound_from: Vec::new(),
            })
        })?;
        if let Some(chair) = &self.chair
            && self.rebind
            && (record.host != chair.host || record.chair_actor != chair.actor)
        {
            record.rebound_from.push(ChairRebind {
                host: record.host,
                actor: record.chair_actor,
                changed_at: now,
            });
            let excess = record.rebound_from.len().saturating_sub(REBIND_LIMIT);
            record.rebound_from.drain(..excess);
            record.host = chair.host;
            record.chair_actor = chair.actor;
        }
        record.last_saved_at = now;
        record.checkpoint = checkpoint.clone();
        record.summary = summary;
        Some(record)
    }

    /// Reads never lock, create directories, allocate identity or repair state.
    /// Missing registration remains unknown even when a checkpoint exists.
    pub fn discovery(&self) -> Result<Option<DiscoveredSession>, SessionError> {
        let dir = match self.directory(false) {
            Ok(dir) => dir,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let Some(record) = self.load_discovery(&dir)? else {
            return Ok(None);
        };
        let manifest = match load(&dir) {
            Ok(manifest) => manifest,
            Err(SessionError::Filesystem(error)) if error.is_not_found() => Manifest::default(),
            Err(error) => return Err(error),
        };
        self.check_discovery_identity(Some(&record))?;
        let consistency = match read_current(&dir)? {
            None => DiscoveryConsistency::CheckpointMissing,
            Some((bytes, _))
                if provenant_records::CanonicalV1::file_digest(&bytes)
                    == record.checkpoint.sha256
                    && manifest.revisions.last() == Some(&record.checkpoint) =>
            {
                DiscoveryConsistency::Current
            }
            Some(_) => DiscoveryConsistency::CheckpointChanged,
        };
        Ok(Some(DiscoveredSession {
            record,
            consistency,
        }))
    }
}

fn location_key(workspace: &std::path::Path, session: &HostSessionId) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = workspace.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(session.as_str().as_bytes());
    format!(
        "{}.json",
        provenant_records::CanonicalV1::file_digest(&bytes)
    )
}

fn location_directory(root: &StateRoot, create: bool) -> Result<Directory, FsError> {
    let mut dir = Directory::open_anchor(root.as_path())?;
    for name in ["workflow", "session-locations"] {
        let name = OsStr::new(name);
        if create {
            match dir.mkdir(name, 0o700) {
                Ok(()) => dir.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error),
            }
        }
        dir = dir.open_verified_directory(name)?;
    }
    Ok(dir)
}

/// The most recently saved session a chair actor owns in a project. Chairs
/// bind to their session by saving it, whatever harness they run under, so this
/// needs no harness-specific marker. A locator is advisory: each candidate is
/// confirmed against the session's own canonical record, which must be current
/// and still name this chair, so a stale locator after a rebind is ignored.
pub fn chair_session_for(
    root: &StateRoot,
    project: provenant_core::ProjectId,
    actor: ActorId,
) -> Option<HostSessionId> {
    let entries =
        std::fs::read_dir(root.as_path().join("workflow").join("session-locations")).ok()?;
    let mut candidates: Vec<SessionDiscovery> = entries
        .filter_map(|entry| {
            let bytes = std::fs::read(entry.ok()?.path()).ok()?;
            if bytes.len() > DISCOVERY_LIMIT as usize {
                return None;
            }
            serde_json::from_slice::<SessionDiscovery>(&bytes).ok()
        })
        .filter(|found| found.project_id == project && found.chair_actor == actor)
        .collect();
    candidates.sort_by_key(|found| std::cmp::Reverse(found.last_saved_at));
    candidates.into_iter().find_map(|locator| {
        let canonical = SessionStore::new(root.clone(), project, locator.host_session_id.clone())
            .discovery()
            .ok()??;
        (canonical.consistency == DiscoveryConsistency::Current
            && canonical.record.project_id == project
            && canonical.record.chair_actor == actor
            && canonical.record.host_session_id == locator.host_session_id)
            .then_some(locator.host_session_id)
    })
}

/// Direct workspace/session lookup of an explicitly saved chair. No enumeration,
/// allocation, locking, Git discovery or repair. Stale locators remain unknown.
/// The deadline is checked between steps. Blocking filesystem operations,
/// including canonicalisation and checkpoint reads, cannot be interrupted.
/// Callers needing a hard bound must run this off-thread and time out the wait.
#[allow(clippy::disallowed_methods)]
pub fn discover_session_at(
    root: &StateRoot,
    workspace: &std::path::Path,
    session: &HostSessionId,
    deadline: std::time::Instant,
) -> Result<Option<DiscoveredSession>, SessionError> {
    let check = || {
        if std::time::Instant::now() >= deadline {
            Err(SessionError::Deadline)
        } else {
            Ok(())
        }
    };
    check()?;
    let read = || -> Result<Vec<u8>, FsError> {
        let workspace = provenant_os::fs::canonicalize(workspace)?;
        location_directory(root, false)?.read_bounded(
            OsStr::new(&location_key(&workspace, session)),
            DISCOVERY_LIMIT,
        )
    };
    let bytes = match read() {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    check()?;
    let locator: SessionDiscovery =
        serde_json::from_slice(&bytes).map_err(|_| SessionError::History)?;
    if locator.schema_version != 1 || &locator.host_session_id != session {
        return Err(SessionError::DiscoveryIdentity);
    }
    let found = SessionStore::new(root.clone(), locator.project_id, session.clone()).discovery()?;
    check()?;
    Ok(found.filter(|found| {
        found.record == locator && found.consistency == DiscoveryConsistency::Current
    }))
}
