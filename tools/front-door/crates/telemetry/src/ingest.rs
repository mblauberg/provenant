//! Native session ingest: per-message token rows from Claude Code transcripts.
//! Transcript paths locate sessions; chair records carry per-session identity evidence.
//!
//! The store is the `ingest` directory in the state root: one dated segment per
//! UTC day of message time (`2026-10-01.jsonl`), one row per assistant message,
//! keyed by session and message ID so a re-run adds nothing. A row holds counts
//! and identifiers only: never a prompt, a reply or a path. Ingest rewrites
//! only the segments it touches and readers stream the days they ask for, so
//! no operation meets a whole-store size.
use crate::{claude::Raw, segments};
use provenant_records::AccountIdentity;
use crate::usage::{Identity, UnknownIdentityReason};
use provenant_core::Roots;
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};
use thiserror::Error;

pub const ROW_SCHEMA_VERSION: u64 = 3;
/// The only provider whose sessions leave a native transcript.
pub const PROVIDER: &str = "claude";
const DIRECTORY: &str = "ingest";
const LOCK: &str = "ingest.lock";
const CURSORS: &str = "ingest-cursors.json";
const IDENTITY_OVERRIDES: &str = "identity-overrides";
const IDENTITY_OVERRIDE_PREFIX: &str = "session-identity-";
const IDENTITY_OVERRIDE_SCHEMA_VERSION: u64 = 1;
const IDENTITY_OVERRIDE_CAP: u64 = 16 << 10;
const CURSOR_CAP: u64 = 32 << 20;
/// Largest day ingest rewrites. At about 250 bytes a row this is roughly a
/// quarter of a million messages in one day; past it ingest stops adding rows
/// to that day and says so. Readers stream and have no such limit.
const SEGMENT_CAP: u64 = 64 << 20;
/// A longer stored line is not a row.
const MAX_ROW_LINE: usize = 1 << 20;
/// Days before the newest segments that ingest always reloads, so a chair
/// recorded or an attempt settled after its rows were ingested still updates them.
const RECENT_SEGMENTS: usize = 2;
const MAX_FILE_BYTES: u64 = 1 << 30;
/// A longer line is a tool result or attachment, never a usage record.
const MAX_LINE_BYTES: usize = 8 << 20;
const MAX_PROJECT_DIRS: usize = 10_000;
const MAX_SESSIONS: usize = 100_000;
const MAX_SUBAGENTS: usize = 2_000;
const MAX_MESSAGES: usize = 2_000_000;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error("cannot read or write the ingest store: {0}")]
    Io(#[from] std::io::Error),
    #[error("another ingest is running")]
    Busy,
    #[error("the ingest identity-override store is invalid")]
    IdentityOverrideStore,
}

impl IngestError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Filesystem(_) => "filesystem",
            Self::Io(_) => "io",
            Self::Busy => "busy",
            Self::IdentityOverrideStore => "identity_override_store",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A session the chair's SessionStart hook recorded.
    Chair,
    /// A native sub-agent transcript under a session.
    Subagent,
    /// Any other top-level session, such as a lane's.
    Lane,
}

/// One assistant message's usage and per-session identity evidence. The five
/// buckets are disjoint: `cache_write` is the short-TTL part and `cache_write_1h` the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub schema_version: u64,
    pub identity: AccountIdentity,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub uncached_input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_write_1h: u64,
    pub output: u64,
    pub timestamp: String,
    pub session: String,
    pub message: String,
    pub source: Source,
    /// Native transcript identity, absent for top-level sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Digest of the config directory that supplied this row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_config: Option<String>,
    /// Native tool-use ID, present when an attempt matches this transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_attempt: Option<String>,
    /// Provider-reported reasoning tokens; absent means unreported, never zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<crate::signals::ReplyContext>,
}

impl Row {
    pub fn buckets(&self) -> [u64; 5] {
        [
            self.uncached_input,
            self.cache_read,
            self.cache_write,
            self.cache_write_1h,
            self.output,
        ]
    }
    pub fn unix_seconds(&self) -> Option<i64> {
        self.timestamp
            .parse::<jiff::Timestamp>()
            .ok()
            .map(|t| t.as_second())
    }
}

/// Names of the five buckets, in [`Row::buckets`] order.
pub const BUCKETS: [&str; 5] = [
    "uncached_input",
    "cache_read",
    "cache_write",
    "cache_write_1h",
    "output",
];

/// A transcript root. Its path locates sessions and never supplies identity.
#[derive(Clone, Debug)]
pub struct ConfigDir {
    pub path: PathBuf,
}

