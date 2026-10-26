//! Disposal authority for detached external payload trees, such as a cache
//! tombstone or a terminal attempt's `tmp/`. Ordinary removal refuses
//! lock-named entries so stable owner locks keep their inodes; payload trees
//! hold build output whose `*.lock` names guard nothing once custody clears.
use super::*;

/// Grant for one payload removal. Only `PayloadTree` constructs it, so a
/// `SafeDir` adapter can forward a removal but no caller can start one.
#[derive(Debug)]
pub struct PayloadRemoval(());

/// A claimed payload directory and its descendants. Unlike ordinary handles,
/// `unlink` and `rmdir` here also remove lock-named entries, still without
/// following a final link.
///
/// `claim` is public for the supervisor's internal consumers, but is a
/// deliberate opt-in rather than proof of custody. Callers must already hold
/// whatever lock or reservation excludes every live user of the tree, and
/// must never claim supervisor state that contains stable owner locks.
pub struct PayloadTree(Box<dyn SafeDir>);

impl PayloadTree {
    /// Open `name` under `parent` as a payload root. The opened directory must
    /// keep the `expected` identity, belong to the effective user and have
    /// mode 0700; anything else is refused before any removal.
    pub fn claim(parent: &dyn SafeDir, name: &OsStr, expected: &Metadata) -> Result<Self> {
        let directory = parent.open_verified_directory(name)?;
        let opened = directory.metadata()?;
        if !expected.same_identity(&opened) {
            return Err(FsError::RaceRefused { name: name.into() });
        }
        if opened.uid != effective_uid() || opened.mode & 0o7777 != 0o700 {
            return Err(FsError::NotPrivate { name: name.into() });
        }
        Ok(Self(directory))
    }
    /// Ordinary operations on the claimed directory. These keep lock-name
    /// protection; only this tree's `unlink` and `rmdir` dispose of payload.
    pub fn directory(&self) -> &dyn SafeDir {
        self.0.as_ref()
    }
    pub fn metadata(&self) -> Result<Metadata> {
        self.0.metadata()
    }
    pub fn metadata_at(&self, name: &OsStr) -> Result<Metadata> {
        self.0.metadata_at(name, false)
    }
    pub fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<OsString>>>> {
        self.0.entries()
    }
    pub fn make_writable(&self) -> Result<()> {
        self.0.make_writable()
    }
    /// A verified child directory inherits the claim; nested payload need not
    /// be private, since the claimed root already excludes other users.
    pub fn child(&self, name: &OsStr) -> Result<Self> {
        Ok(Self(self.0.open_verified_directory(name)?))
    }
    pub fn unlink(&self, name: &OsStr) -> Result<()> {
        self.0.remove_payload(name, false, PayloadRemoval(()))
    }
    pub fn rmdir(&self, name: &OsStr) -> Result<()> {
        self.0.remove_payload(name, true, PayloadRemoval(()))
    }
}
