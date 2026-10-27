//! Explicit host-keyed Markdown checkpoints. No transcript or latest-file lookup.
use crate::store::{WaitStop, check_wait};
use provenant_core::{CancelToken, Clock, ProjectId, StateRoot, UtcMicros, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use thiserror::Error;

mod hooks;
mod state;
pub use hooks::*;
pub use state::*;

pub const CHECKPOINT_LIMIT: u64 = 32 * 1024;
pub const HOST_SESSION_ID_LIMIT: usize = 128;

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
    #[error("checkpoint exceeds 32 KiB")]
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
}
impl SessionStore {
    pub fn new(root: StateRoot, project: ProjectId, session: HostSessionId) -> Self {
        Self {
            root,
            project,
            session,
        }
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
        dir.atomic_write(
            OsStr::new("checkpoint.md"),
            ".checkpoint.",
            text,
            PublishMode::Replace,
        )?
        .require_durable()
        .map_err(SessionError::Durability)
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
        dir.atomic_write(
            OsStr::new(AUTOSAVE_FILE),
            ".autosave.",
            text,
            PublishMode::Replace,
        )?
        .require_durable()
        .map_err(SessionError::Durability)
    }
    /// The hook-owned autosave file, if any hook has written one.
    pub fn read_autosave(&self) -> Result<Option<Autosave>, SessionError> {
        let read = || -> Result<Autosave, SessionError> {
            let dir = self.directory(false)?;
            let mut file = dir.open_read(OsStr::new(AUTOSAVE_FILE))?;
            let text = file.read_bounded(CHECKPOINT_LIMIT)?;
            let markdown = std::str::from_utf8(&text)?.to_owned();
            let micros = i64::try_from(file.metadata()?.modified_ns / 1000)
                .map_err(|_| provenant_core::TimeError::WallRange)?;
            Ok(Autosave {
                markdown,
                modified_at: UtcMicros::from_micros(micros)?,
            })
        };
        match read() {
            Ok(autosave) => Ok(Some(autosave)),
            Err(SessionError::Filesystem(error)) if error.is_not_found() => Ok(None),
            Err(error) => Err(error),
        }
    }
    /// Atomic replacement allows a consistent unlocked read of one held file.
    pub fn read(&self) -> Result<Option<Checkpoint>, SessionError> {
        let read = || -> Result<Checkpoint, SessionError> {
            let dir = self.directory(false)?;
            let mut file = dir.open_read(OsStr::new("checkpoint.md"))?;
            let text = file.read_bounded(CHECKPOINT_LIMIT)?;
            let markdown = std::str::from_utf8(&text)?.to_owned();
            let micros = i64::try_from(file.metadata()?.modified_ns / 1000)
                .map_err(|_| provenant_core::TimeError::WallRange)?;
            Ok(Checkpoint {
                markdown,
                modified_at: UtcMicros::from_micros(micros)?,
            })
        };
        match read() {
            Ok(checkpoint) => Ok(Some(checkpoint)),
            Err(SessionError::Filesystem(error)) if error.is_not_found() => Ok(None),
            Err(error) => Err(error),
        }
    }
    /// Read, edit and replace the checkpoint under one exclusive lock, so a
    /// concurrent `save` is never overwritten from a stale read. `edit` gets the
    /// current Markdown and returns the replacement, or `None` to leave the file
    /// alone. Directories are created only when `create` is set and the
    /// checkpoint is missing; returns whether a replacement was written.
    pub fn update(
        &self,
        create: bool,
        edit: impl FnOnce(Option<&str>) -> Result<Option<String>, SessionError>,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<bool, SessionError> {
        let dir = match self.directory(create) {
            Ok(dir) => dir,
            Err(error) if !create && error.is_not_found() => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let _lock = dir.lock(
            OsStr::new("checkpoint.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        let current = match dir.open_read(OsStr::new("checkpoint.md")) {
            Ok(mut file) => {
                let text = file.read_bounded(CHECKPOINT_LIMIT)?;
                Some(std::str::from_utf8(&text)?.to_owned())
            }
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error.into()),
        };
        let Some(next) = edit(current.as_deref())? else {
            return Ok(false);
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
        dir.atomic_write(
            OsStr::new("checkpoint.md"),
            ".checkpoint.",
            next.as_bytes(),
            PublishMode::Replace,
        )?
        .require_durable()
        .map_err(SessionError::Durability)?;
        Ok(true)
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
        assert!(!wrote && !path(&root, project, "host-1").exists());
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
        assert!(wrote);
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
            !selected
                .update(true, |_| Ok(None), budget, &cancel, &clock)
                .unwrap()
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