/// The directories to ingest: registered transcript roots plus other
/// `~/.claude*` roots, including `~/.claude`. Paths locate transcripts only;
/// account identity is carried from the chair session record when present.
pub fn config_dirs(registered: &[PathBuf], home: &Path) -> Vec<ConfigDir> {
    let mut dirs: Vec<ConfigDir> = registered
        .iter()
        .map(|path| ConfigDir { path: path.clone() })
        .collect();
    let claimed: BTreeSet<PathBuf> = dirs
        .iter()
        .map(|dir| std::fs::canonicalize(&dir.path).unwrap_or_else(|_| dir.path.clone()))
        .collect();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(home).into_iter().flatten().flatten() {
        let path = entry.path();
        let named = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".claude"));
        if named
            && plain(&path, true).is_some()
            && plain(&path.join("projects"), true).is_some()
            && !claimed.contains(&std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()))
        {
            found.push(path);
        }
    }
    found.sort();
    dirs.extend(found.into_iter().map(|path| ConfigDir { path }));
    dirs
}

#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub dirs: &'a [ConfigDir],
    /// Per-session identities recorded by the chair hook; other sessions stay unknown.
    pub chair_sessions: &'a BTreeMap<String, AccountIdentity>,
    /// Only `claude` has native transcripts; any other value ingests nothing.
    pub provider: Option<&'a str>,
    /// Skip files not written since and messages stamped before.
    pub since: Option<jiff::Timestamp>,
    /// Stop reading at this instant. Rows read before it are still written.
    pub deadline: Option<Instant>,
    /// Resume complete transcript lines from durable cursors.
    pub incremental: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct AccountCount {
    pub added: u64,
    pub present: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub directories: u64,
    pub files: u64,
    /// Transcript bytes processed after the cursor during this invocation.
    pub bytes_read: u64,
    pub messages: u64,
    pub added: u64,
    pub already_present: u64,
    /// Messages without a usable timestamp are counted, not guessed.
    pub skipped_no_timestamp: u64,
    pub unreadable_files: u64,
    /// Native attempt metadata could not be read, so linking was skipped.
    pub attempts_unreadable: bool,
    /// A day's segment hit its cap, so some new messages were not recorded.
    pub store_full: bool,
    /// The request's deadline passed, so some transcripts were not read.
    pub deadline_reached: bool,
    pub accounts: BTreeMap<String, AccountCount>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    effort: Option<String>,
    message: Option<Message>,
}
#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    usage: Option<Raw>,
}
struct Pending {
    identity: AccountIdentity,
    model: String,
    effort: Option<String>,
    raw: Raw,
    timestamp: jiff::Timestamp,
    source: Source,
    agent_id: Option<String>,
    native_config: Option<String>,
    reply: crate::signals::ReplyContext,
}

