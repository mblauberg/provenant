//! Durable per-session chair updates.
use crate::{persistence, session::HostSessionId};
use provenant_core::{CancelToken, Clock, OperationId, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use provenant_records::{CanonicalV1, Limits, decode_v1};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;

pub const ENTRY_LIMIT: usize = 16 * 1024;
pub const PAGE_LIMIT: usize = 100;
const MANIFEST_LIMIT: u64 = 32 * 1024 * 1024;
const RECEIPT_LIMIT: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Append {
    pub operation_id: OperationId,
    pub host: String,
    pub kind: String,
    pub markdown: String,
    #[serde(default)]
    pub request_ids: Vec<String>,
    #[serde(default)]
    pub attempt_ids: Vec<String>,
    #[serde(default)]
    pub issue_ids: Vec<String>,
    #[serde(default)]
    pub pr_ids: Vec<String>,
    pub channel: String,
    #[serde(default)]
    pub correction_of: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub schema_version: u64,
    pub id: String,
    pub sequence: u64,
    pub project_id: ProjectId,
    pub session_id: HostSessionId,
    /// Seconds since the Unix epoch (UTC).
    pub created_at: i64,
    #[serde(flatten)]
    pub update: Append,
}
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub max_age_seconds: u64,
    pub max_entries: u64,
    pub max_bytes: u64,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            max_age_seconds: 30 * 86400,
            max_entries: 10_000,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<LogEntry>,
    pub next_cursor: Option<u64>,
    pub total_appended: u64,
    pub total_pruned: u64,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Decode(#[from] provenant_records::DecodeError),
    #[error(transparent)]
    Canonical(#[from] provenant_records::CanonicalError),
    #[error("chair log entry {sequence} could not be read: {source}")]
    Entry { sequence: u64, source: Box<Error> },
    #[error("{0}")]
    Invalid(&'static str),
    #[error("operation ID was already used for a different update")]
    Conflict,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    operation_id: OperationId,
    digest: String,
    sequence: u64,
    created_at: i64,
    bytes: u64,
    retained: bool,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u64,
    receipts: Vec<Receipt>,
    total_pruned: u64,
}
pub struct LogStore {
    root: StateRoot,
    project: ProjectId,
    session: HostSessionId,
}
impl LogStore {
    pub fn new(root: StateRoot, project: ProjectId, session: HostSessionId) -> Self {
        Self {
            root,
            project,
            session,
        }
    }
    fn dir(&self, create: bool) -> Result<Directory, FsError> {
        persistence::directory(
            &self.root,
            self.project,
            "chair-logs",
            self.session.as_str(),
            create,
        )
    }
    fn load(dir: &Directory) -> Result<Manifest, Error> {
        match dir.read_bounded(OsStr::new("manifest.json"), MANIFEST_LIMIT) {
            Ok(bytes) => {
                let manifest: Manifest = decode_v1(
                    bytes.as_slice(),
                    Limits {
                        max_bytes: MANIFEST_LIMIT,
                        max_depth: 64,
                        max_members: 1_000_000,
                    },
                )?;
                if manifest.schema_version != 1 || manifest.receipts.len() > RECEIPT_LIMIT {
                    return Err(Error::Invalid("invalid chair log manifest"));
                }
                Ok(manifest)
            }
            Err(e) if e.is_not_found() => Ok(Manifest {
                schema_version: 1,
                ..Manifest::default()
            }),
            Err(e) => Err(e.into()),
        }
    }
    fn publish(dir: &Directory, name: &str, bytes: &[u8]) -> Result<(), Error> {
        dir.atomic_write(OsStr::new(name), ".log-", bytes, PublishMode::Replace)?
            .require_durable()?;
        Ok(())
    }
    fn filename(sequence: u64) -> String {
        format!("entry-{sequence:020}.json")
    }
    fn read(dir: &Directory, sequence: u64) -> Result<LogEntry, Error> {
        let read = || -> Result<LogEntry, Error> {
            Ok(serde_json::from_slice(&dir.read_bounded(
                OsStr::new(&Self::filename(sequence)),
                (ENTRY_LIMIT * 2) as u64,
            )?)?)
        };
        read().map_err(|source| Error::Entry {
            sequence,
            source: Box::new(source),
        })
    }
    fn lock(dir: &Directory) -> Result<provenant_os::fs::LockGuard, Error> {
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)
            .map_err(|_| Error::Invalid("invalid lock budget"))?;
        Ok(dir.lock(
            OsStr::new("log.lock"),
            LockMode::Exclusive,
            budget,
            &CancelToken::new(),
            &clock,
        )?)
    }
    fn read_lock(dir: &Directory) -> Result<Option<provenant_os::fs::LockGuard>, Error> {
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)
            .map_err(|_| Error::Invalid("invalid lock budget"))?;
        match dir.lock_existing(
            OsStr::new("log.lock"),
            LockMode::Shared,
            budget,
            &CancelToken::new(),
            &clock,
        ) {
            Ok(lock) => Ok(Some(lock)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn expired(receipt: &Receipt, retention: Retention, now: i64) -> bool {
        now.saturating_sub(receipt.created_at)
            > retention.max_age_seconds.min(i64::MAX as u64) as i64
    }
    /// Apply age retention, returning the number of payloads that would be pruned.
    pub fn gc(&self, retention: Retention, now: i64, dry_run: bool) -> Result<u64, Error> {
        let dir = match self.dir(false) {
            Ok(dir) => dir,
            Err(e) if e.is_not_found() => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let _lock = if dry_run {
            Self::read_lock(&dir)?
        } else {
            Some(Self::lock(&dir)?)
        };
        let mut manifest = Self::load(&dir)?;
        let mut count = 0;
        for receipt in &mut manifest.receipts {
            if receipt.retained && Self::expired(receipt, retention, now) {
                receipt.retained = false;
                count += 1;
            }
        }
        if !dry_run {
            manifest.total_pruned += count;
            if count > 0 {
                Self::publish(&dir, "manifest.json", &serde_json::to_vec(&manifest)?)?;
            }
            Self::cleanup(&dir, &manifest)?;
            dir.sync()?;
        }
        Ok(count)
    }
    fn cleanup(dir: &Directory, manifest: &Manifest) -> Result<(), Error> {
        for old in manifest.receipts.iter().filter(|r| !r.retained) {
            match dir.unlink(OsStr::new(&Self::filename(old.sequence))) {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    pub fn append(&self, input: Append, retention: Retention, now: i64) -> Result<LogEntry, Error> {
        let encoded = serde_json::to_vec(&input)?;
        if encoded.len() > ENTRY_LIMIT
            || input.markdown.trim().is_empty()
            || input.host.is_empty()
            || input.kind.is_empty()
            || input.channel.is_empty()
        {
            return Err(Error::Invalid(
                "chair log update must contain host, kind, channel and markdown, and fit in 16 KiB",
            ));
        }
        if retention.max_entries == 0
            || retention.max_bytes < (ENTRY_LIMIT * 2) as u64
            || retention.max_age_seconds == 0
        {
            return Err(Error::Invalid("invalid chair log retention"));
        }
        let dir = self.dir(true)?;
        let _lock = Self::lock(&dir)?;
        let mut manifest = Self::load(&dir)?;
        Self::cleanup(&dir, &manifest)?;
        #[derive(Serialize)]
        struct Payload<'a> {
            kind: &'a str,
            markdown: &'a str,
            request_ids: &'a [String],
            attempt_ids: &'a [String],
            issue_ids: &'a [String],
            pr_ids: &'a [String],
            channel: &'a str,
            correction_of: Option<u64>,
        }
        let digest = CanonicalV1::record_digest(
            "chair-log-append",
            1,
            &Payload {
                kind: &input.kind,
                markdown: &input.markdown,
                request_ids: &input.request_ids,
                attempt_ids: &input.attempt_ids,
                issue_ids: &input.issue_ids,
                pr_ids: &input.pr_ids,
                channel: &input.channel,
                correction_of: input.correction_of,
            },
        )?
        .to_string();
        if let Some(old) = manifest
            .receipts
            .iter()
            .find(|r| r.operation_id == input.operation_id)
        {
            if old.digest != digest {
                return Err(Error::Conflict);
            }
            return if old.retained {
                Self::read(&dir, old.sequence)
            } else {
                // The caller supplied the original payload, proven by its digest.
                // Reconstruct the response without retaining pruned content.
                Ok(LogEntry {
                    schema_version: 1,
                    id: format!("{}-{}", self.session.as_str(), old.sequence),
                    sequence: old.sequence,
                    project_id: self.project,
                    session_id: self.session.clone(),
                    created_at: old.created_at,
                    update: input,
                })
            };
        }
        if manifest.receipts.len() >= RECEIPT_LIMIT {
            return Err(Error::Invalid("chair log operation receipt limit reached"));
        }
        let sequence = manifest.receipts.len() as u64 + 1;
        if input
            .correction_of
            .is_some_and(|id| id == 0 || id >= sequence)
        {
            return Err(Error::Invalid("correction must refer to an earlier entry"));
        }
        let entry = LogEntry {
            schema_version: 1,
            id: format!("{}-{sequence}", self.session.as_str()),
            sequence,
            project_id: self.project,
            session_id: self.session.clone(),
            created_at: now,
            update: input,
        };
        let payload = serde_json::to_vec(&entry)?;
        Self::publish(&dir, &Self::filename(sequence), &payload)?;
        manifest.receipts.push(Receipt {
            operation_id: entry.update.operation_id.clone(),
            digest,
            sequence,
            created_at: now,
            bytes: payload.len() as u64,
            retained: true,
        });
        let mut count = manifest.receipts.iter().filter(|r| r.retained).count() as u64;
        let mut bytes: u64 = manifest
            .receipts
            .iter()
            .filter(|r| r.retained)
            .map(|r| r.bytes)
            .sum();
        for receipt in &mut manifest.receipts {
            if receipt.retained
                && (now.saturating_sub(receipt.created_at)
                    > retention.max_age_seconds.min(i64::MAX as u64) as i64
                    || count > retention.max_entries
                    || bytes > retention.max_bytes)
            {
                receipt.retained = false;
                count -= 1;
                bytes -= receipt.bytes;
                manifest.total_pruned += 1;
            }
        }
        let encoded_manifest = serde_json::to_vec(&manifest)?;
        if encoded_manifest.len() as u64 > MANIFEST_LIMIT {
            return Err(Error::Invalid("chair log receipt storage limit reached"));
        }
        Self::publish(&dir, "manifest.json", &encoded_manifest)?;
        Self::cleanup(&dir, &manifest)?;
        dir.sync()?;
        Ok(entry)
    }
    pub fn page(
        &self,
        after: Option<u64>,
        limit: usize,
        retention: Retention,
        now: i64,
    ) -> Result<Page, Error> {
        if limit == 0 || limit > PAGE_LIMIT {
            return Err(Error::Invalid("chair log page limit must be 1-100"));
        }
        let dir = match self.dir(false) {
            Ok(dir) => dir,
            Err(e) if e.is_not_found() => {
                return Ok(Page {
                    entries: Vec::new(),
                    next_cursor: None,
                    total_appended: 0,
                    total_pruned: 0,
                });
            }
            Err(e) => return Err(e.into()),
        };
        let _lock = Self::read_lock(&dir)?;
        let manifest = Self::load(&dir)?;
        let mut selected = manifest
            .receipts
            .iter()
            .filter(|r| {
                r.retained && !Self::expired(r, retention, now) && r.sequence > after.unwrap_or(0)
            })
            .peekable();
        let mut entries = Vec::new();
        let mut bytes = 0;
        while let Some(receipt) = selected.peek() {
            if entries.len() == limit || bytes + receipt.bytes > 64 * 1024 {
                break;
            }
            bytes += receipt.bytes;
            entries.push(Self::read(&dir, receipt.sequence)?);
            selected.next();
        }
        let next_cursor = if selected.peek().is_some() {
            entries.last().map(|e| e.sequence)
        } else {
            None
        };
        Ok(Page {
            entries,
            next_cursor,
            total_appended: manifest.receipts.len() as u64,
            total_pruned: manifest.total_pruned
                + manifest
                    .receipts
                    .iter()
                    .filter(|r| r.retained && Self::expired(r, retention, now))
                    .count() as u64,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use provenant_core::{PathResolver, RootInputs};
    use std::path::{Path, PathBuf};
    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            Ok(path.into())
        }
    }
    fn update(op: &str, text: &str) -> Append {
        Append {
            operation_id: OperationId::parse(op).unwrap(),
            host: "host".into(),
            kind: "update".into(),
            markdown: text.into(),
            request_ids: vec![],
            attempt_ids: vec![],
            issue_ids: vec![],
            pr_ids: vec![],
            channel: "chat".into(),
            correction_of: None,
        }
    }
    #[test]
    fn log_operation_replay_pagination_correction_pruning_and_restart() {
        let tmp = crate::test_support::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let state = base.join("state");
        std::fs::create_dir(&state).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(state);
        let root = StateRoot::resolve(&inputs, &Resolver).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let session = HostSessionId::parse("chair").unwrap();
        let store = LogStore::new(root.clone(), project, session.clone());
        let retention = Retention {
            max_age_seconds: 100,
            max_entries: 2,
            max_bytes: 65536,
        };
        let first = update("op-1", "first");
        assert_eq!(
            store.append(first.clone(), retention, 1).unwrap().sequence,
            1
        );
        assert_eq!(
            store.append(first.clone(), retention, 2).unwrap().sequence,
            1
        );
        assert!(matches!(
            store.append(update("op-1", "different"), retention, 2),
            Err(Error::Conflict)
        ));
        let mut handoff = first.clone();
        handoff.host = "other-host".into();
        assert_eq!(store.append(handoff, retention, 2).unwrap().sequence, 1);
        let expired_view = store.page(None, 100, retention, 102).unwrap();
        assert!(expired_view.entries.is_empty());
        assert_eq!(expired_view.total_pruned, 1);
        assert_eq!(store.gc(retention, 102, true).unwrap(), 1);
        assert_eq!(
            store.page(None, 100, retention, 2).unwrap().entries.len(),
            1
        );

        let mut correction = update("op-2", "correction");
        correction.correction_of = Some(1);
        store.append(correction, retention, 3).unwrap();
        let page = store.page(None, 1, retention, 3).unwrap();
        assert_eq!(page.entries[0].sequence, 1);
        assert_eq!(page.next_cursor, Some(1));
        assert_eq!(
            store
                .page(page.next_cursor, 1, retention, 3)
                .unwrap()
                .entries[0]
                .update
                .correction_of,
            Some(1)
        );
        store.append(update("op-3", "third"), retention, 4).unwrap();
        let restarted = LogStore::new(root, project, session);
        let page = restarted.page(None, 100, retention, 4).unwrap();
        assert_eq!(
            page.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!((page.total_appended, page.total_pruned), (3, 1));
        assert_eq!(restarted.append(first, retention, 5).unwrap().sequence, 1);
        restarted
            .append(update("op-4", "later"), retention, 200)
            .unwrap();
        let page = restarted.page(None, 100, retention, 200).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.total_pruned, 3);
        assert!(restarted.page(None, 0, retention, 200).is_err());
        assert!(
            restarted
                .append(update("large", &"x".repeat(ENTRY_LIMIT)), retention, 201)
                .is_err()
        );
        let size_retention = Retention {
            max_age_seconds: 1000,
            max_entries: 100,
            max_bytes: 32768,
        };
        for n in 0..4 {
            restarted
                .append(
                    update(&format!("s-{n}"), &"x".repeat(9000)),
                    size_retention,
                    202,
                )
                .unwrap();
        }
        let page = restarted.page(None, 100, size_retention, 202).unwrap();
        assert_eq!(page.entries.len(), 3);
        assert_eq!(page.total_pruned, 5);
        assert_eq!(restarted.gc(size_retention, 1203, false).unwrap(), 3);
        let page = restarted.page(None, 100, size_retention, 1203).unwrap();
        assert!(page.entries.is_empty());
        assert_eq!(page.total_pruned, 8);
        let dir = restarted.dir(false).unwrap();
        assert!(matches!(
            LogStore::read(&dir, 1),
            Err(Error::Entry { sequence: 1, .. })
        ));
        LogStore::publish(
            &dir,
            "manifest.json",
            br#"{"schema_version":1,"receipts":[],"total_pruned":0,"total_pruned":1}"#,
        )
        .unwrap();
        assert!(matches!(LogStore::load(&dir), Err(Error::Decode(_))));
    }
}
