//! Held-parent installation and verification for a single retained output file.
use provenant_core::RelativePath;
use provenant_records::{CanonicalV1, FileDigest, PathWire};
use provenant_supervisor::fs::{FsError, Metadata, Publication, PublishMode, SafeDir};
use serde::Serialize;
use std::ffi::OsStr;
use std::path::{Component, Path};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OutputCustodyError {
    #[error("output path must be one normal component")]
    InvalidName,
    #[error("output source or destination is not a single-link regular file")]
    UnsupportedFile,
    #[error("output exceeded its declared byte bound")]
    TooLarge,
    #[error("output changed while being read or verified")]
    Changed,
    #[error("output digest or retained identity does not match")]
    IdentityMismatch,
    #[error("output publication is visible but directory durability is unconfirmed")]
    DurabilityUnconfirmed,
    #[error("filesystem operation was refused: {0}")]
    Filesystem(#[from] FsError),
    #[error("output read or sync failed")]
    Io(#[from] std::io::Error),
}

/// An edge-built, non-deserializable receipt for one installed file.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledOutput {
    name: PathWire,
    digest: FileDigest,
    device: u64,
    inode: u64,
    size: u64,
    modified_ns: i64,
}
impl InstalledOutput {
    pub fn name(&self) -> &PathWire {
        &self.name
    }

    pub fn digest(&self) -> FileDigest {
        self.digest
    }
    pub fn device(&self) -> u64 {
        self.device
    }
    pub fn inode(&self) -> u64 {
        self.inode
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Installs output bytes through the supervisor's held parent capability. The
/// source and destination names are single components, never arbitrary paths.
pub struct OutputCustody;
impl OutputCustody {
    pub fn install(
        source_parent: &dyn SafeDir,
        source_name: &OsStr,
        destination_parent: &dyn SafeDir,
        destination_name: &OsStr,
        limit: u64,
    ) -> Result<InstalledOutput, OutputCustodyError> {
        let name = wire_name(destination_name)?;
        let (bytes, _) = read_regular(source_parent, source_name, limit)?;
        let digest = CanonicalV1::file_digest(&bytes);
        let parent = destination_parent.metadata()?;
        let publication = destination_parent.atomic_write(
            destination_name,
            ".provenant-output-",
            &bytes,
            0o600,
            PublishMode::NoReplace,
        )?;
        match publication {
            Publication::Durable => {}
            Publication::VisibleDurabilityUnconfirmed { .. } => {
                return Err(OutputCustodyError::DurabilityUnconfirmed);
            }
        }
        let (installed_bytes, installed) =
            read_regular(destination_parent, destination_name, limit)?;
        if installed_bytes != bytes
            || CanonicalV1::file_digest(&installed_bytes) != digest
            || installed.device != parent.device
            || installed.links != 1
        {
            return Err(OutputCustodyError::IdentityMismatch);
        }
        Ok(InstalledOutput {
            name,
            digest,
            device: installed.device,
            inode: installed.inode,
            size: installed.size,
            modified_ns: i64::try_from(installed.modified_ns)
                .map_err(|_| OutputCustodyError::Changed)?,
        })
    }

    pub fn verify(
        destination_parent: &dyn SafeDir,
        destination_name: &OsStr,
        expected: &InstalledOutput,
        limit: u64,
    ) -> Result<(), OutputCustodyError> {
        let actual_name = wire_name(destination_name)?;
        if actual_name != expected.name {
            return Err(OutputCustodyError::IdentityMismatch);
        }
        let (bytes, metadata) = read_regular(destination_parent, destination_name, limit)?;
        let directory = destination_parent.metadata()?;
        if metadata.device != directory.device
            || metadata.links != 1
            || metadata.device != expected.device
            || metadata.inode != expected.inode
            || metadata.size != expected.size
            || i64::try_from(metadata.modified_ns).map_err(|_| OutputCustodyError::Changed)?
                != expected.modified_ns
            || CanonicalV1::file_digest(&bytes) != expected.digest
        {
            return Err(OutputCustodyError::IdentityMismatch);
        }
        // Recheck the lexical leaf after hashing. `open_read` retained the old
        // inode, but only the held parent can say whether the name still points
        // to that same object.
        let final_metadata = destination_parent.metadata_at(destination_name, false)?;
        if !crate::stable_read::same_metadata(&metadata, &final_metadata)
            || final_metadata.links != 1
        {
            return Err(OutputCustodyError::Changed);
        }
        Ok(())
    }
}

fn wire_name(name: &OsStr) -> Result<PathWire, OutputCustodyError> {
    let path = Path::new(name);
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(OutputCustodyError::InvalidName);
    }
    let relative = RelativePath::parse(path).map_err(|_| OutputCustodyError::InvalidName)?;
    PathWire::from_relative(&relative).map_err(|_| OutputCustodyError::InvalidName)
}

fn read_regular(
    parent: &dyn SafeDir,
    name: &OsStr,
    limit: u64,
) -> Result<(Vec<u8>, Metadata), OutputCustodyError> {
    crate::stable_read::read_regular(parent, name, limit).map_err(|error| match error {
        crate::stable_read::ReadError::Unsupported => OutputCustodyError::UnsupportedFile,
        crate::stable_read::ReadError::TooLarge => OutputCustodyError::TooLarge,
        crate::stable_read::ReadError::Changed => OutputCustodyError::Changed,
        crate::stable_read::ReadError::Filesystem(error) => OutputCustodyError::Filesystem(error),
        crate::stable_read::ReadError::Io(error) => OutputCustodyError::Io(error),
    })
}

#[cfg(test)]
mod tests {
    use super::{OutputCustody, OutputCustodyError};
    use provenant_supervisor::fs::{LocalFs, SafeFs};
    use std::error::Error;
    use std::ffi::OsStr;

    #[test]
    fn install_binds_digest_and_inode_and_verify_rejects_replacement() -> Result<(), Box<dyn Error>>
    {
        let root = tempfile::tempdir_in("/tmp")?;
        std::fs::write(root.path().join("source"), b"complete output")?;
        let fs = LocalFs;
        let parent = fs.open_anchor(root.path())?;
        let receipt = OutputCustody::install(
            parent.as_ref(),
            OsStr::new("source"),
            parent.as_ref(),
            OsStr::new("result"),
            64,
        )?;
        OutputCustody::verify(parent.as_ref(), OsStr::new("result"), &receipt, 64)?;
        assert!(
            OutputCustody::install(
                parent.as_ref(),
                OsStr::new("source"),
                parent.as_ref(),
                OsStr::new("result"),
                64
            )
            .is_err()
        );
        OutputCustody::verify(parent.as_ref(), OsStr::new("result"), &receipt, 64)?;
        std::fs::write(root.path().join("result"), b"changed output")?;
        assert!(matches!(
            OutputCustody::verify(parent.as_ref(), OsStr::new("result"), &receipt, 64),
            Err(OutputCustodyError::IdentityMismatch | OutputCustodyError::Changed)
        ));
        Ok(())
    }

    #[test]
    fn install_refuses_oversize_and_linked_sources() -> Result<(), Box<dyn Error>> {
        let root = tempfile::tempdir_in("/tmp")?;
        std::fs::write(root.path().join("large"), b"0123456789")?;
        std::fs::write(root.path().join("target"), b"x")?;
        std::fs::hard_link(root.path().join("target"), root.path().join("linked"))?;
        let fs = LocalFs;
        let parent = fs.open_anchor(root.path())?;
        assert!(matches!(
            OutputCustody::install(
                parent.as_ref(),
                OsStr::new("large"),
                parent.as_ref(),
                OsStr::new("out"),
                4
            ),
            Err(OutputCustodyError::TooLarge)
        ));
        assert!(matches!(
            OutputCustody::install(
                parent.as_ref(),
                OsStr::new("linked"),
                parent.as_ref(),
                OsStr::new("out"),
                8
            ),
            Err(OutputCustodyError::UnsupportedFile)
        ));
        Ok(())
    }
}