/// A message ID or model name is a short opaque token, never free text.
fn valid_key(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

/// An effort label is a short identifier, never free text.
fn valid_effort(effort: &str) -> bool {
    !effort.is_empty()
        && effort.len() <= 32
        && effort
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_session(session: &str) -> bool {
    (1..=64).contains(&session.len())
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn plain(path: &Path, directory: bool) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| if directory { m.is_dir() } else { m.is_file() })
}

struct Transcript {
    path: PathBuf,
    session: String,
    source: Source,
    agent_id: Option<String>,
    native_config: Option<String>,
}

/// Transcripts under one config directory. Links are never followed.
fn locate(
    dir: &ConfigDir,
    request: &Request<'_>,
    cutoff: Option<SystemTime>,
    out: &mut Vec<Transcript>,
) -> Result<(), ()> {
    if plain(&dir.path, true).is_none() || plain(&dir.path.join("projects"), true).is_none() {
        return Ok(());
    }
    let Ok(projects) = std::fs::read_dir(dir.path.join("projects")) else {
        return Ok(());
    };
    let config_key = crate::native_attempts::config_key(&dir.path);
    let fresh = |meta: &std::fs::Metadata| match (cutoff, meta.modified()) {
        (Some(cutoff), Ok(modified)) => modified >= cutoff,
        _ => true,
    };
    for (seen, project) in projects.enumerate() {
        if seen >= MAX_PROJECT_DIRS {
            return Err(());
        }
        let project = project.map_err(|_| ())?.path();
        if plain(&project, true).is_none() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&project) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let path = entry.path();
            if let Some(session) = name.strip_suffix(".jsonl") {
                let Some(meta) = plain(&path, false) else {
                    continue;
                };
                if !valid_session(session) || !fresh(&meta) {
                    continue;
                }
                let source = if request.chair_sessions.contains_key(session) {
                    Source::Chair
                } else {
                    Source::Lane
                };
                out.push(Transcript {
                    path,
                    session: session.into(),
                    source,
                    agent_id: None,
                    native_config: None,
                });
            } else if valid_session(name) {
                if plain(&path, true).is_none() {
                    continue;
                }
                let subagents = path.join("subagents");
                if plain(&subagents, true).is_none() {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(&subagents) else {
                    continue;
                };
                for file in files.flatten().take(MAX_SUBAGENTS) {
                    let file_name = file.file_name();
                    let file_name = file_name.to_string_lossy();
                    let Some(agent_id) = file_name
                        .strip_prefix("agent-")
                        .and_then(|name| name.strip_suffix(".jsonl"))
                    else {
                        continue;
                    };
                    let agent_id = valid_session(agent_id).then(|| agent_id.to_owned());
                    let Some(meta) = plain(&file.path(), false) else {
                        continue;
                    };
                    if fresh(&meta) {
                        out.push(Transcript {
                            path: file.path(),
                                    session: name.into(),
                            source: Source::Subagent,
                            agent_id,
                            native_config: Some(config_key.clone()),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Cursors contain only an irreversible path digest, file identity and a digest
/// of the bytes immediately before the offset. No transcript text is stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cursor {
    device: u64,
    inode: u64,
    offset: u64,
    tail: String,
    /// Pending structural events survive across incremental ingestion.
    reply: crate::signals::ReplyContext,
    last_reply: Option<String>,
}

fn identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

fn digest(bytes: &[u8]) -> String {
    provenant_core::hex(&Sha256::digest(bytes))
}

fn cursor_key(transcript: &Transcript) -> String {
    use std::os::unix::ffi::OsStrExt;
    digest(transcript.path.as_os_str().as_bytes())
}

fn tail_digest(file: &mut std::fs::File, offset: u64) -> std::io::Result<String> {
    let length = offset.min(128) as usize;
    file.seek(SeekFrom::Start(offset - length as u64))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(digest(&bytes))
}

#[derive(Serialize, Deserialize)]
struct CursorStore {
    schema_version: u64,
    store_digest: String,
    files: BTreeMap<String, Cursor>,
}

/// A durable mask for every row from a session with conflicting identity evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionIdentityOverride {
    schema_version: u64,
    provider: String,
    session: String,
    identity: AccountIdentity,
}

impl SessionIdentityOverride {
    fn conflict(session: &str) -> Self {
        Self {
            schema_version: IDENTITY_OVERRIDE_SCHEMA_VERSION,
            provider: PROVIDER.to_owned(),
            session: session.to_owned(),
            identity: AccountIdentity::conflicting_provider_observations(),
        }
    }
}

fn read_cursors(directory: &Directory, store: &str) -> BTreeMap<String, Cursor> {
    let Ok(file) = directory.open_read(OsStr::new(CURSORS)) else {
        return BTreeMap::new();
    };
    let mut bytes = Vec::new();
    if file.metadata().map_or(true, |meta| meta.size > CURSOR_CAP)
        || file.take(CURSOR_CAP + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > CURSOR_CAP
    {
        return BTreeMap::new();
    }
    serde_json::from_slice::<CursorStore>(&bytes)
        .ok()
        .filter(|cursor| cursor.schema_version == 2 && cursor.store_digest == store)
        .map(|cursor| cursor.files)
        .unwrap_or_default()
}

/// Real time is sampled only here, for a request's wall budget.
#[allow(clippy::disallowed_methods)]
fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|at| Instant::now() >= at)
}

fn read_transcript(
    transcript: &Transcript,
    since: Option<jiff::Timestamp>,
    deadline: Option<Instant>,
    messages: &mut BTreeMap<(String, String), Pending>,
    report: &mut Report,
    cursor: Option<&Cursor>,
    incremental: bool,
) -> Result<Cursor, ()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&transcript.path).map_err(|_| ())?;
    let meta = file.metadata().map_err(|_| ())?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(());
    }
    let (device, inode) = identity(&meta);
    let start = cursor
        .filter(|cursor| {
            cursor.device == device
                && cursor.inode == inode
                && cursor.offset <= meta.len()
                && tail_digest(&mut file, cursor.offset).ok().as_ref() == Some(&cursor.tail)
        })
        .map_or(0, |cursor| cursor.offset);
    file.seek(SeekFrom::Start(start)).map_err(|_| ())?;
    // The file can grow while read; bound the bytes taken, not only its size.
    let mut reader = BufReader::with_capacity(
        1 << 20,
        file.take(
            meta.len()
                .saturating_add(1 << 20)
                .min(MAX_FILE_BYTES)
                .saturating_sub(start),
        ),
    );
    let mut offset = start;
    let mut complete = start;
    let mut last_reply = cursor
        .filter(|_| start > 0)
        .and_then(|cursor| cursor.last_reply.clone());
    let mut reply = cursor
        .filter(|_| start > 0)
        .map(|cursor| cursor.reply.clone())
        .unwrap_or_default();
    let mut line = Vec::new();
    'read_loop: loop {
        if expired(deadline) {
            report.deadline_reached = true;
            break;
        }
        line.clear();
        let read = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| ())?;
        if read == 0 {
            break;
        }
        offset += read as u64;
        report.bytes_read += read as u64;
        if line.len() > MAX_LINE_BYTES && line.last() != Some(&b'\n') {
            // Drain the rest of an oversized line without keeping it.
            let mut sink = Vec::new();
            loop {
                if expired(deadline) {
                    report.deadline_reached = true;
                    break 'read_loop;
                }
                sink.clear();
                let drained = (&mut reader)
                    .take(1 << 20)
                    .read_until(b'\n', &mut sink)
                    .map_err(|_| ())?;
                offset += drained as u64;
                report.bytes_read += drained as u64;
                if drained == 0 || sink.last() == Some(&b'\n') {
                    if sink.last() == Some(&b'\n') {
                        complete = offset;
                    }
                    break;
                }
            }
            continue;
        }
        if line.last() == Some(&b'\n') {
            complete = offset;
        } else if incremental {
            break;
        }
        // Parse transiently; persist structural enums only, never content.
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) {
            reply.observe_native(&value);
        }
        if !line.windows(7).any(|window| window == b"\"usage\"") {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Record>(&line) else {
            continue;
        };
        if record.kind.as_deref() != Some("assistant") {
            continue;
        }
        let Some(Message {
            id: Some(id),
            model: Some(model),
            effort,
            usage: Some(usage),
        }) = record.message
        else {
            continue;
        };
        if !valid_key(&id, 128) || !valid_key(&model, 128) || model == "<synthetic>" {
            continue;
        }
        let effort = record
            .effort
            .or(effort)
            .filter(|effort| valid_effort(effort));
        let key = (transcript.session.clone(), id);
        // A distinct assistant boundary consumes preceding events even when
        // its timestamp is excluded or unavailable. Stream repeats do not.
        let observed_reply = if last_reply.as_ref() != Some(&key.1) {
            last_reply = Some(key.1.clone());
            std::mem::take(&mut reply)
        } else {
            crate::signals::ReplyContext::default()
        };
        if let Some(pending) = messages.get_mut(&key) {
            if pending.model == model {
                pending.raw.merge(usage);
                if pending.effort.is_none() {
                    pending.effort = effort;
                }
            }
            continue;
        }
        let Some(timestamp) = record
            .timestamp
            .as_deref()
            .and_then(|t| t.parse::<jiff::Timestamp>().ok())
        else {
            report.skipped_no_timestamp += 1;
            continue;
        };
        if since.is_some_and(|since| timestamp < since) {
            continue;
        }
        if messages.len() >= MAX_MESSAGES {
            return Err(());
        }
        messages.insert(
            key,
            Pending {
                identity: request
                    .chair_sessions
                    .get(&transcript.session)
                    .cloned()
                    .unwrap_or_else(AccountIdentity::unknown),
                model,
                effort,
                raw: usage,
                timestamp,
                source: transcript.source,
                agent_id: transcript.agent_id.clone(),
                native_config: transcript.native_config.clone(),
                reply: observed_reply,
            },
        );
    }
    let mut file = reader.into_inner().into_inner();
    let tail = tail_digest(&mut file, complete).map_err(|_| ())?;
    Ok(Cursor {
        device,
        inode,
        offset: complete,
        tail,
        reply,
        last_reply,
    })
}

