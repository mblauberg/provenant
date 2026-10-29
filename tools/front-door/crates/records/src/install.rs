//! Version-one installation records. These describe desired ownership, never
//! authorise adopting a foreign entry. Config validates paths and selectors.
use crate::{FileDigest, PathWire, SourceDigest};
use provenant_core::RelativePath;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbsolutePath {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    relative_to_root: PathWire,
}
impl AbsolutePath {
    pub fn new(path: &Path) -> Result<Self, crate::PathWireError> {
        // The wire model deliberately reuses PathWire's native-byte encoding.
        let tail = path.strip_prefix("/").unwrap_or(path);
        let relative = RelativePath::parse(tail)?;
        if !path.is_absolute() {
            return Err(crate::PathWireError::InvalidNativePath);
        }
        Ok(Self {
            extra: Default::default(),
            relative_to_root: PathWire::from_relative(&relative)?,
        })
    }
    /// UTF-8 bytes of relative_to_root.path, including native-byte hex encoding.
    pub fn wire_path_bytes(&self) -> usize {
        self.relative_to_root.encoded_len()
    }
    pub fn to_path_buf(&self) -> Result<PathBuf, crate::PathWireError> {
        Ok(Path::new("/").join(self.relative_to_root.to_relative_path()?.as_path()))
    }
}

pub use crate::adapters::Harness;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseFile {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub path: PathWire,
    pub digest: FileDigest,
    pub bytes: u64,
    pub mode: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub schema_version: u64,
    pub source_digest: SourceDigest,
    pub asset_digest: SourceDigest,
    pub target: String,
    pub toolchain: String,
    pub files: Vec<ReleaseFile>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OwnedValue {
    File {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        digest: FileDigest,
        mode: u64,
    },
    Link {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        target: AbsolutePath,
    },
    Selector {
        #[serde(flatten)]
        extra: crate::ExtraFields,
        value: serde_json::Value,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedEntry {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub path: AbsolutePath,
    pub harness: Option<Harness>,
    pub selector: String,
    pub expected: OwnedValue,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedHarnessTarget {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub harness: Harness,
    pub skills_dir: AbsolutePath,
    pub mcp_file: AbsolutePath,
    pub hook_file: Option<AbsolutePath>,
    pub instruction: Option<AbsolutePath>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallReceipt {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub schema_version: u64,
    pub source_digest: SourceDigest,
    pub manifest_digest: FileDigest,
    pub asset_digest: SourceDigest,
    pub target: String,
    pub toolchain: String,
    pub bin_dir: AbsolutePath,
    pub harnesses: Vec<Harness>,
    pub harness_targets: Vec<ResolvedHarnessTarget>,
    pub entries: Vec<OwnedEntry>,
    #[serde(default, skip_serializing)]
    pub legacy: Option<serde::de::IgnoredAny>,
}

/// Build identity emitted at the binary edge without ambient source discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub source_digest: SourceDigest,
    pub asset_digest: SourceDigest,
    pub target: String,
    pub toolchain: String,
    pub record_schema_version: u64,
    pub config_schema_version: u64,
}
