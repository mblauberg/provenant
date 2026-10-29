//! Explicit host-keyed Markdown checkpoints. No transcript or latest-file lookup.
use crate::store::{WaitStop, check_wait};
use provenant_core::{CancelToken, Clock, ProjectId, StateRoot, UtcMicros, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use thiserror::Error;

mod diff;
mod discovery;
mod hooks;
mod state;
mod summary;
pub use discovery::*;
pub use hooks::*;
pub use state::*;
pub use summary::*;

/// The most a checkpoint may hold: the size of the largest workflow input the CLI
/// reads, so a saved file never fits the reader but not the store. Readers that
/// need less cut it: hooks inject at most 8 KiB and a hand-off prompt 32 KiB.
pub const CHECKPOINT_LIMIT: u64 = 256 * 1024;
pub const HOST_SESSION_ID_LIMIT: usize = 128;
/// Revisions a session keeps by default: the current checkpoint and the seven
/// before it. `retention.session_revisions` sets it from 1 (no history) to 64.
pub const REVISIONS_DEFAULT: u64 = 8;
pub const REVISIONS_MAX: u64 = 64;
const MANIFEST: &str = "revisions.json";
const MANIFEST_LIMIT: u64 = 64 * 1024;
const REVISION_PREFIX: &str = "rev-";

/// One saved state of a session checkpoint. Revisions are numbered from 1 and
/// each names the revision it replaced, so the chain shows what came before and
/// the content hash shows it was not altered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub revision: u64,
    pub parent: Option<u64>,
    pub sha256: provenant_records::FileDigest,
    pub bytes: u64,
    pub saved_at: UtcMicros,
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u64,
    /// Oldest first; the last is the checkpoint in `checkpoint.md`.
    revisions: Vec<Revision>,
}
/// Every decision of one checkpoint publication, journaled before any file
/// changes so that recovery replays exactly what was intended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublishPlan {
    pub(super) now: UtcMicros,
    /// The replaced revision number and text, stored before the checkpoint.
    #[serde(with = "utf8_pair")]
    prev: Option<(u64, Vec<u8>)>,
    #[serde(with = "utf8_bytes")]
    text: Vec<u8>,
    /// The whole chain before pruning, oldest first.
    chain: Vec<Revision>,
    unchanged: bool,
    keep: u64,
    /// The discovery record to publish, fixed when the plan is made so that
    /// recovery never derives it from whichever writer recovers.
    discovery: Option<SessionDiscovery>,
}
/// Checkpoints are UTF-8, so plans carry them as JSON strings.
mod utf8_bytes {
    pub fn serialize<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(std::str::from_utf8(bytes).map_err(serde::ser::Error::custom)?)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        <String as serde::Deserialize>::deserialize(d).map(String::into_bytes)
    }
}
mod utf8_pair {
    use serde::{Deserialize, Serialize};
    pub fn serialize<S: serde::Serializer>(
        pair: &Option<(u64, Vec<u8>)>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let text = pair
            .as_ref()
            .map(|(n, b)| std::str::from_utf8(b).map(|t| (*n, t)))
            .transpose()
            .map_err(serde::ser::Error::custom)?;
        text.serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<(u64, Vec<u8>)>, D::Error> {
        Ok(<Option<(u64, String)>>::deserialize(d)?.map(|(n, t)| (n, t.into_bytes())))
    }
}
/// What a save recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub revision: u64,
    pub parent: Option<u64>,
    /// The text equalled the current checkpoint, so no revision was added.
    pub unchanged: bool,
    /// Revisions kept after pruning, including this one.
    pub retained: usize,
    /// Advisory discovery publication failed after the manifest was committed.
    pub warnings: Vec<String>,
}
/// A retained revision's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionText {
    pub revision: Revision,
    pub markdown: String,
}
/// What changed between a retained revision and the current checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionDiff {
    pub from: Revision,
    pub to: Revision,
    /// A unified line diff; empty when the text is the same.
    pub text: String,
}

fn revision_file(revision: u64) -> String {
    format!("{REVISION_PREFIX}{revision}.md")
}
fn revision_number(name: &OsStr) -> Option<u64> {
    name.to_str()?
        .strip_prefix(REVISION_PREFIX)?
        .strip_suffix(".md")?
        .parse()
        .ok()
}
fn publish(dir: &Directory, name: &str, bytes: &[u8]) -> Result<(), SessionError> {
    dir.atomic_write(OsStr::new(name), ".session.", bytes, PublishMode::Replace)?
        .require_durable()
        .map_err(SessionError::Durability)
}
/// The checkpoint text and when it was last written.
fn read_current(dir: &Directory) -> Result<Option<(Vec<u8>, UtcMicros)>, SessionError> {
    let read = || -> Result<(Vec<u8>, UtcMicros), SessionError> {
        let mut file = dir.open_read(OsStr::new("checkpoint.md"))?;
        let bytes = file.read_bounded(CHECKPOINT_LIMIT)?;
        let micros = i64::try_from(file.metadata()?.modified_ns / 1000)
            .map_err(|_| provenant_core::TimeError::WallRange)?;
        Ok((bytes, UtcMicros::from_micros(micros)?))
    };
    match read() {
        Ok(found) => Ok(Some(found)),
        Err(SessionError::Filesystem(error)) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
}
fn load(dir: &Directory) -> Result<Manifest, SessionError> {
    let bytes = dir.read_bounded(OsStr::new(MANIFEST), MANIFEST_LIMIT)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| SessionError::History)?;
    if manifest.schema_version != 1 || manifest.revisions.len() as u64 > REVISIONS_MAX {
        return Err(SessionError::History);
    }
    Ok(manifest)
}
fn highest_revision_file(dir: &Directory) -> Result<u64, SessionError> {
    let mut highest = 0;
    for name in dir.entries()? {
        highest = highest.max(revision_number(&name?).unwrap_or(0));
    }
    Ok(highest)
}
/// The recorded chain, plus the checkpoint on disk when no revision records
/// it: the first save after upgrading, a save that stopped before its history
/// was written, or a file edited by hand.
fn reconcile(
    manifest: &Manifest,
    existing: Option<&(Vec<u8>, UtcMicros)>,
    floor: u64,
) -> Vec<Revision> {
    let mut chain = manifest.revisions.clone();
    if let Some((bytes, modified)) = existing {
        let digest = provenant_records::CanonicalV1::file_digest(bytes);
        if chain.last().is_none_or(|last| last.sha256 != digest) {
            let parent = chain.last().map(|last| last.revision);
            chain.push(Revision {
                revision: parent.unwrap_or(0).max(floor) + 1,
                parent,
                sha256: digest,
                bytes: bytes.len() as u64,
                saved_at: *modified,
            });
        }
    }
    chain
}
/// Delete revision files the history does not name.
fn remove_orphans(dir: &Directory, manifest: &Manifest) {
    let Ok(entries) = dir.entries() else { return };
    let named: Vec<u64> = manifest.revisions.iter().map(|r| r.revision).collect();
    for name in entries.flatten() {
        if revision_number(&name).is_some_and(|number| !named.contains(&number)) {
            let _ = dir.unlink(&name);
        }
    }
}