fn row(session: String, message: String, pending: Pending) -> Row {
    let raw = pending.raw;
    // A split that names the one-hour part without a total still counts it.
    let one_hour_named = raw
        .cache_creation
        .and_then(|split| split.ephemeral_1h_input_tokens)
        .unwrap_or(0);
    let creation = raw
        .cache_creation_input_tokens
        .unwrap_or(0)
        .max(one_hour_named);
    let one_hour = one_hour_named.min(creation);
    Row {
        schema_version: ROW_SCHEMA_VERSION,
        identity: pending.identity,
        provider: PROVIDER.into(),
        model: pending.model,
        effort: pending.effort,
        uncached_input: raw.input_tokens.unwrap_or(0),
        cache_read: raw.cache_read_input_tokens.unwrap_or(0),
        cache_write: creation - one_hour,
        cache_write_1h: one_hour,
        output: raw.output_tokens.unwrap_or(0),
        timestamp: pending.timestamp.to_string(),
        session,
        message,
        source: pending.source,
        agent_id: pending.agent_id,
        native_config: pending.native_config,
        native_attempt: None,
        reasoning_tokens: raw.reasoning_tokens,
        reply: Some(pending.reply),
    }
}

/// Parses a segment. A current-version row is read. Any other line, such as a
/// row another schema version wrote or a torn line, is returned as its bytes so
/// a rewrite keeps it unchanged rather than erasing it.
fn split(bytes: &[u8]) -> (Vec<Row>, Vec<&[u8]>) {
    let mut rows = Vec::new();
    let mut kept = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        match serde_json::from_slice::<Row>(line) {
            Ok(row) if row.schema_version == ROW_SCHEMA_VERSION => rows.push(row),
            _ => kept.push(line),
        }
    }
    (rows, kept)
}

