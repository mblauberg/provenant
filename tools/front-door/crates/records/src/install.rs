//! Version-one installation records. These describe desired ownership, never
//! authorise adopting a foreign entry. Config validates paths and selectors.
use crate::{FileDigest, PathWire, SourceDigest};
use provenant_core::RelativePath;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbsolutePath {
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
            relative_to_root: PathWire::from_relative(&relative)?,
        })
    }
    pub fn to_path_buf(&self) -> Result<PathBuf, crate::PathWireError> {
        Ok(Path::new("/").join(self.relative_to_root.to_relative_path()?.as_path()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Claude,
    Codex,
    Cursor,
    Agy,
    Opencode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseFile {
    pub path: PathWire,
    pub digest: FileDigest,
    pub bytes: u64,
    pub mode: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u64,
    pub source_digest: SourceDigest,
    pub asset_digest: SourceDigest,
    pub target: String,
    pub toolchain: String,
    pub files: Vec<ReleaseFile>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnedValue {
    File { digest: FileDigest, mode: u64 },
    Link { target: AbsolutePath },
    Selector { value: serde_json::Value },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedEntry {
    pub path: AbsolutePath,
    pub harness: Option<Harness>,
    pub selector: String,
    pub expected: OwnedValue,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedHarnessTarget {
    pub harness: Harness,
    pub skills_dir: AbsolutePath,
    pub mcp_file: AbsolutePath,
    pub hook_file: Option<AbsolutePath>,
    pub instruction: Option<AbsolutePath>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallReceipt {
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
    pub source_digest: SourceDigest,
    pub asset_digest: SourceDigest,
    pub target: String,
    pub toolchain: String,
    pub record_schema_version: u64,
    pub config_schema_version: u64,
}
