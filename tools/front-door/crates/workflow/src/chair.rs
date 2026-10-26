//! Account-switch handoffs, scoped to a canonical working directory.
use crate::session::HostSessionId;
use provenant_core::StateRoot;
use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
use provenant_records::CanonicalV1;
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, path::PathBuf};
const LIMIT: u64 = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub session_id: HostSessionId,
    pub cwd: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub state_handoff_file: Option<PathBuf>,
    pub created_unix: i64,
    pub open_lanes: Vec<String>,
    pub target_account: Option<String>,
    pub reason: String,
    pub consumed: bool,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(&'static str),
}
pub struct HandoffStore {
    root: StateRoot,
    cwd: PathBuf,
}
impl Handoff {
    fn live(&self, now: i64) -> bool {
        (0..=3600).contains(&now.saturating_sub(self.created_unix))
    }
}
impl HandoffStore {
    pub fn new(root: StateRoot, cwd: PathBuf) -> Self {
        Self { root, cwd }
    }
    fn key(&self) -> String {
        CanonicalV1::file_digest(self.cwd.as_os_str().as_encoded_bytes())
            .to_string()
            .replace("sha256:", "")
    }
    fn directory(&self, create: bool) -> Result<Directory, Error> {
        fn private(dir: &Directory) -> Result<(), Error> {
            let meta = dir.metadata()?;
            if meta.mode & 0o077 != 0 || meta.uid != provenant_os::fs::effective_uid() {
                return Err(Error::Invalid(
                    "handoff directories must be private and owned",
                ));
            }
            Ok(())
        }
        let mut dir = Directory::open_anchor(self.root.as_path())?;
        private(&dir)?;
        for name in ["chair-switch", &self.key()] {
            let name = OsStr::new(name);
            if create {
                match dir.mkdir(name, 0o700) {
                    Ok(()) => dir.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
            }
            dir = dir.open_verified_directory(name)?;
            private(&dir)?;
        }
        Ok(dir)
    }
    pub fn path(&self, session: &HostSessionId) -> PathBuf {
        self.root
            .as_path()
            .join("chair-switch")
            .join(self.key())
            .join(format!("{}.json", session.as_str()))
    }
    fn read(&self, dir: &Directory, name: &str) -> Result<Option<Handoff>, Error> {
        let bytes = match dir.read_bounded(OsStr::new(name), LIMIT) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let handoff: Handoff = serde_json::from_slice(&bytes)?;
        if handoff.cwd != self.cwd {
            return Err(Error::Invalid("handoff working directory mismatch"));
        }
        Ok(Some(handoff))
    }
    fn write(dir: &Directory, name: &str, handoff: &Handoff) -> Result<(), Error> {
        let bytes = serde_json::to_vec(handoff)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Invalid("handoff exceeds 32 KiB"));
        }
        dir.atomic_write(OsStr::new(name), ".handoff-", &bytes, PublishMode::Replace)?
            .require_durable()?;
        Ok(())
    }
    pub fn pending(&self, now: i64) -> Result<Option<Handoff>, Error> {
        let dir = match self.directory(false) {
            Ok(dir) => dir,
            Err(Error::Filesystem(error)) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(self
            .read(&dir, ".pending.json")?
            .filter(|h| !h.consumed && h.live(now)))
    }
    pub fn recorded_refusal(&self, session: &HostSessionId) -> Result<bool, Error> {
        let dir = match self.directory(false) {
            Ok(dir) => dir,
            Err(Error::Filesystem(error)) if error.is_not_found() => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(self
            .read(&dir, &format!("{}.json", session.as_str()))?
            .is_some_and(|h| h.target_account.is_none()))
    }
    pub fn prepared(&self, session: &HostSessionId) -> Result<Option<Handoff>, Error> {
        let dir = match self.directory(false) {
            Ok(dir) => dir,
            Err(Error::Filesystem(error)) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(self
            .read(&dir, &format!(".prepared-{}.json", session.as_str()))?
            .filter(|h| h.target_account.is_some()))
    }
    /// Persist before launching. A source session with a prepared handoff is
    /// idempotent even after consumption; repeated prompts cannot launch twice.
    pub fn save(&self, handoff: &Handoff) -> Result<(PathBuf, bool), Error> {
        if handoff.cwd != self.cwd || handoff.consumed {
            return Err(Error::Invalid("invalid new handoff"));
        }
        let dir = self.directory(true)?;
        let _lock = dir.try_lock(OsStr::new("handoff.lock"), LockMode::Exclusive)?;
        let name = format!("{}.json", handoff.session_id.as_str());
        let prepared = format!(".prepared-{}.json", handoff.session_id.as_str());
        let path = self.path(&handoff.session_id);
        if self
            .read(&dir, &prepared)?
            .is_some_and(|old| old.target_account.is_some())
        {
            return Ok((path, false));
        }
        // A target may have consumed a pending record after an interrupted
        // publication. Retain that consumption instead of reissuing the handoff.
        if let Some(pending) = self.read(&dir, ".pending.json")?
            && pending.session_id == handoff.session_id
            && pending.consumed
        {
            Self::write(&dir, &prepared, &pending)?;
            return Ok((path, false));
        }
        if handoff.target_account.is_some()
            && self.read(&dir, ".pending.json")?.is_some_and(|pending| {
                !pending.consumed
                    && pending.live(handoff.created_unix)
                    && pending.session_id != handoff.session_id
            })
        {
            return Err(Error::Invalid(
                "another source session has an unconsumed handoff for this cwd",
            ));
        }
        if handoff.target_account.is_none() && self.read(&dir, &name)?.is_some() {
            return Ok((path, false));
        }
        Self::write(&dir, &name, handoff)?;
        if handoff.target_account.is_some() {
            Self::write(&dir, ".pending.json", handoff)?;
            // Only this final durable record commits preparation. An earlier
            // checkpoint or pending write alone must never suppress a retry.
            Self::write(&dir, &prepared, handoff)?;
        }
        Ok((path, true))
    }
    /// Only the selected account's new session consumes a handoff within one hour.
    /// The atomic consumed marker is durable before context leaves this store.
    pub fn consume(
        &self,
        target: &str,
        session: &HostSessionId,
        now: i64,
    ) -> Result<Option<Handoff>, Error> {
        let dir = match self.directory(false) {
            Ok(dir) => dir,
            Err(Error::Filesystem(error)) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
        let _lock = dir.try_lock(OsStr::new("handoff.lock"), LockMode::Exclusive)?;
        let Some(mut handoff) = self.read(&dir, ".pending.json")? else {
            return Ok(None);
        };
        if handoff.consumed
            || !handoff.live(now)
            || handoff.target_account.as_deref() != Some(target)
            || handoff.session_id == *session
        {
            return Ok(None);
        }
        handoff.consumed = true;
        Self::write(&dir, ".pending.json", &handoff)?;
        Ok(Some(handoff))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use provenant_core::{PathResolver, RootInputs};
    use std::path::Path;
    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            Ok(path.into())
        }
    }
    #[test]
    fn chair_handoff_preserves_pending_source_and_consumes_once_for_target_only() {
        let tmp = crate::test_support::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let state = base.join("state");
        std::fs::create_dir(&state).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(state);
        let root = StateRoot::resolve(&inputs, &Resolver).unwrap();
        let store = HandoffStore::new(root.clone(), base.clone());
        let id = |s| HostSessionId::parse(s).unwrap();
        let mut handoff = Handoff {
            session_id: id("old"),
            cwd: base.clone(),
            transcript_path: Some(base.join("transcript.jsonl")),
            state_handoff_file: None,
            created_unix: 1000,
            open_lanes: vec!["run/task/attempt".into()],
            target_account: Some("target".into()),
            reason: "quota threshold".into(),
            consumed: false,
        };
        // Simulate failure after the source checkpoint is durable but before
        // pending publication. No launch claim may survive this incomplete save.
        let dir = store.directory(true).unwrap();
        std::fs::write(
            store.path(&handoff.session_id),
            serde_json::to_vec(&handoff).unwrap(),
        )
        .unwrap();
        dir.mkdir(OsStr::new(".pending.json"), 0o700).unwrap();
        assert!(store.save(&handoff).is_err());
        assert!(store.path(&handoff.session_id).is_file());
        assert!(store.prepared(&handoff.session_id).unwrap().is_none());
        dir.rmdir(OsStr::new(".pending.json")).unwrap();
        let (path, fresh) = store.save(&handoff).unwrap();
        assert!(fresh && path.is_file());
        assert!(!store.save(&handoff).unwrap().1);
        assert!(store.consume("wrong", &id("new"), 1001).unwrap().is_none());
        assert!(store.consume("target", &id("old"), 1001).unwrap().is_none());
        handoff.session_id = id("newer");
        assert!(store.save(&handoff).is_err());
        handoff.session_id = id("old");
        let restored = store.consume("target", &id("new"), 1001).unwrap().unwrap();
        assert_eq!(restored.session_id, id("old"));
        assert_eq!(restored.open_lanes, handoff.open_lanes);
        // Interruption after pending publication and target consumption, but
        // before the prepare marker, must not overwrite consumed context.
        dir.unlink(OsStr::new(".prepared-old.json")).unwrap();
        let original = std::fs::read(store.path(&handoff.session_id)).unwrap();
        handoff.reason = "a different retry reason".into();
        assert!(!store.save(&handoff).unwrap().1);
        assert_eq!(
            std::fs::read(store.path(&handoff.session_id)).unwrap(),
            original
        );

        assert!(
            store
                .consume("target", &id("another"), 1001)
                .unwrap()
                .is_none()
        );
        handoff.session_id = id("expiring");
        handoff.created_unix = 1002;
        store.save(&handoff).unwrap();
        assert!(store.pending(4603).unwrap().is_none());
        assert!(
            store
                .consume("target", &id("late"), 4603)
                .unwrap()
                .is_none()
        );
        handoff.session_id = id("replacement");
        handoff.created_unix = 4603;
        assert!(store.save(&handoff).unwrap().1);
        assert_eq!(
            store.pending(4603).unwrap().unwrap().session_id,
            id("replacement")
        );
        let other = HandoffStore::new(root, base.join("other"));
        assert!(other.consume("target", &id("new"), 1001).unwrap().is_none());
    }
}