/// Read new messages from the config directories into the store. Directories
/// are visited in order and a path seen twice, or a message found under two
/// directories, counts once, for the first directory's account.
pub fn ingest(roots: &Roots, request: &Request<'_>) -> Result<Report, IngestError> {
    let mut report = Report::default();
    if request.provider.is_some_and(|p| p != PROVIDER) {
        return Ok(report);
    }
    let directory = Directory::open_anchor(roots.state.as_path())?;
    let _lock = directory
        .try_lock(OsStr::new(LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => IngestError::Busy,
            other => other.into(),
        })?;
    let store = open_segments(&directory, true)?;
    sweep_staging(&store)?;
    let mut override_directory = open_identity_override_directory(&store, false)?;
    if let Some(overrides) = &override_directory {
        sweep_staging(overrides)?;
    }
    let mut identity_overrides = read_identity_overrides(override_directory.as_ref())?;
    for (session, identity) in request.chair_sessions {
        if matches!(
            &identity.identity,
            Identity::Unknown {
                reason: UnknownIdentityReason::ConflictingProviderIdentity,
            }
        ) {
            if !valid_session(session)
                || identity != &AccountIdentity::conflicting_provider_observations()
            {
                return Err(IngestError::IdentityOverrideStore);
            }
            if !identity_overrides.contains_key(session) {
                if override_directory.is_none() {
                    override_directory = open_identity_override_directory(&store, true)?;
                }
                let Some(directory) = &override_directory else {
                    return Err(IngestError::IdentityOverrideStore);
                };
                let record = SessionIdentityOverride::conflict(session);
                write_identity_override(directory, &record)?;
                identity_overrides.insert(session.clone(), record.identity);
            }
        }
    }
    let mut cursors = if request.incremental {
        read_cursors(&directory, &fingerprint(&store)?)
    } else {
        BTreeMap::new()
    };
    let cutoff = request.since.map(|since| {
        SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(since.as_second().max(0).unsigned_abs())
    });
    let mut seen_paths = BTreeSet::new();
    let mut transcripts = Vec::new();
    for dir in request.dirs {
        if expired(request.deadline) {
            report.deadline_reached = true;
            break;
        }
        let canonical = std::fs::canonicalize(&dir.path).unwrap_or_else(|_| dir.path.clone());
        if !seen_paths.insert(canonical) {
            continue;
        }
        report.directories += 1;
        if locate(dir, request, cutoff, &mut transcripts).is_err() {
            report.unreadable_files += 1;
        }
        if transcripts.len() > MAX_SESSIONS * 2 {
            break;
        }
    }
    let mut messages = BTreeMap::new();
    for transcript in &transcripts {
        if expired(request.deadline) {
            report.deadline_reached = true;
            break;
        }
        report.files += 1;
        let key = cursor_key(transcript);
        match read_transcript(
            transcript,
            if request.incremental {
                None
            } else {
                request.since
            },
            request.deadline,
            &mut messages,
            &mut report,
            cursors.get(&key),
            request.incremental,
        ) {
            Ok(cursor) if request.incremental => {
                cursors.insert(key, cursor);
            }
            Ok(_) => {}
            Err(()) => report.unreadable_files += 1,
        }
    }
    report.messages = messages.len() as u64;
    for ((session, _), pending) in &mut messages {
        if let Some(identity) = identity_overrides.get(session) {
            pending.identity = identity.clone();
        }
    }

    // Load the days new messages fall on and the newest few, so later chair
    // and attempt records still reach recent rows. Older days stay on disk.
    // A message that straddles UTC midnight can be stored under one day by an
    // earlier run and arrive under the next now, so each day's neighbours load
    // too and a row is looked up across all of them.
    let mut names: BTreeSet<String> = BTreeSet::new();
    for pending in messages.values() {
        let day = pending.timestamp.to_zoned(jiff::tz::TimeZone::UTC).date();
        for date in [day.yesterday().ok(), Some(day), day.tomorrow().ok()]
            .into_iter()
            .flatten()
        {
            names.insert(format!("{date}{}", segments::SUFFIX));
        }
    }
    let existing = segments::list(&store)?;
    names.extend(existing.iter().rev().take(RECENT_SEGMENTS).cloned());
    let mut days = BTreeMap::new();
    for name in names {
        let segment = Segment::load(&store, &name)?;
        days.insert(name, segment);
    }
    for segment in days.values_mut() {
        for row in segment.rows.values_mut() {
            if row.source == Source::Lane
                && request.chair_sessions.contains_key(&row.session)
            {
                row.source = Source::Chair;
                segment.changed = true;
            }
            if let Some(identity) = identity_overrides.get(&row.session) {
                if row.identity != *identity {
                    row.identity = identity.clone();
                    segment.changed = true;
                }
            } else if matches!(&row.identity.identity, Identity::Unknown { .. })
                && let Some(identity) = request.chair_sessions.get(&row.session)
                && matches!(&identity.identity, Identity::Known { .. })
            {
                row.identity = identity.clone();
                segment.changed = true;
            }
        }
    }
    let mut fresh: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for ((session, message), pending) in messages {
        let account = report
            .accounts
            .entry(pending.identity.account_key())
            .or_default();
        let day = segments::name(pending.timestamp);
        let key = (session.clone(), message.clone());
        let holder = days
            .iter()
            .find(|(_, segment)| segment.rows.contains_key(&key))
            .map_or_else(|| day.clone(), |(name, _)| name.clone());
        let segment = days
            .get_mut(&holder)
            .ok_or_else(|| std::io::Error::other("segment not loaded"))?;
        if segment.full {
            report.store_full = true;
            continue;
        }
        if let Some(previous) = segment.rows.get_mut(&key) {
            account.present += 1;
            report.already_present += 1;
            let current = row(session, message, pending);
            let before = previous.clone();
            previous.uncached_input = previous.uncached_input.max(current.uncached_input);
            previous.cache_read = previous.cache_read.max(current.cache_read);
            let total_write = previous
                .cache_write
                .saturating_add(previous.cache_write_1h)
                .max(current.cache_write.saturating_add(current.cache_write_1h));
            previous.cache_write_1h = previous.cache_write_1h.max(current.cache_write_1h);
            previous.cache_write = total_write.saturating_sub(previous.cache_write_1h);
            previous.output = previous.output.max(current.output);
            previous.reasoning_tokens = previous.reasoning_tokens.max(current.reasoning_tokens);
            if previous
                .reply
                .as_ref()
                .is_none_or(|reply| reply.recorded.is_empty())
            {
                previous.reply = current.reply;
            }
            if previous.agent_id.is_none() {
                previous.agent_id = current.agent_id;
                previous.native_config = current.native_config;
            }
            // A row first seen as a lane is a chair once the hook records it.
            if previous.source == Source::Lane && current.source == Source::Chair {
                previous.source = Source::Chair;
            }
            segment.changed |= *previous != before;
        } else {
            fresh
                .entry(day)
                .or_default()
                .push(row(session, message, pending));
        }
    }
    let attempts = match crate::native_attempts::read(roots) {
        Ok(attempts) => attempts,
        Err(_) => {
            report.attempts_unreadable = true;
            Vec::new()
        }
    };
    let links: BTreeMap<_, _> = attempts
        .iter()
        .filter_map(|attempt| {
            Some((
                (
                    attempt.config.as_str(),
                    attempt.session.as_str(),
                    attempt.agent_id.as_deref()?,
                ),
                attempt.id.as_str(),
            ))
        })
        .collect();
    for row in fresh.values_mut().flatten() {
        link(row, &links);
    }
    for segment in days.values_mut() {
        for row in segment.rows.values_mut() {
            segment.changed |= link(row, &links);
        }
    }
    for (day, segment) in &mut days {
        let mut added = fresh.remove(day).unwrap_or_default();
        added.sort_by(|a, b| {
            (&a.timestamp, &a.session, &a.message).cmp(&(&b.timestamp, &b.session, &b.message))
        });
        if segment.full {
            continue;
        }
        let mut out = segment.encode()?;
        let mut size = out.len() as u64;
        for row in added {
            let mut line = serde_json::to_vec(&row).map_err(std::io::Error::other)?;
            line.push(b'\n');
            if size + line.len() as u64 > SEGMENT_CAP {
                report.store_full = true;
                break;
            }
            size += line.len() as u64;
            report
                .accounts
                .entry(row.identity.account_key())
                .or_default()
                .added += 1;
            report.added += 1;
            segment.changed = true;
            out.extend_from_slice(&line);
        }
        if segment.changed {
            store
                .atomic_write(OsStr::new(day), ".ingest-", &out, PublishMode::Replace)?
                .require_durable()?;
        }
    }
    if request.incremental && !report.store_full {
        let bytes = serde_json::to_vec(&CursorStore {
            schema_version: 2,
            store_digest: fingerprint(&store)?,
            files: cursors,
        })
        .map_err(std::io::Error::other)?;
        if bytes.len() as u64 <= CURSOR_CAP {
            directory
                .atomic_write(
                    OsStr::new(CURSORS),
                    ".ingest-cursors-",
                    &bytes,
                    PublishMode::Replace,
                )?
                .require_durable()?;
        }
    }
    Ok(report)
}

