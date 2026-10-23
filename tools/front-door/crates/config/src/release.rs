//! Verified staged releases and bounded packaging. Build orchestration calls
//! `package_release` after linking; runtime startup never builds itself.
use crate::managed::{InstallError, directory, file_bytes, normal_absolute, path_wire, snapshot};
use provenant_records::{CanonicalV1, Limits, ReleaseFile, ReleaseManifest, SourceDigest};
use provenant_supervisor::fs::{FileKind, PublishMode, SafeFs};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub(crate) const FILE_LIMIT: u64 = 134_217_728;
const RELEASE_LIMIT: u64 = 536_870_912;
const ENTRY_LIMIT: usize = 10_000;

/// Constructed only after validating the complete inventory and every mode and
/// digest. Bytes are retained so later source edits cannot alter installation.
#[derive(Debug, Clone)]
pub struct Release {
    pub(crate) manifest: ReleaseManifest,
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) files: BTreeMap<PathBuf, (Vec<u8>, u64)>,
    pub(crate) source: PathBuf,
}
impl Release {
    pub fn manifest(&self) -> &ReleaseManifest {
        &self.manifest
    }
    pub fn source(&self) -> &Path {
        &self.source
    }
    pub fn read(source: &Path, target: &str, fs: &dyn SafeFs) -> Result<Self, InstallError> {
        normal_absolute(source)?;
        let bytes = file_bytes(
            source.join("release.json").as_path(),
            Limits::CONFIG.max_bytes,
            fs,
        )?;
        let manifest: ReleaseManifest =
            provenant_records::decode_v1(bytes.as_slice(), Limits::CONFIG)?;
        if manifest.target != target {
            return Err(InstallError::Platform);
        }
        if manifest.toolchain.is_empty()
            || manifest.toolchain.len() > 256
            || manifest.target.len() > 256
        {
            return Err(InstallError::Manifest("invalid build identity"));
        }
        if manifest.files.is_empty() || manifest.files.len() > ENTRY_LIMIT {
            return Err(InstallError::Manifest("inventory bounds"));
        }
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        for item in &manifest.files {
            let relative = item.path.to_relative_path()?.as_path().to_path_buf();
            validate_name(&relative, item.mode)?;
            total = total
                .checked_add(item.bytes)
                .ok_or(InstallError::Manifest("release byte bound"))?;
            if total > RELEASE_LIMIT || item.bytes > FILE_LIMIT {
                return Err(InstallError::Manifest("release byte bound"));
            }
            let path = source.join(&relative);
            let current = snapshot(&path, fs)?;
            let Some(crate::managed::Snapshot {
                value: crate::managed::Value::File { bytes, mode },
                ..
            }) = current
            else {
                return Err(InstallError::Tampered(path));
            };
            if mode != item.mode
                || bytes.len() as u64 != item.bytes
                || CanonicalV1::file_digest(&bytes) != item.digest
            {
                return Err(InstallError::Tampered(path));
            }
            if files.insert(relative, (bytes, item.mode)).is_some() {
                return Err(InstallError::Manifest("duplicate inventory path"));
            }
        }
        if !files.contains_key(Path::new("provenant")) {
            return Err(InstallError::Manifest("missing executable"));
        }
        let actual = inventory(source, fs)?;
        let expected: Vec<_> = files.keys().cloned().collect();
        if actual != expected {
            return Err(InstallError::Manifest(
                "inventory differs from release tree",
            ));
        }
        Ok(Self {
            manifest,
            manifest_bytes: bytes,
            files,
            source: source.into(),
        })
    }
}
fn validate_name(path: &Path, mode: u64) -> Result<(), InstallError> {
    let allowed_mode = if path == Path::new("provenant") {
        0o755
    } else if path.starts_with("assets") {
        0o600
    } else {
        return Err(InstallError::Manifest("file outside release layout"));
    };
    if mode != allowed_mode {
        return Err(InstallError::Manifest("invalid file mode"));
    }
    Ok(())
}
/// The staged tree is already populated by build orchestration. Refuses links,
/// unexpected files and oversized trees; publishes the manifest with NoReplace.
/// Repeating packaging requires exactly identical manifest bytes.
pub fn package_release(
    source: &Path,
    digest: SourceDigest,
    target: &str,
    toolchain: &str,
    fs: &dyn SafeFs,
) -> Result<Release, InstallError> {
    normal_absolute(source)?;
    let names = inventory(source, fs)?;
    let mut files = Vec::new();
    for relative in names {
        let path = source.join(&relative);
        let current = snapshot(&path, fs)?.ok_or_else(|| InstallError::Tampered(path.clone()))?;
        let crate::managed::Value::File { bytes, mode } = current.value else {
            return Err(InstallError::Tampered(path));
        };
        validate_name(&relative, mode)?;
        files.push(ReleaseFile {
            path: path_wire(&relative)?,
            digest: CanonicalV1::file_digest(&bytes),
            bytes: bytes.len() as u64,
            mode,
        });
    }
    let manifest = ReleaseManifest {
        schema_version: 1,
        source_digest: digest,
        target: target.into(),
        toolchain: toolchain.into(),
        files,
    };
    let bytes = provenant_records::encode(&manifest)?;
    // Validate all fields/total bounds before publishing even the manifest.
    if manifest.files.len() > ENTRY_LIMIT
        || manifest
            .files
            .iter()
            .try_fold(0u64, |n, f| n.checked_add(f.bytes))
            .is_none_or(|n| n > RELEASE_LIMIT)
        || target.is_empty()
        || target.len() > 256
        || toolchain.is_empty()
        || toolchain.len() > 256
        || !manifest
            .files
            .iter()
            .any(|f| f.path.as_path_text() == Some("provenant"))
    {
        return Err(InstallError::Manifest("inventory bounds"));
    }
    let path = source.join("release.json");
    if let Some(old) = snapshot(&path, fs)? {
        if old.value
            != (crate::managed::Value::File {
                bytes: bytes.clone(),
                mode: 0o600,
            })
        {
            return Err(InstallError::Tampered(path));
        }
    } else {
        crate::managed::durable(directory(source, false, fs)?.publish(
            OsStr::new("release.json"),
            &bytes,
            0o600,
            PublishMode::NoReplace,
        )?)?;
    }
    Release::read(source, target, fs)
}
fn inventory(root: &Path, fs: &dyn SafeFs) -> Result<Vec<PathBuf>, InstallError> {
    fn walk(
        root: &Path,
        tail: &Path,
        depth: usize,
        fs: &dyn SafeFs,
        output: &mut Vec<PathBuf>,
        count: &mut usize,
    ) -> Result<(), InstallError> {
        if depth > 64 {
            return Err(InstallError::Manifest("inventory bounds"));
        }
        let dir = directory(&root.join(tail), false, fs)?;
        for entry in dir.entries()? {
            let name = entry?;
            *count += 1;
            if *count > ENTRY_LIMIT {
                return Err(InstallError::Manifest("inventory bounds"));
            }
            let path = tail.join(&name);
            let metadata = dir.metadata_at(&name, false)?;
            match metadata.kind {
                FileKind::Directory if path.starts_with("assets") => {
                    walk(root, &path, depth + 1, fs, output, count)?
                }
                FileKind::Regular if path == Path::new("release.json") => {}
                FileKind::Regular => output.push(path),
                _ => return Err(InstallError::Tampered(root.join(path))),
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    walk(root, Path::new(""), 0, fs, &mut output, &mut 0)?;
    output.sort();
    Ok(output)
}
