//! Metadata for explicitly tracked native Claude sub-agents. No transcript
//! content or credential data is accepted by this store.
use provenant_core::Roots;
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, io::Read, path::Path};
use thiserror::Error;

const STORE: &str = "native-attempts.json";
const LOCK: &str = "native-attempts.lock";
const CAP: u64 = 16 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Settlement {
    Succeeded,
    Failed,
    Cancelled,
}

/// A native attempt is separate from Fabric lanes and records metadata only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub id: String,
    pub alias: String,
    pub model: String,
    pub effort: Option<String>,
    pub session: String,
    /// SHA-256 of the config directory path, not the path itself.
    pub config: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub settlement: Option<Settlement>,
    pub agent_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    schema_version: u64,
    attempts: Vec<Attempt>,
}

#[derive(Debug, Error)]
pub enum AttemptError {
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid native attempt metadata")]
    Invalid,
    #[error("native attempt store is unreadable or exceeds its size cap")]
    Store,
    #[error("native attempt already exists, is missing, or is already settled")]
    Conflict,
}

fn token(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value.bytes().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/' | b':' | b'@')
        })
}

/// Identify the serving config directory without retaining its path.
pub fn config_key(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    provenant_core::hex(&Sha256::digest(canonical.as_os_str().as_bytes()))
}

fn valid(attempt: &Attempt) -> bool {
    token(&attempt.id, 128)
        && token(&attempt.alias, 128)
        && token(&attempt.model, 128)
        && attempt.effort.as_ref().is_none_or(|v| token(v, 32))
        && token(&attempt.session, 64)
        && attempt.config.len() == 64
        && attempt.config.bytes().all(|c| c.is_ascii_hexdigit())
        && attempt.started_at.parse::<jiff::Timestamp>().is_ok()
        && attempt
            .ended_at
            .as_ref()
            .is_none_or(|t| t.parse::<jiff::Timestamp>().is_ok())
        && attempt
            .agent_id
            .as_ref()
            .is_none_or(|id| token(id, 128) && !id.contains('/'))
        && (attempt.ended_at.is_some() == attempt.settlement.is_some())
        && (attempt.agent_id.is_none() || attempt.ended_at.is_some())
}

