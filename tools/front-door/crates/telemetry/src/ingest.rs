//! Native session ingest: per-message token rows from Claude Code transcripts,
//! attributed to an account by the config directory they were found under.
//!
//! The store is `ingest.jsonl` in the state root, one row per assistant
//! message, keyed by session and message ID so a re-run adds nothing. A row
//! holds counts and identifiers only: never a prompt, a reply or a path.
use provenant_core::Roots;
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};
use thiserror::Error;

pub const ROW_SCHEMA_VERSION: u64 = 1;
/// The only provider whose sessions leave a native transcript.
pub const PROVIDER: &str = "claude";
const STORE: &str = "ingest.jsonl";
const LOCK: &str = "ingest.lock";
/// Largest store read or grown. At about 250 bytes a row this is roughly
/// half a million messages; ingest stops adding rows and says so.
const STORE_CAP: u64 = 128 << 20;
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
    #[error("the ingest store exceeds its size cap")]
    StoreTooLarge,
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

/// One assistant message's usage. The five buckets are disjoint: `cache_write`
/// is the short-TTL part of cache creation and `cache_write_1h` the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub schema_version: u64,
    pub account: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// A Claude config directory and the account label it is attributed to.
#[derive(Clone, Debug)]
pub struct ConfigDir {
    pub account: String,
    pub path: PathBuf,
}

/// Reserved account for config directories no registered account claims.
pub const UNATTRIBUTED: &str = crate::usage::UNATTRIBUTED;

/// The directories to ingest: each registered account's directories under its
/// label, then every other `~/.claude*` directory with transcripts, including
/// `~/.claude` itself, as `unattributed`. Nothing is dropped and nothing is
/// guessed. `accounts` is `(label, directories)` in registration order.
pub fn config_dirs(accounts: &[(String, Vec<PathBuf>)], home: &Path) -> Vec<ConfigDir> {
    let mut dirs: Vec<ConfigDir> = accounts
        .iter()
        .flat_map(|(label, paths)| {
            paths.iter().map(|path| ConfigDir {
                account: label.clone(),
                path: path.clone(),
            })
        })
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
    dirs.extend(found.into_iter().map(|path| ConfigDir {
        account: UNATTRIBUTED.into(),
        path,
    }));
    dirs
}

#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub dirs: &'a [ConfigDir],
    /// Session IDs the chair hook recorded; other top-level sessions are lanes.
    pub chair_sessions: &'a BTreeSet<String>,
    /// Only `claude` has native transcripts; any other value ingests nothing.
    pub provider: Option<&'a str>,
    /// Skip files not written since and messages stamped before.
    pub since: Option<jiff::Timestamp>,
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
    pub messages: u64,
    pub added: u64,
    pub already_present: u64,
    /// Messages without a usable timestamp are counted, not guessed.
    pub skipped_no_timestamp: u64,
    pub unreadable_files: u64,
    /// The store hit its cap, so some new messages were not recorded.
    pub store_full: bool,
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
#[derive(Clone, Default, Deserialize)]
struct Raw {
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_creation: Option<Split>,
    output_tokens: Option<u64>,
}
#[derive(Clone, Default, Deserialize)]
struct Split {
    ephemeral_1h_input_tokens: Option<u64>,
}
impl Raw {
    /// A message repeats as it streams; the largest count of each field is its
    /// final one.
    fn merge(&mut self, other: Self) {
        let max = |a: Option<u64>, b: Option<u64>| a.max(b);
        self.input_tokens = max(self.input_tokens, other.input_tokens);
        self.cache_read_input_tokens =
            max(self.cache_read_input_tokens, other.cache_read_input_tokens);
        self.cache_creation_input_tokens = max(
            self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        );
        self.output_tokens = max(self.output_tokens, other.output_tokens);
        let one_hour = |raw: &Self| raw.cache_creation.as_ref()?.ephemeral_1h_input_tokens;
        self.cache_creation = max(one_hour(self), one_hour(&other)).map(|n| Split {
            ephemeral_1h_input_tokens: Some(n),
        });
    }
}

struct Pending {
    account: String,
    model: String,
    effort: Option<String>,
    raw: Raw,
    timestamp: jiff::Timestamp,
    source: Source,
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
    account: String,
    session: String,
    source: Source,
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
                let source = if request.chair_sessions.contains(session) {
                    Source::Chair
                } else {
                    Source::Lane
                };
                out.push(Transcript {
                    path,
                    account: dir.account.clone(),
                    session: session.into(),
                    source,
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
                    if !file_name.starts_with("agent-") || !file_name.ends_with(".jsonl") {
                        continue;
                    }
                    let Some(meta) = plain(&file.path(), false) else {
                        continue;
                    };
                    if fresh(&meta) {
                        out.push(Transcript {
                            path: file.path(),
                            account: dir.account.clone(),
                            session: name.into(),
                            source: Source::Subagent,
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

fn read_transcript(
    transcript: &Transcript,
    since: Option<jiff::Timestamp>,
    messages: &mut BTreeMap<(String, String), Pending>,
    report: &mut Report,
) -> Result<(), ()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&transcript.path).map_err(|_| ())?;
    let meta = file.metadata().map_err(|_| ())?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(());
    }
    // The file can grow while read; bound the bytes taken, not only its size.
    let mut reader = BufReader::with_capacity(
        1 << 20,
        file.take(meta.len().saturating_add(1 << 20).min(MAX_FILE_BYTES)),
    );
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| ())?;
        if read == 0 {
            return Ok(());
        }
        if line.len() > MAX_LINE_BYTES && line.last() != Some(&b'\n') {
            // Drain the rest of an oversized line without keeping it.
            let mut sink = Vec::new();
            loop {
                sink.clear();
                let drained = (&mut reader)
                    .take(1 << 20)
                    .read_until(b'\n', &mut sink)
                    .map_err(|_| ())?;
                if drained == 0 || sink.last() == Some(&b'\n') {
                    break;
                }
            }
            continue;
        }
        // Most lines are prompts and tool results; only an assistant record
        // carries a `usage` object.
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
                account: transcript.account.clone(),
                model,
                effort,
                raw: usage,
                timestamp,
                source: transcript.source,
            },
        );
    }
}

