//! Append-only source board. Only the host publishes; lane input has no provenance fields.
use provenant_core::{AttemptId, CancelToken, Clock, ProjectId, WaitBudget, WallTime};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use provenant_records::lane_contract::{
    Coverage, ObservationCursor, ObservationEvent, ObservationKind, ObservationLineage, Sourced,
};
use provenant_records::{CommandResult, LimitSource, Outcome, RecordHeader, WorkKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, path::Path};

/// Scopes are logical work paths, never filesystem paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    #[default]
    Project,
    Graph {
        graph: String,
    },
    Subtree {
        graph: String,
        node: String,
    },
}
impl Scope {
    pub fn validate(&self, project: ProjectId) -> Result<(), Error> {
        let (graph, node) = match self {
            Self::Project => return Ok(()),
            Self::Graph { graph } => (graph.as_str(), "root"),
            Self::Subtree { graph, node } => (graph.as_str(), node.as_str()),
        };
        WorkKey::new(project, graph, node, 1).map_err(|_| Error::Invalid("scope"))?;
        Ok(())
    }
    fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Project, _) | (_, Self::Project) => true,
            (Self::Graph { graph: a }, Self::Graph { graph: b }) => a == b,
            (Self::Graph { graph: a }, Self::Subtree { graph: b, .. })
            | (Self::Subtree { graph: a, .. }, Self::Graph { graph: b }) => a == b,
            (Self::Subtree { graph: a, node: x }, Self::Subtree { graph: b, node: y }) => {
                a == b && (descendant(x, y) || descendant(y, x))
            }
        }
    }
}
fn descendant(node: &str, root: &str) -> bool {
    node == root
        || node
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with("--"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Add {
        text: String,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        scope: Scope,
        #[serde(default)]
        resolves: Option<i64>,
    },
    Read {
        reader: String,
        #[serde(default)]
        scope: Scope,
        /// None resumes the saved cursor; Some(0) explicitly replays everything.
        #[serde(default)]
        after: Option<i64>,
        #[serde(default)]
        limit: Option<u64>,
    },
    /// Sent only after successful output/processing. Repeated ACKs are harmless.
    Ack {
        reader: String,
        #[serde(default)]
        scope: Scope,
        seq: i64,
    },
}

/// Constructed by the host composition edge from its bound attempt and route.
/// Deliberately not deserialisable from a lane request.
#[derive(Clone, Debug, Serialize)]
pub struct Provenance {
    pub lane: Option<AttemptId>,
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub commit: Option<String>,
    pub commit_source: &'static str,
    pub time: WallTime,
    pub verified: bool,
}
#[derive(Clone, Debug)]
pub struct Host {
    pub project: ProjectId,
    pub lineage: ObservationLineage,
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub commit: Option<String>,
    pub commit_source: &'static str,
}
impl Host {
    pub fn from_attempt(
        record: &provenant_records::run::AttemptRecord,
        parent_attempt: Option<AttemptId>,
    ) -> Self {
        let route = record
            .applied_route
            .as_ref()
            .or(record.resolved_route.as_ref());
        Self {
            project: record.project_id,
            lineage: ObservationLineage {
                attempt: Some(record.attempt_id),
                parent_attempt,
                work: Some(record.key.clone()),
                extra: Default::default(),
            },
            adapter: route.and_then(|r| r.provider.clone()),
            model: route.and_then(|r| r.model.clone()),
            commit_source: "host_retained_attempt",
            commit: record
                .head
                .as_ref()
                .or(record.subject.as_ref())
                .map(ToString::to_string),
        }
    }
    pub fn chair(project: ProjectId, commit: Option<String>) -> Self {
        Self {
            project,
            lineage: ObservationLineage {
                attempt: None,
                parent_attempt: None,
                work: None,
                extra: Default::default(),
            },
            adapter: None,
            model: None,
            commit,
            commit_source: "host_checkout_head",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Fs(#[from] provenant_os::fs::FsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Time(#[from] provenant_core::TimeError),
    #[error("invalid notes {0}")]
    Invalid(&'static str),
    #[error(
        "note cannot fit the existing result transport capacity ({limit} bytes); split the note"
    )]
    ResponseCapacity { limit: u64 },
    #[error("corrupt notes source: {0}")]
    Corrupt(&'static str),
}

#[derive(Serialize, Deserialize)]
struct Detail {
    text: String,
    tags: Vec<String>,
    scope: Scope,
    resolves: Option<i64>,
    provenance: StoredProvenance,
}
// Persisted timestamps and provenance remain readable across process lifetimes.
#[derive(Serialize, Deserialize)]
struct StoredProvenance {
    lane: Option<AttemptId>,
    adapter: Option<String>,
    model: Option<String>,
    commit: Option<String>,
    commit_source: String,
    time: WallTime,
    verified: bool,
}

/// Source note identity is stable; its sequence is not a shared stream cursor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    #[serde(flatten)]
    pub header: RecordHeader,
    pub id: String,
    pub project: ProjectId,
    pub seq: i64,
    pub lineage: ObservationLineage,
    pub kind: ObservationKind,
    pub detail: serde_json::Value,
}
impl Note {
    /// Delivery supplies its own project-stream sequence when indexing the note.
    pub fn observation(&self, cursor: ObservationCursor) -> Result<ObservationEvent, Error> {
        if cursor.project != self.project {
            return Err(Error::Invalid("observation project"));
        }
        Ok(ObservationEvent {
            header: self.header.clone(),
            id: self.id.clone(),
            cursor,
            lineage: self.lineage.clone(),
            kind: self.kind.clone(),
            coverage: Coverage::Complete,
            detail: self.detail.clone(),
            extra: Default::default(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub events: Vec<Note>,
    pub next: i64,
    pub truncated: bool,
    pub cursor_recovered: bool,
    pub read_cap: Sourced<u64>,
    pub byte_cap: Sourced<u64>,
    pub source: String,
}

impl Page {
    fn response_bytes(&self) -> Result<usize, Error> {
        // Pretty CLI output is larger than the compact host-relay envelope.
        Ok(serde_json::to_vec_pretty(&CommandResult::new("notes read", Outcome::Ok, self))?.len())
    }
}

/// Immutable numbered event files, with disposable per-reader cursor files.
/// The notes sequence is source-local. Delivery must allocate a project-stream
/// cursor when indexing the stable event identity into the shared stream.
pub struct Board {
    directory: Directory,
    project: ProjectId,
    read_cap: Sourced<u64>,
    byte_cap: Sourced<u64>,
}
impl Board {
    pub fn open(
        state: &Path,
        project: ProjectId,
        read_cap: Sourced<u64>,
        byte_cap: Sourced<u64>,
    ) -> Result<Self, Error> {
        if read_cap.value == 0 || byte_cap.value == 0 {
            return Err(Error::Invalid("read cap"));
        }
        let state = Directory::open_anchor(state)?;
        let notes = subdir(&state, "notes")?;
        let directory = subdir(&notes, &project.to_string())?;
        Ok(Self {
            directory,
            project,
            read_cap,
            byte_cap,
        })
    }
    fn lock(
        &self,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<provenant_os::fs::LockGuard, Error> {
        Ok(self.directory.lock(
            OsStr::new("board.lock"),
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 5)?,
            cancel,
            clock,
        )?)
    }
    fn sequences(&self) -> Result<Vec<i64>, Error> {
        let mut seqs = Vec::new();
        for entry in self.directory.entries()? {
            let entry = entry?;
            if let Some(s) = entry.to_str().and_then(|n| n.strip_suffix(".note.json")) {
                let seq = s.parse::<i64>().map_err(|_| Error::Corrupt("filename"))?;
                if seq < 1 || OsStr::new(&filename(seq)) != entry {
                    return Err(Error::Corrupt("sequence"));
                }
                seqs.push(seq);
            }
        }
        seqs.sort_unstable();
        Ok(seqs)
    }
    fn event(&self, seq: i64) -> Result<Note, Error> {
        // Source files are never modified. Read exact file size, without following links.
        let mut file = self.directory.open_read(OsStr::new(&filename(seq)))?;
        let event: Note = serde_json::from_reader(&mut file)?;
        if event.project != self.project
            || event.seq != seq
            || event.kind != ObservationKind::Note
            || event.id != self.id(seq)
        {
            return Err(Error::Corrupt("event identity"));
        }
        Ok(event)
    }
    fn id(&self, seq: i64) -> String {
        format!("notes:{}:{seq}", self.project)
    }
    fn cursor_name(
        &self,
        owner: Option<AttemptId>,
        reader: &str,
        scope: &Scope,
    ) -> Result<String, Error> {
        if reader.is_empty() {
            return Err(Error::Invalid("reader"));
        }
        let bytes = serde_json::to_vec(&(owner, reader, scope))?;
        let hash: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(format!("reader-{hash}.json"))
    }
    fn saved_cursor(&self, name: &str, high: i64) -> Result<(i64, bool), Error> {
        match self.directory.read_stable(OsStr::new(name), 128) {
            Ok(bytes) => match serde_json::from_slice::<i64>(&bytes) {
                Ok(seq) if seq >= 0 && seq <= high => Ok((seq, false)),
                _ => Ok((0, true)),
            },
            Err(e) if e.is_not_found() => Ok((0, false)),
            Err(provenant_os::fs::FsError::TooLarge { .. }) => Ok((0, true)),
            Err(e) => Err(e.into()),
        }
    }
    fn page(&self, next: i64, recovered: bool, read_cap: Sourced<u64>) -> Page {
        Page {
            events: Vec::new(),
            next,
            truncated: false,
            cursor_recovered: recovered,
            read_cap,
            byte_cap: self.byte_cap.clone(),
            source: "notes immutable source files; source-local sequence".into(),
        }
    }
    pub fn execute(
        &self,
        host: &Host,
        input: Input,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, Error> {
        if host.project != self.project {
            return Err(Error::Invalid("project provenance"));
        }
        let _lock = self.lock(clock, cancel)?;
        let seqs = self.sequences()?;
        let high = seqs.last().copied().unwrap_or(0);
        match input {
            Input::Add {
                text,
                tags,
                scope,
                resolves,
            } => {
                scope.validate(self.project)?;
                if text.trim().is_empty() {
                    return Err(Error::Invalid("empty text"));
                }
                if let Some(target) = resolves
                    && !seqs.contains(&target)
                {
                    return Err(Error::Invalid("correction target"));
                }
                let seq = high
                    .checked_add(1)
                    .ok_or(Error::Corrupt("sequence overflow"))?;
                let detail = serde_json::json!({"text": text, "tags": tags, "scope": scope,
                    "resolves": resolves, "provenance": Provenance { lane: host.lineage.attempt,
                    adapter: host.adapter.clone(), model: host.model.clone(), commit: host.commit.clone(),
                    commit_source: host.commit_source, time: clock.wall_now()?, verified: false }});
                let event = Note {
                    header: RecordHeader::V1,
                    id: self.id(seq),
                    project: self.project,
                    seq,
                    lineage: host.lineage.clone(),
                    kind: ObservationKind::Note,
                    detail,
                };
                // Validate representability before publishing authoritative source.
                // Reserve the largest possible read-control metadata so even a
                // later explicit cap and high cursor cannot make this note unreadable.
                let mut preview = self.page(
                    i64::MAX,
                    false,
                    Sourced {
                        value: u64::MAX,
                        source: LimitSource::Template,
                    },
                );
                preview.byte_cap = Sourced {
                    value: u64::MAX,
                    source: LimitSource::Template,
                };
                preview.events.push(event.clone());
                if preview.response_bytes()? as u64
                    > provenant_records::COMMAND_RESULT_MAX_BYTES as u64
                {
                    return Err(Error::ResponseCapacity {
                        limit: provenant_records::COMMAND_RESULT_MAX_BYTES as u64,
                    });
                }
                self.directory.atomic_write(
                    OsStr::new(&filename(seq)),
                    ".note-",
                    &serde_json::to_vec(&event)?,
                    PublishMode::NoReplace,
                )?;
                Ok(serde_json::to_value(event)?)
            }
            Input::Read {
                reader,
                scope,
                after,
                limit,
            } => {
                let read_cap = match limit {
                    Some(0) => return Err(Error::Invalid("read cap")),
                    Some(value) => Sourced {
                        value,
                        source: LimitSource::Launch,
                    },
                    None => self.read_cap.clone(),
                };
                scope.validate(self.project)?;
                let name = self.cursor_name(host.lineage.attempt, &reader, &scope)?;
                let (saved, recovered) = self.saved_cursor(&name, high)?;
                let after = after.unwrap_or(saved);
                if after < 0 || after > high {
                    return Err(Error::Invalid("cursor range"));
                }
                let mut page = self.page(after, recovered, read_cap);
                let mut bytes = 0;
                for seq in seqs.into_iter().filter(|seq| *seq > after) {
                    let event = self.event(seq)?;
                    let detail: Detail = serde_json::from_value(event.detail.clone())?;
                    if scope.overlaps(&detail.scope) {
                        let size = serde_json::to_vec(&event)?.len() as u64;
                        if page.events.len() as u64 >= page.read_cap.value
                            || (!page.events.is_empty() && bytes + size > self.byte_cap.value)
                        {
                            page.truncated = true;
                            break;
                        }
                        // An oversized first entry is returned whole so replay can always progress.
                        let previous = page.next;
                        page.next = seq;
                        page.events.push(event);
                        if page.response_bytes()? as u64
                            > provenant_records::COMMAND_RESULT_MAX_BYTES as u64
                        {
                            page.events.pop();
                            page.next = previous;
                            if page.events.is_empty() {
                                return Err(Error::ResponseCapacity {
                                    limit: provenant_records::COMMAND_RESULT_MAX_BYTES as u64,
                                });
                            }
                            page.truncated = true;
                            break;
                        }
                        bytes += size;
                    }
                    page.next = seq;
                }
                Ok(serde_json::to_value(page)?)
            }
            Input::Ack { reader, scope, seq } => {
                scope.validate(self.project)?;
                if seq < 0 || seq > high {
                    return Err(Error::Invalid("cursor range"));
                }
                let name = self.cursor_name(host.lineage.attempt, &reader, &scope)?;
                let (saved, recovered) = self.saved_cursor(&name, high)?;
                self.directory.atomic_write(
                    OsStr::new(&name),
                    ".cursor-",
                    &serde_json::to_vec(&saved.max(seq))?,
                    PublishMode::Replace,
                )?;
                Ok(
                    serde_json::json!({"acknowledged": saved.max(seq), "cursor_recovered": recovered}),
                )
            }
        }
    }
}
fn filename(seq: i64) -> String {
    format!("{seq:020}.note.json")
}
fn subdir(parent: &Directory, name: &str) -> Result<Directory, Error> {
    match parent.mkdir(OsStr::new(name), 0o700) {
        Ok(()) => parent.sync()?,
        Err(e) if e.is_exists() => (),
        Err(e) => return Err(e.into()),
    }
    Ok(parent.open_verified_directory(OsStr::new(name))?)
}

pub fn default_read_cap() -> Sourced<u64> {
    Sourced {
        value: 50,
        source: LimitSource::Default,
    }
}
pub fn default_byte_cap() -> Sourced<u64> {
    Sourced {
        value: 262_144,
        source: LimitSource::Default,
    }
}

#[cfg(test)]
mod tests;
