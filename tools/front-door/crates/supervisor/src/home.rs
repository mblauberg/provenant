//! Execute bounded private-home data recipes through held filesystem handles.
//! No process custody or signalling capability is accepted here.
use crate::fs::{FileKind, FsError, Metadata, Publication, PublishMode, SafeDir, SafeFs};
use provenant_core::RelativePath;
use std::{
    ffi::OsStr,
    io::Read,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HomeError {
    #[error("private home recipe is outside its supported bounds")]
    InvalidRecipe,
    #[error("private home contains an unsafe or changed entry")]
    UnsafeEntry,
    #[error("home state exceeds its total byte bound")]
    TooLarge,
    #[error("transactional state requires a snapshot mechanism; private state retained")]
    UnsupportedState,
    #[error("existing user state was preserved; private state retained: {name}")]
    Conflict { name: String },
    #[error("home publication is visible but durability is unconfirmed")]
    DurabilityUnconfirmed(#[source] FsError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error("bounded home state read failed")]
    Read(#[source] std::io::Error),
}

/// Ephemeral data, not a stored record. Contains names and bounds only.
#[derive(Debug)]
pub struct HomeRecipe {
    source: PathBuf,
    private_parent: PathBuf,
    home: RelativePath,
    state: RelativePath,
    files: HomeFiles,
    byte_limit: u64,
}

/// Selected ordinary files and transactional state that cannot safely be copied.
#[derive(Clone, Debug)]
pub struct HomeFiles {
    pub copy: Vec<String>,
    pub refuse_source: Vec<String>,
    pub refuse_private: Vec<String>,
}

impl HomeRecipe {
    pub fn new(
        source: PathBuf,
        private_parent: PathBuf,
        home: RelativePath,
        state: RelativePath,
        files: HomeFiles,
        byte_limit: u64,
    ) -> Result<Self, HomeError> {
        if !source.is_absolute()
            || !private_parent.is_absolute()
            || source.starts_with(&private_parent)
            || private_parent.starts_with(&source)
            || home.as_path().components().count() != 1
            || state.as_path().components().count() > 8
            || files.copy.is_empty()
            || files.copy.len() + files.refuse_source.len() + files.refuse_private.len() > 32
            || byte_limit == 0
            || byte_limit > 64 * 1024 * 1024
        {
            return Err(HomeError::InvalidRecipe);
        }
        for name in files
            .copy
            .iter()
            .chain(&files.refuse_source)
            .chain(&files.refuse_private)
        {
            let relative = RelativePath::parse(name).map_err(|_| HomeError::InvalidRecipe)?;
            if relative.as_path().components().count() != 1 || name.len() > 255 || name == "bin" {
                return Err(HomeError::InvalidRecipe);
            }
        }
        Ok(Self {
            source,
            private_parent,
            home,
            state,
            files,
            byte_limit,
        })
    }

    /// Exclusively create a 0700 home under retained provider-state. A failed
    /// preparation remains retained; the owner decides settlement, never Drop.
    pub fn prepare(&self, fs: &dyn SafeFs) -> Result<PreparedHome, HomeError> {
        let source = canonical_anchor(fs, &self.source)?;
        let metadata = source.metadata()?;
        if metadata.uid != crate::fs::effective_uid() || metadata.mode & 0o022 != 0 {
            return Err(HomeError::UnsafeEntry);
        }
        refuse_present(source.as_ref(), &self.files.refuse_source)?;
        let parent = canonical_anchor(fs, &self.private_parent)?;
        private_directory(parent.as_ref())?;
        let name = self.home.as_path().as_os_str();
        parent.mkdir(name, 0o700)?;
        let home = parent.open_verified_directory(name)?;
        let home_identity = home.metadata()?;
        let state = descend(home.as_ref(), self.state.as_path(), true)?;
        state.mkdir(OsStr::new("bin"), 0o700)?;
        let mut remaining = self.byte_limit;
        for name in &self.files.copy {
            if self.files.refuse_source.contains(name) {
                continue;
            }
            if let Some(bytes) = read_optional(source.as_ref(), OsStr::new(name), &mut remaining)? {
                durable(state.atomic_write(
                    OsStr::new(name),
                    ".home-",
                    &bytes,
                    0o600,
                    PublishMode::NoReplace,
                )?)?;
            }
        }
        refuse_present(source.as_ref(), &self.files.refuse_source)?;
        state.sync()?;
        home.sync()?;
        parent.sync()?;
        Ok(PreparedHome {
            path: self.private_parent.join(self.home.as_path()),
            source,
            parent,
            home,
            home_identity,
            state_identity: state.metadata()?,
            state,
            home_name: self.home.clone(),
            state_path: self.state.clone(),
            files: self.files.clone(),
            byte_limit: self.byte_limit,
        })
    }
}

/// Held parents bind both phases to the selected roots. Private contents never
/// enter a plan, launch argv, environment or Debug implementation.
pub struct PreparedHome {
    path: PathBuf,
    source: Box<dyn SafeDir>,
    parent: Box<dyn SafeDir>,
    home: Box<dyn SafeDir>,
    home_identity: Metadata,
    state: Box<dyn SafeDir>,
    state_identity: Metadata,
    home_name: RelativePath,
    state_path: RelativePath,
    files: HomeFiles,
    byte_limit: u64,
}

impl PreparedHome {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Call only after provider quiescence. Never replace an existing file:
    /// matching bytes need no write, changed bytes yield a typed conflict.
    /// No check-then-replace window, including writers ignoring advisory locks.
    /// On any error the retained private home remains available for settlement.
    pub fn copy_back(&self) -> Result<(), HomeError> {
        let named = self
            .parent
            .open_verified_directory(self.home_name.as_path().as_os_str())?;
        if !named.metadata()?.same_identity(&self.home_identity) {
            return Err(HomeError::UnsafeEntry);
        }
        let state = descend(self.home.as_ref(), self.state_path.as_path(), false)?;
        if !state.metadata()?.same_identity(&self.state_identity) {
            return Err(HomeError::UnsafeEntry);
        }
        refuse_present(self.state.as_ref(), &self.files.refuse_private)?;
        let mut remaining = self.byte_limit;
        let mut user_remaining = self.byte_limit;
        for name in &self.files.copy {
            let name_os = OsStr::new(name);
            let Some(bytes) = read_optional(self.state.as_ref(), name_os, &mut remaining)? else {
                continue;
            };
            if let Some(existing) =
                read_optional(self.source.as_ref(), name_os, &mut user_remaining)?
            {
                if existing == bytes {
                    continue;
                }
                return Err(HomeError::Conflict { name: name.clone() });
            }
            match self
                .source
                .atomic_write(name_os, ".home-", &bytes, 0o600, PublishMode::NoReplace)
            {
                Ok(publication) => durable(publication)?,
                Err(error) if error.is_exists() => {
                    return Err(HomeError::Conflict { name: name.clone() });
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

fn refuse_present(dir: &dyn SafeDir, names: &[String]) -> Result<(), HomeError> {
    for name in names {
        match dir.metadata_at(OsStr::new(name), false) {
            Ok(_) => return Err(HomeError::UnsupportedState),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn canonical_anchor(fs: &dyn SafeFs, path: &Path) -> Result<Box<dyn SafeDir>, HomeError> {
    let held = fs.open_anchor(path)?;
    if crate::fs::canonicalize(path)? != path {
        return Err(HomeError::UnsafeEntry);
    }
    let named = fs.open_anchor(path)?;
    if !held.metadata()?.same_identity(&named.metadata()?) {
        return Err(HomeError::UnsafeEntry);
    }
    Ok(held)
}

fn private_directory(dir: &dyn SafeDir) -> Result<(), HomeError> {
    let m = dir.metadata()?;
    if m.kind != FileKind::Directory || m.uid != crate::fs::effective_uid() || m.mode & 0o077 != 0 {
        return Err(HomeError::UnsafeEntry);
    }
    Ok(())
}

fn descend(root: &dyn SafeDir, path: &Path, create: bool) -> Result<Box<dyn SafeDir>, HomeError> {
    let mut parts = path.iter();
    let first = parts.next().ok_or(HomeError::InvalidRecipe)?;
    if create {
        root.mkdir(first, 0o700)?;
    }
    let mut dir = root.open_verified_directory(first)?;
    for part in parts {
        if create {
            dir.mkdir(part, 0o700)?;
        }
        dir = dir.open_verified_directory(part)?;
    }
    private_directory(dir.as_ref())?;
    Ok(dir)
}

fn durable(publication: Publication) -> Result<(), HomeError> {
    match publication {
        Publication::Durable => Ok(()),
        Publication::VisibleDurabilityUnconfirmed { sync_error } => {
            Err(HomeError::DurabilityUnconfirmed(sync_error))
        }
    }
}

fn unchanged(a: &Metadata, b: &Metadata) -> bool {
    a.same_identity(b)
        && a.kind == b.kind
        && a.links == b.links
        && a.size == b.size
        && a.modified_ns == b.modified_ns
        && a.mode == b.mode
        && a.uid == b.uid
}

fn read_optional(
    dir: &dyn SafeDir,
    name: &OsStr,
    remaining: &mut u64,
) -> Result<Option<Vec<u8>>, HomeError> {
    let entry = match dir.metadata_at(name, false) {
        Ok(entry) => entry,
        Err(e) if e.is_not_found() => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if entry.kind != FileKind::Regular || entry.links != 1 {
        return Err(HomeError::UnsafeEntry);
    }
    if entry.size > *remaining {
        return Err(HomeError::TooLarge);
    }
    let mut file = dir.open_read(name)?;
    if !unchanged(&entry, &file.metadata()?) {
        return Err(HomeError::UnsafeEntry);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(*remaining + 1)
        .read_to_end(&mut bytes)
        .map_err(HomeError::Read)?;
    if bytes.len() as u64 > *remaining {
        return Err(HomeError::TooLarge);
    }
    if bytes.len() as u64 != entry.size
        || !unchanged(&entry, &file.metadata()?)
        || !unchanged(&entry, &dir.metadata_at(name, false)?)
    {
        return Err(HomeError::UnsafeEntry);
    }
    *remaining -= bytes.len() as u64;
    Ok(Some(bytes))
}
