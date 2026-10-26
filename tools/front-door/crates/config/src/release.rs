//! Verified staged releases and bounded packaging. Build orchestration calls
//! `package_release` after linking; runtime startup never builds itself.
use crate::inspection::InspectionEvidence;
use crate::managed::{InstallError, ReleaseSmoke, directory, normal_absolute, path_wire, snapshot};
use provenant_core::CancelToken;
use provenant_os::fs::{FileKind, PublishMode};
use provenant_records::{
    CanonicalV1, Limits, ReleaseFile, ReleaseManifest, SourceDigest, SourceInput, VersionInfo,
    source_digest,
};
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
    pub fn read(source: &Path, target: &str) -> Result<Self, InstallError> {
        let anchor = directory(source, false).map_err(|error| match error {
            InstallError::Fs(error) if error.is_not_found() => {
                InstallError::Tampered(source.join("release.json"))
            }
            other => other,
        })?;
        Self::read_held(source, target, anchor)
    }
    pub(crate) fn read_scoped_inspected(
        source: &Path,
        target: &str,
        state_root: &Path,
        mut evidence: Option<&mut InspectionEvidence>,
    ) -> Result<Self, InstallError> {
        let anchor = match crate::managed::directory_scoped(source, state_root, false) {
            Ok(anchor) => anchor,
            Err(InstallError::Fs(error)) if error.is_not_found() => {
                if let Some(evidence) = evidence.as_deref_mut() {
                    evidence.observe(b"snapshot", &source.join("release.json"), None);
                }
                return Err(InstallError::Fs(error));
            }
            Err(error) => return Err(error),
        };
        Self::read_held_inspected(source, target, anchor, evidence)
    }
    fn read_held(
        source: &Path,
        target: &str,
        anchor: provenant_os::fs::Directory,
    ) -> Result<Self, InstallError> {
        Self::read_held_inspected(source, target, anchor, None)
    }
    fn read_held_inspected(
        source: &Path,
        target: &str,
        anchor: provenant_os::fs::Directory,
        mut evidence: Option<&mut InspectionEvidence>,
    ) -> Result<Self, InstallError> {
        normal_absolute(source)?;
        let manifest_path = source.join("release.json");
        let Some(crate::managed::Snapshot {
            value: crate::managed::Value::File { bytes, mode: 0o600 },
            ..
        }) = crate::managed::snapshot_below_inspected(
            &manifest_path,
            source,
            &anchor,
            evidence.as_deref_mut(),
        )?
        else {
            return Err(InstallError::Tampered(manifest_path));
        };
        let manifest: ReleaseManifest =
            provenant_records::decode_v1(bytes.as_slice(), Limits::CONFIG)?;
        if manifest.target != target {
            return Err(InstallError::Platform);
        }
        check_bounds(&manifest)?;
        let actual = inventory_held_inspected(source, &anchor, false, evidence.as_deref_mut())?;
        let mut files = BTreeMap::new();
        for item in &manifest.files {
            let relative = item.path.to_relative_path()?.as_path().to_path_buf();
            validate_name(&relative, item.mode)?;
            let path = source.join(&relative);
            let current = crate::managed::snapshot_below_inspected(
                &path,
                source,
                &anchor,
                evidence.as_deref_mut(),
            )?;
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
        if !files.contains_key(Path::new("bin/provenant")) {
            return Err(InstallError::Manifest("missing executable"));
        }
        let expected: Vec<_> = files.keys().cloned().collect();
        if actual != expected {
            return Err(InstallError::Manifest(
                "inventory differs from release tree",
            ));
        }
        if asset_inventory_digest(&files)? != manifest.asset_digest {
            return Err(InstallError::Manifest(
                "asset inventory differs from release manifest",
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

fn is_product_asset(path: &Path) -> bool {
    [
        "skills",
        "library",
        "templates",
        "config",
        "instructions",
        "scripts",
        "runtime",
    ]
    .iter()
    .any(|root| path.starts_with(root) && path != Path::new(root))
}

/// Asset modes are normalized for installation: executables become 0700 and
/// other files 0600. The command binary retains its standard 0755 mode.
fn source_mode(path: &Path, actual: u64) -> Result<u64, InstallError> {
    if path == Path::new("bin/provenant") {
        if actual & 0o111 == 0 || actual & 0o7000 != 0 {
            return Err(InstallError::Manifest("binary must be executable"));
        }
        Ok(0o755)
    } else if is_product_asset(path) {
        if actual & 0o7000 != 0 {
            return Err(InstallError::Manifest("invalid asset mode"));
        }
        Ok(if actual & 0o111 == 0 { 0o600 } else { 0o700 })
    } else {
        Err(InstallError::Manifest("file outside release layout"))
    }
}
fn validate_name(path: &Path, mode: u64) -> Result<(), InstallError> {
    let allowed_mode = if path == Path::new("bin/provenant") {
        mode == 0o755
    } else {
        is_product_asset(path) && matches!(mode, 0o600 | 0o700)
    };
    if !allowed_mode {
        return Err(InstallError::Manifest(
            "file outside release layout or invalid mode",
        ));
    }
    Ok(())
}

pub(crate) fn asset_inventory_digest(
    files: &BTreeMap<PathBuf, (Vec<u8>, u64)>,
) -> Result<SourceDigest, InstallError> {
    let mut encoded = Vec::<(String, Vec<u8>)>::new();
    for (path, (bytes, mode)) in files {
        if path == Path::new("bin/provenant") {
            continue;
        }
        if !is_product_asset(path) || !matches!(mode, 0o600 | 0o700) {
            return Err(InstallError::Manifest(
                "file outside release asset inventory",
            ));
        }
        let name = path
            .to_str()
            .ok_or(InstallError::Manifest("release asset path is not UTF-8"))?;
        let mut contents = Vec::with_capacity(bytes.len() + 1);
        contents.push(u8::from(*mode == 0o700));
        contents.extend_from_slice(bytes);
        encoded.push((name.to_owned(), contents));
    }
    if encoded.is_empty() {
        return Err(InstallError::Manifest("release asset inventory is empty"));
    }
    let inputs: Vec<_> = encoded
        .iter()
        .map(|(name, bytes)| SourceInput { name, bytes })
        .collect();
    source_digest(&inputs).map_err(|_| InstallError::Manifest("invalid release asset inventory"))
}

/// Packages a staged tree only when its binary reports the expected build
/// identity and its release assets match the build-script inventory digest.
/// `release.json` is the completion marker and is always published last.
pub fn package_release(
    source: &Path,
    expected: &VersionInfo,
    smoke: &dyn ReleaseSmoke,
    cancel: &CancelToken,
) -> Result<Release, InstallError> {
    normal_absolute(source)?;
    let names = inventory(source)?;
    let mut files = BTreeMap::new();
    let mut release_files = Vec::new();
    for relative in names {
        let path = source.join(&relative);
        let current = crate::managed::snapshot_scoped(&path, source)?
            .ok_or_else(|| InstallError::Tampered(path.clone()))?;
        let crate::managed::Value::File { bytes, mode } = current.value else {
            return Err(InstallError::Tampered(path));
        };
        let mode = source_mode(&relative, mode)?;
        release_files.push(ReleaseFile {
            path: path_wire(&relative)?,
            digest: CanonicalV1::file_digest(&bytes),
            bytes: bytes.len() as u64,
            mode,
        });
        files.insert(relative, (bytes, mode));
    }
    let asset_digest = asset_inventory_digest(&files)?;
    if asset_digest != expected.asset_digest {
        return Err(InstallError::Manifest(
            "release assets differ from build inventory",
        ));
    }
    let manifest = ReleaseManifest {
        schema_version: 1,
        source_digest: expected.source_digest,
        asset_digest,
        target: expected.target.clone(),
        toolchain: expected.toolchain.clone(),
        files: release_files,
    };
    let bytes = provenant_records::encode(&manifest)?;
    let candidate = Release {
        manifest,
        manifest_bytes: bytes.clone(),
        files,
        source: source.into(),
    };
    let manifest_path = source.join("release.json");
    if let Some(old) = snapshot(&manifest_path)? {
        let crate::managed::Value::File {
            bytes: old_bytes,
            mode: 0o600,
        } = old.value
        else {
            return Err(InstallError::Tampered(manifest_path));
        };
        if old_bytes != bytes {
            return Err(InstallError::ReleaseConflict {
                existing_manifest_digest: CanonicalV1::file_digest(&old_bytes),
                requested_manifest_digest: CanonicalV1::file_digest(&bytes),
                recovery: "keep the completed release immutable; use a fresh state root or a new source digest for the other build",
            });
        }
        let release = Release::read(source, &expected.target)?;
        verify_binary_identity(&release, smoke, cancel)?;
        return Ok(release);
    }

    // A published manifest is immutable, so refuse it before publication.
    check_bounds(&candidate.manifest)?;
    verify_binary_identity(&candidate, smoke, cancel)?;
    normalise_staged_modes(source, &candidate.files)?;
    crate::managed::durable(directory(source, false)?.atomic_write_with_permissions(
        OsStr::new("release.json"),
        ".provenant-staging.",
        &bytes,
        0o600,
        PublishMode::NoReplace,
    )?)?;
    Release::read(source, &expected.target)
}

fn check_bounds(manifest: &ReleaseManifest) -> Result<(), InstallError> {
    if manifest.schema_version != 1 {
        return Err(InstallError::Manifest("unsupported schema version"));
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
    let mut total = 0u64;
    for item in &manifest.files {
        total = total
            .checked_add(item.bytes)
            .ok_or(InstallError::Manifest("release byte bound"))?;
        if total > RELEASE_LIMIT || item.bytes > FILE_LIMIT {
            return Err(InstallError::Manifest("release byte bound"));
        }
    }
    Ok(())
}

fn normalise_staged_modes(
    source: &Path,
    files: &BTreeMap<PathBuf, (Vec<u8>, u64)>,
) -> Result<(), InstallError> {
    for (relative, (expected_bytes, expected_mode)) in files {
        let path = source.join(relative);
        let Some(current) = crate::managed::snapshot_scoped(&path, source)? else {
            return Err(InstallError::Tampered(path));
        };
        let crate::managed::Value::File { bytes, mode } = current.value else {
            return Err(InstallError::Tampered(path));
        };
        if bytes != *expected_bytes {
            return Err(InstallError::Drift(path));
        }
        if mode == *expected_mode {
            continue;
        }
        let parent = path
            .parent()
            .ok_or_else(|| InstallError::Path(path.clone()))?;
        let name = path
            .file_name()
            .ok_or_else(|| InstallError::Path(path.clone()))?;
        let permissions = u32::try_from(*expected_mode)
            .map_err(|_| InstallError::Manifest("invalid release mode"))?;
        let published = crate::managed::directory_scoped(parent, source, false)?
            .atomic_write_with_permissions(
                name,
                ".provenant-staging.",
                expected_bytes,
                permissions,
                PublishMode::Replace,
            )?;
        crate::managed::durable(published)?;
    }
    Ok(())
}

pub(crate) fn verify_binary_identity(
    release: &Release,
    smoke: &dyn ReleaseSmoke,
    cancel: &CancelToken,
) -> Result<(), InstallError> {
    let actual = smoke.check(release, cancel).map_err(InstallError::Smoke)?;
    if actual.source_digest != release.manifest.source_digest
        || actual.asset_digest != release.manifest.asset_digest
        || actual.target != release.manifest.target
        || actual.toolchain != release.manifest.toolchain
    {
        return Err(InstallError::Manifest(
            "binary identity differs from release manifest",
        ));
    }
    Ok(())
}

pub(crate) fn inventory(root: &Path) -> Result<Vec<PathBuf>, InstallError> {
    inventory_held(root, &directory(root, false)?, false)
}

// These are the exact names produced by OS temporary_name and
// replace_link/atomic_write with the release staging prefix.
fn is_staging_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(".provenant-staging.") else {
        return false;
    };
    let random = suffix
        .strip_prefix(".provenant-staging.")
        .and_then(|name| name.strip_suffix(".tmp"))
        .unwrap_or(suffix);
    random.len() == 24
        && random
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn inventory_held(
    root: &Path,
    anchor: &provenant_os::fs::Directory,
    remove_staging: bool,
) -> Result<Vec<PathBuf>, InstallError> {
    inventory_held_inspected(root, anchor, remove_staging, None)
}
pub(crate) fn inventory_held_inspected(
    root: &Path,
    anchor: &provenant_os::fs::Directory,
    remove_staging: bool,
    evidence: Option<&mut InspectionEvidence>,
) -> Result<Vec<PathBuf>, InstallError> {
    struct Scan<'a> {
        count: usize,
        evidence: Option<&'a mut InspectionEvidence>,
    }
    fn allowed_directory(path: &Path) -> bool {
        path == Path::new("bin")
            || [
                "skills",
                "library",
                "templates",
                "config",
                "instructions",
                "scripts",
                "runtime",
            ]
            .iter()
            .any(|root| path.starts_with(root))
    }
    fn walk(
        root: &Path,
        tail: &Path,
        depth: usize,
        dir: &provenant_os::fs::Directory,
        output: &mut Vec<PathBuf>,
        scan: &mut Scan<'_>,
        remove_staging: bool,
    ) -> Result<(), InstallError> {
        if depth > 64 {
            return Err(InstallError::Manifest("inventory bounds"));
        }
        let mut names = Vec::new();
        for entry in dir.entries()? {
            names.push(entry?);
            scan.count += 1;
            if scan.count > ENTRY_LIMIT {
                return Err(InstallError::Manifest("inventory bounds"));
            }
        }
        names.sort();
        if let Some(evidence) = scan.evidence.as_deref_mut() {
            for name in &names {
                evidence.observe(b"inventory-entry", &root.join(tail).join(name), Some(&[]));
            }
        }
        for name in names {
            let path = tail.join(&name);
            let metadata = dir.metadata_at(&name, false)?;
            if let Some(evidence) = scan.evidence.as_deref_mut() {
                evidence.metadata(&root.join(&path), metadata);
            }
            if matches!(metadata.kind, FileKind::Regular | FileKind::Symlink)
                && is_staging_name(&name)
            {
                if remove_staging {
                    dir.unlink(&name)?;
                    dir.sync()?;
                }
                continue;
            }
            match metadata.kind {
                FileKind::Directory if allowed_directory(&path) => {
                    #[cfg(test)]
                    crate::managed::tests::fault_directory(&name)?;
                    walk(
                        root,
                        &path,
                        depth + 1,
                        &dir.open_verified_directory(&name)?,
                        output,
                        scan,
                        remove_staging,
                    )?;
                }
                FileKind::Regular if path == Path::new("release.json") => {}
                FileKind::Regular => output.push(path),
                _ => return Err(InstallError::Tampered(root.join(path))),
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    walk(
        root,
        Path::new(""),
        0,
        anchor,
        &mut output,
        &mut Scan { count: 0, evidence },
        remove_staging,
    )?;
    output.sort();
    Ok(output)
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