fn load(directory: &Directory) -> Result<Vec<Attempt>, AttemptError> {
    let file = match directory.open_read(OsStr::new(STORE)) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.size > CAP {
        return Err(AttemptError::Store);
    }
    let mut bytes = Vec::new();
    file.take(CAP + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CAP {
        return Err(AttemptError::Store);
    }
    let store: Store = serde_json::from_slice(&bytes).map_err(|_| AttemptError::Store)?;
    if store.schema_version != 1 || store.attempts.iter().any(|attempt| !valid(attempt)) {
        return Err(AttemptError::Store);
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut agents = std::collections::BTreeSet::new();
    for attempt in &store.attempts {
        if !ids.insert((&attempt.config, &attempt.session, &attempt.id))
            || attempt
                .agent_id
                .as_ref()
                .is_some_and(|agent| !agents.insert((&attempt.config, &attempt.session, agent)))
        {
            return Err(AttemptError::Store);
        }
    }
    Ok(store.attempts)
}

/// Read an atomic snapshot; an absent state root or store is empty.
pub fn read(roots: &Roots) -> Result<Vec<Attempt>, AttemptError> {
    let directory = match Directory::open_anchor(roots.state.as_path()) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    load(&directory)
}

fn update(
    roots: &Roots,
    f: impl FnOnce(&mut Vec<Attempt>) -> Result<(), AttemptError>,
) -> Result<(), AttemptError> {
    let directory = Directory::open_anchor(roots.state.as_path())?;
    let _lock = directory.try_lock(OsStr::new(LOCK), LockMode::Exclusive)?;
    let mut attempts = load(&directory)?;
    f(&mut attempts)?;
    let bytes = serde_json::to_vec(&Store {
        schema_version: 1,
        attempts,
    })
    .map_err(std::io::Error::other)?;
    if bytes.len() as u64 > CAP {
        return Err(AttemptError::Store);
    }
    directory
        .atomic_write(
            OsStr::new(STORE),
            ".native-attempts-",
            &bytes,
            PublishMode::Replace,
        )?
        .require_durable()?;
    Ok(())
}

/// Begin an attempt with a caller-supplied opaque tool-use ID.
pub fn start(roots: &Roots, attempt: Attempt) -> Result<(), AttemptError> {
    if !valid(&attempt) || attempt.ended_at.is_some() || attempt.agent_id.is_some() {
        return Err(AttemptError::Invalid);
    }
    update(roots, |attempts| {
        if attempts.iter().any(|old| {
            old.id == attempt.id && old.session == attempt.session && old.config == attempt.config
        }) {
            return Err(AttemptError::Conflict);
        }
        attempts.push(attempt);
        Ok(())
    })
}

/// Metadata supplied when settling a native tool call.
pub struct End<'a> {
    pub config: &'a str,
    pub session: &'a str,
    pub id: &'a str,
    pub ended_at: &'a str,
    pub settlement: Settlement,
    /// A failed launch may not have produced a transcript.
    pub agent_id: Option<&'a str>,
}

/// Settle an attempt and, when known, associate its transcript's agent ID.
pub fn end(roots: &Roots, end: End<'_>) -> Result<(), AttemptError> {
    if !token(end.id, 128)
        || !token(end.session, 64)
        || end.agent_id.is_some_and(|id| {
            !token(id, 64)
                || id
                    .bytes()
                    .any(|c| !(c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
        })
        || end.ended_at.parse::<jiff::Timestamp>().is_err()
    {
        return Err(AttemptError::Invalid);
    }
    update(roots, |attempts| {
        if end.agent_id.is_some_and(|agent| {
            attempts.iter().any(|a| {
                a.config == end.config
                    && a.session == end.session
                    && a.id != end.id
                    && a.agent_id.as_deref() == Some(agent)
            })
        }) {
            return Err(AttemptError::Conflict);
        }
        let attempt = attempts
            .iter_mut()
            .find(|a| a.config == end.config && a.session == end.session && a.id == end.id)
            .ok_or(AttemptError::Conflict)?;
        if attempt.ended_at.is_some() {
            if attempt.settlement.as_ref() != Some(&end.settlement)
                || (attempt.agent_id.is_some() && attempt.agent_id.as_deref() != end.agent_id)
            {
                return Err(AttemptError::Conflict);
            }
        } else {
            attempt.ended_at = Some(end.ended_at.to_owned());
            attempt.settlement = Some(end.settlement);
        }
        if let Some(agent) = end.agent_id {
            attempt.agent_id = Some(agent.to_owned());
        }
        Ok(())
    })
}

/// A native attempt and the message buckets linked to it. These amounts are
/// a host-wide breakdown that can overlap chair usage, so callers must not
/// add them to chair totals.
#[derive(Serialize)]
pub struct AttemptUsage {
    pub attempt: Attempt,
    pub messages: u64,
    pub buckets: [u64; 5],
    /// Serving models observed in the linked transcripts.
    pub models: std::collections::BTreeMap<String, [u64; 5]>,
}

#[derive(Serialize)]
pub struct UnlinkedUsage {
    pub model: String,
    pub messages: u64,
    pub buckets: [u64; 5],
}

#[derive(Serialize)]
pub struct Report {
    pub attempts: Vec<AttemptUsage>,
    pub unlinked: Vec<UnlinkedUsage>,
}

/// Join previously ingested rows to attempt metadata without adding usage to
/// aggregate totals. Rows without a matching attempt remain visible by model.
pub fn report(attempts: Vec<Attempt>, rows: &[crate::ingest::Row]) -> Report {
    use std::collections::BTreeMap;
    let mut index = BTreeMap::new();
    let mut output: Vec<_> = attempts
        .into_iter()
        .map(|attempt| AttemptUsage {
            attempt,
            messages: 0,
            buckets: [0; 5],
            models: BTreeMap::new(),
        })
        .collect();
    for (position, item) in output.iter().enumerate() {
        if let Some(agent) = &item.attempt.agent_id {
            index.insert(
                (
                    item.attempt.config.clone(),
                    item.attempt.session.clone(),
                    agent.clone(),
                    item.attempt.id.clone(),
                ),
                position,
            );
        }
    }
    let mut unlinked = BTreeMap::<String, (u64, [u64; 5])>::new();
    for row in rows
        .iter()
        .filter(|row| row.source == crate::ingest::Source::Subagent)
    {
        let matched = row
            .native_config
            .as_ref()
            .zip(row.agent_id.as_ref())
            .zip(row.native_attempt.as_ref())
            .and_then(|((config, agent), id)| {
                index.get(&(
                    config.clone(),
                    row.session.clone(),
                    agent.clone(),
                    id.clone(),
                ))
            })
            .copied();
        let (messages, buckets) = if let Some(position) = matched {
            let item = &mut output[position];
            let model = item.models.entry(row.model.clone()).or_insert([0; 5]);
            for (total, value) in model.iter_mut().zip(row.buckets()) {
                *total = total.saturating_add(value);
            }
            (&mut item.messages, &mut item.buckets)
        } else {
            let item = unlinked.entry(row.model.clone()).or_default();
            (&mut item.0, &mut item.1)
        };
        *messages = messages.saturating_add(1);
        for (total, value) in buckets.iter_mut().zip(row.buckets()) {
            *total = total.saturating_add(value);
        }
    }
    Report {
        attempts: output,
        unlinked: unlinked
            .into_iter()
            .map(|(model, (messages, buckets))| UnlinkedUsage {
                model,
                messages,
                buckets,
            })
            .collect(),
    }
}
