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
const DISCOVERY_LIMIT: u64 = 4096;
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

/// Enumerate only explicitly registered chairs for an already bound project.
/// The bounded scan fails on unreadable entries rather than claiming coverage.
pub fn discover_sessions(
    root: &StateRoot,
    project: ProjectId,
) -> Result<Vec<DiscoveredSession>, SessionError> {
    let open = || -> Result<Directory, FsError> {
        let mut dir = Directory::open_anchor(root.as_path())?;
        for name in ["workflow", "projects", &project.to_string(), "sessions"] {
            dir = dir.open_verified_directory(OsStr::new(name))?;
        }
        Ok(dir)
    };
    let dir = match open() {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
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
) -> Result<(Vec<DiscoveredSession>, usize, bool), SessionError> {
    if budget == 0 {
        return Ok((Vec::new(), 0, true));
    }
    let open = || -> Result<Directory, FsError> {
        let mut dir = Directory::open_anchor(root.as_path())?;
        for name in ["workflow", "projects", &project.to_string(), "sessions"] {
            dir = dir.open_verified_directory(OsStr::new(name))?;
        }
        Ok(dir)
    };
    let dir = match open() {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => return Ok((Vec::new(), 0, false)),
        Err(error) => return Err(error.into()),
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
    Ok((found, examined, examined == budget))
}

impl SessionStore {
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
        publish(dir, DISCOVERY, &bytes)
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