/// Point a native sub-agent row at its attempt. True when the row changed.
fn link(row: &mut Row, links: &BTreeMap<(&str, &str, &str), &str>) -> bool {
    if row.source != Source::Subagent {
        return false;
    }
    let found = row
        .native_config
        .as_deref()
        .zip(row.agent_id.as_deref())
        .and_then(|(config, agent)| links.get(&(config, row.session.as_str(), agent)))
        .map(|id| (*id).to_owned());
    if found.is_some() && row.native_attempt != found {
        row.native_attempt = found;
        return true;
    }
    false
}

/// One day of rows. Lines this build cannot read are kept as they are.
#[derive(Default)]
struct Segment {
    rows: BTreeMap<(String, String), Row>,
    kept: Vec<Vec<u8>>,
    /// Over [`SEGMENT_CAP`]: not loaded and not rewritten.
    full: bool,
    changed: bool,
}

impl Segment {
    fn load(directory: &Directory, name: &str) -> Result<Self, IngestError> {
        let file = match directory.open_read(OsStr::new(name)) {
            Ok(file) => file,
            Err(error) if error.is_not_found() => return Ok(Self::default()),
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.size > SEGMENT_CAP {
            return Ok(Self {
                full: true,
                ..Self::default()
            });
        }
        let mut bytes = Vec::new();
        file.take(SEGMENT_CAP).read_to_end(&mut bytes)?;
        let (rows, kept) = split(&bytes);
        Ok(Self {
            rows: rows
                .into_iter()
                .map(|row| ((row.session.clone(), row.message.clone()), row))
                .collect(),
            kept: kept.into_iter().map(<[u8]>::to_vec).collect(),
            full: false,
            changed: false,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, IngestError> {
        let mut out = Vec::new();
        for line in &self.kept {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
        for row in self.rows.values() {
            serde_json::to_writer(&mut out, row).map_err(std::io::Error::other)?;
            out.push(b'\n');
        }
        Ok(out)
    }
}

fn identity_override_name(provider: &str, session: &str) -> String {
    let key = format!("{provider}\0{session}");
    format!("{IDENTITY_OVERRIDE_PREFIX}{}.json", digest(key.as_bytes()))
}

fn open_identity_override_directory(
    store: &Directory,
    create: bool,
) -> Result<Option<Directory>, IngestError> {
    if create {
        match store.mkdir(OsStr::new(IDENTITY_OVERRIDES), 0o700) {
            Ok(()) => store.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
    }
    match store.open_directory(OsStr::new(IDENTITY_OVERRIDES)) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_identity_overrides(
    directory: Option<&Directory>,
) -> Result<BTreeMap<String, AccountIdentity>, IngestError> {
    let Some(directory) = directory else {
        return Ok(BTreeMap::new());
    };
    let mut overrides = BTreeMap::new();
    for entry in directory.entries()? {
        let entry = entry?;
        let name = entry.to_str().ok_or(IngestError::IdentityOverrideStore)?;
        if name.starts_with(".ingest-") {
            continue;
        }
        let Some(hash) = name
            .strip_prefix(IDENTITY_OVERRIDE_PREFIX)
            .and_then(|name| name.strip_suffix(".json"))
        else {
            return Err(IngestError::IdentityOverrideStore);
        };
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(IngestError::IdentityOverrideStore);
        }
        let file = directory.open_read(OsStr::new(name))?;
        if file.metadata()?.size > IDENTITY_OVERRIDE_CAP {
            return Err(IngestError::IdentityOverrideStore);
        }
        let mut bytes = Vec::new();
        file.take(IDENTITY_OVERRIDE_CAP + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > IDENTITY_OVERRIDE_CAP {
            return Err(IngestError::IdentityOverrideStore);
        }
        let record: SessionIdentityOverride = serde_json::from_slice(&bytes)
            .map_err(|_| IngestError::IdentityOverrideStore)?;
        if record.schema_version != IDENTITY_OVERRIDE_SCHEMA_VERSION
            || record.provider != PROVIDER
            || !valid_session(&record.session)
            || record.identity != AccountIdentity::conflicting_provider_observations()
            || name != identity_override_name(&record.provider, &record.session)
            || overrides.insert(record.session, record.identity).is_some()
        {
            return Err(IngestError::IdentityOverrideStore);
        }
    }
    Ok(overrides)
}

fn write_identity_override(
    directory: &Directory,
    record: &SessionIdentityOverride,
) -> Result<(), IngestError> {
    let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    if bytes.len() as u64 > IDENTITY_OVERRIDE_CAP {
        return Err(IngestError::IdentityOverrideStore);
    }
    directory
        .atomic_write(
            OsStr::new(&identity_override_name(&record.provider, &record.session)),
            ".ingest-identity-",
            &bytes,
            PublishMode::Replace,
        )?
        .require_durable()?;
    Ok(())
}

/// The segments directory, created on request.
fn open_segments(root: &Directory, create: bool) -> Result<Directory, IngestError> {
    if create {
        match root.mkdir(OsStr::new(DIRECTORY), 0o700) {
            Ok(()) => root.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root.open_directory(OsStr::new(DIRECTORY))?)
}

/// Removes `.ingest-*` files a crashed write left in the segments directory.
/// The ingest lock is held, so no write of ours is in flight and every one is
/// stale. (`staging_gc` only scans the state root, where the cursor file's
/// staging files go.)
fn sweep_staging(store: &Directory) -> Result<(), IngestError> {
    let mut stale = Vec::new();
    for entry in store.entries()? {
        let entry = entry?;
        if entry
            .to_str()
            .is_some_and(|name| name.starts_with(".ingest-"))
        {
            stale.push(entry);
        }
    }
    for name in stale {
        match store.unlink(&name) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Identifies the store's current contents for the cursors: the segments and
/// their sizes. A cursor made against other contents is not trusted.
fn fingerprint(directory: &Directory) -> Result<String, IngestError> {
    let mut listing = String::new();
    for name in segments::list(directory)? {
        let size = directory.open_read(OsStr::new(&name))?.metadata()?.size;
        listing.push_str(&format!("{name}:{size}\n"));
    }
    Ok(digest(listing.as_bytes()))
}

/// Calls `each` with every line, without its newline. A line longer than
/// [`MAX_ROW_LINE`] is not a row and is dropped; the count of those is returned.
fn each_line(reader: impl Read, mut each: impl FnMut(&[u8])) -> std::io::Result<u64> {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut dropped = 0;
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_ROW_LINE as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(dropped);
        }
        if line.len() > MAX_ROW_LINE && line.last() != Some(&b'\n') {
            dropped += 1;
            let mut rest = Vec::new();
            loop {
                rest.clear();
                let drained = (&mut reader).take(1 << 20).read_until(b'\n', &mut rest)?;
                if drained == 0 || rest.last() == Some(&b'\n') {
                    break;
                }
            }
            continue;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        each(&line);
    }
}

#[derive(Clone, Debug, Default)]
pub struct RowFilter {
    pub account: Option<String>,
    pub model: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

/// Rows matching the filter, oldest first. Only the days the filter's window
/// reaches are read, each streamed. A missing store is empty.
pub fn read_rows(roots: &Roots, filter: &RowFilter) -> Result<Vec<Row>, IngestError> {
    let root = match Directory::open_anchor(roots.state.as_path()) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let directory = match open_segments(&root, false) {
        Ok(directory) => directory,
        Err(IngestError::Filesystem(error)) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let override_directory = open_identity_override_directory(&directory, false)?;
    let identity_overrides = read_identity_overrides(override_directory.as_ref())?;
    let day = |seconds: i64| {
        jiff::Timestamp::from_second(seconds)
            .ok()
            .map(segments::name)
    };
    let (first, last) = (filter.since.and_then(day), filter.until.and_then(day));
    let mut rows: Vec<(i64, Row)> = Vec::new();
    for name in segments::list(&directory)? {
        if first.as_ref().is_some_and(|first| name < *first)
            || last.as_ref().is_some_and(|last| name > *last)
        {
            continue;
        }
        each_line(directory.open_read(OsStr::new(&name))?, |line| {
            let Ok(mut row) = serde_json::from_slice::<Row>(line) else {
                return;
            };
            if let Some(identity) = identity_overrides.get(&row.session) {
                row.identity = identity.clone();
            }
            if row.schema_version != ROW_SCHEMA_VERSION
                || filter.account.as_ref().is_some_and(|a| a != &row.identity.account_key())
                || filter.model.as_ref().is_some_and(|m| *m != row.model)
            {
                return;
            }
            if let Some(at) = row.unix_seconds()
                && filter.since.is_none_or(|s| at >= s)
                && filter.until.is_none_or(|u| at <= u)
            {
                rows.push((at, row));
            }
        })?;
    }
    rows.sort_by_key(|(at, _)| *at);
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

#[cfg(test)]
#[path = "ingest/tests.rs"]
mod tests;