/// Host-provided IDs use command-safe components, independently of core IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostSessionId(String);
#[derive(Debug, Error)]
#[error(
    "host session ID must be 1-128 lowercase ASCII letters/digits/._-, start alphanumeric and contain no '..'"
)]
pub struct HostSessionIdError;
impl HostSessionId {
    pub fn parse(text: &str) -> Result<Self, HostSessionIdError> {
        if text.is_empty()
            || text.len() > HOST_SESSION_ID_LIMIT
            || text.contains("..")
            || !text
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            || !text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        {
            return Err(HostSessionIdError);
        }
        Ok(Self(text.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for HostSessionId {
    type Error = HostSessionIdError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}
impl From<HostSessionId> for String {
    fn from(id: HostSessionId) -> Self {
        id.0
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("checkpoint exceeds 256 KiB")]
    TooLarge,
    #[error("checkpoint is not UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("filesystem failure while accessing checkpoint")]
    Filesystem(#[from] FsError),
    #[error("checkpoint is visible but directory sync failed; reread before retrying")]
    Durability(#[source] FsError),
    #[error("checkpoint timestamp is outside core's supported range")]
    Time(#[from] provenant_core::TimeError),
    #[error("checkpoint save deadline expired")]
    Deadline,
    #[error("session revision history is unreadable or does not match its recorded hash")]
    History,
    #[error(
        "session discovery identity does not match the registered chair; repeat workflow session save <id> with the same options and add --rebind to explicitly replace the binding"
    )]
    DiscoveryIdentity,
    #[error("summary update requires a bound chair identity")]
    ChairIdentityRequired,
    #[error("summary revision is stale; current revision is {current_revision}")]
    SummaryStale { current_revision: u64 },
    #[error("{0}")]
    Summary(#[from] SummaryError),
    #[error("checkpoint for session {session} not found")]
    MissingCheckpoint { session: String },
    #[error("operation id was already used with a different summary update")]
    OperationConflict,
    #[error("pending summary journal is unreadable")]
    Journal,
    #[error("summary update would exceed the 8192-byte frontmatter limit")]
    SummaryTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checkpoint {
    pub markdown: String,
    pub modified_at: UtcMicros,
}

/// The state a hook derived and saved beside the checkpoint. Only hooks write
/// it; the chair's checkpoint is never modified by an autosave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Autosave {
    pub markdown: String,
    pub modified_at: UtcMicros,
}
/// Hook-owned file beside `checkpoint.md`.
const AUTOSAVE_FILE: &str = "autosave.md";

/// StateRoot must already exist. Reads create nothing; saves create only this key.
pub struct SessionStore {
    root: StateRoot,
    project: ProjectId,
    session: HostSessionId,
    keep: u64,
    chair: Option<ChairIdentity>,
    rebind: bool,
    workspace: Option<std::path::PathBuf>,
}
impl SessionStore {
    pub fn new(root: StateRoot, project: ProjectId, session: HostSessionId) -> Self {
        Self {
            root,
            project,
            session,
            keep: REVISIONS_DEFAULT,
            chair: None,
            rebind: false,
            workspace: None,
        }
    }
    /// Keep this many revisions, current included, clamped to 1 through 64.
    #[must_use]
    pub fn with_revisions(mut self, keep: u64) -> Self {
        self.keep = keep.clamp(1, REVISIONS_MAX);
        self
    }
    fn directory(&self, create: bool) -> Result<Directory, FsError> {
        crate::persistence::directory(
            &self.root,
            self.project,
            "sessions",
            self.session.as_str(),
            create,
        )
    }
    /// Replaces all Markdown. The caller must preserve originating authority refs.
    /// The final cancellation check is the publication boundary. After it,
    /// finish the bounded atomic replacement, including staging and directory
    /// sync, without observing later cancellation requests.
    pub fn save(
        &self,
        text: &[u8],
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), SessionError> {
        self.save_revision(text, budget, cancel, clock).map(|_| ())
    }
    /// As `save`, keeping the checkpoint it replaces as a numbered revision.
    /// The previous text is stored first, then the new checkpoint is published,
    /// then the history that names both and the advisory discovery sidecar.
    /// Discovery publication failure warns after the manifest commit; the next
    /// save repairs it. A failure before that commit leaves a complete
    /// checkpoint; the next save records it.
    /// Revisions beyond the keep count are deleted after the history is
    /// written; a deletion that fails is retried by the next save. Unreadable
    /// revision history restarts after the highest revision file present.
    /// Unreadable discovery metadata blocks a save to preserve chair identity.
    pub fn save_revision(
        &self,
        text: &[u8],
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Saved, SessionError> {
        self.save_revision_with(text, budget, cancel, clock, |dir, record| {
            self.publish_discovery(dir, record)
        })
    }

    fn save_revision_with(
        &self,
        text: &[u8],
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
        publish_discovery: impl FnOnce(&Directory, &SessionDiscovery) -> Result<(), SessionError>,
    ) -> Result<Saved, SessionError> {
        if text.len() as u64 > CHECKPOINT_LIMIT {
            return Err(SessionError::TooLarge);
        }
        std::str::from_utf8(text)?;
        let dir = self.directory(true)?;
        let _lock = dir.lock(
            OsStr::new("checkpoint.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        check_wait(budget, cancel, clock).map_err(|stop| match stop {
            WaitStop::Cancelled => SessionError::from(FsError::Cancelled {
                path: self.root.as_path().into(),
            }),
            WaitStop::Deadline => SessionError::Deadline,
        })?;
        self.publish_revision_locked(&dir, text, clock, false, publish_discovery)
    }

    /// Publish a replacement while holding the checkpoint lock. Any pending
    /// summary journal is replayed first, so no writer interleaves with it.
    fn publish_revision_locked(
        &self,
        dir: &Directory,
        text: &[u8],
        clock: &dyn Clock,
        field_update: bool,
        publish_discovery: impl FnOnce(&Directory, &SessionDiscovery) -> Result<(), SessionError>,
    ) -> Result<Saved, SessionError> {
        self.recover_pending(dir)?;
        let plan = self.plan_publication(dir, text, clock.wall_now()?)?;
        self.execute_plan(dir, &plan, field_update, publish_discovery)
    }

    /// Decide every publication step from the current state. The result is
    /// self-contained, so replaying it after a crash repeats the same steps
    /// and yields the same history, whatever the files now hold.
    fn plan_publication(
        &self,
        dir: &Directory,
        text: &[u8],
        now: UtcMicros,
    ) -> Result<PublishPlan, SessionError> {
        if text.len() as u64 > CHECKPOINT_LIMIT {
            return Err(SessionError::TooLarge);
        }
        let markdown = std::str::from_utf8(text)?;
        // A checkpoint that cannot be read (a symlink, say) is replaced, not kept.
        let existing = read_current(dir).ok().flatten();
        let discovery = self.load_discovery(dir)?;
        self.check_discovery_identity(discovery.as_ref())?;
        let manifest = load(dir).unwrap_or_default();
        let floor = highest_revision_file(dir)?;
        let mut chain = reconcile(&manifest, existing.as_ref(), floor);
        let digest = provenant_records::CanonicalV1::file_digest(text);
        let unchanged = chain.last().is_some_and(|last| last.sha256 == digest);
        let mut prev = None;
        if !unchanged {
            // Keep the text being replaced before replacing it.
            if let (Some(last), Some((bytes, _))) = (chain.last(), existing.as_ref()) {
                prev = Some((last.revision, bytes.clone()));
            }
            let parent = chain.last().map(|last| last.revision);
            chain.push(Revision {
                revision: parent.unwrap_or(floor).max(floor) + 1,
                parent,
                sha256: digest,
                bytes: text.len() as u64,
                saved_at: now,
            });
        }
        let current = chain.last().ok_or(SessionError::History)?;
        let summary = parse_summary(markdown).ok().flatten();
        let discovery = self.saved_discovery(discovery, current, summary, now);
        Ok(PublishPlan {
            now,
            prev,
            text: text.to_vec(),
            chain,
            unchanged,
            keep: self.keep,
            discovery,
        })
    }

    /// Perform a plan's steps in order. Each step is idempotent.
    fn execute_plan(
        &self,
        dir: &Directory,
        plan: &PublishPlan,
        field_update: bool,
        publish_discovery: impl FnOnce(&Directory, &SessionDiscovery) -> Result<(), SessionError>,
    ) -> Result<Saved, SessionError> {
        let markdown = std::str::from_utf8(&plan.text)?;
        if let Some((revision, bytes)) = &plan.prev {
            publish(dir, &revision_file(*revision), bytes)?;
        }
        publish(dir, "checkpoint.md", &plan.text)?;
        let mut chain = plan.chain.clone();
        let current = chain.last().cloned().ok_or(SessionError::History)?;
        let keep = usize::try_from(plan.keep).unwrap_or(usize::MAX);
        let pruned: Vec<Revision> = chain.drain(..chain.len().saturating_sub(keep)).collect();
        let manifest = Manifest {
            schema_version: 1,
            revisions: chain,
        };
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(|_| SessionError::History)?;
        publish(dir, MANIFEST, &manifest_bytes)?;
        let mut warnings = Vec::new();
        if let Err(error) = parse_summary(markdown) {
            warnings.push(format!("checkpoint saved; summary unavailable: {error}"));
        }
        if let Some(record) = &plan.discovery
            && let Err(error) = publish_discovery(dir, record)
        {
            let rebind = if self.rebind {
                " (including --rebind)"
            } else {
                ""
            };
            let repair = if field_update {
                "reread the current checkpoint and save its Markdown".to_string()
            } else {
                format!("repeat the same command{rebind}")
            };
            warnings.push(format!("checkpoint saved; discovery publication failed: {error}; {repair} to repair discovery"));
        }
        // The history is durable, so these files are unreachable. A failed
        // delete leaves an orphan that the next save removes.
        for old in &pruned {
            let _ = dir.unlink(OsStr::new(&revision_file(old.revision)));
        }
        remove_orphans(dir, &manifest);
        Ok(Saved {
            revision: current.revision,
            parent: current.parent,
            unchanged: plan.unchanged,
            retained: manifest.revisions.len(),
            warnings,
        })
    }
    /// The retained revisions, oldest first; the last is the current checkpoint.
    /// A checkpoint changed without a save shows as the newest revision.
    pub fn revisions(&self) -> Result<Vec<Revision>, SessionError> {
        let dir = match self.directory(false) {
            Ok(dir) => dir,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let existing = read_current(&dir)?;
        let manifest = match load(&dir) {
            Ok(manifest) => manifest,
            Err(SessionError::Filesystem(error)) if error.is_not_found() => Manifest::default(),
            Err(error) => return Err(error),
        };
        let floor = highest_revision_file(&dir)?;
        Ok(reconcile(&manifest, existing.as_ref(), floor))
    }
    /// Capture current text once, then read retained files from one manifest.
    /// A concurrent save may prune or replace a file between these reads.
    fn summary_snapshot(&self) -> Result<(String, Vec<String>), SessionError> {
        self.summary_snapshot_with(|| {})
    }
    fn summary_snapshot_with(
        &self,
        after_snapshot: impl FnOnce(),
    ) -> Result<(String, Vec<String>), SessionError> {
        let missing = || SessionError::MissingCheckpoint {
            session: self.session.as_str().into(),
        };
        let dir = match self.directory(false) {
            Ok(dir) => dir,
            Err(error) if error.is_not_found() => return Err(missing()),
            Err(error) => return Err(error.into()),
        };
        let current = read_current(&dir)?.ok_or_else(missing)?;
        let manifest = match load(&dir) {
            Ok(manifest) => manifest,
            Err(SessionError::Filesystem(error)) if error.is_not_found() => Manifest::default(),
            Err(error) => return Err(error),
        };
        let chain = reconcile(&manifest, Some(&current), highest_revision_file(&dir)?);
        after_snapshot();
        let mut history = Vec::new();
        for revision in chain.iter().take(chain.len().saturating_sub(1)) {
            let bytes = match dir.read_bounded(
                OsStr::new(&revision_file(revision.revision)),
                CHECKPOINT_LIMIT,
            ) {
                Ok(bytes) => bytes,
                Err(error) if error.is_not_found() => continue,
                Err(error) => return Err(error.into()),
            };
            if provenant_records::CanonicalV1::file_digest(&bytes) != revision.sha256 {
                // Preserve corruption detection when the snapshot is stable.
                // If it moved, this revision cannot be trusted in this scan.
                let latest = read_current(&dir)?;
                let latest_manifest = load(&dir)?;
                if latest.as_ref().map(|(bytes, _)| bytes) == Some(&current.0)
                    && latest_manifest.revisions == manifest.revisions
                {
                    return Err(SessionError::History);
                }
                continue;
            }
            history.push(std::str::from_utf8(&bytes)?.to_owned());
        }
        Ok((std::str::from_utf8(&current.0)?.to_owned(), history))
    }
    /// The text of one retained revision, checked against its recorded hash.
    pub fn read_revision(&self, revision: u64) -> Result<Option<RevisionText>, SessionError> {
        let chain = self.revisions()?;
        let Some(found) = chain.iter().find(|r| r.revision == revision).cloned() else {
            return Ok(None);
        };
        let dir = self.directory(false)?;
        let bytes = if chain.last() == Some(&found) {
            read_current(&dir)?.map(|(bytes, _)| bytes)
        } else {
            match dir.read_bounded(OsStr::new(&revision_file(revision)), CHECKPOINT_LIMIT) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.is_not_found() => None,
                Err(error) => return Err(error.into()),
            }
        };
        // A checkpoint changed outside a save keeps no copy of the text it
        // replaced, so a listed revision can have no text.
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        if provenant_records::CanonicalV1::file_digest(&bytes) != found.sha256 {
            return Err(SessionError::History);
        }
        Ok(Some(RevisionText {
            revision: found,
            markdown: std::str::from_utf8(&bytes)?.to_owned(),
        }))
    }
    /// A line diff from a retained revision to the current checkpoint, or
    /// `None` when that revision is not retained.
    pub fn diff(&self, revision: u64) -> Result<Option<SessionDiff>, SessionError> {
        let Some(from) = self.read_revision(revision)? else {
            return Ok(None);
        };
        let Some(current) = self.revisions()?.last().map(|r| r.revision) else {
            return Ok(None);
        };
        let Some(to) = self.read_revision(current)? else {
            return Ok(None);
        };
        let text = diff::unified(
            &from.markdown,
            &to.markdown,
            &format!("revision {}", from.revision.revision),
            &format!("revision {} (current)", to.revision.revision),
        );
        Ok(Some(SessionDiff {
            from: from.revision,
            to: to.revision,
            text,
        }))
    }
    /// Replaces the hook-owned autosave file, creating the session directory
    /// when missing. `checkpoint.md` is never opened for writing.
    pub fn write_autosave(
        &self,
        text: &[u8],
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), SessionError> {
        self.write_file(AUTOSAVE_FILE, ".autosave.", text, budget, cancel, clock)
    }

    fn write_file(
        &self,
        name: &str,
        staging: &str,
        text: &[u8],
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), SessionError> {
        if text.len() as u64 > CHECKPOINT_LIMIT {
            return Err(SessionError::TooLarge);
        }
        std::str::from_utf8(text)?;
        let dir = self.directory(true)?;
        let _lock = dir.lock(
            OsStr::new("checkpoint.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        check_wait(budget, cancel, clock).map_err(|stop| match stop {
            WaitStop::Cancelled => SessionError::from(FsError::Cancelled {
                path: self.root.as_path().into(),
            }),
            WaitStop::Deadline => SessionError::Deadline,
        })?;
        dir.atomic_write(OsStr::new(name), staging, text, PublishMode::Replace)?
            .require_durable()
            .map_err(SessionError::Durability)
    }
    /// The hook-owned autosave file, if any hook has written one.
    pub fn read_autosave(&self) -> Result<Option<Autosave>, SessionError> {
        self.read_file(AUTOSAVE_FILE).map(|saved| {
            saved.map(|(markdown, modified_at)| Autosave {
                markdown,
                modified_at,
            })
        })
    }
    /// Atomic replacement allows a consistent unlocked read of one held file.
    pub fn read(&self) -> Result<Option<Checkpoint>, SessionError> {
        self.read_file("checkpoint.md").map(|saved| {
            saved.map(|(markdown, modified_at)| Checkpoint {
                markdown,
                modified_at,
            })
        })
    }

    fn read_file(&self, name: &str) -> Result<Option<(String, UtcMicros)>, SessionError> {
        let read = || -> Result<_, SessionError> {
            let dir = self.directory(false)?;
            let mut file = dir.open_read(OsStr::new(name))?;
            let text = file.read_bounded(CHECKPOINT_LIMIT)?;
            let markdown = std::str::from_utf8(&text)?.to_owned();
            let micros = i64::try_from(file.metadata()?.modified_ns / 1000)
                .map_err(|_| provenant_core::TimeError::WallRange)?;
            Ok((markdown, UtcMicros::from_micros(micros)?))
        };
        match read() {
            Ok(saved) => Ok(Some(saved)),
            Err(SessionError::Filesystem(error)) if error.is_not_found() => Ok(None),
            Err(error) => Err(error),
        }
    }
    /// Read, edit and replace the checkpoint under one exclusive lock, so a
    /// concurrent `save` is never overwritten from a stale read. `edit` gets the
    /// current Markdown and returns the replacement, or `None` to leave the file
    /// alone. Directories are created only when `create` is set and the
    /// checkpoint is missing. Returns publication details when a replacement
    /// was written, including any advisory discovery warning.
    pub fn update(
        &self,
        create: bool,
        edit: impl FnOnce(Option<&str>) -> Result<Option<String>, SessionError>,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Option<Saved>, SessionError> {
        let dir = match self.directory(create) {
            Ok(dir) => dir,
            Err(error) if !create && error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let _lock = dir.lock(
            OsStr::new("checkpoint.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        self.recover_pending(&dir)?;
        self.check_discovery_identity(self.load_discovery(&dir)?.as_ref())?;
        let current = match dir.open_read(OsStr::new("checkpoint.md")) {
            Ok(mut file) => {
                let text = file.read_bounded(CHECKPOINT_LIMIT)?;
                Some(std::str::from_utf8(&text)?.to_owned())
            }
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error.into()),
        };
        let Some(next) = edit(current.as_deref())? else {
            return Ok(None);
        };
        if next.len() as u64 > CHECKPOINT_LIMIT {
            return Err(SessionError::TooLarge);
        }
        check_wait(budget, cancel, clock).map_err(|stop| match stop {
            WaitStop::Cancelled => SessionError::from(FsError::Cancelled {
                path: self.root.as_path().into(),
            }),
            WaitStop::Deadline => SessionError::Deadline,
        })?;
        let saved =
            self.publish_revision_locked(&dir, next.as_bytes(), clock, true, |dir, record| {
                self.publish_discovery(dir, record)
            })?;
        Ok(Some(saved))
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use provenant_core::{DurationMicros, MonoTime, RootInputs, TestClock};
    use std::io::{self, Read};
    use std::os::unix::fs::symlink;
    struct Resolver;
    impl provenant_core::PathResolver for Resolver {
        fn resolve_existing_ancestor(
            &self,
            path: &std::path::Path,
        ) -> io::Result<std::path::PathBuf> {
            Ok(path.into())
        }
    }
    fn setup() -> (tempfile::TempDir, StateRoot, ProjectId) {
        // Explicit roots; no environment or real HOME/harness settings are read.
        let temp = crate::test_support::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir(&home).unwrap();
        let state = base.join("state");
        std::fs::create_dir(&state).unwrap();
        let mut inputs = RootInputs::new(home, base.join("checkout"));
        inputs.state_dir = Some(state);
        (
            temp,
            StateRoot::resolve(&inputs, &Resolver).unwrap(),
            ProjectId::from_bytes([1; 16]),
        )
    }
    fn store(root: &StateRoot, project: ProjectId, session: &str) -> SessionStore {
        SessionStore::new(
            root.clone(),
            project,
            HostSessionId::parse(session).unwrap(),
        )
    }
    fn op(text: &str) -> provenant_core::OperationId {
        provenant_core::OperationId::parse(text).unwrap()
    }
    fn clock(micros: i64) -> TestClock {
        TestClock::new(
            UtcMicros::from_micros(micros).unwrap(),
            MonoTime::from_micros(0),
        )
    }
    fn save(store: &SessionStore, bytes: &[u8]) -> Result<(), SessionError> {
        let clock = clock(0);
        store.save(
            bytes,
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &clock,
        )
    }
    fn path(root: &StateRoot, project: ProjectId, session: &str) -> std::path::PathBuf {
        root.as_path()
            .join(format!("workflow/projects/{project}/sessions/{session}"))
    }
    #[test]
    fn saves_only_under_the_explicit_project_and_host_session_key() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        assert!(selected.read().unwrap().is_none());
        assert!(!root.as_path().join("workflow").exists());
        save(&selected, b"# Goal\nKeep originating authority ref.").unwrap();
        let saved = selected.read().unwrap().unwrap();
        assert_eq!(saved.markdown, "# Goal\nKeep originating authority ref.");
        let lock =
            std::fs::metadata(path(&root, project, "host-1").join("checkpoint.lock")).unwrap();
        save(&selected, b"complete replacement").unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            lock.ino(),
            std::fs::metadata(path(&root, project, "host-1").join("checkpoint.lock"))
                .unwrap()
                .ino()
        );
        assert_eq!(
            selected.read().unwrap().unwrap().markdown,
            "complete replacement"
        );
        assert!(store(&root, project, "host-2").read().unwrap().is_none());
        assert!(
            store(&root, ProjectId::from_bytes([2; 16]), "host-1")
                .read()
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn session_save_discovery_restart() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let host = HostId::from_bytes([3; 16]);
        let actor = ActorId::from_bytes([4; 16]);
        let selected = store(&root, project, "explicit-chair").with_chair(host, actor);
        assert!(selected.discovery().unwrap().is_none());
        assert!(discover_sessions(&root, project).unwrap().is_empty());
        assert!(!root.as_path().join("workflow").exists());
        saved(&selected, "chair notes\n");
        drop(selected);
        let restarted = store(&root, project, "explicit-chair");
        let found = restarted.discovery().unwrap().unwrap();
        assert_eq!(
            discover_sessions(&root, project).unwrap(),
            std::slice::from_ref(&found)
        );
        assert_eq!(found.consistency, DiscoveryConsistency::Current);
        let record = found.record;
        assert_eq!(
            (record.host, record.project_id, record.chair_actor),
            (host, project, actor)
        );
        assert_eq!(record.host_session_id.as_str(), "explicit-chair");
        assert_eq!(record.checkpoint, restarted.revisions().unwrap()[0]);
        // Unchanged saves refresh observation without adding a revision, and
        // saves through an existing store preserve the explicit registration.
        let later = clock(10);
        restarted
            .save(
                b"chair notes\n",
                WaitBudget::cli(later.mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &later,
            )
            .unwrap();
        let updated = restarted.discovery().unwrap().unwrap().record;
        assert_eq!(updated.created_at, record.created_at);
        assert_eq!(updated.last_saved_at, later.wall_now().unwrap());
        assert_eq!(updated.checkpoint.revision, 1);
        for wrong in [
            store(&root, project, "another-chair"),
            store(&root, ProjectId::from_bytes([2; 16]), "explicit-chair"),
        ] {
            assert!(wrong.discovery().unwrap().is_none());
        }
        let wrong_actor =
            store(&root, project, "explicit-chair").with_chair(host, ActorId::from_bytes([5; 16]));
        assert!(matches!(
            save(&wrong_actor, b"wrong"),
            Err(SessionError::DiscoveryIdentity)
        ));
        let wrong_host =
            store(&root, project, "explicit-chair").with_chair(HostId::from_bytes([6; 16]), actor);
        assert!(matches!(
            save(&wrong_host, b"wrong"),
            Err(SessionError::DiscoveryIdentity)
        ));
        assert_eq!(restarted.read().unwrap().unwrap().markdown, "chair notes\n");
    }

    #[test]
    fn discovery_reports_interrupted_publication_without_read_repair() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([4; 16]));
        saved(&selected, "before");
        let dir = path(&root, project, "chair");
        let manifest = std::fs::read(dir.join(MANIFEST)).unwrap();
        let discovery_path = dir.join("discovery.json");
        let discovery = std::fs::read(&discovery_path).unwrap();
        // Crash after checkpoint replacement, before manifest publication.
        std::fs::write(dir.join("checkpoint.md"), "interrupted").unwrap();
        assert_eq!(
            selected.discovery().unwrap().unwrap().consistency,
            DiscoveryConsistency::CheckpointChanged
        );
        assert_eq!(std::fs::read(dir.join(MANIFEST)).unwrap(), manifest);
        saved(&selected, "interrupted");
        let repaired = selected.discovery().unwrap().unwrap();
        assert_eq!(repaired.consistency, DiscoveryConsistency::Current);
        assert_eq!(repaired.record.checkpoint.revision, 2);
        // Crash after manifest publication but before index publication.
        std::fs::write(&discovery_path, &discovery).unwrap();
        assert_eq!(
            selected.discovery().unwrap().unwrap().consistency,
            DiscoveryConsistency::CheckpointChanged
        );
        saved(&selected, "interrupted");
        assert_eq!(
            selected
                .discovery()
                .unwrap()
                .unwrap()
                .record
                .checkpoint
                .revision,
            2
        );
        // Unreadable identity must not overwrite either checkpoint or history.
        let manifest = std::fs::read(dir.join(MANIFEST)).unwrap();
        let discovery = std::fs::read(&discovery_path).unwrap();
        std::fs::write(&discovery_path, "{not json").unwrap();
        assert!(matches!(
            save(&selected, b"blocked"),
            Err(SessionError::History)
        ));
        assert_eq!(std::fs::read(dir.join(MANIFEST)).unwrap(), manifest);
        assert_eq!(selected.read().unwrap().unwrap().markdown, "interrupted");
        std::fs::write(&discovery_path, discovery).unwrap();
        // History recovery still works without losing the separate identity.
        std::fs::write(dir.join(MANIFEST), "{not json").unwrap();
        saved(&selected, "interrupted");
        assert_eq!(
            selected.discovery().unwrap().unwrap().consistency,
            DiscoveryConsistency::Current
        );
        std::fs::remove_file(dir.join("checkpoint.md")).unwrap();
        assert_eq!(
            selected.discovery().unwrap().unwrap().consistency,
            DiscoveryConsistency::CheckpointMissing
        );
        // On the first save, a crash before the index leaves unknown identity.
        std::fs::remove_file(dir.join(MANIFEST)).unwrap();
        std::fs::remove_file(discovery_path).unwrap();
        std::fs::write(dir.join("checkpoint.md"), "legacy").unwrap();
        assert!(selected.discovery().unwrap().is_none());
        assert_eq!(selected.read().unwrap().unwrap().markdown, "legacy");
        // Autosave and legacy writes alone never establish chair identity.
        let worker = store(&root, project, "worker");
        saved(&worker, "worker checkpoint");
        worker
            .write_autosave(
                b"derived",
                WaitBudget::cli(clock(0).mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &clock(0),
            )
            .unwrap();
        assert!(worker.discovery().unwrap().is_none());
        assert!(discover_sessions(&root, project).unwrap().is_empty());
    }

    #[test]
    fn discovery_truncates_overlong_rebind_history_on_read() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([20; 16]));
        saved(&selected, "notes");
        let mut record = selected.discovery().unwrap().unwrap().record;
        record.rebound_from = (0..10)
            .map(|actor| ChairRebind {
                host: record.host,
                actor: ActorId::from_bytes([actor; 16]),
                changed_at: record.last_saved_at,
            })
            .collect();
        let discovery = path(&root, project, "chair").join("discovery.json");
        let bytes = serde_json::to_vec(&record).unwrap();
        std::fs::write(&discovery, &bytes).unwrap();
        let expected = record.rebound_from[2..].to_vec();
        assert_eq!(
            selected.discovery().unwrap().unwrap().record.rebound_from,
            expected
        );
        assert_eq!(std::fs::read(&discovery).unwrap(), bytes);
        saved(&selected, "notes");
        let persisted: SessionDiscovery =
            serde_json::from_slice(&std::fs::read(discovery).unwrap()).unwrap();
        assert_eq!(persisted.rebound_from, expected);
    }

    #[test]
    fn rebind_keeps_eight_changes_and_upgrades_single_hop_records() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let host = HostId::from_bytes([3; 16]);
        let selected = |actor| {
            store(&root, project, "chair")
                .with_chair(host, ActorId::from_bytes([actor; 16]))
                .with_rebind(true)
        };
        saved(&selected(0), "notes");
        for actor in 1..=10 {
            saved(&selected(actor), "notes");
        }
        let record = selected(10).discovery().unwrap().unwrap().record;
        assert_eq!(record.rebound_from.len(), 8);
        assert_eq!(record.rebound_from[0].actor, ActorId::from_bytes([2; 16]));
        assert_eq!(record.rebound_from[7].actor, ActorId::from_bytes([9; 16]));
        saved(&selected(10).with_rebind(false), "same chair");
        assert_eq!(
            selected(10)
                .discovery()
                .unwrap()
                .unwrap()
                .record
                .rebound_from,
            record.rebound_from
        );
        // Old single-hop JSON remains readable and is retained on migration.
        let mut legacy = serde_json::to_value(&record).unwrap();
        legacy["rebound_from"] = serde_json::to_value(&record.rebound_from[7]).unwrap();
        let discovery = path(&root, project, "chair").join("discovery.json");
        std::fs::write(&discovery, serde_json::to_vec(&legacy).unwrap()).unwrap();
        saved(&selected(11), "notes");
        let record = selected(11).discovery().unwrap().unwrap().record;
        assert_eq!(record.rebound_from.len(), 2);
        assert_eq!(record.rebound_from[0].actor, ActorId::from_bytes([9; 16]));
        assert_eq!(record.rebound_from[1].actor, ActorId::from_bytes([10; 16]));
        assert!(std::fs::metadata(discovery).unwrap().len() <= 4096);
    }

    #[test]
    fn hook_save_commands_preserve_the_installing_harness() {
        let (_temp, root, project) = setup();
        for (harness, fixture) in [
            (provenant_records::Harness::Claude, FIXTURES[0].0),
            (provenant_records::Harness::Codex, FIXTURES[2].0),
        ] {
            let response = hook_context_with(
                &root,
                project,
                HookEvent::SessionStart,
                fixture.as_bytes(),
                &clock(0),
                DurationMicros::ZERO,
                Advice {
                    digest: "",
                    commands: &CommandPrefix::default(),
                    client: Some(harness),
                },
            );
            let text = context(&response);
            assert!(
                text.contains(&format!(
                    "--file <checkpoint.md> --client {}",
                    harness.spec().name
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn discovery_publication_failure_warns_after_manifest_commit_and_next_save_repairs() {
        use provenant_core::{ActorId, HostId};
        for rebind in [false, true] {
            let (_temp, root, project) = setup();
            let selected = store(&root, project, "chair")
                .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([4; 16]))
                .with_rebind(rebind);
            for first_save in [true, false] {
                let clock = clock(10);
                let saved = selected
                    .save_revision_with(
                        if first_save { b"first" } else { b"second" },
                        WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                        &CancelToken::new(),
                        &clock,
                        |dir, record| {
                            // Inject failure at the actual second publication boundary.
                            assert_eq!(
                                load(dir).unwrap().revisions.last(),
                                Some(&record.checkpoint)
                            );
                            Err(SessionError::Deadline)
                        },
                    )
                    .unwrap();
                assert_eq!(saved.warnings.len(), 1);
                assert!(
                    saved.warnings[0].contains("checkpoint saved; discovery publication failed")
                );
                assert!(saved.warnings[0].contains("repeat the same command"));
                assert_eq!(saved.warnings[0].contains("including --rebind"), rebind);
                let expected = if first_save { "first" } else { "second" };
                assert_eq!(selected.read().unwrap().unwrap().markdown, expected);
                if first_save {
                    assert!(selected.discovery().unwrap().is_none());
                } else {
                    assert_eq!(
                        selected.discovery().unwrap().unwrap().consistency,
                        DiscoveryConsistency::CheckpointChanged
                    );
                }
                let repaired = super::tests::saved(&selected, expected);
                assert!(repaired.unchanged);
                assert!(repaired.warnings.is_empty());
                assert_eq!(
                    selected.discovery().unwrap().unwrap().consistency,
                    DiscoveryConsistency::Current
                );
            }
        }
    }
    #[test]
    fn summary_index_recovers_after_interrupted_publication() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([4; 16]));
        let header = |revision, now| {
            format!(
                "---\nprovenant_summary:\n  schema_version: 1\n  revision: {revision}\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"{now}\"\n  next: \"Next\"\n  needs_you: []\n  risk: \"None\"\n---\nBody\n"
            )
        };
        let first = header(1, "First");
        let second = header(2, "Second");
        saved(&selected, &first);
        assert_eq!(
            selected
                .discovery()
                .unwrap()
                .unwrap()
                .record
                .summary
                .unwrap()
                .now,
            "First"
        );
        let clock = clock(10);
        let result = selected
            .save_revision_with(
                second.as_bytes(),
                WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &clock,
                |dir, record| {
                    assert_eq!(
                        load(dir).unwrap().revisions.last(),
                        Some(&record.checkpoint)
                    );
                    assert_eq!(record.summary.as_ref().unwrap().now, "Second");
                    Err(SessionError::Deadline)
                },
            )
            .unwrap();
        assert_eq!(result.warnings.len(), 1);
        let stale = selected.discovery().unwrap().unwrap();
        assert_eq!(stale.consistency, DiscoveryConsistency::CheckpointChanged);
        assert_eq!(stale.record.summary.unwrap().now, "First");
        assert!(saved(&selected, &second).unchanged);
        let repaired = selected.discovery().unwrap().unwrap();
        assert_eq!(repaired.consistency, DiscoveryConsistency::Current);
        assert_eq!(repaired.record.summary.unwrap().now, "Second");
        saved(&selected, "legacy checkpoint");
        assert!(
            selected
                .discovery()
                .unwrap()
                .unwrap()
                .record
                .summary
                .is_none()
        );
    }
    #[test]
    fn retained_summary_diff_compares_fields_and_reports_missing() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "chair").with_revisions(3);
        let header = |revision, now, needs| {
            format!(
                "---\nprovenant_summary:\n  schema_version: 1\n  revision: {revision}\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"{now}\"\n  next: \"Next\"\n  needs_you: {needs}\n  risk: \"None\"\n---\nBody\n"
            )
        };
        let first = header(1, "First", "[]");
        saved(&selected, &first);
        saved(&selected, &first.replace("Body", "Other body"));
        saved(&selected, &header(2, "Second", "[\"request\"]"));
        let diff = serde_json::to_value(selected.summary_diff(1, Some(2)).unwrap()).unwrap();
        assert_eq!(diff["unavailable"], serde_json::json!([]));
        assert_eq!(
            diff["now"],
            serde_json::json!({"from":"First","to":"Second"})
        );
        assert_eq!(
            diff["needs_you"],
            serde_json::json!({"added":["request"],"removed":[]})
        );
        assert!(diff["next"].is_null());
        saved(&selected, &header(3, "Third", "[]"));
        saved(&selected, &header(4, "Fourth", "[]"));
        assert!(selected.viewer_summary_diff(None, None).unwrap().is_none());
        let diff = selected
            .viewer_summary_diff(Some(1), None)
            .unwrap()
            .unwrap();
        assert_eq!(diff.unavailable, vec![1]);
        assert!(diff.now.is_none());
        saved(&selected, &header(4, "Conflicting", "[]"));
        assert!(matches!(
            selected.summary_diff(4, Some(4)),
            Err(SessionError::History)
        ));
    }
    #[test]
    fn summary_diff_skips_invalid_history_and_snapshots_during_save() {
        let (_temp, root, project) = setup();
        let missing = store(&root, project, "missing");
        assert!(matches!(
            missing.summary_diff(1, Some(2)),
            Err(SessionError::MissingCheckpoint { .. })
        ));
        let selected = store(&root, project, "chair").with_revisions(6);
        saved(&selected, "A checkpoint without a summary");
        assert!(matches!(
            selected.summary_diff(1, Some(2)),
            Err(SessionError::Summary(_))
        ));
        let header = |revision, needs: &str| {
            format!(
                "---\nprovenant_summary:\n  schema_version: 1\n  revision: {revision}\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"Working\"\n  next: \"Next\"\n  needs_you: {needs}\n  risk: \"None\"\n---\nBody\n"
            )
        };
        saved(&selected, &header(1, "[\"old\",\"same\"]"));
        saved(&selected, "---\nprovenant_summary: invalid\n---\nBody\n");
        saved(&selected, &header(2, "[\"new\",\"same\"]"));
        let diff = serde_json::to_value(selected.summary_diff(1, None).unwrap()).unwrap();
        assert_eq!(
            diff["needs_you"],
            serde_json::json!({"added":["new"],"removed":["old"]})
        );
        assert_eq!(diff["to"], 2);
        let (current, history) = selected
            .summary_snapshot_with(|| {
                saved(&selected, &header(3, "[]"));
            })
            .unwrap();
        assert_eq!(parse_summary(&current).unwrap().unwrap().revision, 2);
        assert!(history.iter().any(|text| {
            parse_summary(text)
                .ok()
                .flatten()
                .is_some_and(|s| s.revision == 1)
        }));
        std::fs::write(path(&root, project, "chair").join("rev-2.md"), "corrupt").unwrap();
        assert!(matches!(
            selected.summary_diff(1, Some(3)),
            Err(SessionError::History)
        ));
    }
    #[test]
    fn field_update_publishes_revision_and_discovery() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([4; 16]));
        let first = "---\nprovenant_summary:\n  schema_version: 1\n  revision: 1\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"First\"\n  next: \"Next\"\n  needs_you: []\n  risk: \"None\"\n---\nBody\n";
        saved(&selected, first);
        let clock = clock(10);
        let result = selected
            .update(
                false,
                |current| Ok(Some(current.unwrap().replace("First", "Second"))),
                WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &clock,
            )
            .unwrap()
            .unwrap();
        assert_eq!(result.revision, 2);
        assert!(result.warnings.is_empty());
        assert_eq!(selected.read_revision(1).unwrap().unwrap().markdown, first);
        let discovery = selected.discovery().unwrap().unwrap();
        assert_eq!(discovery.consistency, DiscoveryConsistency::Current);
        assert_eq!(discovery.record.checkpoint.revision, 2);
        assert_eq!(discovery.record.summary.unwrap().now, "Second");
        let dir = selected.directory(false).unwrap();
        let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
        let lock = dir
            .lock(
                OsStr::new("checkpoint.lock"),
                LockMode::Exclusive,
                budget,
                &CancelToken::new(),
                &clock,
            )
            .unwrap();
        let current = selected.read().unwrap().unwrap().markdown;
        let interrupted = selected
            .publish_revision_locked(&dir, current.as_bytes(), &clock, true, |_, _| {
                Err(SessionError::Deadline)
            })
            .unwrap();
        assert!(
            interrupted.warnings[0].contains("reread the current checkpoint and save its Markdown")
        );
        drop(lock);
        assert!(saved(&selected, &current).warnings.is_empty());
    }
    #[test]
    fn summary_requires_chair_and_rechecks_identity_after_preflight() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let host = HostId::from_bytes([3; 16]);
        let original =
            store(&root, project, "chair").with_chair(host, ActorId::from_bytes([4; 16]));
        let rebound = store(&root, project, "chair")
            .with_chair(host, ActorId::from_bytes([5; 16]))
            .with_rebind(true);
        let unbound = store(&root, project, "chair");
        let text = "---\nprovenant_summary:\n  schema_version: 1\n  revision: 1\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"First\"\n  next: \"Next\"\n  needs_you: []\n  risk: \"None\"\n---\nBody\n";
        saved(&original, text);
        let clock = clock(10);
        let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
        let patch = || SummaryPatch {
            now: Some("changed".into()),
            ..Default::default()
        };
        let before = original.read().unwrap();
        let history = original.revisions().unwrap();
        let discovery = original.discovery().unwrap();
        assert!(matches!(
            unbound.update_summary(patch(), 1, &op("a"), budget, &CancelToken::new(), &clock),
            Err(SessionError::ChairIdentityRequired)
        ));
        assert_eq!(original.read().unwrap(), before);
        assert_eq!(original.revisions().unwrap(), history);
        assert_eq!(original.discovery().unwrap(), discovery);

        original.validate_chair().unwrap();
        // Rebind after preflight and before the old chair acquires the lock.
        saved(&rebound, text);
        let before = original.read().unwrap();
        let history = original.revisions().unwrap();
        let discovery = unbound.discovery().unwrap();
        // A stale revision would otherwise return SummaryStale from the edit
        // closure, before the publication path can check the new binding.
        let result =
            original.update_summary(patch(), 999, &op("a"), budget, &CancelToken::new(), &clock);
        assert!(
            matches!(&result, Err(SessionError::DiscoveryIdentity)),
            "{result:?}"
        );
        assert_eq!(original.read().unwrap(), before);
        assert_eq!(original.revisions().unwrap(), history);
        assert_eq!(unbound.discovery().unwrap(), discovery);
    }
    #[test]
    fn summary_save_race() {
        use provenant_core::{ActorId, Clock, HostId};
        use provenant_os::clock::SystemClock;
        use std::sync::{Arc, Barrier, mpsc};
        struct SignallingClock(mpsc::Sender<()>);
        impl Clock for SignallingClock {
            fn wall_now(&self) -> Result<UtcMicros, provenant_core::TimeError> {
                SystemClock::new().wall_now()
            }
            fn mono_now(&self) -> MonoTime {
                let _ = self.0.send(());
                SystemClock::new().mono_now()
            }
            fn wait_until(&self, deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
                SystemClock::new().wait_until(deadline)
            }
        }
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([4; 16]));
        let first = "---\r\nother: \"keep\"\r\n\"provenant_summary\":\r\n  schema_version: 1\r\n  # interior comment\r\n\r\n  revision: 1\r\n  updated_at: \"2026-10-28T03:00:00Z\"\r\n  now: \"First\"\r\n  next: \"Next\"\r\n  needs_you: []\r\n  risk: \"None\"\r\n# retain comment\r\nafter: \"keep too\"\r\n---\r\nBody\r\n";
        saved(&selected, first);
        let changed = first.replace("Body", "Saved concurrently");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let selected_ref = &selected;
            let changed_ref = &changed;
            let saving = scope.spawn(move || {
                let clock = provenant_os::clock::SystemClock::new();
                selected_ref
                    .save_revision_with(
                        changed_ref.as_bytes(),
                        WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                        &CancelToken::new(),
                        &clock,
                        |dir, record| {
                            entered_tx.send(()).unwrap();
                            release_rx.recv().unwrap();
                            selected_ref.publish_discovery(dir, record)
                        },
                    )
                    .unwrap()
            });
            entered_rx.recv().unwrap();
            // A zero-budget contender proves the save holds the same lock.
            let expired = TestClock::new(
                UtcMicros::from_micros(0).unwrap(),
                MonoTime::from_micros(1_000_000),
            );
            let budget = WaitBudget::cli(MonoTime::from_micros(0), 1).unwrap();
            assert!(matches!(
                selected.update_summary(
                    SummaryPatch {
                        now: Some("contender".into()),
                        ..Default::default()
                    },
                    1,
                    &op("a"),
                    budget,
                    &CancelToken::new(),
                    &expired
                ),
                Err(SessionError::Filesystem(FsError::LockTimeout { .. }))
            ));
            let selected_ref = &selected;
            let editing = scope.spawn(move || {
                let budget = WaitBudget::cli(SystemClock::new().mono_now(), 5).unwrap();
                let clock = SignallingClock(waiting_tx);
                let result = selected_ref.update_summary(
                    SummaryPatch {
                        now: Some("Edited".into()),
                        ..Default::default()
                    },
                    1,
                    &op("b"),
                    budget,
                    &CancelToken::new(),
                    &clock,
                );
                done_tx.send(()).unwrap();
                result
            });
            waiting_rx.recv().unwrap();
            assert!(matches!(
                done_rx.recv_timeout(std::time::Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            release_tx.send(()).unwrap();
            saving.join().unwrap();
            editing.join().unwrap().unwrap();
        });
        let checkpoint = selected.read().unwrap().unwrap().markdown;
        assert!(checkpoint.contains("Saved concurrently\r\n"));
        assert!(checkpoint.contains("other: \"keep\"\r\n"));
        assert!(checkpoint.contains("  # interior comment\r\n\r\nprovenant_summary:"));
        assert!(checkpoint.contains("# retain comment\r\nafter: \"keep too\"\r\n"));
        assert_eq!(parse_summary(&checkpoint).unwrap().unwrap().now, "Edited");
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let attempt = |value: &'static str, barrier: Arc<Barrier>| {
                let selected = &selected;
                scope.spawn(move || {
                    let clock = provenant_os::clock::SystemClock::new();
                    barrier.wait();
                    selected.update_summary(
                        SummaryPatch {
                            next: Some(value.into()),
                            ..Default::default()
                        },
                        2,
                        &op(value),
                        WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                        &CancelToken::new(),
                        &clock,
                    )
                })
            };
            let one = attempt("one", barrier.clone());
            let two = attempt("two", barrier);
            let results = [one.join().unwrap(), two.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert_eq!(
                results
                    .iter()
                    .filter(|r| matches!(
                        r,
                        Err(SessionError::SummaryStale {
                            current_revision: 3
                        })
                    ))
                    .count(),
                1
            );
        });
        let current = selected.read().unwrap().unwrap().markdown;
        let discovery = selected.discovery().unwrap().unwrap();
        assert_eq!(discovery.consistency, DiscoveryConsistency::Current);
        assert_eq!(discovery.record.summary, parse_summary(&current).unwrap());
        assert_eq!(
            discovery.record.checkpoint,
            *selected.revisions().unwrap().last().unwrap()
        );
        assert_eq!(
            selected.read_revision(2).unwrap().unwrap().markdown,
            changed
        );
    }
    #[test]
    fn summary_empty_header_and_oversize_are_safe() {
        use provenant_core::{ActorId, HostId};
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([4; 16]));
        let original = "---\r\n# existing comment\r\n\r\n---\r\nBody\r\n";
        saved(&selected, original);
        assert!(parse_summary(original).unwrap().is_none());
        let clock = clock(10);
        let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
        let patch = SummaryPatch {
            now: Some("started".into()),
            ..Default::default()
        };
        selected
            .update_summary(patch, 0, &op("a"), budget, &CancelToken::new(), &clock)
            .unwrap();
        let before = selected.read().unwrap().unwrap().markdown;
        assert!(before.contains("# existing comment\r\n\r\nprovenant_summary:"));
        assert!(before.ends_with("---\r\nBody\r\n"));
        let revisions = selected.revisions().unwrap();
        let discovery = selected.discovery().unwrap().unwrap();
        let patch = SummaryPatch {
            now: Some("x".repeat(8192)),
            ..Default::default()
        };
        assert!(matches!(
            selected.update_summary(patch, 1, &op("b"), budget, &CancelToken::new(), &clock),
            Err(SessionError::SummaryTooLarge)
        ));
        assert_eq!(selected.read().unwrap().unwrap().markdown, before);
        assert_eq!(selected.revisions().unwrap(), revisions);
        assert_eq!(selected.discovery().unwrap().unwrap(), discovery);
    }
    const CHAIR_TEXT: &str = "---\nprovenant_summary:\n  schema_version: 1\n  revision: 1\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"First\"\n  next: \"Next\"\n  needs_you: []\n  risk: \"None\"\n---\nBody\n";
    fn chair_store(root: &StateRoot, project: ProjectId) -> SessionStore {
        use provenant_core::{ActorId, HostId};
        store(root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([4; 16]))
    }
    fn change(text: &str) -> SummaryPatch {
        SummaryPatch {
            now: Some(text.into()),
            ..Default::default()
        }
    }
    fn update_at(
        store: &SessionStore,
        patch: SummaryPatch,
        expected: u64,
        id: &str,
        micros: i64,
    ) -> Result<SummaryOutcome, SessionError> {
        let clock = clock(micros);
        store.update_summary(
            patch,
            expected,
            &op(id),
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &clock,
        )
    }

    #[test]
    fn summary_operation_replays_and_refuses_a_different_request() {
        let (_temp, root, project) = setup();
        let chair = chair_store(&root, project);
        saved(&chair, CHAIR_TEXT);
        let first = update_at(&chair, change("one"), 1, "op-1", 10).unwrap();
        assert!(!first.replayed);
        let revisions = chair.revisions().unwrap();
        // A retry after a lost response replays the receipt, later and with
        // the revision already advanced.
        let again = update_at(&chair, change("one"), 1, "op-1", 99).unwrap();
        assert!(again.replayed);
        assert_eq!(again.summary, first.summary);
        assert_eq!(again.saved, first.saved);
        assert_eq!(chair.revisions().unwrap(), revisions);
        assert!(matches!(
            update_at(&chair, change("two"), 1, "op-1", 99),
            Err(SessionError::OperationConflict)
        ));
        assert!(matches!(
            update_at(&chair, change("one"), 2, "op-1", 99),
            Err(SessionError::OperationConflict)
        ));
        // A new id with the old revision is an ordinary stale write.
        assert!(matches!(
            update_at(&chair, change("one"), 1, "op-2", 99),
            Err(SessionError::SummaryStale {
                current_revision: 2
            })
        ));
        assert_eq!(
            parse_summary(&chair.read().unwrap().unwrap().markdown)
                .unwrap()
                .unwrap()
                .now,
            "one"
        );
    }

    #[test]
    fn concurrent_retries_of_one_operation_apply_once() {
        let (_temp, root, project) = setup();
        let chair = chair_store(&root, project);
        saved(&chair, CHAIR_TEXT);
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let chair = &chair;
                    scope.spawn(move || {
                        let clock = provenant_os::clock::SystemClock::new();
                        chair.update_summary(
                            change("once"),
                            1,
                            &op("same"),
                            WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                            &CancelToken::new(),
                            &clock,
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap().unwrap())
                .collect()
        });
        assert_eq!(results.iter().filter(|r| !r.replayed).count(), 1);
        assert!(results.iter().all(|r| r.summary.revision == 2));
        assert_eq!(chair.revisions().unwrap().len(), 2);
    }

    /// Stop a summary update after `stage` publication steps, as a crash would.
    fn crash_after(chair: &SessionStore, stage: u8, id: &str) {
        let clock = clock(10);
        let dir = chair.directory(false).unwrap();
        let _lock = dir
            .lock(
                OsStr::new("checkpoint.lock"),
                LockMode::Exclusive,
                WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &clock,
            )
            .unwrap();
        let begun = chair
            .begin_update(
                &dir,
                change("crashed"),
                1,
                &op(id),
                WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &clock,
            )
            .unwrap();
        let summary::Begun::Pending(pending) = begun else {
            panic!("fresh operation");
        };
        let plan = &pending.plan;
        match stage {
            0 => {}
            1 => publish(
                &dir,
                &revision_file(plan.prev.as_ref().unwrap().0),
                &plan.prev.as_ref().unwrap().1,
            )
            .unwrap(),
            2 => {
                let (n, bytes) = plan.prev.as_ref().unwrap();
                publish(&dir, &revision_file(*n), bytes).unwrap();
                publish(&dir, "checkpoint.md", &plan.text).unwrap();
            }
            3 => {
                chair.execute_plan(&dir, plan, true, |_, _| Ok(())).unwrap();
            }
            _ => {
                // Everything done and the receipt written; only the journal
                // removal is lost.
                let journal = std::fs::read(
                    path(&root_of(chair), chair.project, "chair").join("summary-pending.json"),
                )
                .unwrap();
                chair.finish_pending(&dir, pending).unwrap();
                publish(&dir, "summary-pending.json", &journal).unwrap();
            }
        }
    }
    fn root_of(chair: &SessionStore) -> StateRoot {
        chair.root.clone()
    }

    #[test]
    fn summary_crash_recovery_replays_the_journaled_plan_at_every_step() {
        let (_c, control_root, control_project) = setup();
        let control = chair_store(&control_root, control_project);
        saved(&control, CHAIR_TEXT);
        let expected = update_at(&control, change("crashed"), 1, "op-1", 10).unwrap();
        for stage in 0..=4u8 {
            let (_t, root, project) = setup();
            let chair = chair_store(&root, project);
            saved(&chair, CHAIR_TEXT);
            crash_after(&chair, stage, "op-1");
            // The retry runs later; the replayed plan keeps the original time,
            // revision numbers and history.
            let retry = update_at(&chair, change("crashed"), 1, "op-1", 99).unwrap();
            assert!(retry.replayed || stage < 4, "stage {stage}");
            assert_eq!(retry.summary, expected.summary, "stage {stage}");
            assert_eq!(
                retry.saved.revision, expected.saved.revision,
                "stage {stage}"
            );
            assert_eq!(
                chair.revisions().unwrap(),
                control.revisions().unwrap(),
                "stage {stage}"
            );
            assert_eq!(
                chair.read().unwrap().unwrap().markdown,
                control.read().unwrap().unwrap().markdown,
                "stage {stage}"
            );
            for number in control.revisions().unwrap().iter().map(|r| r.revision) {
                assert_eq!(
                    chair.read_revision(number).unwrap().map(|r| r.markdown),
                    control.read_revision(number).unwrap().map(|r| r.markdown),
                    "stage {stage}"
                );
            }
            let dir = chair.directory(false).unwrap();
            assert!(
                dir.read_bounded(OsStr::new("summary-pending.json"), 1024)
                    .is_err()
            );
        }
    }

    #[test]
    fn another_writer_completes_a_pending_summary_before_saving() {
        for stage in 0..=3u8 {
            let (_t, root, project) = setup();
            let chair = chair_store(&root, project);
            saved(&chair, CHAIR_TEXT);
            crash_after(&chair, stage, "op-1");
            let next = saved(&chair, "---\n---\nlater\n");
            assert_eq!(next.revision, 3, "stage {stage}");
            let numbers: Vec<_> = chair
                .revisions()
                .unwrap()
                .iter()
                .map(|r| r.revision)
                .collect();
            assert_eq!(numbers, [1, 2, 3], "stage {stage}");
            assert!(
                chair
                    .read_revision(2)
                    .unwrap()
                    .unwrap()
                    .markdown
                    .contains("crashed")
            );
            // The crashed update is not applied twice and still replays.
            let retry = update_at(&chair, change("crashed"), 1, "op-1", 99).unwrap();
            assert!(retry.replayed);
            assert_eq!(retry.summary.revision, 2);
        }
    }
    #[test]
    fn recovery_reads_a_journal_inflated_by_escaping() {
        let (_t, root, project) = setup();
        let chair = chair_store(&root, project);
        // 200 KB of control characters escape to about 1.2 MB each in the
        // previous and the new text of the journal.
        let heavy = format!("{CHAIR_TEXT}{}", "\u{1}".repeat(200_000));
        saved(&chair, &heavy);
        crash_after(&chair, 0, "op-1");
        let dir = chair.directory(false).unwrap();
        let size = dir
            .read_bounded(OsStr::new("summary-pending.json"), 8 << 20)
            .unwrap()
            .len();
        assert!(size > 1 << 20, "{size}");
        let retry = update_at(&chair, change("crashed"), 1, "op-1", 99).unwrap();
        assert_eq!(retry.summary.revision, 2);
        saved(&chair, "---\n---\nafter\n");
    }

    #[test]
    fn recovery_publishes_the_journaled_discovery_not_the_recoverers() {
        use provenant_core::{ActorId, HostId};
        let (_t, root, project) = setup();
        saved(&store(&root, project, "chair"), CHAIR_TEXT);
        let a = chair_store(&root, project);
        crash_after(&a, 0, "op-1");
        let b = store(&root, project, "chair")
            .with_chair(HostId::from_bytes([3; 16]), ActorId::from_bytes([5; 16]));
        let clock = clock(20);
        let result = b.save_revision(
            b"other",
            WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            &CancelToken::new(),
            &clock,
        );
        assert!(matches!(result, Err(SessionError::DiscoveryIdentity)));
        let record = a.discovery().unwrap().unwrap().record;
        assert_eq!(record.chair_actor, ActorId::from_bytes([4; 16]));
        assert_eq!(record.checkpoint.revision, 2);
    }

    #[test]
    fn enforces_utf8_and_byte_bounds_on_read_and_write() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        let exact = "é".repeat(CHECKPOINT_LIMIT as usize / 2);
        save(&selected, exact.as_bytes()).unwrap();
        assert_eq!(selected.read().unwrap().unwrap().markdown, exact);
        assert!(matches!(
            save(&selected, &vec![b'x'; CHECKPOINT_LIMIT as usize + 1]),
            Err(SessionError::TooLarge)
        ));
        assert!(matches!(
            save(&selected, &[0xff]),
            Err(SessionError::Utf8(_))
        ));
        let file = path(&root, project, "host-1").join("checkpoint.md");
        std::fs::write(&file, [0xff]).unwrap();
        assert!(matches!(selected.read(), Err(SessionError::Utf8(_))));
        std::fs::write(file, vec![b'x'; CHECKPOINT_LIMIT as usize + 1]).unwrap();
        assert!(matches!(
            selected.read(),
            Err(SessionError::Filesystem(FsError::TooLarge { .. }))
        ));
    }
    fn saved(store: &SessionStore, text: &str) -> Saved {
        let clock = clock(5);
        store
            .save_revision(
                text.as_bytes(),
                WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &clock,
            )
            .unwrap()
    }
    fn numbers(store: &SessionStore) -> Vec<u64> {
        store
            .revisions()
            .unwrap()
            .iter()
            .map(|r| r.revision)
            .collect()
    }
    #[test]
    fn each_save_keeps_the_replaced_checkpoint_as_a_linked_hashed_revision() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        assert_eq!(saved(&selected, "first\n").revision, 1);
        let second = saved(&selected, "second\n");
        assert_eq!(
            (second.revision, second.parent, second.retained),
            (2, Some(1), 2)
        );
        assert_eq!(saved(&selected, "third\n").parent, Some(2));
        let chain = selected.revisions().unwrap();
        assert_eq!(
            chain
                .iter()
                .map(|r| (r.revision, r.parent))
                .collect::<Vec<_>>(),
            [(1, None), (2, Some(1)), (3, Some(2))]
        );
        assert_eq!(
            chain[0].sha256,
            provenant_records::CanonicalV1::file_digest(b"first\n")
        );
        assert_eq!(
            selected.read_revision(1).unwrap().unwrap().markdown,
            "first\n"
        );
        assert_eq!(
            selected.read_revision(2).unwrap().unwrap().markdown,
            "second\n"
        );
        assert_eq!(
            selected.read_revision(3).unwrap().unwrap().markdown,
            "third\n"
        );
        assert_eq!(selected.read().unwrap().unwrap().markdown, "third\n");
        assert!(selected.read_revision(9).unwrap().is_none());
        let diff = selected.diff(1).unwrap().unwrap();
        assert_eq!((diff.from.revision, diff.to.revision), (1, 3));
        assert_eq!(
            diff.text,
            "--- revision 1\n+++ revision 3 (current)\n@@ -1,1 +1,1 @@\n-first\n+third\n"
        );
        assert_eq!(selected.diff(3).unwrap().unwrap().text, "");
    }

    #[test]
    fn saving_the_same_text_adds_no_revision_but_refreshes_the_checkpoint() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        saved(&selected, "same\n");
        let again = saved(&selected, "same\n");
        assert!(again.unchanged);
        assert_eq!((again.revision, again.retained), (1, 1));
        assert_eq!(numbers(&selected), [1]);
    }

    #[test]
    fn only_the_newest_revisions_are_kept_and_older_files_are_deleted() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1").with_revisions(3);
        for n in 1..=6 {
            saved(&selected, &format!("save {n}\n"));
        }
        assert_eq!(numbers(&selected), [4, 5, 6]);
        assert!(selected.read_revision(3).unwrap().is_none());
        assert_eq!(
            selected.read_revision(4).unwrap().unwrap().markdown,
            "save 4\n"
        );
        let mut files: Vec<String> = std::fs::read_dir(path(&root, project, "host-1"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("rev-"))
            .collect();
        files.sort();
        // The current checkpoint lives in checkpoint.md, not a revision file.
        assert_eq!(files, ["rev-4.md", "rev-5.md"]);
        // One revision keeps no history at all.
        let single = store(&root, project, "host-2").with_revisions(1);
        saved(&single, "a\n");
        saved(&single, "b\n");
        assert_eq!(numbers(&single), [2]);
        assert!(single.diff(1).unwrap().is_none());
        const { assert!(REVISIONS_DEFAULT <= REVISIONS_MAX) };
        assert_eq!(store(&root, project, "x").with_revisions(0).keep, 1);
        assert_eq!(
            store(&root, project, "x").with_revisions(999).keep,
            REVISIONS_MAX
        );
    }

    #[test]
    fn an_existing_checkpoint_or_an_interrupted_save_is_recorded_not_lost() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        // A checkpoint saved before revisions existed has no history.
        let dir = path(&root, project, "host-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("checkpoint.md"), "legacy\n").unwrap();
        assert_eq!(numbers(&selected), [1]);
        let first = saved(&selected, "next\n");
        assert_eq!((first.revision, first.parent), (2, Some(1)));
        assert_eq!(
            selected.read_revision(1).unwrap().unwrap().markdown,
            "legacy\n"
        );
        // A save that stored the old text and published the checkpoint, but not
        // the history, leaves the new text on disk; the next save records it.
        std::fs::write(dir.join("rev-2.md"), "next\n").unwrap();
        std::fs::write(dir.join("checkpoint.md"), "interrupted\n").unwrap();
        assert_eq!(numbers(&selected), [1, 2, 3]);
        let last = saved(&selected, "after\n");
        assert_eq!((last.revision, last.parent), (4, Some(3)));
        assert_eq!(
            selected.read_revision(3).unwrap().unwrap().markdown,
            "interrupted\n"
        );
        assert_eq!(
            selected.read_revision(2).unwrap().unwrap().markdown,
            "next\n"
        );
        // A hand edit keeps no copy of what it replaced, and says so.
        std::fs::write(dir.join("checkpoint.md"), "edited\n").unwrap();
        saved(&selected, "again\n");
        assert_eq!(numbers(&selected), [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            selected.read_revision(5).unwrap().unwrap().markdown,
            "edited\n"
        );
        assert!(selected.read_revision(4).unwrap().is_none());
    }

    #[test]
    fn unreadable_history_never_blocks_a_save_and_a_changed_revision_is_refused() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        saved(&selected, "one\n");
        saved(&selected, "two\n");
        let dir = path(&root, project, "host-1");
        // A revision altered after the fact fails its recorded hash.
        std::fs::write(dir.join("rev-1.md"), "tampered\n").unwrap();
        assert!(matches!(
            selected.read_revision(1),
            Err(SessionError::History)
        ));
        // History that cannot be read is reported, yet the next save succeeds,
        // keeps the checkpoint it finds and numbers past every revision file.
        std::fs::write(dir.join("revisions.json"), "{not json").unwrap();
        assert!(matches!(selected.revisions(), Err(SessionError::History)));
        let recovered = saved(&selected, "three\n");
        assert_eq!((recovered.revision, recovered.parent), (3, Some(2)));
        assert_eq!(numbers(&selected), [2, 3]);
        assert_eq!(
            selected.read_revision(2).unwrap().unwrap().markdown,
            "two\n"
        );
        assert!(!dir.join("rev-1.md").exists(), "an unnamed file is removed");
    }

    #[test]
    fn every_revision_is_held_to_the_checkpoint_size_bound() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        let exact = "x".repeat(CHECKPOINT_LIMIT as usize);
        saved(&selected, &exact);
        saved(&selected, "small\n");
        assert_eq!(
            selected.read_revision(1).unwrap().unwrap().markdown.len() as u64,
            CHECKPOINT_LIMIT
        );
        let clock = clock(0);
        let over = vec![b'x'; CHECKPOINT_LIMIT as usize + 1];
        assert!(matches!(
            selected.save_revision(
                &over,
                WaitBudget::cli(clock.mono_now(), 1).unwrap(),
                &CancelToken::new(),
                &clock
            ),
            Err(SessionError::TooLarge)
        ));
        assert_eq!(numbers(&selected), [1, 2]);
    }

    #[test]
    fn autosave_has_the_same_bounds_and_lock_but_keeps_the_chair_file_separate() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        let clock = clock(0);
        let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
        let write = |text: &[u8], cancel: &CancelToken, clock: &dyn Clock| {
            selected.write_autosave(text, budget, cancel, clock)
        };
        let cancel = CancelToken::new();
        let big = vec![b'x'; CHECKPOINT_LIMIT as usize + 1];
        assert!(selected.read_autosave().unwrap().is_none());
        assert!(matches!(
            write(&big, &cancel, &clock),
            Err(SessionError::TooLarge)
        ));
        assert!(matches!(
            write(&[0xff], &cancel, &clock),
            Err(SessionError::Utf8(_))
        ));
        assert!(!path(&root, project, "host-1").exists());

        let exact = "é".repeat(CHECKPOINT_LIMIT as usize / 2);
        write(exact.as_bytes(), &cancel, &clock).unwrap();
        assert_eq!(selected.read_autosave().unwrap().unwrap().markdown, exact);
        assert!(selected.read().unwrap().is_none());
        save(&selected, b"chair notes").unwrap();
        write(b"replacement", &cancel, &clock).unwrap();
        let saved = selected.read_autosave().unwrap().unwrap();
        assert_eq!(saved.markdown, "replacement");

        cancel.cancel();
        assert!(matches!(
            write(b"cancelled", &cancel, &clock),
            Err(SessionError::Filesystem(FsError::Cancelled { .. }))
        ));
        let expired = TestClock::new(
            UtcMicros::from_micros(0).unwrap(),
            MonoTime::from_micros(1_000_000),
        );
        selected
            .update(
                true,
                |_| {
                    assert!(matches!(
                        write(b"contended", &CancelToken::new(), &expired),
                        Err(SessionError::Filesystem(FsError::LockTimeout { .. }))
                    ));
                    Ok(None)
                },
                budget,
                &CancelToken::new(),
                &clock,
            )
            .unwrap();
        assert_eq!(selected.read_autosave().unwrap().unwrap(), saved);
        assert_eq!(selected.read().unwrap().unwrap().markdown, "chair notes");

        let file = path(&root, project, "host-1").join(AUTOSAVE_FILE);
        std::fs::write(&file, [0xff]).unwrap();
        assert!(matches!(
            selected.read_autosave(),
            Err(SessionError::Utf8(_))
        ));
        std::fs::write(file, big).unwrap();
        assert!(matches!(
            selected.read_autosave(),
            Err(SessionError::Filesystem(FsError::TooLarge { .. }))
        ));
    }
    #[test]
    fn refuses_traversal_and_command_injection_in_host_keys() {
        for id in [
            "",
            ".",
            "..",
            "a..b",
            "../x",
            "a/b",
            "a\\b",
            "a\0b",
            "a b",
            "x;id",
            "x'",
            "-flag",
            "é",
            "ABC",
            "7C1E4B2A-93D5-4F0E-B6A8-2D4C9E1F5A37",
        ] {
            assert!(HostSessionId::parse(id).is_err(), "{id:?}");
            assert!(
                serde_json::from_str::<HostSessionId>(&serde_json::to_string(id).unwrap()).is_err()
            );
        }
        assert!(HostSessionId::parse(&"a".repeat(HOST_SESSION_ID_LIMIT)).is_ok());
        assert!(HostSessionId::parse(&"a".repeat(HOST_SESSION_ID_LIMIT + 1)).is_err());
    }
    #[test]
    fn refuses_symlinked_directories_checkpoints_and_locks() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        save(&selected, b"original").unwrap();
        let directory = path(&root, project, "host-1");
        let external = root.as_path().join("outside.md");
        std::fs::write(&external, "private").unwrap();
        std::fs::remove_file(directory.join("checkpoint.md")).unwrap();
        symlink(&external, directory.join("checkpoint.md")).unwrap();
        assert!(selected.read().is_err());
        // Replacing the link must not follow it or modify its target.
        save(&selected, b"replacement").unwrap();
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "private");
        std::fs::remove_file(directory.join("checkpoint.lock")).unwrap();
        symlink(&external, directory.join("checkpoint.lock")).unwrap();
        assert!(save(&selected, b"denied").is_err());
        symlink(&directory, path(&root, project, "alias")).unwrap();
        assert!(save(&store(&root, project, "alias"), b"denied").is_err());
    }
    #[test]
    fn cancelled_or_contended_save_keeps_previous_complete_bytes() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        save(&selected, b"original").unwrap();
        let clock = clock(0);
        let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            selected.save(b"cancelled", budget, &cancel, &clock),
            Err(SessionError::Filesystem(FsError::Cancelled { .. }))
        ));
        let dir = Directory::open_anchor(&path(&root, project, "host-1")).unwrap();
        let _lock = dir
            .lock(
                OsStr::new("checkpoint.lock"),
                LockMode::Exclusive,
                budget,
                &CancelToken::new(),
                &clock,
            )
            .unwrap();
        let expired_clock = TestClock::new(
            UtcMicros::from_micros(0).unwrap(),
            MonoTime::from_micros(1_000_000),
        );
        assert!(matches!(
            selected.save(b"contended", budget, &CancelToken::new(), &expired_clock),
            Err(SessionError::Filesystem(FsError::LockTimeout { .. }))
        ));
        assert_eq!(selected.read().unwrap().unwrap().markdown, "original");
        // An unrelated key is not held by the first key's lock.
        save(&store(&root, project, "other"), b"independent").unwrap();
    }
    #[test]
    fn update_edits_under_the_checkpoint_lock_and_creates_only_when_asked() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        let clock = clock(0);
        let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
        let cancel = CancelToken::new();
        // Without `create`, a missing checkpoint is neither created nor edited.
        let wrote = selected
            .update(
                false,
                |_| panic!("no checkpoint to edit"),
                budget,
                &cancel,
                &clock,
            )
            .unwrap();
        assert!(wrote.is_none() && !path(&root, project, "host-1").exists());
        let wrote = selected
            .update(
                true,
                |current| {
                    assert_eq!(current, None);
                    Ok(Some("first".into()))
                },
                budget,
                &cancel,
                &clock,
            )
            .unwrap();
        assert!(wrote.is_some());
        // A concurrent save cannot interleave: the lock is held while editing.
        let expired = TestClock::new(
            UtcMicros::from_micros(0).unwrap(),
            MonoTime::from_micros(1_000_000),
        );
        selected
            .update(
                true,
                |current| {
                    assert_eq!(current, Some("first"));
                    assert!(matches!(
                        selected.save(b"racing", budget, &CancelToken::new(), &expired),
                        Err(SessionError::Filesystem(FsError::LockTimeout { .. }))
                    ));
                    Ok(Some("second".into()))
                },
                budget,
                &cancel,
                &clock,
            )
            .unwrap();
        assert_eq!(selected.read().unwrap().unwrap().markdown, "second");
        // Declining, failing and oversize edits keep the previous bytes.
        assert!(
            selected
                .update(true, |_| Ok(None), budget, &cancel, &clock)
                .unwrap()
                .is_none()
        );
        assert!(
            selected
                .update(
                    true,
                    |_| Err(SessionError::Deadline),
                    budget,
                    &cancel,
                    &clock
                )
                .is_err()
        );
        let big = "x".repeat(CHECKPOINT_LIMIT as usize + 1);
        assert!(matches!(
            selected.update(true, |_| Ok(Some(big)), budget, &cancel, &clock),
            Err(SessionError::TooLarge)
        ));
        assert_eq!(selected.read().unwrap().unwrap().markdown, "second");
    }
    #[test]
    fn deadline_expiring_after_lock_keeps_previous_complete_bytes() {
        struct ExpiringAfterLock(std::cell::Cell<u32>);
        impl Clock for ExpiringAfterLock {
            fn wall_now(&self) -> Result<UtcMicros, provenant_core::TimeError> {
                UtcMicros::from_micros(0)
            }
            fn mono_now(&self) -> MonoTime {
                let call = self.0.get();
                self.0.set(call + 1);
                // Call 0 is the uncontended lock poll; call 1 is the post-lock check.
                MonoTime::from_micros(if call < 1 { 0 } else { 1_000_000 })
            }
            fn wait_until(&self, _deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
                Ok(())
            }
        }
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        save(&selected, b"original").unwrap();
        let budget = WaitBudget::cli(MonoTime::from_micros(0), 1).unwrap();
        let clock = ExpiringAfterLock(std::cell::Cell::new(0));
        assert!(matches!(
            selected.save(b"late", budget, &CancelToken::new(), &clock),
            Err(SessionError::Deadline)
        ));
        assert_eq!(selected.read().unwrap().unwrap().markdown, "original");
    }
    #[test]
    fn unreadable_checkpoint_diagnostic_carries_bounded_cause() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, "host-1");
        save(&selected, b"original").unwrap();
        let directory = path(&root, project, "host-1");
        let external = root.as_path().join("outside.md");
        std::fs::write(&external, "private").unwrap();
        std::fs::remove_file(directory.join("checkpoint.md")).unwrap();
        symlink(&external, directory.join("checkpoint.md")).unwrap();
        let input = br#"{"hook_event_name":"SessionStart","session_id":"host-1"}"#;
        let response = hook_context(
            &root,
            project,
            HookEvent::SessionStart,
            &input[..],
            &clock(0),
            DurationMicros::ZERO,
        );
        assert_eq!(response.status, CheckpointStatus::Unavailable);
        assert!(response.diagnostic.contains("\nCause: filesystem "));
        assert!(!response.diagnostic.contains(external.to_str().unwrap()));
    }
    const FIXTURES: [(&str, HookEvent); 4] = [
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/hooks/claude-SessionStart.json"
            )),
            HookEvent::SessionStart,
        ),
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/hooks/claude-PreCompact.json"
            )),
            HookEvent::PreCompact,
        ),
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/hooks/codex-SessionStart.json"
            )),
            HookEvent::SessionStart,
        ),
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/hooks/codex-PreCompact.json"
            )),
            HookEvent::PreCompact,
        ),
    ];
    #[test]
    fn native_host_fixtures_select_only_their_key_and_emit_native_json() {
        let (_temp, root, project) = setup();
        save(
            &store(&root, project, "latest-unrelated"),
            b"never inject this",
        )
        .unwrap();
        for (fixture, event) in FIXTURES {
            let input: serde_json::Value = serde_json::from_str(fixture).unwrap();
            let id = input["session_id"].as_str().unwrap();
            let selected = store(&root, project, id);
            let missing = hook_context(
                &root,
                project,
                event,
                fixture.as_bytes(),
                &clock(0),
                DurationMicros::ZERO,
            );
            // Some native fixtures share a key already populated by a prior row.
            if selected.read().unwrap().is_none() {
                assert_eq!(missing.status, CheckpointStatus::Missing);
            }
            assert!(
                !String::from_utf8(missing.stdout)
                    .unwrap()
                    .contains("never inject this")
            );
            save(&selected, b"# Goal\nSpecific authorised checkpoint.").unwrap();
            let mtime = selected.read().unwrap().unwrap().modified_at.as_micros();
            let response = hook_context(
                &root,
                project,
                event,
                fixture.as_bytes(),
                &clock(mtime),
                DurationMicros::ZERO,
            );
            assert_eq!(response.status, CheckpointStatus::Present);
            assert!(!response.needs_saving);
            let wire: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
            let text = match event {
                HookEvent::SessionStart => {
                    assert_eq!(wire["hookSpecificOutput"]["hookEventName"], "SessionStart");
                    let text = wire["hookSpecificOutput"]["additionalContext"]
                        .as_str()
                        .unwrap();
                    assert!(text.contains("Specific authorised checkpoint"));
                    text
                }
                HookEvent::PreCompact => {
                    assert!(wire.get("hookSpecificOutput").is_none());
                    assert!(wire.get("decision").is_none());
                    let text = wire["systemMessage"].as_str().unwrap();
                    assert!(!text.contains("Specific authorised checkpoint"));
                    text
                }
            };
            assert!(text.contains(&format!(
                "provenant workflow session save {id} --file <checkpoint.md>"
            )));
            for now in [mtime - 1, mtime + 1] {
                let stale = hook_context(
                    &root,
                    project,
                    event,
                    fixture.as_bytes(),
                    &clock(now),
                    DurationMicros::ZERO,
                );
                assert_eq!(stale.status, CheckpointStatus::Stale);
                assert!(stale.needs_saving);
                assert!(
                    !String::from_utf8(stale.stdout)
                        .unwrap()
                        .contains("Specific authorised checkpoint")
                );
            }
        }
    }
    #[test]
    fn hook_compacts_merged_prs_and_succeeded_lanes() {
        let (_temp, root, project) = setup();
        let state = SessionState {
            settled: (0..20)
                .map(|n| LaneLine {
                    id: format!("lane-{n}"),
                    state: "succeeded".into(),
                    ..Default::default()
                })
                .collect(),
            prs: (1..=10)
                .map(|number| PrLine {
                    number,
                    state: "MERGED".into(),
                    summary: String::new(),
                })
                .collect(),
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        let response = hook_context_with(
            &root,
            project,
            HookEvent::SessionStart,
            FIXTURES[0].0.as_bytes(),
            &clock(0),
            DurationMicros::ZERO,
            Advice {
                digest: &digest,
                commands: &CommandPrefix::default(),
                client: None,
            },
        );
        let text = String::from_utf8(response.stdout).unwrap();
        assert!(text.len() < 2048, "{} bytes", text.len());
        assert!(text.contains("10 merged, 0 closed"));
        assert!(text.contains("succeeded: 20"));
        assert!(!text.contains("#1 "));
    }
    #[test]
    fn hook_output_cap_counts_json_escaping_and_keeps_utf8_and_save_command() {
        let (_temp, root, project) = setup();
        let (fixture, event) = FIXTURES[0];
        let selected = store(&root, project, "7c1e4b2a-93d5-4f0e-b6a8-2d4c9e1f5a37");
        for text in [
            "é🦀\n\"\\".repeat(2800),
            "\0".repeat(CHECKPOINT_LIMIT as usize),
        ] {
            save(&selected, text.as_bytes()).unwrap();
            let mtime = selected.read().unwrap().unwrap().modified_at.as_micros();
            let response = hook_context(
                &root,
                project,
                event,
                fixture.as_bytes(),
                &clock(mtime),
                DurationMicros::ZERO,
            );
            assert!(response.stdout.len() <= HOOK_OUTPUT_LIMIT);
            assert_eq!(response.stdout.last(), Some(&b'\n'));
            let wire: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
            let context = wire["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap();
            assert!(context.contains(&text.chars().take(80).collect::<String>()));
            assert!(context.contains("Checkpoint excerpt"));
            assert!(context.contains("session save 7c1e4b2a-93d5-4f0e-b6a8-2d4c9e1f5a37 --file"));
        }
    }
    fn hook_with(
        root: &StateRoot,
        project: ProjectId,
        commands: &CommandPrefix,
        micros: i64,
    ) -> HookContext {
        hook_context_with(
            root,
            project,
            HookEvent::SessionStart,
            FIXTURES[0].0.as_bytes(),
            &clock(micros),
            DurationMicros::ZERO,
            Advice {
                digest: "",
                commands,
                client: None,
            },
        )
    }
    fn context(response: &HookContext) -> String {
        let wire: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
        wire["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    const FIXTURE_ID: &str = "7c1e4b2a-93d5-4f0e-b6a8-2d4c9e1f5a37";

    #[test]
    fn the_save_command_carries_a_custom_state_dir_or_refuses_to_show_one() {
        use std::os::unix::ffi::OsStrExt;
        let (_temp, root, project) = setup();
        let cwd = std::path::Path::new("/work");
        let custom = CommandPrefix::new(Some(std::path::Path::new("rel/it's state")), cwd);
        let text = context(&hook_with(&root, project, &custom, 0));
        assert!(
            text.contains(&format!(
                "Save: provenant --state-dir '/work/rel/it'\\''s state' workflow session save {FIXTURE_ID} --file <checkpoint.md>"
            )),
            "{text}"
        );
        // The default store shows the plain command.
        let text = context(&hook_with(&root, project, &CommandPrefix::default(), 0));
        assert!(text.contains(&format!(
            "Save: provenant workflow session save {FIXTURE_ID} --file"
        )));
        // A directory that cannot round-trip is never shown as a command.
        let hostile = [
            std::path::PathBuf::from("/s/a\nb"),
            std::path::PathBuf::from("/s/a>b"),
            std::path::PathBuf::from("/s/a`b"),
            std::path::PathBuf::from(format!("/{}", "d".repeat(600))),
            std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/s/\xff")),
        ];
        for dir in hostile {
            let prefix = CommandPrefix::new(Some(&dir), cwd);
            let text = context(&hook_with(&root, project, &prefix, 0));
            assert!(
                !text.contains(&format!(
                    "Save: provenant workflow session save {FIXTURE_ID}"
                )),
                "{text}"
            );
            assert!(
                text.contains("default store") && text.contains("this hook's --state-dir"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_damaged_or_unreadable_autosave_never_hides_a_readable_checkpoint() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, FIXTURE_ID);
        save(
            &selected,
            b"# Goal\nChair authority: user approved #1829 only.",
        )
        .unwrap();
        let mtime = selected.read().unwrap().unwrap().modified_at.as_micros();
        let directory = path(&root, project, FIXTURE_ID);
        let autosave = directory.join("autosave.md");
        let external = root.as_path().join("outside.md");
        std::fs::write(&external, "private").unwrap();
        let check = |expect_problem: bool| {
            let response = hook_with(&root, project, &CommandPrefix::default(), mtime);
            assert_eq!(response.status, CheckpointStatus::Present);
            assert!(!response.needs_saving);
            let text = context(&response);
            assert!(text.contains("user approved #1829 only"), "{text}");
            assert_eq!(
                text.contains("\nAutosave unavailable: "),
                expect_problem,
                "{text}"
            );
            // One line, and never a path.
            let line = text
                .lines()
                .find(|line| line.starts_with("Autosave unavailable: "));
            assert_eq!(line.is_some(), expect_problem, "{text}");
            assert!(!text.contains(external.to_str().unwrap()));
            assert!(!text.contains(root.as_path().to_str().unwrap()));
        };
        // Missing: nothing to report.
        check(false);
        // Damaged: not UTF-8.
        std::fs::write(&autosave, [0xff, 0xfe]).unwrap();
        check(true);
        // Unreadable: a symlink the store refuses to follow.
        std::fs::remove_file(&autosave).unwrap();
        symlink(&external, &autosave).unwrap();
        check(true);
        std::fs::remove_file(&autosave).unwrap();
        // Valid: shown after the checkpoint, with no problem line.
        std::fs::write(&autosave, "## Provenant session state\nAuto-saved: t").unwrap();
        check(false);
        let saved_at = selected.read_autosave().unwrap().unwrap().modified_at;
        let shown = hook_context(
            &root,
            project,
            HookEvent::SessionStart,
            FIXTURES[0].0.as_bytes(),
            &clock(saved_at.as_micros()),
            DurationMicros::from_micros(3_600_000_000),
        );
        let text = context(&shown);
        assert!(text.find("Chair authority").unwrap() < text.find("Auto-saved: t").unwrap());
        // With no checkpoint at all, a damaged autosave is still just a line.
        std::fs::remove_file(directory.join("checkpoint.md")).unwrap();
        std::fs::write(&autosave, [0xff]).unwrap();
        let response = hook_with(&root, project, &CommandPrefix::default(), mtime);
        assert_eq!(response.status, CheckpointStatus::Missing);
        assert!(context(&response).contains("\nAutosave unavailable: "));
        // An unreadable checkpoint is still reported as unavailable.
        std::fs::write(directory.join("checkpoint.md"), [0xff]).unwrap();
        assert_eq!(
            hook_with(&root, project, &CommandPrefix::default(), mtime).status,
            CheckpointStatus::Unavailable
        );
    }

    #[test]
    fn the_excerpt_notice_carries_the_state_dir_and_stays_within_the_cap() {
        let (_temp, root, project) = setup();
        let selected = store(&root, project, FIXTURE_ID);
        save(&selected, "x".repeat(20_000).as_bytes()).unwrap();
        let mtime = selected.read().unwrap().unwrap().modified_at.as_micros();
        let long = std::path::PathBuf::from(format!("/{}", "s".repeat(400)));
        for (dir, shown) in [
            (std::path::PathBuf::from("/st ate"), true),
            (long, true),
            (std::path::PathBuf::from("/s/a\nb"), false),
        ] {
            let prefix = CommandPrefix::new(Some(&dir), std::path::Path::new("/work"));
            let response = hook_with(&root, project, &prefix, mtime);
            assert!(response.stdout.len() <= HOOK_OUTPUT_LIMIT);
            let text = context(&response);
            let quoted = format!("--state-dir '{}'", dir.display());
            assert_eq!(
                text.contains(&format!(
                    "read it with `provenant {quoted} workflow session show {FIXTURE_ID}`"
                )),
                shown,
                "{text}"
            );
            assert!(text.contains("Checkpoint excerpt"), "{text}");
        }
    }

    #[test]
    fn malformed_missing_wrong_event_and_oversize_hooks_are_advisory_without_fallback() {
        let (_temp, root, project) = setup();
        save(&store(&root, project, "other"), b"must not load").unwrap();
        for input in [
            b"{".as_slice(),
            b"[]",
            b"{}",
            b"{\"hook_event_name\":\"SessionStart\"}",
            b"{\"hook_event_name\":\"SessionStart\",\"session_id\":null}",
            b"{\"hook_event_name\":\"SessionStart\",\"session_id\":5}",
            b"{\"hook_event_name\":\"SessionStart\",\"session_id\":\"../other\"}",
            FIXTURES[1].0.as_bytes(),
        ] {
            let response = hook_context(
                &root,
                project,
                HookEvent::SessionStart,
                input,
                &clock(0),
                DurationMicros::ZERO,
            );
            assert_eq!(response.status, CheckpointStatus::InvalidInput);
            assert!(response.session_id.is_none());
            assert!(
                !String::from_utf8(response.stdout)
                    .unwrap()
                    .contains("must not load")
            );
        }
        struct Endless(u64);
        impl Read for Endless {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.0 += bytes.len() as u64;
                bytes.fill(b' ');
                Ok(bytes.len())
            }
        }
        let mut reader = Endless(0);
        let response = hook_context(
            &root,
            project,
            HookEvent::SessionStart,
            &mut reader,
            &clock(0),
            DurationMicros::ZERO,
        );
        assert_eq!(reader.0, HOOK_INPUT_LIMIT + 1);
        assert_eq!(response.status, CheckpointStatus::InvalidInput);
        let selected = store(&root, project, "7c1e4b2a-93d5-4f0e-b6a8-2d4c9e1f5a37");
        save(&selected, b"x").unwrap();
        std::fs::write(
            path(&root, project, "7c1e4b2a-93d5-4f0e-b6a8-2d4c9e1f5a37").join("checkpoint.md"),
            [0xff],
        )
        .unwrap();
        let response = hook_context(
            &root,
            project,
            HookEvent::SessionStart,
            FIXTURES[0].0.as_bytes(),
            &clock(0),
            DurationMicros::ZERO,
        );
        assert_eq!(response.status, CheckpointStatus::Unavailable);
    }
}