fn row(session: String, message: String, pending: Pending) -> Row {
    let raw = pending.raw;
    let creation = raw.cache_creation_input_tokens.unwrap_or(0);
    let one_hour = raw
        .cache_creation
        .and_then(|split| split.ephemeral_1h_input_tokens)
        .unwrap_or(0)
        .min(creation);
    Row {
        schema_version: ROW_SCHEMA_VERSION,
        account: pending.account,
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
    }
}

fn read_store(directory: &Directory) -> Result<Vec<u8>, IngestError> {
    match directory.open_read(OsStr::new(STORE)) {
        Ok(file) => {
            if file.metadata()?.size > STORE_CAP {
                return Err(IngestError::StoreTooLarge);
            }
            let mut bytes = Vec::new();
            file.take(STORE_CAP).read_to_end(&mut bytes)?;
            Ok(bytes)
        }
        Err(error) if error.is_not_found() => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn decode(bytes: &[u8]) -> impl Iterator<Item = Row> + '_ {
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<Row>(line).ok())
        .filter(|row| row.schema_version == ROW_SCHEMA_VERSION)
}

/// Read new messages from the config directories into the store. Directories
/// are visited in order and a path seen twice, or a message found under two
/// directories, counts once, for the first directory's account.
pub fn ingest(roots: &Roots, request: &Request<'_>) -> Result<Report, IngestError> {
    let mut report = Report::default();
    if request.provider.is_some_and(|p| p != PROVIDER) {
        return Ok(report);
    }
    let cutoff = request.since.map(|since| {
        SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(since.as_second().max(0).unsigned_abs())
    });
    let mut seen_paths = BTreeSet::new();
    let mut transcripts = Vec::new();
    for dir in request.dirs {
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
        report.files += 1;
        if read_transcript(transcript, request.since, &mut messages, &mut report).is_err() {
            report.unreadable_files += 1;
        }
    }
    report.messages = messages.len() as u64;

    let directory = Directory::open_anchor(roots.state.as_path())?;
    let _lock = directory
        .try_lock(OsStr::new(LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => IngestError::Busy,
            other => other.into(),
        })?;
    let existing = read_store(&directory)?;
    let mut known: BTreeMap<(String, String), Row> = decode(&existing)
        .map(|row| ((row.session.clone(), row.message.clone()), row))
        .collect();
    let mut fresh: Vec<Row> = Vec::new();
    let mut changed = false;
    for ((session, message), pending) in messages {
        let account = report.accounts.entry(pending.account.clone()).or_default();
        if let Some(previous) = known.get_mut(&(session.clone(), message.clone())) {
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
            changed |= *previous != before;
        } else {
            fresh.push(row(session, message, pending));
        }
    }
    fresh.sort_by(|a, b| {
        (&a.timestamp, &a.session, &a.message).cmp(&(&b.timestamp, &b.session, &b.message))
    });
    let mut out = Vec::new();
    for row in known.values() {
        serde_json::to_writer(&mut out, row).map_err(std::io::Error::other)?;
        out.push(b'\n');
    }
    let mut size = out.len() as u64;
    for row in fresh {
        let mut line = serde_json::to_vec(&row).map_err(std::io::Error::other)?;
        line.push(b'\n');
        if size + line.len() as u64 > STORE_CAP {
            report.store_full = true;
            break;
        }
        size += line.len() as u64;
        report
            .accounts
            .entry(row.account.clone())
            .or_default()
            .added += 1;
        report.added += 1;
        changed = true;
        out.extend_from_slice(&line);
    }
    if changed {
        directory
            .atomic_write(OsStr::new(STORE), ".ingest-", &out, PublishMode::Replace)?
            .require_durable()?;
    }
    Ok(report)
}

#[derive(Clone, Debug, Default)]
pub struct RowFilter {
    pub account: Option<String>,
    pub model: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

/// Rows matching the filter, oldest first. A missing store is empty.
pub fn read_rows(roots: &Roots, filter: &RowFilter) -> Result<Vec<Row>, IngestError> {
    let directory = match Directory::open_anchor(roots.state.as_path()) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let bytes = read_store(&directory)?;
    let mut rows: Vec<(i64, Row)> = decode(&bytes)
        .filter(|row| filter.account.as_ref().is_none_or(|a| *a == row.account))
        .filter(|row| filter.model.as_ref().is_none_or(|m| *m == row.model))
        .filter_map(|row| Some((row.unix_seconds()?, row)))
        .filter(|(at, _)| {
            filter.since.is_none_or(|s| *at >= s) && filter.until.is_none_or(|u| *at <= u)
        })
        .collect();
    rows.sort_by_key(|(at, _)| *at);
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

#[cfg(test)]
#[path = "ingest/tests.rs"]
mod tests;
